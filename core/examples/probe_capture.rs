//! Report the structure of pcap / pcapng / MF4 files, to plan how nerka
//! imports DLT from them. No dependencies.
//!
//!     cargo run --release -p nerka-core --example probe_capture -- trace.pcapng log.mf4
//!
//! Only structure is printed: file format, link types, protocol and port
//! counts, how DLT is framed (storage header, messages per datagram,
//! header flags), MF4 blocks, channel groups and channel names. Payload
//! bytes, IP and MAC addresses and DLT ids are never printed (DLT ECU /
//! APID / CTID only as counts of distinct values). With `--no-names`, MF4
//! channel and group names are replaced by `#n`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};

fn main() {
    let mut no_names = false;
    let mut files = Vec::new();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--no-names" => no_names = true,
            "-h" | "--help" => {
                eprintln!("usage: probe_capture [--no-names] FILE...");
                return;
            }
            _ => files.push(a),
        }
    }
    if files.is_empty() {
        eprintln!("usage: probe_capture [--no-names] FILE...");
        std::process::exit(2);
    }
    for f in &files {
        println!("=== {}", file_name(f));
        if let Err(e) = probe(f, no_names) {
            println!("error: {e}");
        }
        println!();
    }
}

/// The file name without its directories (paths may be private).
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn probe(path: &str, no_names: bool) -> io::Result<()> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    println!("size: {size} bytes ({:.1} MiB)", size as f64 / 1048576.0);
    let mut head = [0u8; 8];
    let n = f.read(&mut head)?;
    f.seek(SeekFrom::Start(0))?;
    let magic = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    match (n, magic, &head) {
        (8.., 0x0A0D_0D0A, _) => pcapng(f),
        (4.., 0xA1B2_C3D4 | 0xD4C3_B2A1 | 0xA1B2_3C4D | 0x4D3C_B2A1, _) => pcap(f),
        (8.., _, b"MDF     ") => mf4(f, no_names),
        _ => {
            println!("format: unknown (first bytes as hex: {})", hex(&head[..n.min(8)]));
            Ok(())
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn u16_at(b: &[u8], at: usize, be: bool) -> u16 {
    let x = [b[at], b[at + 1]];
    if be { u16::from_be_bytes(x) } else { u16::from_le_bytes(x) }
}

fn u32_at(b: &[u8], at: usize, be: bool) -> u32 {
    let x = [b[at], b[at + 1], b[at + 2], b[at + 3]];
    if be { u32::from_be_bytes(x) } else { u32::from_le_bytes(x) }
}

fn u64_le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

// ---------------------------------------------------------------- packets

/// Counts over all packets of a capture.
#[derive(Default)]
struct Stats {
    packets: u64,
    bytes: u64,
    truncated: u64,
    link_types: BTreeMap<u32, u64>,
    ethertypes: BTreeMap<u16, u64>,
    vlan_tagged: u64,
    ip_protos: BTreeMap<u8, u64>,
    ipv4_fragments_first: u64,
    ipv4_fragments_rest: u64,
    ipv6_fragments: u64,
    udp_ports: BTreeMap<u16, Port>,
    tcp_ports: BTreeMap<u16, Port>,
    undecoded: BTreeMap<&'static str, u64>,
    ts_min: Option<f64>,
    ts_max: Option<f64>,
    ts_backwards: u64,
    ts_last: Option<f64>,
}

/// Counts for one destination port.
#[derive(Default)]
struct Port {
    datagrams: u64,
    bytes: u64,
    /// Datagrams (or TCP segments) that start with a DLT message.
    dlt_start: u64,
    /// ... and are exactly a whole number of DLT messages.
    dlt_exact: u64,
    dlt_msgs: u64,
    max_msgs_per_datagram: u64,
    storage_header: u64,
    weid: u64,
    wsid: u64,
    wtms: u64,
    ueh: u64,
    verbose: u64,
    big_endian_payload: u64,
    ecus: BTreeSet<[u8; 4]>,
    apids: BTreeSet<[u8; 4]>,
    ctids: BTreeSet<[u8; 4]>,
    msg_len_max: u64,
}

impl Stats {
    fn time(&mut self, t: f64) {
        self.ts_min = Some(self.ts_min.map_or(t, |m| m.min(t)));
        self.ts_max = Some(self.ts_max.map_or(t, |m| m.max(t)));
        if self.ts_last.is_some_and(|l| t < l) {
            self.ts_backwards += 1;
        }
        self.ts_last = Some(t);
    }

    fn packet(&mut self, link: u32, data: &[u8], orig_len: u64) {
        self.packets += 1;
        self.bytes += data.len() as u64;
        if (data.len() as u64) < orig_len {
            self.truncated += 1;
        }
        *self.link_types.entry(link).or_default() += 1;
        match link {
            1 => self.ethernet(data),
            113 if data.len() >= 16 => self.ether_payload(u16_at(data, 14, true), &data[16..]),
            276 if data.len() >= 20 => self.ether_payload(u16_at(data, 0, true), &data[20..]),
            101 | 12 | 14 => self.raw_ip(data),
            228 => self.ipv4(data),
            229 => self.ipv6(data),
            0 | 108 if data.len() >= 4 => {
                // BSD loopback: address family in host order.
                let fam = u32::from_le_bytes(data[..4].try_into().unwrap())
                    .min(u32::from_be_bytes(data[..4].try_into().unwrap()));
                match fam {
                    2 => self.ipv4(&data[4..]),
                    24 | 28 | 30 => self.ipv6(&data[4..]),
                    _ => self.miss("loopback family"),
                }
            }
            _ => self.miss("link type"),
        }
    }

    fn miss(&mut self, what: &'static str) {
        *self.undecoded.entry(what).or_default() += 1;
    }

    fn ethernet(&mut self, d: &[u8]) {
        if d.len() < 14 {
            return self.miss("short ethernet");
        }
        self.ether_payload(u16_at(d, 12, true), &d[14..]);
    }

    fn ether_payload(&mut self, mut ty: u16, mut d: &[u8]) {
        let mut tagged = false;
        while matches!(ty, 0x8100 | 0x88A8 | 0x9100) && d.len() >= 4 {
            tagged = true;
            ty = u16_at(d, 2, true);
            d = &d[4..];
        }
        if tagged {
            self.vlan_tagged += 1;
        }
        *self.ethertypes.entry(ty).or_default() += 1;
        match ty {
            0x0800 => self.ipv4(d),
            0x86DD => self.ipv6(d),
            _ => {}
        }
    }

    fn raw_ip(&mut self, d: &[u8]) {
        match d.first().map(|b| b >> 4) {
            Some(4) => self.ipv4(d),
            Some(6) => self.ipv6(d),
            _ => self.miss("raw ip version"),
        }
    }

    fn ipv4(&mut self, d: &[u8]) {
        if d.len() < 20 || d[0] >> 4 != 4 {
            return self.miss("ipv4 header");
        }
        let ihl = (d[0] & 0x0F) as usize * 4;
        let total = (u16_at(d, 2, true) as usize).min(d.len());
        let frag = u16_at(d, 6, true);
        let (more, offset) = (frag & 0x2000 != 0, frag & 0x1FFF);
        let proto = d[9];
        *self.ip_protos.entry(proto).or_default() += 1;
        if offset != 0 {
            self.ipv4_fragments_rest += 1;
            return;
        }
        if more {
            self.ipv4_fragments_first += 1;
        }
        if ihl < 20 || total < ihl {
            return self.miss("ipv4 length");
        }
        self.transport(proto, &d[ihl..total], more);
    }

    fn ipv6(&mut self, d: &[u8]) {
        if d.len() < 40 || d[0] >> 4 != 6 {
            return self.miss("ipv6 header");
        }
        let mut next = d[6];
        let end = (40 + u16_at(d, 4, true) as usize).min(d.len());
        let mut at = 40;
        // Extension headers: hop-by-hop, routing, fragment, destination.
        loop {
            match next {
                0 | 43 | 60 if at + 8 <= end => {
                    let len = (d[at + 1] as usize + 1) * 8;
                    next = d[at];
                    at += len;
                }
                44 if at + 8 <= end => {
                    self.ipv6_fragments += 1;
                    let offset = u16_at(d, at + 2, true) >> 3;
                    next = d[at];
                    at += 8;
                    if offset != 0 {
                        *self.ip_protos.entry(next).or_default() += 1;
                        return;
                    }
                }
                _ => break,
            }
        }
        *self.ip_protos.entry(next).or_default() += 1;
        if at <= end {
            self.transport(next, &d[at..end], false);
        }
    }

    fn transport(&mut self, proto: u8, d: &[u8], fragmented: bool) {
        match proto {
            17 if d.len() >= 8 => {
                let port = u16_at(d, 2, true);
                let p = self.udp_ports.entry(port).or_default();
                p.datagrams += 1;
                p.bytes += d.len() as u64 - 8;
                // A first fragment holds only the start of the datagram.
                dlt_scan(p, &d[8..], !fragmented);
            }
            6 if d.len() >= 20 => {
                let port = u16_at(d, 2, true);
                let off = ((d[12] >> 4) as usize * 4).min(d.len());
                let payload = &d[off..];
                if payload.is_empty() {
                    return;
                }
                let p = self.tcp_ports.entry(port).or_default();
                p.datagrams += 1;
                p.bytes += payload.len() as u64;
                dlt_scan(p, payload, true);
            }
            17 | 6 => self.miss("short udp/tcp header"),
            _ => {}
        }
    }

    fn report(&self) {
        println!("packets: {} ({} captured bytes, {} truncated by snaplen)", self.packets, self.bytes, self.truncated);
        if let (Some(a), Some(b)) = (self.ts_min, self.ts_max) {
            println!("time span: {:.3} s; timestamps going backwards: {}", b - a, self.ts_backwards);
        }
        println!("link types: {}", list(&self.link_types, |k| link_name(*k as u16)));
        println!("ethertypes: {}  (VLAN tagged: {})", list(&self.ethertypes, |k| ethertype(*k)), self.vlan_tagged);
        println!("ip protocols: {}", list(&self.ip_protos, |k| ip_proto(*k)));
        println!(
            "fragments: ipv4 first {} / later {}, ipv6 {}",
            self.ipv4_fragments_first, self.ipv4_fragments_rest, self.ipv6_fragments
        );
        if !self.undecoded.is_empty() {
            println!("not decoded: {}", list(&self.undecoded, |k| (*k).to_owned()));
        }
        for (name, ports) in [("UDP", &self.udp_ports), ("TCP", &self.tcp_ports)] {
            if ports.is_empty() {
                continue;
            }
            println!("{name} destination ports (top 25 by count):");
            let mut v: Vec<_> = ports.iter().collect();
            v.sort_by_key(|(_, p)| std::cmp::Reverse(p.datagrams));
            for (port, p) in v.iter().take(25) {
                let unit = if name == "UDP" { "datagrams" } else { "segments" };
                print!("  {port:>5}: {} {unit}, {} payload bytes", p.datagrams, p.bytes);
                if p.dlt_start > 0 {
                    print!(
                        "; DLT-like start {} (whole messages {}), {} msgs (max {}/{}), max msg {} B, \
                         storage hdr {}, WEID {} WSID {} WTMS {} UEH {} verbose {} MSBF {}, \
                         distinct ECU {} APID {} CTID {}",
                        p.dlt_start,
                        p.dlt_exact,
                        p.dlt_msgs,
                        p.max_msgs_per_datagram,
                        if name == "UDP" { "datagram" } else { "segment" },
                        p.msg_len_max,
                        p.storage_header,
                        p.weid,
                        p.wsid,
                        p.wtms,
                        p.ueh,
                        p.verbose,
                        p.big_endian_payload,
                        p.ecus.len(),
                        p.apids.len(),
                        p.ctids.len()
                    );
                }
                println!();
            }
            if ports.len() > 25 {
                println!("  … {} more ports", ports.len() - 25);
            }
        }
    }
}

/// Walk DLT messages at the start of `d`; count what the headers say.
/// With `whole`, `d` is the complete datagram.
fn dlt_scan(p: &mut Port, mut d: &[u8], whole: bool) {
    let mut msgs = 0u64;
    let mut first = true;
    loop {
        let mut storage = false;
        let mut m = d;
        if m.len() >= 16 && &m[..4] == b"DLT\x01" {
            storage = true;
            m = &m[16..];
        }
        if m.len() < 4 {
            break;
        }
        let htyp = m[0];
        let len = u16_at(m, 2, true) as usize;
        let std_len = 4 + 4 * [0x04, 0x08, 0x10].iter().filter(|&&f| htyp & f != 0).count();
        let ext = if htyp & 0x01 != 0 { 10 } else { 0 };
        let plausible = (htyp >> 5) & 0x07 == 1 && len >= std_len + ext && len <= m.len();
        if !plausible {
            break;
        }
        if first {
            p.dlt_start += 1;
            first = false;
        }
        msgs += 1;
        p.msg_len_max = p.msg_len_max.max(len as u64);
        p.storage_header += storage as u64;
        p.weid += (htyp & 0x04 != 0) as u64;
        p.wsid += (htyp & 0x08 != 0) as u64;
        p.wtms += (htyp & 0x10 != 0) as u64;
        p.big_endian_payload += (htyp & 0x02 != 0) as u64;
        if htyp & 0x04 != 0 {
            p.ecus.insert(m[4..8].try_into().unwrap());
        }
        if ext > 0 {
            p.ueh += 1;
            let e = &m[std_len..std_len + ext];
            p.verbose += (e[0] & 0x01) as u64;
            p.apids.insert(e[2..6].try_into().unwrap());
            p.ctids.insert(e[6..10].try_into().unwrap());
        }
        let used = len + if storage { 16 } else { 0 };
        d = &d[used..];
        if d.is_empty() {
            if whole {
                p.dlt_exact += 1;
            }
            break;
        }
    }
    p.dlt_msgs += msgs;
    p.max_msgs_per_datagram = p.max_msgs_per_datagram.max(msgs);
}

fn list<K>(m: &BTreeMap<K, u64>, name: impl Fn(&K) -> String) -> String {
    if m.is_empty() {
        return "none".into();
    }
    m.iter().map(|(k, n)| format!("{} ×{n}", name(k))).collect::<Vec<_>>().join(", ")
}

fn link_name(l: u16) -> String {
    let n = match l {
        0 => "null/loopback",
        1 => "ethernet",
        12 | 14 | 101 => "raw ip",
        108 => "loopback",
        113 => "linux sll",
        227 => "socketcan",
        228 => "ipv4",
        229 => "ipv6",
        276 => "linux sll2",
        _ => "",
    };
    if n.is_empty() { format!("linktype {l}") } else { format!("{n} ({l})") }
}

fn ethertype(t: u16) -> String {
    let n = match t {
        0x0800 => "ipv4",
        0x86DD => "ipv6",
        0x0806 => "arp",
        0x88F7 => "ptp",
        0x22F0 => "avtp",
        0x88CC => "lldp",
        _ => "",
    };
    if n.is_empty() { format!("0x{t:04x}") } else { format!("{n} (0x{t:04x})") }
}

fn ip_proto(p: u8) -> String {
    match p {
        6 => "tcp".into(),
        17 => "udp".into(),
        1 => "icmp".into(),
        58 => "icmpv6".into(),
        2 => "igmp".into(),
        _ => format!("proto {p}"),
    }
}

// ------------------------------------------------------------------- pcap

fn pcap(f: File) -> io::Result<()> {
    let mut r = BufReader::with_capacity(1 << 20, f);
    let mut h = [0u8; 24];
    r.read_exact(&mut h)?;
    let magic = u32::from_le_bytes(h[..4].try_into().unwrap());
    let be = matches!(magic, 0xD4C3_B2A1 | 0x4D3C_B2A1);
    let nanos = matches!(magic, 0xA1B2_3C4D | 0x4D3C_B2A1);
    let link = u32_at(&h, 20, be);
    println!(
        "format: pcap {}.{} ({} endian, {} timestamps), snaplen {}, link type {}",
        u16_at(&h, 4, be),
        u16_at(&h, 6, be),
        if be { "big" } else { "little" },
        if nanos { "ns" } else { "µs" },
        u32_at(&h, 16, be),
        link_name((link & 0xFFFF) as u16)
    );
    let mut s = Stats::default();
    let mut rec = [0u8; 16];
    let mut data = Vec::new();
    loop {
        match r.read_exact(&mut rec) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let (sec, frac, incl, orig) =
            (u32_at(&rec, 0, be), u32_at(&rec, 4, be), u32_at(&rec, 8, be), u32_at(&rec, 12, be));
        if incl > 256 << 20 {
            println!("corrupt record length {incl}; stopping");
            break;
        }
        data.resize(incl as usize, 0);
        if r.read_exact(&mut data).is_err() {
            println!("file ends inside a record");
            break;
        }
        s.time(sec as f64 + frac as f64 / if nanos { 1e9 } else { 1e6 });
        s.packet(link & 0xFFFF, &data, orig as u64);
    }
    s.report();
    Ok(())
}

// ----------------------------------------------------------------- pcapng

struct Interface {
    link: u32,
    snaplen: u32,
    /// Seconds per timestamp unit.
    unit: f64,
}

fn pcapng(f: File) -> io::Result<()> {
    let mut r = BufReader::with_capacity(1 << 20, f);
    let mut s = Stats::default();
    let mut be = false;
    let mut ifaces: Vec<Interface> = Vec::new();
    let mut blocks: BTreeMap<u32, u64> = BTreeMap::new();
    let mut sections = 0;
    let mut iface_report: Vec<String> = Vec::new();
    let mut head = [0u8; 8];
    let mut body = Vec::new();
    loop {
        match r.read_exact(&mut head) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let raw_ty = u32::from_le_bytes(head[..4].try_into().unwrap());
        if raw_ty == 0x0A0D_0D0A {
            // Section header: its byte-order magic decides the endianness.
            let mut bom = [0u8; 4];
            r.read_exact(&mut bom)?;
            be = u32::from_le_bytes(bom) != 0x1A2B_3C4D;
            let len = u32_at(&head, 4, be) as usize;
            body.resize(len.saturating_sub(12), 0);
            r.read_exact(&mut body)?;
            sections += 1;
            ifaces.clear();
            *blocks.entry(raw_ty).or_default() += 1;
            continue;
        }
        let ty = u32_at(&head, 0, be);
        let len = u32_at(&head, 4, be) as usize;
        if !(12..=256 << 20).contains(&len) {
            println!("corrupt block length {len}; stopping");
            break;
        }
        body.resize(len - 8, 0);
        if r.read_exact(&mut body).is_err() {
            println!("file ends inside a block");
            break;
        }
        let b = &body[..len - 12];
        *blocks.entry(ty).or_default() += 1;
        match ty {
            1 if b.len() >= 8 => {
                let link = u16_at(b, 0, be) as u32;
                let snaplen = u32_at(b, 4, be);
                let mut unit = 1e-6;
                let mut opts = Vec::new();
                let mut at = 8;
                while at + 4 <= b.len() {
                    let (code, olen) = (u16_at(b, at, be), u16_at(b, at + 2, be) as usize);
                    if code == 0 {
                        break;
                    }
                    if code == 9 && olen >= 1 && at + 4 < b.len() {
                        let v = b[at + 4];
                        unit = if v & 0x80 != 0 { 2f64.powi(-((v & 0x7F) as i32)) } else { 10f64.powi(-(v as i32)) };
                    }
                    opts.push(match code {
                        2 => "if_name",
                        3 => "if_description",
                        9 => "if_tsresol",
                        12 => "if_os",
                        13 => "if_fcslen",
                        14 => "if_tsoffset",
                        _ => "other",
                    });
                    at += 4 + olen.div_ceil(4) * 4;
                }
                iface_report.push(format!(
                    "  interface {}: {}, snaplen {snaplen}, timestamp unit {unit:e} s, options [{}]",
                    ifaces.len(),
                    link_name(link as u16),
                    opts.join(", ")
                ));
                ifaces.push(Interface { link, snaplen, unit });
            }
            6 if b.len() >= 20 => {
                let id = u32_at(b, 0, be) as usize;
                let ts = ((u32_at(b, 4, be) as u64) << 32) | u32_at(b, 8, be) as u64;
                let (cap, orig) = (u32_at(b, 12, be) as usize, u32_at(b, 16, be) as u64);
                let Some(i) = ifaces.get(id) else {
                    s.miss("packet for unknown interface");
                    continue;
                };
                s.time(ts as f64 * i.unit);
                s.packet(i.link, &b[20..(20 + cap).min(b.len())], orig);
            }
            3 if b.len() >= 4 => {
                let Some(i) = ifaces.first() else { continue };
                let orig = u32_at(b, 0, be) as usize;
                let cap = orig.min(i.snaplen as usize).min(b.len() - 4);
                s.packet(i.link, &b[4..4 + cap], orig as u64);
            }
            2 if b.len() >= 20 => {
                let id = u16_at(b, 0, be) as usize;
                let ts = ((u32_at(b, 4, be) as u64) << 32) | u32_at(b, 8, be) as u64;
                let (cap, orig) = (u32_at(b, 12, be) as usize, u32_at(b, 16, be) as u64);
                let Some(i) = ifaces.get(id) else { continue };
                s.time(ts as f64 * i.unit);
                s.packet(i.link, &b[20..(20 + cap).min(b.len())], orig);
            }
            _ => {}
        }
    }
    println!("format: pcapng ({} endian), {sections} section(s)", if be { "big" } else { "little" });
    for l in &iface_report {
        println!("{l}");
    }
    let block_name = |t: &u32| {
        match *t {
            0x0A0D_0D0A => "section header",
            1 => "interface description",
            2 => "packet (obsolete)",
            3 => "simple packet",
            4 => "name resolution",
            5 => "interface statistics",
            6 => "enhanced packet",
            7 => "irig timestamp",
            10 => "decryption secrets",
            0x0000_0BAD | 0x4000_0BAD => "custom",
            _ => "",
        }
        .to_owned()
    };
    println!(
        "blocks: {}",
        list(&blocks, |t| match block_name(t) {
            n if n.is_empty() => format!("0x{t:08x}"),
            n => n,
        })
    );
    s.report();
    Ok(())
}

// -------------------------------------------------------------------- MF4

struct Mdf {
    f: File,
    size: u64,
    no_names: bool,
    names: BTreeMap<String, usize>,
    /// Data blocks reached from data groups and channels: type -> (count, bytes).
    data_blocks: BTreeMap<String, (u64, u64)>,
    zip_types: BTreeMap<u8, u64>,
    seen: BTreeSet<u64>,
    lines: usize,
}

/// One block: id (`HD`, `DG`, …), links and data.
struct Block {
    id: String,
    len: u64,
    links: Vec<u64>,
    data: Vec<u8>,
}

impl Mdf {
    fn block(&mut self, at: u64) -> io::Result<Option<Block>> {
        if at == 0 || at + 24 > self.size {
            return Ok(None);
        }
        let mut h = [0u8; 24];
        self.f.seek(SeekFrom::Start(at))?;
        self.f.read_exact(&mut h)?;
        if &h[..2] != b"##" {
            return Ok(None);
        }
        let id = String::from_utf8_lossy(&h[2..4]).into_owned();
        let len = u64_le(&h, 8);
        let nlinks = u64_le(&h, 16);
        if len < 24 || nlinks.saturating_mul(8) > len - 24 || at + len > self.size {
            return Ok(None);
        }
        // Link section always; data only for small blocks (no payload: data
        // blocks are measured, not read).
        let data_len = len - 24 - nlinks * 8;
        let read_data = !matches!(id.as_str(), "DT" | "SD" | "RD" | "DZ" | "RV" | "RI") || id == "DZ";
        let want = if read_data { data_len.min(if id == "DZ" { 24 } else { 1 << 16 }) } else { 0 };
        let mut buf = vec![0u8; (nlinks * 8 + want) as usize];
        self.f.read_exact(&mut buf)?;
        let links = (0..nlinks as usize).map(|i| u64_le(&buf, i * 8)).collect();
        let data = buf[nlinks as usize * 8..].to_vec();
        Ok(Some(Block { id, len, links, data }))
    }

    fn text(&mut self, at: u64) -> io::Result<String> {
        let Some(b) = self.block(at)? else { return Ok(String::new()) };
        if b.id != "TX" {
            return Ok(format!("<{}>", b.id));
        }
        let end = b.data.iter().position(|&c| c == 0).unwrap_or(b.data.len());
        Ok(String::from_utf8_lossy(&b.data[..end]).into_owned())
    }

    /// A name to print: as is, or `#n` with `--no-names`.
    fn name(&mut self, s: String) -> String {
        if !self.no_names || s.is_empty() {
            return s;
        }
        let n = self.names.len();
        format!("#{}", self.names.entry(s).or_insert(n))
    }

    fn out(&mut self, s: String) {
        const MAX_LINES: usize = 3000;
        self.lines += 1;
        if self.lines <= MAX_LINES {
            println!("{s}");
        } else if self.lines == MAX_LINES + 1 {
            println!("… (output cut at {MAX_LINES} lines)");
        }
    }

    /// Measure the data reached from `at` (DT, DZ, DL, HL, SD, …).
    fn data(&mut self, at: u64, depth: usize) -> io::Result<()> {
        if depth > 64 || !self.seen.insert(at) {
            return Ok(());
        }
        let Some(b) = self.block(at)? else { return Ok(()) };
        let e = self.data_blocks.entry(b.id.clone()).or_default();
        e.0 += 1;
        e.1 += b.len;
        match b.id.as_str() {
            "DZ" if b.data.len() >= 4 => {
                *self.zip_types.entry(b.data[2]).or_default() += 1;
            }
            "DL" => {
                // dl_dl_next, then the data blocks.
                let links = b.links.clone();
                for &l in links.iter().skip(1) {
                    self.data(l, depth + 1)?;
                }
                if let Some(&next) = links.first() {
                    self.data(next, depth + 1)?;
                }
            }
            "HL" | "LD" => {
                for &l in b.links.clone().iter() {
                    self.data(l, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn source(&mut self, at: u64) -> io::Result<String> {
        let Some(b) = self.block(at)? else { return Ok(String::new()) };
        if b.id != "SI" || b.data.len() < 3 {
            return Ok(String::new());
        }
        let name = self.text(b.links[0])?;
        let name = self.name(name);
        let src = ["other", "ecu", "bus", "i/o", "tool", "user"].get(b.data[0] as usize).copied().unwrap_or("?");
        let bus = ["none", "other", "CAN", "LIN", "MOST", "FlexRay", "K-Line", "Ethernet", "USB"]
            .get(b.data[1] as usize)
            .copied()
            .unwrap_or("?");
        Ok(format!(" source[{name}: {src}, bus {bus}, flags 0x{:02x}]", b.data[2]))
    }

    fn channels(&mut self, mut at: u64, indent: usize, depth: usize) -> io::Result<()> {
        let mut n = 0;
        while let Some(b) = self.block(at)? {
            if b.id != "CN" || depth > 16 {
                break;
            }
            n += 1;
            let pad = " ".repeat(indent);
            if n > 200 {
                self.out(format!("{pad}… more channels"));
                break;
            }
            let name = self.text(b.links[2])?;
            let name = self.name(name);
            let d = &b.data;
            let (ty, sync, dt) = (d.first().copied(), d.get(1).copied(), d.get(2).copied());
            let ty_s = match ty {
                Some(0) => "fixed",
                Some(1) => "VLSD",
                Some(2) => "master",
                Some(3) => "virtual master",
                Some(4) => "sync",
                Some(5) => "MLSD",
                Some(6) => "virtual data",
                _ => "?",
            };
            let sync_s = match sync {
                Some(0) => "",
                Some(1) => " sync=time",
                Some(2) => " sync=angle",
                Some(3) => " sync=distance",
                Some(4) => " sync=index",
                _ => " sync=?",
            };
            let dt_s = match dt {
                Some(0) => "uint LE",
                Some(1) => "uint BE",
                Some(2) => "int LE",
                Some(3) => "int BE",
                Some(4) => "float LE",
                Some(5) => "float BE",
                Some(6) => "string latin1",
                Some(7) => "string utf8",
                Some(8) => "string utf16le",
                Some(9) => "string utf16be",
                Some(10) => "byte array",
                Some(11) => "mime sample",
                Some(12) => "mime stream",
                Some(13) => "can open date",
                Some(14) => "can open time",
                _ => "?",
            };
            let (byte_off, bits) = if d.len() >= 12 {
                (u32::from_le_bytes(d[4..8].try_into().unwrap()), u32::from_le_bytes(d[8..12].try_into().unwrap()))
            } else {
                (0, 0)
            };
            let flags = if d.len() >= 16 { u32::from_le_bytes(d[12..16].try_into().unwrap()) } else { 0 };
            let src = if b.links[3] != 0 { self.source(b.links[3])? } else { String::new() };
            let conv = if b.links[4] != 0 { " conversion" } else { "" };
            self.out(format!(
                "{pad}CN {name}: {ty_s}{sync_s}, {dt_s}, byte {byte_off} bit {} × {bits} bits, flags 0x{flags:x}{conv}{src}",
                d.get(3).copied().unwrap_or(0)
            ));
            if b.links[5] != 0 {
                self.data(b.links[5], 0)?;
            }
            // Composition: a structure (CN list) or an array (CA).
            let comp = b.links[1];
            if comp != 0 {
                match self.block(comp)? {
                    Some(c) if c.id == "CN" => self.channels(comp, indent + 4, depth + 1)?,
                    Some(c) => self.out(format!("{pad}    composition: {} block", c.id)),
                    None => {}
                }
            }
            at = b.links[0];
        }
        Ok(())
    }
}

fn mf4(f: File, no_names: bool) -> io::Result<()> {
    let size = f.metadata()?.len();
    let mut m = Mdf {
        f,
        size,
        no_names,
        names: BTreeMap::new(),
        data_blocks: BTreeMap::new(),
        zip_types: BTreeMap::new(),
        seen: BTreeSet::new(),
        lines: 0,
    };
    let mut id = [0u8; 64];
    m.f.read_exact(&mut id)?;
    let ver = String::from_utf8_lossy(&id[8..16]).trim().to_owned();
    let prog = String::from_utf8_lossy(&id[16..24]).trim().to_owned();
    println!("format: MDF {ver} (version number {}), program '{prog}'", u16::from_le_bytes([id[28], id[29]]));
    if !ver.starts_with('4') {
        println!("only MDF 4 is probed");
        return Ok(());
    }
    let Some(hd) = m.block(64)? else {
        println!("no HD block at offset 64");
        return Ok(());
    };
    let start_ns = if hd.data.len() >= 8 { u64_le(&hd.data, 0) } else { 0 };
    let tflags = hd.data.get(12).copied().unwrap_or(0);
    println!(
        "HD: start time {} ns since 1970 (time flags 0x{tflags:02x}), file history {}, attachments {}, events {}",
        start_ns,
        if hd.links.get(1).is_some_and(|&l| l != 0) { "yes" } else { "no" },
        if hd.links.get(3).is_some_and(|&l| l != 0) { "yes" } else { "no" },
        if hd.links.get(4).is_some_and(|&l| l != 0) { "yes" } else { "no" },
    );
    let mut dg_at = hd.links.first().copied().unwrap_or(0);
    let mut dgs = 0;
    while let Some(dg) = m.block(dg_at)? {
        if dg.id != "DG" {
            break;
        }
        dgs += 1;
        let rec_id = dg.data.first().copied().unwrap_or(0);
        let data_kind = match m.block(dg.links[2])? {
            Some(b) => b.id,
            None => "none".into(),
        };
        m.out(format!("DG {dgs}: record id size {rec_id} B, data in {data_kind}"));
        m.data(dg.links[2], 0)?;
        let mut cg_at = dg.links[1];
        while let Some(cg) = m.block(cg_at)? {
            if cg.id != "CG" {
                break;
            }
            let acq = m.text(cg.links[2])?;
            let acq = m.name(acq);
            let src = m.source(cg.links[3])?;
            let d = &cg.data;
            let (rid, cycles) = if d.len() >= 16 { (u64_le(d, 0), u64_le(d, 8)) } else { (0, 0) };
            let flags = if d.len() >= 18 { u16::from_le_bytes([d[16], d[17]]) } else { 0 };
            let (bytes, inval) = if d.len() >= 32 {
                (u32::from_le_bytes(d[24..28].try_into().unwrap()), u32::from_le_bytes(d[28..32].try_into().unwrap()))
            } else {
                (0, 0)
            };
            let mut f = Vec::new();
            for (bit, n) in
                [(0, "VLSD"), (1, "bus event"), (2, "plain bus event"), (3, "remote master"), (4, "event signal")]
            {
                if flags & (1 << bit) != 0 {
                    f.push(n);
                }
            }
            m.out(format!(
                "  CG '{acq}': record id {rid}, {cycles} records of {bytes}+{inval} B, flags [{}]{src}",
                f.join(", ")
            ));
            m.channels(cg.links[1], 4, 0)?;
            cg_at = cg.links[0];
        }
        dg_at = dg.links[0];
    }
    println!("data groups: {dgs}");
    println!(
        "data blocks: {}",
        if m.data_blocks.is_empty() {
            "none".into()
        } else {
            m.data_blocks.iter().map(|(k, (n, b))| format!("{k} ×{n} ({b} B)")).collect::<Vec<_>>().join(", ")
        }
    );
    if !m.zip_types.is_empty() {
        let z =
            m.zip_types.iter().map(|(k, n)| format!("{} ×{n}", if *k == 0 { "deflate" } else { "transpose+deflate" }));
        println!("DZ compression: {}", z.collect::<Vec<_>>().join(", "));
    }
    Ok(())
}

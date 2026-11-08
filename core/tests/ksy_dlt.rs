//! The DLT definition (`data/dlt.ksy`) against Nerka's native DLT reader:
//! the same messages, split the same way, with the same fields, levels,
//! uptimes, lines and payload text (verbose arguments of every kind,
//! control messages, non-verbose ids; no FIBEX). Then both in one capture, on two UDP ports: every query gives the
//! same lines for `proto == DLT` and `proto == DLTK`.
//!
//! A test of the protocol definitions, not a feature: it uses the public
//! API only (doc/protocol-definitions.md).

use nerka_core::input::capture::route::Match;
use nerka_core::input::capture::{self, CaptureOptions};
use nerka_core::input::dlt::Dlt;
use nerka_core::input::proto::ksy::KsyProtocol;
use nerka_core::input::proto::{LoadedProtocol, Origin, Protocol};
use nerka_core::{FieldValue, Filter, LogSource, Progress, Query, search};
use std::sync::Arc;

const DEFINITION: &str = include_str!("data/dlt.ksy");

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

const IDS: [&[u8; 4]; 6] = [b"NAV\0", b"ECU1", b"\0\0\0\0", b"A\0\0\0", b"CTX1", b"SYS "];

/// A well-formed DLT message (standard header on) with flags, ids and
/// payload from `rng`: verbose arguments of every kind, control messages,
/// non-verbose ids or random bytes, sometimes cut short or with more after.
fn message(rng: &mut Rng, htyp_flags: Option<u8>) -> Vec<u8> {
    let mut flags = htyp_flags.unwrap_or_else(|| rng.next() as u8 & 0x1f);
    // What the payload is: 0 random, 1 verbose, 2 control, 3 non-verbose.
    let kind = if htyp_flags.is_some() { 0 } else { rng.below(4) };
    if kind == 1 || kind == 2 {
        flags |= 0x01;
    }
    let be = flags & 0x02 != 0;
    let mut m = vec![0x20 | flags, rng.next() as u8, 0, 0];
    if flags & 0x04 != 0 {
        m.extend_from_slice(*rng.pick(&IDS));
    }
    if flags & 0x08 != 0 {
        m.extend_from_slice(&(rng.below(20) as u32).to_be_bytes());
    }
    if flags & 0x10 != 0 {
        m.extend_from_slice(&(rng.next() as u32).to_be_bytes());
    }
    let mut payload = Vec::new();
    let mut noar = rng.below(4) as u8;
    if flags & 0x01 != 0 {
        // Every message type and type info, verbose or not.
        let msin = match kind {
            1 => (rng.next() as u8 & 0xf0) | 0x01,
            2 => ((rng.below(4) as u8 + 1) << 4) | (3 << 1),
            3 => rng.next() as u8 & 0xf0,
            _ => rng.next() as u8,
        };
        match kind {
            1 => {
                noar = rng.below(5) as u8;
                for _ in 0..noar {
                    verbose_arg(rng, be, &mut payload);
                }
                // NOAR a little off now and then.
                if rng.below(8) == 0 {
                    noar = noar.wrapping_add(1);
                }
            }
            2 => control(rng, be, msin >> 4, &mut payload),
            _ => {}
        }
        m.push(msin);
        m.push(noar);
        m.extend_from_slice(*rng.pick(&IDS));
        m.extend_from_slice(*rng.pick(&IDS));
    }
    if kind == 3 {
        let id = rng.next() as u32;
        payload.extend_from_slice(&if be { id.to_be_bytes() } else { id.to_le_bytes() });
    }
    if kind == 0 || rng.below(6) == 0 {
        payload.extend((0..rng.below(24)).map(|_| rng.next() as u8));
    }
    if !payload.is_empty() && rng.below(8) == 0 {
        payload.truncate(rng.below(payload.len() as u64) as usize);
    }
    m.extend_from_slice(&payload);
    let len = m.len() as u16;
    m[2..4].copy_from_slice(&len.to_be_bytes());
    m
}

fn put16(be: bool, v: u16, out: &mut Vec<u8>) {
    out.extend_from_slice(&if be { v.to_be_bytes() } else { v.to_le_bytes() });
}

fn put32(be: bool, v: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&if be { v.to_be_bytes() } else { v.to_le_bytes() });
}

/// Text for names, units and strings: plain, empty, with a NUL at the
/// end, control characters, bytes that aren't UTF-8.
fn text(rng: &mut Rng) -> Vec<u8> {
    const WORDS: [&[u8]; 8] = [b"speed", b"", b"km/h\0", b"a\tb\nc", b"\xff\xfeoops", b"\0", b"temp\0\0", b"x y"];
    rng.pick(&WORDS).to_vec()
}

/// One verbose argument: type info, then what it says.
fn verbose_arg(rng: &mut Rng, be: bool, out: &mut Vec<u8>) {
    let vari = if rng.below(2) == 0 { 0x800 } else { 0 };
    match rng.below(9) {
        // Strings, trace info and raw data.
        0..=2 => {
            let ty = *rng.pick(&[0x200u32, 0x200, 0x2000, 0x400]);
            let data = text(rng);
            put32(be, ty | vari | (rng.below(8) as u32) << 15, out);
            put16(be, data.len() as u16, out);
            if vari != 0 && ty != 0x2000 {
                let name = text(rng);
                put16(be, name.len() as u16, out);
                out.extend_from_slice(&name);
            }
            out.extend_from_slice(&data);
        }
        3 => {
            put32(be, 0x10 | 1 | vari, out);
            if vari != 0 {
                let name = text(rng);
                put16(be, name.len() as u16, out);
                out.extend_from_slice(&name);
            }
            out.push(rng.below(3) as u8);
        }
        // Integers and floats, every length (some not readable).
        4..=7 => {
            let base = *rng.pick(&[0x20u32, 0x40, 0x80, 0x20 | 0x1000]);
            let tyle = rng.below(7) as u32;
            put32(be, base | tyle | vari, out);
            if vari != 0 {
                let (name, unit) = (text(rng), text(rng));
                put16(be, name.len() as u16, out);
                put16(be, unit.len() as u16, out);
                out.extend_from_slice(&name);
                out.extend_from_slice(&unit);
            }
            let size = if (1..=5).contains(&tyle) { 1usize << (tyle - 1) } else { 4 };
            if base == 0x80 && rng.below(2) == 0 {
                // A float people log.
                let v = *rng.pick(&[0.1f64, -2.5, 1e30, f64::NAN, 0.0]);
                match size {
                    4 => out.extend_from_slice(&if be { (v as f32).to_be_bytes() } else { (v as f32).to_le_bytes() }),
                    8 => out.extend_from_slice(&if be { v.to_be_bytes() } else { v.to_le_bytes() }),
                    _ => out.extend((0..size).map(|_| rng.next() as u8)),
                }
            } else {
                out.extend((0..size).map(|_| rng.next() as u8));
            }
        }
        // Arrays, structs and anything else.
        _ => {
            put32(be, rng.next() as u32, out);
            out.extend((0..rng.below(8)).map(|_| rng.next() as u8));
        }
    }
}

/// A control message: service id, a response's status, a version string.
fn control(rng: &mut Rng, be: bool, mtin: u8, out: &mut Vec<u8>) {
    let sid = *rng.pick(&[0x01u32, 0x03, 0x13, 0x13, 0x1f, 0xf04, 0x77, 0xf02]);
    put32(be, sid, out);
    if mtin == 2 && rng.below(6) != 0 {
        out.push(*rng.pick(&[0u8, 1, 2, 9]));
        if sid == 0x13 {
            let v = text(rng);
            let len = if rng.below(5) == 0 { v.len() as u32 + 3 } else { v.len() as u32 };
            put32(be, len, out);
            out.extend_from_slice(&v);
        }
    }
    out.extend((0..rng.below(3)).map(|_| rng.next() as u8));
}

/// A field's value, comparable across the two readers.
#[derive(Debug, PartialEq)]
enum V {
    Str(Vec<u8>),
    U64(u64),
    F64(u64),
}

fn norm(v: Option<FieldValue<'_>>) -> Option<V> {
    v.map(|v| match v {
        FieldValue::Str(s) => V::Str(s.into_owned()),
        FieldValue::U64(x) => V::U64(x),
        FieldValue::F64(x) => V::F64(x.to_bits()),
    })
}

fn readers() -> (Dlt, KsyProtocol) {
    (Dlt::default(), KsyProtocol::parse(DEFINITION).unwrap_or_else(|e| panic!("dlt.ksy: {e}")))
}

/// Everything the two readers say about one message, compared.
fn same_message(native: &Dlt, ksy: &KsyProtocol, msg: &[u8], at: &Origin<'_>) {
    let ctx = || format!("message {msg:02x?}");
    for (kid, def) in ksy.schema().iter().enumerate() {
        let nid = native
            .schema()
            .iter()
            .position(|d| d.name == def.name)
            .unwrap_or_else(|| panic!("native has no {}", def.name));
        assert_eq!(native.schema()[nid].kind, def.kind, "kind of {}", def.name);
        assert_eq!(norm(ksy.field(msg, at, kid)), norm(native.field(msg, at, nid)), "field {} of {}", def.name, ctx());
    }
    assert_eq!(ksy.level(msg, at), native.level(msg, at), "level of {}", ctx());
    assert_eq!(ksy.uptime(msg, at), native.uptime(msg, at), "uptime of {}", ctx());
    // Native DLT writes an id's control characters into the columns as
    // they are (a tab shifts the columns); the definition writes spaces, as
    // `Protocol::render` asks. Only the columns of such messages differ.
    let control = ["ecu", "apid", "ctid"].iter().any(|name| {
        let id = native.schema().iter().position(|d| d.name == *name).expect("native field");
        matches!(native.field(msg, at, id), Some(FieldValue::Str(s)) if s.iter().any(|&c| c < 0x20 || c == 0x7F))
    });
    if !control {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        native.render(msg, at, &mut a);
        ksy.render(msg, at, &mut b);
        assert_eq!(String::from_utf8_lossy(&b), String::from_utf8_lossy(&a), "line of {}", ctx());
    }
    // The detailed view: the payload keeps its line breaks and tabs.
    assert_eq!(ksy.detail(msg, at).last(), native.detail(msg, at).last(), "detailed payload of {}", ctx());
}

fn same_split(native: &Dlt, ksy: &KsyProtocol, datagram: &[u8], at: &Origin<'_>) -> usize {
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let left_a = native.split(datagram, &mut a);
    let left_b = ksy.split(datagram, &mut b);
    assert_eq!((b.clone(), left_b), (a.clone(), left_a), "split of {datagram:02x?}");
    for &o in &a {
        same_message(native, ksy, &datagram[o..], at);
    }
    a.len()
}

const CHANNELS: [&[u8; 4]; 2] = [b"\0\0\0\0", b"MF4E"];

#[test]
fn the_same_fields_as_native_dlt() {
    let (native, ksy) = readers();
    let mut rng = Rng(0x5eed_0001);
    let mut payloads = Vec::new();
    for i in 0..20_000 {
        let m = message(&mut rng, None);
        let at = Origin { secs: i, usecs: 0, channel: CHANNELS[rng.below(2) as usize] };
        same_message(&native, &ksy, &m, &at);
        // Also with the rest of a buffer behind it, as a capture hands it over.
        let mut buf = m.clone();
        buf.extend_from_slice(&message(&mut rng, None));
        same_message(&native, &ksy, &buf, &at);
        let id = native.schema().iter().position(|d| d.name == "payload").expect("payload");
        if let Some(FieldValue::Str(s)) = native.field(&m, &at, id) {
            payloads.push(String::from_utf8_lossy(&s).into_owned());
        }
    }
    // Every kind of payload was there, read in full and cut short.
    for (what, needle) in [
        ("named numbers with a unit", " km/h"),
        ("booleans", "true"),
        ("strings with a name", "speed="),
        ("text that isn't UTF-8", "\u{fffd}"),
        ("raw data and the undecoded rest", "<undecoded> "),
        ("control responses", " ok]"),
        ("versions", "[get_software_version "),
        ("unknown services", "[service 0x77"),
        ("non-verbose ids", "] "),
    ] {
        let n = payloads.iter().filter(|p| p.contains(needle)).count();
        assert!(n >= 30, "{what}: {n} payloads with {needle:?}");
    }
    let floats = payloads.iter().filter(|p| p.contains("0.1") || p.contains("-2.5") || p.contains("NaN")).count();
    assert!(floats >= 30, "floats: {floats}");
}

#[test]
fn every_flag_byte_splits_the_same() {
    let (native, ksy) = readers();
    let mut rng = Rng(0x5eed_0002);
    let at = Origin { secs: 0, usecs: 0, channel: CHANNELS[0] };
    let mut messages = 0;
    for htyp in 0..=255u8 {
        for _ in 0..8 {
            // A message of the version-1 shape with this byte's flags, then
            // the byte itself (other versions too), and LEN up or down.
            let mut m = message(&mut rng, Some(htyp & 0x1f));
            m[0] = htyp;
            let len = u16::from_be_bytes([m[2], m[3]]);
            let fiddle = rng.below(5) as i32 - 2;
            let len = (i32::from(len) + fiddle).clamp(0, 0xffff) as u16;
            m[2..4].copy_from_slice(&len.to_be_bytes());
            m.extend_from_slice(&message(&mut rng, None));
            messages += same_split(&native, &ksy, &m, &at);
        }
    }
    assert!(messages > 256, "some datagrams had messages ({messages})");
}

#[test]
fn damaged_datagrams_split_the_same() {
    let (native, ksy) = readers();
    let mut rng = Rng(0x5eed_0003);
    let at = Origin { secs: 0, usecs: 0, channel: CHANNELS[1] };
    let (mut messages, mut short) = (0, 0);
    for _ in 0..20_000 {
        let mut d: Vec<u8> = (0..rng.below(6)).flat_map(|_| message(&mut rng, None)).collect();
        match rng.below(4) {
            0 => {}
            1 if !d.is_empty() => {
                for _ in 0..1 + rng.below(3) {
                    let at = rng.below(d.len() as u64) as usize;
                    d[at] = rng.next() as u8;
                }
            }
            2 if !d.is_empty() => {
                d.truncate(rng.below(d.len() as u64) as usize);
                short += 1;
            }
            _ => d.extend((0..rng.below(30)).map(|_| rng.next() as u8)),
        }
        messages += same_split(&native, &ksy, &d, &at);
    }
    assert!(messages > 10_000 && short > 1000, "{messages} messages, {short} cut datagrams");
}

// --- in a capture --------------------------------------------------------

/// A pcap of Ethernet / IPv4 / UDP frames: `(port, payload)` each.
fn pcap(datagrams: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let all: Vec<Udp> =
        datagrams.iter().map(|(port, p)| Udp { src: 1, port: *port, vlan: None, payload: p.clone() }).collect();
    pcap_of(&all)
}

/// A UDP datagram from 10.0.0.`src` to 10.0.0.2, VLAN-tagged or not.
struct Udp {
    src: u8,
    port: u16,
    vlan: Option<u16>,
    payload: Vec<u8>,
}

fn pcap_of(datagrams: &[Udp]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    for x in [0u32, 0, 262_144, 1] {
        out.extend_from_slice(&x.to_le_bytes());
    }
    for (i, Udp { src, port, vlan, payload }) in datagrams.iter().enumerate() {
        let mut f = vec![2; 12];
        if let Some(v) = vlan {
            f.extend_from_slice(&[0x81, 0x00]);
            f.extend_from_slice(&v.to_be_bytes());
        }
        f.extend_from_slice(&[0x08, 0x00, 0x45, 0]);
        f.extend_from_slice(&((20 + 8 + payload.len()) as u16).to_be_bytes());
        f.extend_from_slice(&(i as u16).to_be_bytes());
        f.extend_from_slice(&[0, 0, 64, 17, 0, 0, 10, 0, 0, *src, 10, 0, 0, 2]);
        f.extend_from_slice(&50000u16.to_be_bytes());
        f.extend_from_slice(&port.to_be_bytes());
        f.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(payload);
        for x in [i as u32, 0, f.len() as u32, f.len() as u32] {
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.extend_from_slice(&f);
    }
    out
}

fn count(source: &dyn LogSource, query: &str) -> Vec<u64> {
    let q = Query::compile_for(query, source.schema()).unwrap_or_else(|e| panic!("{query}: {e:?}"));
    search(source, &Filter::Query(q), &Progress::new()).expect("search").lines
}

#[test]
fn both_readers_in_one_capture_find_the_same_lines() {
    let mut rng = Rng(0x5eed_0004);
    let mut datagrams = Vec::new();
    for _ in 0..3000 {
        let d: Vec<u8> = (0..1 + rng.below(4)).flat_map(|_| message(&mut rng, None)).collect();
        // Every datagram twice: native DLT on 3490, the definition on 3491.
        datagrams.push((3490, d.clone()));
        datagrams.push((3491, d));
    }
    let ksy = KsyProtocol::parse(DEFINITION).expect("dlt.ksy");
    let options = CaptureOptions {
        protocols: vec![LoadedProtocol { protocol: Arc::new(ksy), routes: vec![Match::port(3491)] }],
        ..Default::default()
    };
    let (source, stats) = capture::from_bytes("two-ports.pcap", pcap(&datagrams), &options).expect("capture");
    assert_eq!(source.protocols(), ["DLT", "DLTK"]);
    assert_eq!(stats.msg_skipped, 0);
    // The i-th line of each protocol is the same message.
    let native_lines = count(&source, "proto == DLT");
    let ksy_lines = count(&source, "proto == DLTK");
    assert_eq!(native_lines.len() * 2, source.line_count() as usize);
    assert_eq!(native_lines.len(), ksy_lines.len());
    let twin: std::collections::HashMap<u64, u64> =
        native_lines.iter().copied().zip(ksy_lines.iter().copied()).collect();
    for q in [
        "apid == NAV",
        "level >= WARN",
        "apid == NAV AND level >= WARN",
        "ecu == ECU1",
        "ecu ~ /^$/",
        "type == control",
        "mode == non-verbose",
        "mid > 100000",
        "tmsp > 2000000000",
        "seid == 7",
        "mcnt < 10",
        "ctid ~ /C/",
        "NOT apid == NAV AND level == INFO",
        // A capture of two protocols joins the header cells with spaces.
        "raw ~ /ECU1 NAV CTX1/",
    ] {
        let native = count(&source, &format!("proto == DLT AND ({q})"));
        let ksy = count(&source, &format!("proto == DLTK AND ({q})"));
        assert!(!native.is_empty(), "{q}: no lines");
        assert_eq!(native.iter().map(|l| twin[l]).collect::<Vec<_>>(), ksy, "{q}");
    }
}

#[test]
fn rules_share_one_port_by_address_and_vlan() {
    let mut rng = Rng(0x5eed_0005);
    let mut datagrams = Vec::new();
    // On port 3490: from 10.0.0.1 untagged, from 10.0.0.7, and on VLAN 5.
    for i in 0..300u32 {
        let (src, vlan) = match i % 3 {
            0 => (1, None),
            1 => (7, None),
            _ => (1, Some(5)),
        };
        datagrams.push(Udp { src, port: 3490, vlan, payload: message(&mut rng, Some(0x05)) });
    }
    let ksy = Arc::new(KsyProtocol::parse(DEFINITION).expect("dlt.ksy"));
    let open = |routes: &[&str], disabled: &[&str]| {
        let options = CaptureOptions {
            protocols: vec![LoadedProtocol { protocol: ksy.clone(), routes: Vec::new() }],
            routes: routes.iter().map(|r| r.parse().expect("rule")).collect(),
            disabled: disabled.iter().map(|d| d.to_string()).collect(),
            ..Default::default()
        };
        capture::from_bytes("rules.pcap", pcap_of(&datagrams), &options).expect("capture").0
    };
    let per = |source: &dyn LogSource| (count(source, "proto == DLT").len(), count(source, "proto == DLTK").len());
    // Native DLT's default port takes them all.
    let all = per(&open(&[], &[]));
    assert!(all.0 > 200 && all.1 == 0, "{all:?}");
    // The more specific rule wins: one source, one VLAN.
    let by_src = per(&open(&["DLTK:port=3490,src=10.0.0.7"], &[]));
    let by_vlan = per(&open(&["DLTK:3490,vlan=5"], &[]));
    assert!(by_src.1 > 50 && by_vlan.1 > 50, "{by_src:?} {by_vlan:?}");
    assert_eq!(by_src.0 + by_src.1, all.0);
    assert_eq!(by_vlan.0 + by_vlan.1, all.0);
    // A rule for DLT replaces its default port: here only VLAN 5 is DLT.
    let only = per(&open(&["DLT:vlan=5"], &[]));
    assert_eq!(only, (by_vlan.1, 0));
    // Equally specific rules: the protocol listed first (native DLT).
    assert_eq!(per(&open(&["DLTK:3490"], &[])), all);
    // Turned off: its datagrams go to the next rule that matches.
    assert_eq!(per(&open(&["DLTK:3490"], &["DLT"])), (0, all.0));
}

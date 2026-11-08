//! The DLT definition (`data/dlt.ksy`) against Nerka's native DLT reader:
//! the same messages, split the same way, with the same fields, levels,
//! uptimes and columns (all but the payload, which the definition shows as
//! hex). Then both in one capture, on two UDP ports: every query gives the
//! same lines for `proto == DLT` and `proto == DLTK`.
//!
//! A test of the protocol definitions, not a feature: it uses the public
//! API only (doc/protocol-definitions.md).

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
/// payload from `rng`.
fn message(rng: &mut Rng, htyp_flags: Option<u8>) -> Vec<u8> {
    let flags = htyp_flags.unwrap_or_else(|| rng.next() as u8 & 0x1f);
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
    if flags & 0x01 != 0 {
        // Every message type and type info, verbose or not.
        m.push(rng.next() as u8);
        m.push(rng.below(4) as u8);
        m.extend_from_slice(*rng.pick(&IDS));
        m.extend_from_slice(*rng.pick(&IDS));
    }
    for _ in 0..rng.below(24) {
        m.push(rng.next() as u8);
    }
    let len = m.len() as u16;
    m[2..4].copy_from_slice(&len.to_be_bytes());
    m
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

/// Columns before the payload.
fn header_cells(p: &dyn Protocol, msg: &[u8], at: &Origin<'_>) -> Vec<String> {
    let mut out = Vec::new();
    p.render(msg, at, &mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    let mut cells: Vec<String> = text.split('\t').map(str::to_owned).collect();
    cells.truncate(p.columns().len() - 1);
    cells
}

fn readers() -> (Dlt, KsyProtocol) {
    (Dlt::default(), KsyProtocol::parse(DEFINITION).unwrap_or_else(|e| panic!("dlt.ksy: {e}")))
}

/// Everything the two readers say about one message, compared.
fn same_message(native: &Dlt, ksy: &KsyProtocol, msg: &[u8], at: &Origin<'_>) {
    let ctx = || format!("message {msg:02x?}");
    for (kid, def) in ksy.schema().iter().enumerate() {
        if def.name == "payload" {
            continue;
        }
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
    // Native DLT writes an id's tabs and line breaks into the columns as
    // they are (a tab shifts the columns); the definition writes spaces, as
    // `Protocol::render` asks. Only the columns of such messages differ.
    let control = ["ecu", "apid", "ctid"].iter().any(|name| {
        let id = native.schema().iter().position(|d| d.name == *name).expect("native field");
        matches!(native.field(msg, at, id), Some(FieldValue::Str(s)) if s.iter().any(|c| matches!(c, b'\t' | b'\n' | b'\r')))
    });
    if !control {
        assert_eq!(header_cells(ksy, msg, at), header_cells(native, msg, at), "columns of {}", ctx());
    }
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
    for i in 0..20_000 {
        let m = message(&mut rng, None);
        let at = Origin { secs: i, usecs: 0, channel: CHANNELS[rng.below(2) as usize] };
        same_message(&native, &ksy, &m, &at);
        // Also with the rest of a buffer behind it, as a capture hands it over.
        let mut buf = m.clone();
        buf.extend_from_slice(&message(&mut rng, None));
        same_message(&native, &ksy, &buf, &at);
    }
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
    let mut out = Vec::new();
    out.extend_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    for x in [0u32, 0, 262_144, 1] {
        out.extend_from_slice(&x.to_le_bytes());
    }
    for (i, (port, payload)) in datagrams.iter().enumerate() {
        let mut f = vec![2; 12];
        f.extend_from_slice(&[0x08, 0x00, 0x45, 0]);
        f.extend_from_slice(&((20 + 8 + payload.len()) as u16).to_be_bytes());
        f.extend_from_slice(&(i as u16).to_be_bytes());
        f.extend_from_slice(&[0, 0, 64, 17, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2]);
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
        protocols: vec![LoadedProtocol { protocol: Arc::new(ksy), udp_ports: vec![3491] }],
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

use super::*;
use crate::DltFile;
use crate::LineNo;
use crate::LogSource;
use crate::engine::search::tests::run_on;
use crate::input::dlt::writer::{Msg, Value};
use crate::input::proto::MAX_SENDERS;
use crate::{Filter, Level, Query};
use net::FRAGMENT_TIMEOUT_SECS;

/// A DLT message without storage header, as it travels in a datagram.
pub(super) fn dlt(apid: &str, text: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    Msg::log(Level::Info, "ECU1", apid, "CTX", vec![Value::Str(text.into()).into()]).encode(&mut buf);
    buf.split_off(16)
}

pub(super) fn udp(dport: u16, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&50000u16.to_be_bytes());
    v.extend_from_slice(&dport.to_be_bytes());
    v.extend_from_slice(&((8 + data.len()) as u16).to_be_bytes());
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(data);
    v
}

/// An IPv4 packet; `frag` is the flags + fragment offset field.
pub(super) fn ipv4(proto: u8, id: u16, frag: u16, data: &[u8]) -> Vec<u8> {
    let mut v = vec![0x45, 0];
    v.extend_from_slice(&((20 + data.len()) as u16).to_be_bytes());
    v.extend_from_slice(&id.to_be_bytes());
    v.extend_from_slice(&frag.to_be_bytes());
    v.extend_from_slice(&[64, proto, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2]);
    v.extend_from_slice(data);
    v
}

/// IPv4 fragments of a UDP datagram: (offset in 8-byte units, more, bytes).
pub(super) fn fragments(id: u16, datagram: &[u8], size: usize) -> Vec<Vec<u8>> {
    let chunks: Vec<&[u8]> = datagram.chunks(size).collect();
    let n = chunks.len();
    chunks
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let flags = if i + 1 < n { 0x2000 } else { 0 } | (i * size / 8) as u16;
            ipv4(17, id, flags, c)
        })
        .collect()
}

fn ipv6_udp(data: &[u8], frag: Option<(u16, bool, u32)>) -> Vec<u8> {
    let mut ext = Vec::new();
    let next = if let Some((offset, more, id)) = frag {
        ext.extend_from_slice(&[17, 0]);
        ext.extend_from_slice(&((offset << 3) | more as u16).to_be_bytes());
        ext.extend_from_slice(&id.to_be_bytes());
        44
    } else {
        17
    };
    let mut v = vec![0x60, 0, 0, 0];
    v.extend_from_slice(&((ext.len() + data.len()) as u16).to_be_bytes());
    v.extend_from_slice(&[next, 64]);
    v.extend_from_slice(&[0xfe; 16]);
    v.extend_from_slice(&[0xfd; 16]);
    v.extend_from_slice(&ext);
    v.extend_from_slice(data);
    v
}

pub(super) fn eth(ty: u16, data: &[u8]) -> Vec<u8> {
    let mut v = vec![2; 12];
    v.extend_from_slice(&ty.to_be_bytes());
    v.extend_from_slice(data);
    v
}

pub(super) fn vlan(ty: u16, data: &[u8]) -> Vec<u8> {
    let mut v = vec![2; 12];
    v.extend_from_slice(&[0x81, 0x00, 0, 5]);
    v.extend_from_slice(&ty.to_be_bytes());
    v.extend_from_slice(data);
    v
}

/// A classic pcap file (µs, little endian, link type `link`).
fn pcap_file(link: u32, frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&0xA1B2_C3D4u32.to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&4u16.to_le_bytes());
    v.extend_from_slice(&[0; 8]);
    v.extend_from_slice(&65535u32.to_le_bytes());
    v.extend_from_slice(&link.to_le_bytes());
    for (t, f) in frames {
        for x in [*t, 250_000, f.len() as u32, f.len() as u32] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(f);
    }
    v
}

fn block(ty: u32, mut body: Vec<u8>) -> Vec<u8> {
    while !body.len().is_multiple_of(4) {
        body.push(0);
    }
    let n = (12 + body.len()) as u32;
    let mut v = ty.to_le_bytes().to_vec();
    v.extend_from_slice(&n.to_le_bytes());
    v.extend_from_slice(&body);
    v.extend_from_slice(&n.to_le_bytes());
    v
}

/// A pcapng file: interface 0 with ns timestamps, interface 1 with the
/// default µs; frames are (interface, seconds, frame).
fn pcapng_file(frames: &[(u32, u64, Vec<u8>)]) -> Vec<u8> {
    let mut shb = 0x1A2B_3C4Du32.to_le_bytes().to_vec();
    shb.extend_from_slice(&[1, 0, 0, 0]);
    shb.extend_from_slice(&(-1i64).to_le_bytes());
    let mut v = block(0x0A0D_0D0A, shb);
    let mut idb0 = vec![1, 0, 0, 0, 0, 0, 4, 0];
    idb0.extend_from_slice(&[9, 0, 1, 0, 9, 0, 0, 0, 0, 0, 0, 0]);
    v.extend(block(1, idb0));
    v.extend(block(1, vec![1, 0, 0, 0, 0, 0, 4, 0]));
    for (iface, secs, f) in frames {
        let ts = if *iface == 0 { secs * 1_000_000_000 + 500_000_000 } else { secs * 1_000_000 + 500_000 };
        let mut b = iface.to_le_bytes().to_vec();
        b.extend_from_slice(&((ts >> 32) as u32).to_le_bytes());
        b.extend_from_slice(&(ts as u32).to_le_bytes());
        b.extend_from_slice(&(f.len() as u32).to_le_bytes());
        b.extend_from_slice(&(f.len() as u32).to_le_bytes());
        b.extend_from_slice(f);
        v.extend(block(6, b));
    }
    v
}

/// (apid, payload text) of every line.
pub(super) fn lines(f: &DltFile) -> Vec<(String, String)> {
    (0..f.line_count())
        .map(|l| {
            let t = f.line_text(l).unwrap().into_owned();
            let cols: Vec<&str> = t.split('\t').collect();
            (cols[3].to_owned(), cols[6].to_owned())
        })
        .collect()
}

pub(super) fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

#[test]
fn dlt_over_udp_is_read_in_place() {
    let two = [dlt("NAV", "first"), dlt("SYS", "second")].concat();
    let frames = vec![
        (100, eth(0x0800, &ipv4(17, 1, 0, &udp(3490, &two)))),
        (101, vlan(0x0800, &ipv4(17, 2, 0, &udp(3490, &dlt("DIAG", "tagged"))))),
        (102, eth(0x0800, &ipv4(17, 3, 0, &udp(3491, &dlt("XXX", "other port"))))),
        (103, eth(0x0806, &[0; 28])),
        (104, eth(0x0800, &ipv4(17, 4, 0, &udp(3489, &dlt("ALT", "another port"))))),
        (105, eth(0x0800, &ipv4(17, 5, 0, &udp(3490, &[dlt("COM", "then junk"), vec![0xff; 5]].concat())))),
    ];
    let (f, s) = from_bytes("t.pcap", pcap_file(1, &frames), &CaptureOptions::default()).unwrap();
    // Only port 3490 by default.
    assert_eq!(lines(&f), pairs(&[("NAV", "first"), ("SYS", "second"), ("DIAG", "tagged"), ("COM", "then junk")]));
    // Capture time and the message's own ECU.
    assert!(f.line_text(2).unwrap().starts_with("1970-01-01 00:01:41.250000\t"), "{:?}", f.line_text(2));
    assert_eq!(f.field(0, 0), Some(crate::FieldValue::Str(b"ECU1".as_slice().into())));
    assert_eq!((s.datagrams, s.messages, s.msg_skipped), (3, 4, 5));
    assert_eq!((s.net.frames, s.net.udp, s.net.other, s.file_skipped), (6, 5, 1, 0));
    // Nothing was copied: every message is in the file.
    assert_eq!((f.line_count(), f.side_bytes()), (4, 0));

    // Other ports when configured.
    let o = CaptureOptions { dlt_udp_ports: vec![3491, 3489], ..Default::default() };
    let got = lines(&from_bytes("t", pcap_file(1, &frames), &o).unwrap().0);
    assert_eq!(got, pairs(&[("XXX", "other port"), ("ALT", "another port")]));
}

#[test]
fn fragments_are_reassembled_in_any_order() {
    let big = dlt("BIG", &"x".repeat(1000));
    let a = fragments(7, &udp(3490, &[big.clone(), dlt("A2", "after")].concat()), 400);
    let b = fragments(8, &udp(3490, &dlt("B", &"y".repeat(500))), 304);
    let c = fragments(9, &udp(3490, &dlt("C", &"z".repeat(500))), 304);
    assert_eq!((a.len(), b.len()), (3, 2));
    // Out of order and interleaved; C never completes.
    let order = [&a[2], &b[1], &a[0], &c[0], &b[0], &a[1]];
    let frames: Vec<(u32, Vec<u8>)> = order.iter().enumerate().map(|(i, p)| (10 + i as u32, eth(0x0800, p))).collect();
    let (f, s) = from_bytes("t", pcap_file(1, &frames), &CaptureOptions::default()).unwrap();
    let got = lines(&f);
    assert_eq!(got.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>(), ["B", "BIG", "A2"]);
    assert_eq!(got[1].1, "x".repeat(1000));
    assert_eq!((s.net.reassembled, s.net.incomplete), (2, 1));
    // The time is the frame that completed the datagram.
    assert!(f.line_text(0).unwrap().starts_with("1970-01-01 00:00:14.250000"));

    // A datagram whose fragments are too far apart in time is dropped.
    let late = vec![(0, eth(0x0800, &b[0])), (FRAGMENT_TIMEOUT_SECS + 1, eth(0x0800, &b[1]))];
    let (f, s) = from_bytes("t", pcap_file(1, &late), &CaptureOptions::default()).unwrap();
    assert_eq!((f.line_count(), s.net.incomplete), (0, 2));
}

#[test]
fn pcapng_interfaces_ipv6_and_cooked_captures() {
    let frags = {
        let d = udp(3490, &dlt("V6F", &"q".repeat(300)));
        let (x, y) = d.split_at(160);
        [ipv6_udp(x, Some((0, true, 42))), ipv6_udp(y, Some((20, false, 42)))]
    };
    let frames = vec![
        (0, 50, eth(0x86DD, &ipv6_udp(&udp(3490, &dlt("V6", "ipv6")), None))),
        (1, 51, eth(0x0800, &ipv4(17, 1, 0, &udp(3490, &dlt("IF1", "second interface"))))),
        (0, 52, eth(0x86DD, &frags[1])),
        (0, 53, eth(0x86DD, &frags[0])),
    ];
    let (f, s) = from_bytes("t.pcapng", pcapng_file(&frames), &CaptureOptions::default()).unwrap();
    let got = lines(&f);
    assert_eq!(got.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>(), ["V6", "IF1", "V6F"]);
    assert_eq!(s.net.reassembled, 1);
    // ns on interface 0, µs on interface 1: both half a second.
    assert!(f.line_text(0).unwrap().starts_with("1970-01-01 00:00:50.500000"));
    assert!(f.line_text(1).unwrap().starts_with("1970-01-01 00:00:51.500000"));

    // Linux cooked capture (SLL): protocol at byte 14, 16-byte header.
    let mut sll = vec![0; 14];
    sll.extend_from_slice(&0x0800u16.to_be_bytes());
    sll.extend_from_slice(&ipv4(17, 1, 0, &udp(3490, &dlt("SLL", "cooked"))));
    let (f, _) = from_bytes("t", pcap_file(113, &[(1, sll)]), &CaptureOptions::default()).unwrap();
    assert_eq!(lines(&f), pairs(&[("SLL", "cooked")]));
}

/// The messages of a capture read like the same messages in a `.dlt` file
/// with the capture's times, through the engine too (fast path against
/// naive, both).
#[test]
fn a_capture_reads_like_the_same_dlt_file() {
    let levels = [Level::Error, Level::Warn, Level::Info, Level::Debug];
    let (mut frames, mut dlt_file) = (Vec::new(), Vec::new());
    for i in 0..400u32 {
        let mut m = Msg::log(
            levels[i as usize % 4],
            ["ECU1", "ECU2"][i as usize % 2],
            ["NAV", "SYS", "DIAG"][i as usize % 3],
            "CTX",
            vec![Value::Str(format!("message {i} {}", if i % 9 == 0 { "failed" } else { "ok" })).into()],
        );
        m.secs = 1000 + i;
        m.usecs = 250_000;
        m.mcnt = i as u8;
        let mut buf = Vec::new();
        m.encode(&mut buf);
        dlt_file.extend_from_slice(&buf);
        frames.push((1000 + i, eth(0x0800, &ipv4(17, i as u16, 0, &udp(3490, &buf[16..])))));
    }
    let (g, _) = from_bytes("t.pcap", pcap_file(1, &frames), &CaptureOptions::default()).unwrap();
    let f = DltFile::from_bytes("t.dlt", dlt_file);
    assert_eq!(g.line_count(), 400);
    for l in 0..400 {
        assert_eq!(g.line_text(l), f.line_text(l), "line {l}");
    }
    let q = |s: &str| Filter::Query(Query::compile(s).unwrap());
    for s in ["ecu == ECU2 AND level >= WARN", "apid == NAV AND raw ~ /failed/", "mcnt > 200"] {
        let got = run_on(&g, &q(s));
        assert!(!got.is_empty(), "{s}");
        assert_eq!(got, run_on(&f, &q(s)), "{s}");
    }
}

#[test]
fn truncated_files_and_detection() {
    let mut file = pcap_file(1, &[(1, eth(0x0800, &ipv4(17, 1, 0, &udp(3490, &dlt("A", "whole")))))]);
    let whole = file.len();
    file.extend_from_slice(&pcap_file(1, &[(2, eth(0x0800, &ipv4(17, 1, 0, &udp(3490, &dlt("B", "cut")))))])[24..]);
    file.truncate(file.len() - 10);
    let (f, s) = from_bytes("t", file.clone(), &CaptureOptions::default()).unwrap();
    assert_eq!((f.line_count(), s.file_skipped), (1, (file.len() - whole) as u64));
    assert!(from_bytes("t", b"not a capture".to_vec(), &CaptureOptions::default()).is_err());

    // `open` picks the capture reader by content, whatever the extension.
    let dir = std::env::temp_dir().join(format!("nerka-capture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (p1, p2) = (dir.join("trace.bin"), dir.join("trace.pcapng"));
    std::fs::write(&p1, &file).unwrap();
    std::fs::write(&p2, pcapng_file(&[(1, 5, eth(0x0800, &ipv4(17, 1, 0, &udp(3490, &dlt("NG", "x")))))])).unwrap();
    let p = Progress::new();
    assert_eq!(crate::open(&p1, &p).unwrap().line_count(), 1);
    assert_eq!(crate::open(&p2, &p).unwrap().columns().len(), 7);
    std::fs::remove_dir_all(&dir).ok();
}

/// One capture with DLT, SOME/IP and syslog: one table with shared
/// columns, every protocol's fields, and queries across them.
#[test]
fn several_protocols_share_one_view() {
    use crate::input::proto::{MIXED_COLUMNS, someip};
    let sd = someip::encode(0xFFFF, 0x8100, 0, 1, 0x02, 0, &[0xc0, 0, 0, 0]);
    let two =
        [someip::encode(0x1234, 0x8001, 0, 7, 0x02, 0, &[1, 2]), someip::encode(0x1234, 1, 0x10, 8, 0x81, 3, &[])]
            .concat();
    let frames = vec![
        (100, eth(0x0800, &ipv4(17, 1, 0, &udp(3490, &dlt("NAV", "position ok"))))),
        (101, eth(0x0800, &ipv4(17, 2, 0, &udp(30490, &sd)))),
        (102, eth(0x0800, &ipv4(17, 3, 0, &udp(30501, &two)))),
        (103, eth(0x0800, &ipv4(17, 4, 0, &udp(514, b"<11>1 - gw nav 42 - - disk failed\n")))),
        (104, eth(0x0800, &ipv4(17, 5, 0, &udp(514, b"<30>Oct 18 02:00:00 gw sshd[7]: login ok")))),
        (105, eth(0x0800, &ipv4(17, 6, 0, &udp(514, b"not syslog")))),
    ];
    let o = CaptureOptions { someip_udp_ports: vec![30490, 30501], ..Default::default() };
    let (f, s) = from_bytes("t.pcap", pcap_file(1, &frames), &o).unwrap();
    assert_eq!(f.protocols(), ["DLT", "SOMEIP", "syslog"]);
    // What the top bar says (from the protocols found when read, not a
    // walk over every message per frame).
    assert_eq!(f.format(), "pcap: DLT + SOMEIP + syslog");
    assert_eq!((s.datagrams, s.messages, s.msg_skipped), (6, 6, 10));
    let names: Vec<&str> = f.columns().iter().map(|c| c.name.as_ref()).collect();
    assert_eq!(names, MIXED_COLUMNS.iter().map(|c| c.name.as_ref()).collect::<Vec<_>>());
    let cells = |l| f.line_text(l).unwrap().split('\t').skip(1).map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(cells(0), ["DLT", "0.0000 ECU1 NAV CTX info", "position ok"]);
    assert_eq!(cells(1), ["SOMEIP", "0xffff 0x8100 0x0000 0x0001 NOTIFICATION E_OK", "SD [4] c0 00 00 00"]);
    assert_eq!(cells(3), ["SOMEIP", "0x1234 0x0001 0x0010 0x0008 ERROR E_UNKNOWN_METHOD", "[0]"]);
    assert_eq!(cells(4), ["syslog", "user err gw nav 42", "disk failed"]);
    assert_eq!(cells(5), ["syslog", "daemon info gw sshd 7", "login ok"]);
    // The line window has the same cells, the payload with its line breaks.
    assert_eq!(f.line_detail(4).unwrap()[1..], cells(4));

    let q = |s: &str| run_on(&f, &Filter::Query(Query::compile(s).unwrap()));
    assert_eq!(q("proto == SOMEIP AND service == 0x1234"), [2, 3]);
    assert_eq!(q("level >= ERROR"), [3, 4]);
    assert_eq!(q("type == NOTIFICATION"), [1, 2]);
    assert_eq!(q("host == gw AND app ~ /ssh/"), [5]);
    assert_eq!(q("ecu == ECU1 OR (proto == SOMEIP AND rc != E_OK)"), [0, 3]);
    assert_eq!(q("payload ~ /failed|SD/"), [1, 4]);
    assert_eq!(q("proto != DLT AND /0x1234/"), [2, 3]);
    // Protocol names compare without case.
    for p in ["syslog", "SYSLOG", "Syslog"] {
        assert_eq!(q(&format!("proto == {p}")), [4, 5], "{p}");
    }
    assert_eq!(q("proto != dlt AND proto != someip"), [4, 5]);
    // Other protocols' fields have no value on a line: only != holds.
    assert_eq!(q("apid != NAV"), [1, 2, 3, 4, 5]);
}

/// A capture of one protocol shows that protocol's own columns.
#[test]
fn one_protocol_shows_its_own_columns() {
    use crate::input::proto::someip;
    let frames = vec![(100, eth(0x0800, &ipv4(17, 1, 0, &udp(30490, &someip::encode(0x10, 0x20, 1, 2, 0, 0, &[9])))))];
    let (f, _) = from_bytes("t.pcap", pcap_file(1, &frames), &CaptureOptions::default()).unwrap();
    let names: Vec<&str> = f.columns().iter().map(|c| c.name.as_ref()).collect();
    assert_eq!(names, ["time", "service", "method", "client", "session", "type", "rc", "payload"]);
    assert_eq!(
        f.line_text(0).unwrap().split_once('\t').unwrap().1,
        "0x0010\t0x0020\t0x0001\t0x0002\tREQUEST\tE_OK\t[1] 09"
    );
    assert_eq!(f.supplied_level(0), Some(Some(Level::Info)));
    let q = |s: &str| run_on(&f, &Filter::Query(Query::compile(s).unwrap()));
    assert_eq!(q("proto == SOMEIP AND method == 32 AND len == 1"), [0]);

    let frames = vec![(100, eth(0x0800, &ipv4(17, 1, 0, &udp(514, b"<12>x"))))];
    let (f, _) = from_bytes("t.pcap", pcap_file(1, &frames), &CaptureOptions::default()).unwrap();
    assert_eq!(f.columns().len(), 7);
    assert_eq!(f.supplied_level(0), Some(Some(Level::Warn)));
    // Kept behind its length in the side buffer.
    assert!(f.index_memory_bytes() >= 16 + 4 + 5);
}

/// A UDP datagram from `sport` to `dport`.
fn udp_from(sport: u16, dport: u16, data: &[u8]) -> Vec<u8> {
    let mut v = udp(dport, data);
    v[..2].copy_from_slice(&sport.to_be_bytes());
    v
}

fn field_named<'a>(f: &'a Records, line: LineNo, name: &str) -> Option<crate::FieldValue<'a>> {
    let id = f.schema().iter().position(|d| d.name == name).unwrap_or_else(|| panic!("no field {name}"));
    f.field(line, id)
}

/// Owner, 2026-11-08: where a message's datagram came from, as fields
/// (`net.src`, `net.dst`, `net.sport`, `net.port`, `net.vlan`) that
/// queries see but columns and `raw` don't.
#[test]
fn network_fields_say_where_datagrams_came_from() {
    let two = [dlt("TWO", "first"), dlt("TWO", "second")].concat();
    let frames = vec![
        (1, eth(0x0800, &ipv4(17, 1, 0, &udp_from(40001, 3490, &dlt("A", "v4"))))),
        (2, vlan(0x0800, &ipv4(17, 2, 0, &udp(3490, &dlt("V", "tagged"))))),
        (3, eth(0x86DD, &ipv6_udp(&udp(3490, &dlt("SIX", "v6")), None))),
        (4, eth(0x0800, &ipv4(17, 3, 0, &udp(3490, &two)))),
    ];
    let mut frames = frames;
    for (i, p) in fragments(9, &udp(3490, &dlt("FRAG", &"f".repeat(600))), 304).into_iter().enumerate() {
        frames.push((5 + i as u32, eth(0x0800, &p)));
    }
    let (f, s) = from_bytes("t", pcap_file(1, &frames), &CaptureOptions::default()).unwrap();
    assert_eq!(lines(&f).iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>(), ["A", "V", "SIX", "TWO", "TWO", "FRAG"]);
    assert_eq!(s.senders_not_kept, 0);
    let s = |l, n| match field_named(&f, l, n) {
        Some(crate::FieldValue::Str(v)) => String::from_utf8_lossy(&v).into_owned(),
        Some(crate::FieldValue::U64(v)) => v.to_string(),
        other => format!("{other:?}"),
    };
    assert_eq!(
        [s(0, "net.src"), s(0, "net.dst"), s(0, "net.sport"), s(0, "net.port")],
        ["10.0.0.1", "10.0.0.2", "40001", "3490"]
    );
    assert_eq!(s(0, "net.vlan"), "None", "untagged: no value");
    assert_eq!(s(1, "net.vlan"), "5");
    assert_eq!(s(2, "net.src"), "fefe:fefe:fefe:fefe:fefe:fefe:fefe:fefe");
    assert_eq!(s(2, "net.dst"), "fdfd:fdfd:fdfd:fdfd:fdfd:fdfd:fdfd:fdfd");
    assert_eq!([s(3, "net.src"), s(4, "net.src")], ["10.0.0.1", "10.0.0.1"], "every message of a datagram");
    assert_eq!(s(5, "net.port"), "3490", "a reassembled datagram keeps its sender");
    // Fields, not columns, and not in the line's text.
    assert!(f.columns().iter().all(|c| !c.name.starts_with("net.")));
    assert!((0..6).all(|l| !f.line_text(l).unwrap().contains("10.0.0.1")));
    // Through the engine: fast path against naive.
    let q = |s: &str| Filter::Query(Query::compile(s).unwrap_or_else(|e| panic!("{s}: {e}")));
    assert_eq!(run_on(&f, &q("net.src == 10.0.0.1")), [0, 1, 3, 4, 5]);
    assert_eq!(run_on(&f, &q("net.vlan == 5")), [1]);
    assert_eq!(run_on(&f, &q("net.vlan != 5")), [0, 2, 3, 4, 5], "no tag: no value, so it isn't 5");
    assert_eq!(run_on(&f, &q("net.sport == 40001 AND net.port >= 3490")), [0]);
    assert_eq!(run_on(&f, &q("net.src ~ /^fefe:/")), [2]);
    assert_eq!(run_on(&f, &q(r"raw ~ /10\.0\.0\.1/")), Vec::<LineNo>::new());
    // A `.dlt` file has no network: no such fields.
    assert!(DltFile::from_bytes("x.dlt", Vec::new()).schema().iter().all(|d| !d.name.starts_with("net.")));
}

/// A capture with more senders than [`MAX_SENDERS`] (spoofed or garbled
/// addresses): the table stops growing, later senders' messages have no
/// `net.*` fields, and nothing else about them changes.
#[test]
fn senders_past_the_cap_are_not_kept() {
    let n = MAX_SENDERS + 5;
    let mut frames: Vec<(u32, Vec<u8>)> = Vec::with_capacity(n + 1);
    for i in 0..n {
        let mut ip = ipv4(17, 1, 0, &udp_from((i % 60_000) as u16, 3490, &dlt("S", "spoofed")));
        // A different source address per datagram.
        ip[12..16].copy_from_slice(&(i as u32).to_be_bytes());
        frames.push((1, eth(0x0800, &ip)));
    }
    // A sender seen before is still known once the table is full.
    let mut again = ipv4(17, 1, 0, &udp_from(0, 3490, &dlt("S", "again")));
    again[12..16].copy_from_slice(&0u32.to_be_bytes());
    frames.push((2, eth(0x0800, &again)));
    let (f, s) = from_bytes("t", pcap_file(1, &frames), &CaptureOptions::default()).unwrap();
    assert_eq!(f.line_count() as usize, n + 1);
    assert_eq!(s.senders_not_kept, 5);
    // Said (review 2026-11-09, R2: `net.*` queries missed them silently).
    let info = f.info();
    assert_eq!(info.no_sender, 5);
    assert!(info.notes.iter().any(|n| n.starts_with("5 messages have no net.* fields")), "{info:?}");
    let q = |s: &str| Query::compile(s).unwrap().bind(f.schema()).unwrap();
    assert_eq!(crate::query_warnings(&q("net.port == 3490"), &f).len(), 1);
    assert!(crate::query_warnings(&q("apid == S"), &f).is_empty());
    let src = |l: usize| field_named(&f, l as LineNo, "net.src");
    assert_eq!(src(0), Some(crate::FieldValue::Str(b"0.0.0.0".as_slice().into())));
    assert!(src(MAX_SENDERS - 1).is_some());
    assert!((MAX_SENDERS..n).all(|l| src(l).is_none()));
    assert!(src(n).is_some(), "a known sender");
    assert_eq!(field_named(&f, n as LineNo - 1, "apid"), Some(crate::FieldValue::Str(b"S".as_slice().into())));
}

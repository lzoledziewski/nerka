use super::*;
use crate::engine::search::tests::run_on;
use crate::input::dlt::writer::{Msg, Value};
use crate::{DltFile, Filter, Matcher, MatcherOptions, Query, SearchSet, TextFile};

fn text(name: &str, s: &str) -> Box<dyn LogSource> {
    Box::new(TextFile::from_bytes(name, s.as_bytes().to_vec()))
}

fn q(s: &str) -> Filter {
    Filter::Query(Query::compile(s).unwrap_or_else(|e| panic!("{s}: {e}")))
}

fn re(s: &str) -> Filter {
    Filter::Regex(Matcher::new(s, MatcherOptions::default()).unwrap())
}

fn texts() -> Concat {
    Concat::new(vec![
        text("logs/a.log", "[INFO] a one\n[ERROR] a two timeout\n[WARN] a three\n"),
        text("b.log", ""),
        // No final newline: the boundary to c must not glue lines.
        text("other/b2.log", "[ERROR] b timeout\n[INFO] b end"),
        text("c.log", "start of c\n[ERROR] c timeout\n"),
    ])
    .unwrap()
}

#[test]
fn lines_follow_each_other() {
    let c = texts();
    assert_eq!(c.line_count(), 7);
    let all: Vec<String> = (0..7).map(|l| c.line_text(l).unwrap().into_owned()).collect();
    assert_eq!(
        all,
        [
            "[INFO] a one",
            "[ERROR] a two timeout",
            "[WARN] a three",
            "[ERROR] b timeout",
            "[INFO] b end",
            "start of c",
            "[ERROR] c timeout"
        ]
    );
    assert_eq!(c.line_text(7), None);
    assert_eq!(c.part_files(), [(0, 3), (3, 0), (3, 2), (5, 2)]);
    assert_eq!(c.name(), "logs/a.log + b.log + other/b2.log + c.log");
    assert_eq!(c.field(4, c.file_id()), Some(FieldValue::Str(b"b2.log".as_slice().into())));
}

/// Every filter finds the same lines on the fast path (part by part) and
/// the naive per-line path, and the same as each part on its own.
#[test]
fn filters_run_part_by_part_and_match_naive_evaluation() {
    let c = texts();
    assert_eq!(run_on(&c, &re("timeout")), [1, 3, 6]);
    // `$` / `^` at the boundary between b2 (no final newline) and c.
    assert_eq!(run_on(&c, &re("end$")), [4]);
    assert_eq!(run_on(&c, &re("^start")), [5]);
    // A regex reaching over a line end never joins two files.
    assert!(run_on(&c, &re(r"end\nstart")).is_empty());
    assert_eq!(run_on(&c, &q("level >= ERROR")), [1, 3, 6]);
    assert_eq!(run_on(&c, &q("file == b2.log")), [3, 4]);
    assert_eq!(run_on(&c, &q(r#"file ~ /^[ac]\./ AND raw ~ "timeout""#)), [1, 6]);
    assert_eq!(run_on(&c, &q("NOT file == a.log")), [3, 4, 5, 6]);
    // search_within on both of its paths.
    let set = SearchSet { include: vec![re("timeout"), q("level == WARN")], exclude: vec![q("file == c.log")] };
    for path in [crate::engine::within::Path::Scan, crate::engine::within::Path::PerLine] {
        let hits = crate::engine::within::search_within_path(&c, None, &set, &crate::Progress::new(), path).unwrap();
        assert_eq!(hits.lines, [1, 2, 3]);
    }
}

fn dlt_bytes(apids: &[&str]) -> Vec<u8> {
    let mut buf = Vec::new();
    for (i, a) in apids.iter().enumerate() {
        let mut m = Msg::log(crate::Level::Warn, "ECU1", a, "CTX", vec![Value::Str(format!("msg {i}")).into()]);
        m.secs = 100 + i as u32;
        m.encode(&mut buf);
    }
    buf
}

/// Review 2026-11-09, R3: what a reader skipped is said, with the file it
/// is about in a view of several; 2026-11-08, R8: a `proto` no message has
/// is said instead of finding nothing.
#[test]
fn what_a_reader_skipped_and_unknown_protocols_are_said() {
    let mut bytes = dlt_bytes(&["NAV", "SYS"]);
    let half = bytes.len() / 2;
    bytes.splice(half..half, [0xEEu8; 14]);
    let a = DltFile::from_bytes("a.dlt", bytes);
    let info = a.info();
    assert_eq!(info.protocols, ["DLT"]);
    assert_eq!(info.notes.len(), 1, "{info:?}");
    assert!(info.notes[0].ends_with("weren't DLT messages: skipped"), "{info:?}");
    assert!(text("t.log", "x\n").info() == crate::SourceInfo::default());

    let both = Concat::new(vec![Box::new(a), Box::new(DltFile::from_bytes("b.dlt", dlt_bytes(&["NAV"])))]).unwrap();
    let info = both.info();
    assert_eq!(info.protocols, ["DLT"]);
    assert!(info.notes.len() == 1 && info.notes[0].starts_with("a.dlt: "), "{info:?}");

    let warn = |q: &str| crate::query_warnings(&Query::compile(q).unwrap().bind(both.schema()).unwrap(), &both);
    assert_eq!(warn("proto == SENS AND apid == NAV"), ["no message here is of a protocol 'sens' (there are: DLT)"]);
    assert!(warn("proto == dlt").is_empty(), "case doesn't matter");
    assert!(warn("apid == NAV").is_empty());
}

/// Review 2026-11-09 R15: files of one name in different folders are told
/// apart by `file` (the shortest ending of their paths that differs), and a
/// `file` no file is says what to write.
#[test]
fn same_named_files_are_told_apart() {
    let c = Concat::new(vec![
        text("D:/logs/ecu1/trace.log", "[INFO] one\n"),
        text("D:\\logs\\ecu2\\trace.log", "[INFO] two\n"),
        text("D:/logs/other.log", "[INFO] three\n"),
    ])
    .unwrap();
    assert_eq!(c.info().files, ["ecu1/trace.log", "ecu2/trace.log", "other.log"]);
    assert_eq!(run_on(&c, &q(r#"file == "ecu2/trace.log""#)), [1]);
    assert_eq!(run_on(&c, &q("file ~ /ecu1/")), [0]);
    assert_eq!(run_on(&c, &q("file == other.log")), [2]);
    let warn = |s: &str| crate::query_warnings(&Query::compile(s).unwrap().bind(c.schema()).unwrap(), &c);
    assert_eq!(
        warn(r#"file == "D:\\logs\\ecu1\\trace.log""#),
        [r"no file here is 'D:\logs\ecu1\trace.log': write 'ecu1/trace.log'"]
    );
    assert_eq!(warn("file == trace.log"), ["no file here is 'trace.log': write 'ecu1/trace.log' or 'ecu2/trace.log'"]);
    assert_eq!(
        warn("file != nope.log"),
        ["no file here is 'nope.log' (there are: ecu1/trace.log, ecu2/trace.log, other.log)"]
    );
    assert!(warn("file == other.log OR file ~ /trace/").is_empty());
    assert_eq!(short_names(&["a/x.log", "a/x.log"]), ["x.log", "x.log"], "the same file twice");
    assert_eq!(short_names(&["x.log", "b/x.log"]), ["x.log", "b/x.log"]);
}

#[test]
fn dlt_files_and_captures_are_one_kind() {
    let a: Box<dyn LogSource> = Box::new(DltFile::from_bytes("a.dlt", dlt_bytes(&["NAV", "SYS"])));
    // The same kind of messages inside a pcap (via the capture reader).
    let mut pcap = 0xA1B2_C3D4u32.to_le_bytes().to_vec();
    pcap.extend_from_slice(&[2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 1, 0, 0, 0]);
    let msgs = dlt_bytes(&["NAV"]);
    let payload = &msgs[16..];
    let mut udp = vec![0xc3, 0x50, 0x0d, 0xa2];
    udp.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(payload);
    let mut ip = vec![0x45, 0];
    ip.extend_from_slice(&((20 + udp.len()) as u16).to_be_bytes());
    ip.extend_from_slice(&[0, 1, 0, 0, 64, 17, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2]);
    ip.extend_from_slice(&udp);
    let mut frame = vec![2; 12];
    frame.extend_from_slice(&[0x08, 0x00]);
    frame.extend_from_slice(&ip);
    for x in [7u32, 0, frame.len() as u32, frame.len() as u32] {
        pcap.extend_from_slice(&x.to_le_bytes());
    }
    pcap.extend_from_slice(&frame);
    let (b, _) = crate::input::capture::from_bytes("b.pcap", pcap, &Default::default()).unwrap();
    let c = Concat::new(vec![a, Box::new(b)]).unwrap();
    assert_eq!(c.line_count(), 3);
    assert_eq!(c.columns().len(), 7);
    assert_eq!(run_on(&c, &q("apid == NAV")), [0, 2]);
    assert_eq!(run_on(&c, &q("apid == NAV AND file == b.pcap")), [2]);
    assert_eq!(run_on(&c, &q("level >= WARN AND raw ~ /msg 1/")), [1]);

    // Plain text and DLT don't mix.
    let d: Box<dyn LogSource> = Box::new(DltFile::from_bytes("d.dlt", dlt_bytes(&["NAV"])));
    let Err(e) = Concat::new(vec![text("x/app.log", "a\n"), d]) else { panic!("mixed kinds") };
    assert_eq!(e.to_string(), "app.log is plain text but d.dlt is DLT: only files of one kind open as one view");
    assert!(matches!(e, crate::Error::MixedKinds(_)), "a variant of its own: the GUI opens such files as tabs");
}

#[test]
fn open_many_reads_files_in_order() {
    let dir = std::env::temp_dir().join(format!("nerka-concat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b, d) = (dir.join("a.log"), dir.join("b.log"), dir.join("c.dlt"));
    std::fs::write(&a, "one\ntwo\n").unwrap();
    std::fs::write(&b, "three\n").unwrap();
    std::fs::write(&d, dlt_bytes(&["NAV"])).unwrap();
    let p = crate::Progress::new();
    let o = crate::OpenOptions::default();
    let s = open_many(&[b.clone(), a.clone()], &o, &p).unwrap();
    assert_eq!((0..3).map(|l| s.line_text(l).unwrap().into_owned()).collect::<Vec<_>>(), ["three", "one", "two"]);
    assert_eq!(open_many(std::slice::from_ref(&a), &o, &p).unwrap().part_files(), [(0, 2)]);
    assert!(open_many(&[a.clone(), d], &o, &p).is_err());

    // A file that can't be opened is named (review 2026-11-09, R7: the
    // GUI named the first file); its reason is in the text.
    let missing = dir.join("nope.log");
    let Err(e) = open_many(&[a.clone(), missing.clone()], &o, &p) else { panic!("a missing file") };
    assert!(matches!(&e, crate::Error::Open { path, .. } if *path == missing), "{e:?}");
    assert!(e.to_string().starts_with(&format!("{}: I/O error (", missing.display())), "{e}");
    let Err(e) = open_many_by_time(&[missing.clone(), a.clone()], &o, &p) else { panic!("a missing file") };
    assert!(matches!(&e, crate::Error::Open { path, .. } if *path == missing), "{e:?}");

    // JSON files with other keys: said so (R17).
    let (j1, j2) = (dir.join("a.jsonl"), dir.join("b.jsonl"));
    std::fs::write(&j1, "{\"level\":\"INFO\",\"msg\":\"one\"}\n").unwrap();
    std::fs::write(&j2, "{\"sev\":3,\"text\":\"two\"}\n").unwrap();
    let Err(e) = open_many(&[j1, j2], &o, &p) else { panic!("other keys") };
    assert_eq!(
        e.to_string(),
        "a.jsonl and b.jsonl are both JSON but with other keys: only files with the same keys open as one view"
    );
    assert!(matches!(e, crate::Error::MixedKinds(_)));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn files_with_times_open_in_the_order_they_start() {
    let dir = std::env::temp_dir().join(format!("nerka-bytime-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dlt = |secs: u32, apid: &str| {
        let mut buf = Vec::new();
        let mut m = Msg::log(crate::Level::Warn, "ECU1", apid, "CTX", vec![Value::Str(apid.into()).into()]);
        m.secs = secs;
        m.encode(&mut buf);
        buf
    };
    let (early, late, same) = (dir.join("z_early.dlt"), dir.join("a_late.dlt"), dir.join("m_same.dlt"));
    std::fs::write(&early, dlt(100, "EAR")).unwrap();
    std::fs::write(&late, dlt(300, "LAT")).unwrap();
    std::fs::write(&same, dlt(100, "SAM")).unwrap();
    let (p, o) = (crate::Progress::new(), crate::OpenOptions::default());
    // Sorted by start; ties keep the order given.
    let (s, order) = open_many_by_time(&[late.clone(), early.clone(), same.clone()], &o, &p).unwrap();
    assert_eq!(order, [early.clone(), same.clone(), late.clone()]);
    let apids: Vec<String> = (0..3).map(|l| s.line_text(l).unwrap().into_owned()).collect();
    assert!(apids[0].contains("EAR") && apids[1].contains("SAM") && apids[2].contains("LAT"), "{apids:?}");
    assert_eq!(start_time(s.as_ref()), Some(100.0));

    // Text files have no times: the given order stays.
    let (a, b) = (dir.join("a.log"), dir.join("b.log"));
    std::fs::write(&a, "one\n").unwrap();
    std::fs::write(&b, "two\n").unwrap();
    let (s, order) = open_many_by_time(&[b.clone(), a.clone()], &o, &p).unwrap();
    assert_eq!(order, [b, a]);
    assert_eq!(s.line_text(0).unwrap(), "two");
    std::fs::remove_dir_all(&dir).ok();
}

/// A pcap of IPv4 / UDP datagrams: (seconds, destination port, payload).
fn pcap_of(datagrams: &[(u32, u16, Vec<u8>)]) -> Vec<u8> {
    let mut pcap = 0xA1B2_C3D4u32.to_le_bytes().to_vec();
    pcap.extend_from_slice(&[2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 1, 0, 0, 0]);
    for (secs, port, payload) in datagrams {
        let mut udp = vec![0xc3, 0x50];
        udp.extend_from_slice(&port.to_be_bytes());
        udp.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        udp.extend_from_slice(&[0, 0]);
        udp.extend_from_slice(payload);
        let mut ip = vec![0x45, 0];
        ip.extend_from_slice(&((20 + udp.len()) as u16).to_be_bytes());
        ip.extend_from_slice(&[0, 1, 0, 0, 64, 17, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2]);
        ip.extend_from_slice(&udp);
        let mut frame = vec![2; 12];
        frame.extend_from_slice(&[0x08, 0x00]);
        frame.extend_from_slice(&ip);
        for x in [*secs, 0, frame.len() as u32, frame.len() as u32] {
            pcap.extend_from_slice(&x.to_le_bytes());
        }
        pcap.extend_from_slice(&frame);
    }
    pcap
}

/// A protocol definition: a value and a note per datagram on port 41000.
const SENS: &str = "meta: { id: sens, endian: le }
seq:
  - { id: v, type: u2 }
  - { id: note, type: str, size-eos: true, encoding: UTF-8 }
-nerka:
  name: SENS
  framing: datagram
  match: [ 41000 ]
  fields:
    v: v
    note: note
  level:
    from: \"v > 100 ? 2 : 4\"
    map: { 2: error, 4: info }
  columns:
    - { name: v, width: 5 }
    - note
";

fn sens(v: u16, note: &str) -> Vec<u8> {
    let mut m = v.to_le_bytes().to_vec();
    m.extend_from_slice(note.as_bytes());
    m
}

/// Owner, 2026-11-08: DLT from one capture, a definition's messages from
/// another (DLT turned off there) in one view: the mixed layout.
#[test]
fn messages_of_different_protocols_share_the_mixed_layout() {
    use crate::input::capture::{self, CaptureOptions};
    use crate::input::proto::{LoadedProtocol, ksy::KsyProtocol};
    let defs = vec![LoadedProtocol::new(std::sync::Arc::new(KsyProtocol::parse(SENS).unwrap()))];
    let a: Box<dyn LogSource> = Box::new(DltFile::from_bytes("a.dlt", dlt_bytes(&["NAV", "SYS"])));
    // A capture with DLT on 3490 and SENS on 41000.
    let dlt = dlt_bytes(&["GPS"]);
    let both =
        pcap_of(&[(200, 3490, dlt[16..].to_vec()), (201, 41000, sens(150, "hot")), (202, 41000, sens(20, "ok"))]);
    let with = |disabled: &[&str]| CaptureOptions {
        protocols: defs.clone(),
        disabled: disabled.iter().map(|d| d.to_string()).collect(),
        ..Default::default()
    };
    // Turned off there, DLT leaves only SENS: a layout of its own.
    let (b, _) = capture::from_bytes("b.pcap", both.clone(), &with(&["DLT"])).unwrap();
    assert_eq!(b.columns().iter().map(|c| c.name.as_ref()).collect::<Vec<_>>(), ["time", "v", "note"]);
    // Both on: the capture is mixed already.
    let (c, _) = capture::from_bytes("c.pcap", both, &with(&[])).unwrap();
    assert_eq!(c.columns(), MIXED_COLUMNS);

    let all = Concat::new(vec![a, Box::new(b), Box::new(c)]).unwrap();
    assert!(all.is_mixed());
    assert_eq!(all.columns(), MIXED_COLUMNS);
    assert_eq!(all.line_count(), 2 + 2 + 3);
    // Each part's line in the shared layout; one already mixed is as it was.
    let cells = |l| all.line_text(l).unwrap().split('\t').skip(1).map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(cells(0), ["DLT", "0.0000 ECU1 NAV CTX warn", "msg 0"]);
    assert_eq!(cells(2), ["SENS", "150", "hot"]);
    assert_eq!(cells(4), ["DLT", "0.0000 ECU1 GPS CTX warn", "msg 0"]);
    assert_eq!(cells(5), ["SENS", "150", "hot"]);
    let detail = all.line_detail(2).unwrap();
    assert_eq!(detail[1..], ["SENS".to_owned(), "150".to_owned(), "hot".to_owned()]);
    assert_eq!(detail.len(), 4);

    // Fields of every part; one a line's protocol lacks has no value.
    let names: Vec<&str> = all.schema().iter().map(|f| f.name.as_ref()).collect();
    assert_eq!(names[0], "proto");
    assert!(["apid", "payload", "v", "note", "file"].iter().all(|n| names.contains(n)), "{names:?}");
    assert_eq!(run_on(&all, &q("proto == SENS")), [2, 3, 5, 6]);
    assert_eq!(run_on(&all, &q("proto == DLT")), [0, 1, 4]);
    assert_eq!(run_on(&all, &q("apid == NAV OR v > 100")), [0, 2, 5]);
    assert_eq!(run_on(&all, &q("apid != NAV")), [1, 2, 3, 4, 5, 6]);
    assert_eq!(run_on(&all, &q("level >= ERROR")), [2, 5]);
    assert_eq!(run_on(&all, &q("note ~ /ok/ AND file == b.pcap")), [3]);
    assert_eq!(run_on(&all, &q("payload ~ /msg/")), [0, 1, 4]);
    // The captures' `net.*` fields; the `.dlt` file has none.
    assert_eq!(run_on(&all, &q("net.port == 41000")), [2, 3, 5, 6]);
    assert_eq!(run_on(&all, &q("net.src == 10.0.0.1")), [2, 3, 4, 5, 6]);
    // `raw` is the line as shown, on both paths.
    assert_eq!(run_on(&all, &re("\tSENS\t150\t")), [2, 5]);
    assert_eq!(run_on(&all, &q(r#"raw ~ /^\S+ \S+\tDLT\t/"#)), [0, 1, 4]);

    // Interleaved by time, too.
    let merged = crate::input::merged::Merged::new(all, &crate::Progress::new()).unwrap();
    assert_eq!(merged.columns(), MIXED_COLUMNS);
    let order: Vec<String> =
        (0..7).map(|l| merged.line_text(l).unwrap().split('\t').nth(1).unwrap().to_owned()).collect();
    assert_eq!(order, ["DLT", "DLT", "DLT", "SENS", "SENS", "SENS", "SENS"]);
    assert_eq!(run_on(&merged, &q("proto == SENS AND v > 100")).len(), 2);
}

/// Review 2026-11-09, R1: two captures of different protocols whose fields
/// are the same (name and kind) but whose columns differ share the mixed
/// layout; each part's fields must still be found by name (`proto` moves
/// to the front of the whole's fields, so the ids differ from a part's).
#[test]
fn mixed_parts_with_the_same_fields_keep_their_fields() {
    use crate::input::capture::{self, CaptureOptions};
    use crate::input::proto::{LoadedProtocol, ksy::KsyProtocol};
    // The same fields as SENS, other columns, another name and port.
    let other = SENS
        .replace("name: SENS", "name: SENB")
        .replace("41000", "41001")
        .replace("    - { name: v, width: 5 }\n    - note\n", "    - note\n");
    assert_ne!(other, SENS);
    let open = |name: &str, def: &str, port: u16, v: u16, note: &str| -> Box<dyn LogSource> {
        let defs = vec![LoadedProtocol::new(std::sync::Arc::new(KsyProtocol::parse(def).unwrap()))];
        let o = CaptureOptions { protocols: defs, ..Default::default() };
        Box::new(capture::from_bytes(name, pcap_of(&[(100, port, sens(v, note))]), &o).unwrap().0)
    };
    let (a, b) = (open("a.pcap", SENS, 41000, 150, "hot"), open("b.pcap", &other, 41001, 20, "ok"));
    assert_eq!(a.schema(), b.schema(), "the case: equal fields");
    assert_ne!(a.columns(), b.columns(), "the case: other columns");
    let all = Concat::new(vec![a, b]).unwrap();
    assert!(all.is_mixed());
    assert_eq!(run_on(&all, &q("proto == SENS")), [0]);
    assert_eq!(run_on(&all, &q("proto == SENB")), [1]);
    assert_eq!(run_on(&all, &q("v > 100")), [0]);
    assert_eq!(run_on(&all, &q("note == ok")), [1]);
    assert_eq!(run_on(&all, &q("net.port == 41001")), [1]);
    assert_eq!(run_on(&all, &q("file == b.pcap AND v == 20")), [1]);
}

/// A definition whose `type` is a number: DLT's `type` is text.
const TYPED: &str = "meta: { id: typed, endian: le }
seq:
  - { id: kind, type: u1 }
  - { id: note, type: str, size-eos: true, encoding: UTF-8 }
-nerka:
  name: TYPED
  framing: datagram
  match: [ 41001 ]
  fields:
    type: kind
    note: note
  columns:
    - note
";

/// Every field of `whole`, by name, on every line, against the part's own
/// (review 2026-11-09, ARCH-10: R1 and R9 were positions read in the wrong
/// part's schema). A field split by a clash (`dlt.type`) is the part's
/// `type` in the part it is named after, none elsewhere; `file` is the
/// whole's own.
fn fields_agree(whole: &dyn LogSource, parts: &[(&dyn LogSource, &str)]) {
    let lines = whole.part_files();
    for (i, d) in whole.schema().iter().enumerate() {
        if d.name == FILE_FIELD {
            continue;
        }
        for (k, &(first, n)) in lines.iter().enumerate() {
            let (part, qual) = parts[k];
            let own = if part.schema().iter().any(|p| p.name == d.name) {
                Some(d.name.as_ref())
            } else {
                match d.name.split_once('.') {
                    Some((q, base)) if !matches!(q, "net" | "json") => (q == qual).then_some(base),
                    _ => Some(d.name.as_ref()),
                }
            };
            let id = own.and_then(|o| part.schema().iter().position(|p| p.name == o && p.kind == d.kind));
            for l in 0..n {
                let want = id.and_then(|id| part.field(l, id)).map(|v| format!("{v:?}"));
                let got = whole.field(first + l, i).map(|v| format!("{v:?}"));
                assert_eq!(got, want, "field {} on line {} (part {k}, line {l})", d.name, first + l);
            }
        }
    }
}

/// Review 2026-11-09, R9: a name two protocols give different kinds is a
/// field of each (`dlt.type`, `typed.type`), in a capture of both and in a
/// view of files; a plain `type` says which to write. Every field reads
/// its part's own value, merged by time too.
#[test]
fn a_name_of_two_kinds_is_a_field_of_each() {
    use crate::input::capture::{self, CaptureOptions};
    use crate::input::proto::{LoadedProtocol, ksy::KsyProtocol};
    let defs = vec![LoadedProtocol::new(std::sync::Arc::new(KsyProtocol::parse(TYPED).unwrap()))];
    let typed = |k: u8, note: &str| {
        let mut m = vec![k];
        m.extend_from_slice(note.as_bytes());
        m
    };
    let dlt = dlt_bytes(&["GPS"]);
    let datagrams = [(200, 3490, dlt[16..].to_vec()), (201, 41001, typed(7, "seven")), (202, 41001, typed(3, "three"))];
    let options = CaptureOptions { protocols: defs, ..Default::default() };

    // One capture of both: its own fields are split.
    let (c, _) = capture::from_bytes("c.pcap", pcap_of(&datagrams), &options).unwrap();
    let names: Vec<&str> = c.schema().iter().map(|f| f.name.as_ref()).collect();
    assert!(names.contains(&"dlt.type") && names.contains(&"typed.type") && !names.contains(&"type"), "{names:?}");
    assert_eq!(run_on(&c, &q("typed.type == 7")), [1]);
    let e = Query::compile("type == 7").unwrap().bind(c.schema()).unwrap_err();
    assert!(e.msg.contains("write dlt.type or typed.type"), "{e}");

    // A `.dlt` file and a capture of TYPED only: the view splits them.
    let a = DltFile::from_bytes("a.dlt", dlt_bytes(&["NAV", "SYS"]));
    let only = CaptureOptions { disabled: vec!["DLT".into()], ..options.clone() };
    let (b, _) = capture::from_bytes("b.pcap", pcap_of(&datagrams), &only).unwrap();
    let parts: [(&dyn LogSource, &str); 2] = [(&a, "dlt"), (&b, "typed")];
    let a2 = DltFile::from_bytes("a.dlt", dlt_bytes(&["NAV", "SYS"]));
    let (b2, _) = capture::from_bytes("b.pcap", pcap_of(&datagrams), &only).unwrap();
    let view = Concat::new(vec![Box::new(a2), Box::new(b2)]).unwrap();
    let names: Vec<&str> = view.schema().iter().map(|f| f.name.as_ref()).collect();
    assert!(names.contains(&"dlt.type") && names.contains(&"typed.type"), "{names:?}");
    fields_agree(&view, &parts);
    assert_eq!(run_on(&view, &q("typed.type < 5")), [3]);
    assert_eq!(run_on(&view, &q("dlt.type ~ /log/ AND apid == SYS")), [1]);

    // The earlier layouts: same kind (mapped), mixed, mixed with one schema.
    let same = Concat::new(vec![
        Box::new(DltFile::from_bytes("a.dlt", dlt_bytes(&["NAV"]))),
        Box::new(DltFile::from_bytes("b.dlt", dlt_bytes(&["SYS", "GPS"]))),
    ])
    .unwrap();
    let (x, y) =
        (DltFile::from_bytes("a.dlt", dlt_bytes(&["NAV"])), DltFile::from_bytes("b.dlt", dlt_bytes(&["SYS", "GPS"])));
    fields_agree(&same, &[(&x, "dlt"), (&y, "dlt")]);
    let (c2, _) = capture::from_bytes("c.pcap", pcap_of(&datagrams), &options).unwrap();
    let (c3, _) = capture::from_bytes("c.pcap", pcap_of(&datagrams), &options).unwrap();
    let a3 = DltFile::from_bytes("a.dlt", dlt_bytes(&["NAV", "SYS"]));
    let a4 = DltFile::from_bytes("a.dlt", dlt_bytes(&["NAV", "SYS"]));
    let mixed = Concat::new(vec![Box::new(a3), Box::new(c2)]).unwrap();
    assert!(mixed.is_mixed());
    fields_agree(&mixed, &[(&a4, "dlt"), (&c3, "c")]);
    // The `.dlt` file's `type` is `dlt.type` there too: one name for DLT's.
    assert!(mixed.schema().iter().all(|f| f.name != "type"));
    assert_eq!(run_on(&mixed, &q("dlt.type ~ /log/")), [0, 1, 2]);

    // Merged by time: the same values, by name.
    let merged = crate::input::merged::Merged::new(view, &crate::Progress::new()).unwrap();
    let typed_rows = run_on(&merged, &q("typed.type == 7 OR typed.type == 3"));
    assert_eq!(typed_rows.len(), 2);
    assert_eq!(run_on(&merged, &q("dlt.type ~ /log/")).len(), 2);
}

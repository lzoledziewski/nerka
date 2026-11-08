use super::writer::{Arg, Extended, Msg, Payload, Value, id};
use super::*;
use crate::engine::search::tests::run_on;
use crate::input::proto::{Framed, MAX_OFFSET, MAX_ORIGINS, MAX_PROTOCOLS, Origins};
use crate::{Filter, LineNo, LogSource, Matcher, Query};
use fibex::Fibex;

fn file(msgs: &[Msg]) -> DltFile {
    let mut buf = Vec::new();
    msgs.iter().for_each(|m| m.encode(&mut buf));
    DltFile::from_bytes("t.dlt", buf)
}

fn line(f: &DltFile, l: LineNo) -> String {
    f.line_text(l).unwrap().into_owned()
}

fn payload(f: &DltFile, l: LineNo) -> String {
    line(f, l).splitn(COLUMNS.len(), '\t').last().unwrap().to_owned()
}

fn named(value: Value, name: &str, unit: &str) -> Arg {
    Arg { value, name: Some((name.into(), unit.into())) }
}

fn all_types() -> Vec<Arg> {
    vec![
        Value::Str("hello".into()).into(),
        Value::U(42, 4).into(),
        Value::I(-5, 1).into(),
        Value::I(-300, 2).into(),
        Value::F32(1.5).into(),
        Value::F64(-2.25).into(),
        Value::Bool(true).into(),
        Value::Raw(vec![0xde, 0xad]).into(),
        Value::U(u64::MAX, 8).into(),
        Value::I(i64::MIN, 8).into(),
        named(Value::U(88, 2), "speed", "km/h"),
        named(Value::Str("x".into()), "label", ""),
        named(Value::Bool(false), "ok", ""),
    ]
}

const ALL_TYPES: &str =
    "hello 42 -5 -300 1.5 -2.25 true de ad 18446744073709551615 -9223372036854775808 speed=88 km/h label=x ok=false";

#[test]
fn verbose_arguments_both_byte_orders() {
    for big_endian in [false, true] {
        let m = Msg { big_endian, ..Msg::log(Level::Info, "ECU1", "NAV", "POS", all_types()) };
        let f = file(&[m]);
        assert_eq!(f.skipped_bytes(), 0);
        assert_eq!(payload(&f, 0), ALL_TYPES, "big_endian={big_endian}");
    }
}

#[test]
fn line_columns_and_fields() {
    let m = Msg {
        usecs: 123_456,
        timestamp: Some(12_345_678),
        session_id: Some(7),
        mcnt: 200,
        ..Msg::log(Level::Warn, "ECU1", "NAV", "POS", vec![Value::Str("slow".into()).into()])
    };
    let f = file(&[m]);
    assert_eq!(f.line_count(), 1);
    assert_eq!(line(&f, 0), "2025-10-18 00:00:00.123456\t1234.5678\tECU1\tNAV\tPOS\twarn\tslow");
    let field = |name| f.field(0, SCHEMA.iter().position(|d| d.name == name).unwrap());
    assert_eq!(field("ecu"), Some(FieldValue::Str(b"ECU1"[..].into())));
    assert_eq!(field("apid"), Some(FieldValue::Str(b"NAV"[..].into())));
    assert_eq!(field("type"), Some(FieldValue::Str(b"log"[..].into())));
    assert_eq!(field("mcnt"), Some(FieldValue::U64(200)));
    assert_eq!(field("tmsp"), Some(FieldValue::U64(12_345_678)));
    assert_eq!(field("seid"), Some(FieldValue::U64(7)));
    assert_eq!(field("mid"), None);
    assert_eq!(field("mode"), Some(FieldValue::Str(b"verbose"[..].into())));
    assert_eq!(field("payload"), Some(FieldValue::Str(b"slow"[..].into())));
    assert_eq!(f.supplied_level(0), Some(Some(Level::Warn)));
}

#[test]
fn optional_headers_missing() {
    // No WEID, no timestamp, no extended header: non-verbose by definition.
    let m = Msg {
        storage_ecu: id("STOR"),
        ecu: None,
        timestamp: None,
        ext: None,
        payload: Payload::NonVerbose { id: 0x1234, data: vec![1, 2] },
        ..Msg::log(Level::Info, "X", "", "", vec![])
    };
    let f = file(&[m]);
    assert_eq!(line(&f, 0), "2025-10-18 00:00:00.000000\t\tSTOR\t\t\t\t[4660] 01 02");
    assert_eq!(f.field(0, ECU), Some(FieldValue::Str(b"STOR"[..].into())));
    assert_eq!(f.field(0, APID), None);
    assert_eq!(f.field(0, TYPE), None);
    assert_eq!(f.field(0, TMSP), None);
    assert_eq!(f.field(0, MID), Some(FieldValue::U64(0x1234)));
    assert_eq!(f.field(0, MODE), Some(FieldValue::Str(b"non-verbose"[..].into())));
    assert_eq!(f.supplied_level(0), Some(None));
}

#[test]
fn non_verbose_with_extended_header() {
    let m = Msg {
        big_endian: true,
        payload: Payload::NonVerbose { id: 7, data: vec![0xff] },
        ..Msg::log(Level::Error, "ECU1", "DIAG", "UDS", vec![])
    };
    let f = file(&[m]);
    assert_eq!(payload(&f, 0), "[7] ff");
    assert_eq!(f.field(0, MID), Some(FieldValue::U64(7)));
    assert_eq!(f.supplied_level(0), Some(Some(Level::Error)));
}

#[test]
fn control_messages() {
    let mut resp = 0x13u32.to_le_bytes().to_vec();
    resp.push(0);
    resp.extend_from_slice(&5u32.to_le_bytes());
    resp.extend_from_slice(b"1.2.3");
    let ctrl = |mtin, payload| Msg {
        ext: Some(Extended { mstp: 3, mtin, apid: id("DA1"), ctid: id("DC1") }),
        payload: Payload::Bytes(payload),
        ..Msg::log(Level::Info, "ECU1", "", "", vec![])
    };
    let f = file(&[ctrl(2, resp), ctrl(1, vec![0x03, 0, 0, 0, 9]), ctrl(1, vec![0x99, 0, 0, 0])]);
    assert_eq!(payload(&f, 0), "[get_software_version ok] 1.2.3");
    assert_eq!(payload(&f, 1), "[get_log_info] 09");
    assert_eq!(payload(&f, 2), "[service 0x99]");
    assert!(line(&f, 0).contains("\tcontrol\t"));
    assert_eq!(f.supplied_level(0), Some(None));
    assert_eq!(f.field(0, TYPE), Some(FieldValue::Str(b"control"[..].into())));
    assert_eq!(f.field(0, MID), None);
}

#[test]
fn unsupported_argument_falls_back_to_hex() {
    let mut buf = Vec::new();
    Msg::log(Level::Info, "E", "A", "C", vec![Value::Str("ok".into()).into(), Value::U(1, 4).into()]).encode(&mut buf);
    // Turn the second argument into an array (unsupported).
    let ti = buf.len() - 8;
    buf[ti..ti + 4].copy_from_slice(&0x0100_u32.to_le_bytes());
    let f = DltFile::from_bytes("t", buf);
    assert_eq!(payload(&f, 0), "ok <undecoded> 00 01 00 00 01 00 00 00");
}

#[test]
fn control_characters_stay_in_their_column() {
    let f = file(&[Msg::log(Level::Info, "E", "A", "C", vec![Value::Str("a\tb\nc".into()).into()])]);
    let l = line(&f, 0);
    assert_eq!(l.matches('\t').count(), COLUMNS.len() - 1);
    assert!(l.ends_with("\ta b c"), "{l}");
}

#[test]
fn line_detail_keeps_the_payload_formatting() {
    let multi = Value::Str("first\r\n\tsecond\rthird\nfourth\x01".into());
    let f = file(&[Msg::log(Level::Warn, "E", "A", "C", vec![multi.into(), Value::U(7, 1).into()])]);
    let cells = f.line_detail(0).unwrap();
    assert_eq!(cells.len(), COLUMNS.len());
    assert_eq!(cells.last().unwrap(), "first\n\tsecond\nthird\nfourth  7");
    // The other cells are the line's own.
    let l = line(&f, 0);
    let flat: Vec<&str> = l.splitn(COLUMNS.len(), '\t').collect();
    assert_eq!(cells[..COLUMNS.len() - 1], flat[..COLUMNS.len() - 1]);
    assert_eq!(*flat.last().unwrap(), "first   second third fourth  7");
    assert_eq!(f.line_detail(1), None);
}

#[test]
fn corrupt_data_is_skipped_and_counted() {
    let m = |s: &str| Msg::log(Level::Info, "E", "A", "C", vec![Value::Str(s.into()).into()]);
    let mut buf = Vec::new();
    m("one").encode(&mut buf);
    buf.extend_from_slice(b"garbage");
    // A fake magic followed by a bad header.
    buf.extend_from_slice(b"DLT\x01xx");
    m("two").encode(&mut buf);
    let mut last = Vec::new();
    m("three").encode(&mut last);
    buf.extend_from_slice(&last[..last.len() / 2]);

    let f = DltFile::from_bytes("t", buf);
    assert_eq!(f.line_count(), 2);
    assert_eq!(payload(&f, 0), "one");
    assert_eq!(payload(&f, 1), "two");
    assert_eq!(f.skipped_bytes(), (7 + 6 + last.len() / 2) as u64);
}

#[test]
fn empty_and_tiny_inputs() {
    assert_eq!(DltFile::from_bytes("t", Vec::new()).line_count(), 0);
    let f = DltFile::from_bytes("t", b"DLT\x01".to_vec());
    assert_eq!((f.line_count(), f.skipped_bytes()), (0, 4));
}

/// A mixed file: several ECUs/APIDs/levels, control and non-verbose
/// messages, and messages without optional headers.
fn mixed() -> DltFile {
    let ecus = ["ECU1", "ECU2", "GW"];
    let apids = ["NAV", "DIAG", "SYS", "MEDI"];
    let levels = [Level::Fatal, Level::Error, Level::Warn, Level::Info, Level::Debug, Level::Trace];
    let mut msgs = Vec::new();
    for i in 0..3000u32 {
        let (ecu, apid, level) = (ecus[i as usize % 3], apids[i as usize / 3 % 4], levels[i as usize % 6]);
        let mut m = Msg::log(
            level,
            ecu,
            apid,
            if i % 2 == 0 { "CTX" } else { "MAIN" },
            vec![
                Value::Str(if i % 7 == 0 { "request failed" } else { "ok" }.into()).into(),
                named(Value::U(i as u64 % 30, 2), "speed", ""),
            ],
        );
        m.mcnt = i as u8;
        m.timestamp = Some(i * 100);
        m.big_endian = i % 5 == 0;
        m.session_id = (i % 4 != 0).then_some(i % 4);
        match i % 50 {
            10 => m.payload = Payload::NonVerbose { id: i % 9, data: vec![1, 2, 3] },
            20 => {
                m.ext = Some(Extended { mstp: 3, mtin: 1, apid: id("DA1"), ctid: id("DC1") });
                m.payload = Payload::Bytes(vec![0x13, 0, 0, 0]);
            }
            30 => {
                m.ext = None;
                m.ecu = None;
                m.payload = Payload::NonVerbose { id: 7, data: vec![] };
            }
            _ => {}
        }
        msgs.push(m);
    }
    file(&msgs)
}

#[test]
fn engine_queries_match_naive_evaluation() {
    let f = mixed();
    let q = |s: &str| Filter::Query(Query::compile(s).unwrap());
    let count = |s: &str| run_on(&f, &q(s)).len();
    assert_eq!(count("ecu == ECU1"), 1000);
    // 250 GW/NAV messages, minus 10 control (apid DA1) and 10 without an
    // extended header.
    assert_eq!(count(r#"ecu == "GW" AND apid == NAV"#), 230);
    assert_eq!(
        count("apid == NAV AND level >= WARN"),
        count("apid == NAV AND (level == FATAL OR level == ERROR OR level == WARN)")
    );
    assert!(count("level >= ERROR AND raw ~ /failed/") > 0);
    assert_eq!(count("type == control"), 60);
    // Every 50th message from 10, 20 and 30 is non-verbose (control included).
    assert_eq!(count("mode == non-verbose"), 180);
    assert_eq!(count("mode == verbose"), 3000 - 180);
    assert_eq!(count("mode == verbose AND mid != 0"), 3000 - 180);
    assert_eq!(count("mid == 7"), count("mid == 7 AND (type == log OR type != log)"));
    // Odd messages; the control and header-less ones are all even.
    assert_eq!(count("ctid ~ /^M/"), 1500);
    assert!(count("tmsp >= 100000 AND tmsp < 0x30d40") > 0);
    assert!(count("seid != 2 AND mcnt > 100") > 0);
    assert!(count(r"raw ~ /speed=1\d/ AND NOT apid == SYS") > 0);
    assert!(count("level == VERBOSE OR level == TRACE") > 0);
    // `payload` sees only the payload column, `raw` the whole line.
    assert_eq!(count(r"payload ~ /speed=1\d/"), count(r"raw ~ /speed=1\d/"));
    assert!(count(r"payload ~ /speed=1\d/") > 0);
    assert_eq!(count(r#"payload ~ "ECU""#), 0);
    assert!(count(r#"raw ~ "ECU""#) > 0);
    assert_eq!(count(r#"payload !~ "ECU" AND apid == NAV"#), count("apid == NAV"));
    // Columns are tab-separated, so a regex can span them.
    assert!(!run_on(&f, &Filter::Regex(Matcher::new("ECU2\tDIAG", Default::default()).unwrap())).is_empty());
    let inv = crate::MatcherOptions { invert: true, ..Default::default() };
    assert!(!run_on(&f, &Filter::Regex(Matcher::new("ok", inv).unwrap())).is_empty());
}

/// `f`'s messages without their storage headers, as a capture holds them:
/// odd ones in the side buffer, junk between the others in the file; the
/// storage ECU becomes the channel's.
fn framed_copy(f: &DltFile) -> DltFile {
    let bytes = f.file_bytes();
    let mut offsets = Vec::new();
    let mut pos = 0;
    while let Some(m) = Message::parse(&bytes[pos..]) {
        offsets.push(pos);
        pos += STORAGE_LEN + m.len();
    }
    assert_eq!(offsets.len() as u64, f.line_count());
    let (mut data, mut side, mut msgs) = (b"capture header".to_vec(), Vec::new(), Vec::new());
    let mut channel_ecus: Vec<[u8; 4]> = Vec::new();
    for (i, &off) in offsets.iter().enumerate() {
        let buf = &bytes[off..];
        let s = format::Storage::parse(buf).unwrap();
        let m = Message::parse(buf).unwrap();
        let bytes = &buf[format::STORAGE_LEN..format::STORAGE_LEN + m.len()];
        let ch = channel_ecus.iter().position(|e| e == s.ecu).unwrap_or_else(|| {
            channel_ecus.push(*s.ecu);
            channel_ecus.len() - 1
        });
        let in_side = i % 2 == 1;
        let target = if in_side { &mut side } else { &mut data };
        target.extend_from_slice(b"\xffjunk");
        msgs.push(Framed::new(target.len() as u64, in_side, 0, ch, s.secs, s.usecs).unwrap());
        target.extend_from_slice(bytes);
    }
    let protos: Vec<Arc<dyn Protocol>> = vec![Arc::new(Dlt::default())];
    let origins = Origins::new(channel_ecus, Vec::new(), Vec::new());
    Records::framed("t.pcap".into(), None, crate::input::Bytes::Owned(data), msgs, side, origins, protos, 0)
}

#[test]
fn messages_read_in_place_match_the_dlt_file() {
    let f = mixed();
    let g = framed_copy(&f);
    assert_eq!(g.line_count(), f.line_count());
    for l in 0..f.line_count() {
        assert_eq!(g.line_text(l), f.line_text(l), "line {l}");
        assert_eq!(g.line_detail(l), f.line_detail(l), "line {l}");
        assert_eq!(g.supplied_level(l), f.supplied_level(l), "line {l}");
        for (id, def) in SCHEMA.iter().enumerate() {
            assert_eq!(g.field(l, id), f.field(l, id), "line {l} field {}", def.name);
        }
    }
    // Same lines through the engine (fast path against naive, both).
    let q = |s: &str| Filter::Query(Query::compile(s).unwrap());
    for s in ["ecu == ECU1", "ecu == GW AND apid == NAV", "level >= ERROR AND raw ~ /failed/", "mode == non-verbose"] {
        assert_eq!(run_on(&g, &q(s)), run_on(&f, &q(s)), "{s}");
    }
    // Out-of-range offsets, protocols and channels are refused.
    assert!(Framed::new(MAX_OFFSET + 1, false, 0, 0, 0, 0).is_none());
    assert!(Framed::new(0, false, 0, MAX_ORIGINS, 0, 0).is_none());
    assert!(Framed::new(0, false, MAX_PROTOCOLS, 0, 0, 0).is_none());
    let x = Framed::new(MAX_OFFSET, true, MAX_PROTOCOLS - 1, MAX_ORIGINS - 1, 1, 2).unwrap();
    let got = (x.offset(), x.in_side(), x.proto(), x.origin());
    assert_eq!(got, (MAX_OFFSET as usize, true, MAX_PROTOCOLS - 1, MAX_ORIGINS - 1));
}

#[test]
fn open_picks_reader_from_content() {
    let dir = std::env::temp_dir().join(format!("nerka-dlt-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut buf = Vec::new();
    Msg::log(Level::Info, "E", "A", "C", vec![Value::Str("hi".into()).into()]).encode(&mut buf);
    let (dlt, txt) = (dir.join("x.log"), dir.join("y.dlt"));
    std::fs::write(&dlt, &buf).unwrap();
    std::fs::write(&txt, "plain\ntext\n").unwrap();
    let p = Progress::new();
    let (a, b) = (crate::open(&dlt, &p).unwrap(), crate::open(&txt, &p).unwrap());
    // DLT's fields, and `proto`.
    assert_eq!((a.line_count(), a.schema().len()), (1, SCHEMA.len() + 1));
    assert_eq!((b.line_count(), b.schema().len()), (2, 0));
    drop((a, b));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn receive_time_is_the_built_in_time_signal() {
    let msg = |secs, usecs, text: &str| {
        let mut m = Msg::log(Level::Info, "E", "A", "C", vec![Value::Str(text.into()).into()]);
        (m.secs, m.usecs) = (secs, usecs);
        m
    };
    // 2025-10-18 00:00:00, +0.5 s, +0.5 s again, +2 s.
    let t0 = 1_760_745_600;
    let f = file(&[msg(t0, 0, "a"), msg(t0, 500_000, "b"), msg(t0, 500_000, "c"), msg(t0 + 2, 0, "d")]);
    assert_eq!(f.time(1), Some(f64::from(t0) + 0.5));
    let mut sig = crate::Signals::default();
    sig.set(crate::SignalDef::source_time());
    let run = |s: &str| run_on(&f, &Filter::Query(Query::compile(s).unwrap().with_signals(&sig).unwrap()));
    assert_eq!(run(r#"@time >= "2025-10-18 00:00:00.5""#), [1, 2, 3]);
    assert_eq!(run(r#"@time < "00:00:01" AND /b|c|d/"#), [1, 2]);
    assert_eq!(run("@time.event"), [0, 1, 2, 3]);
    assert_eq!(run("@time.changed"), [0, 1, 3]);
    assert!(matches!(crate::Clock::of(&f, &sig), crate::Clock::Source));
}

/// A small FIBEX in the common convention: message 10 is a warning from
/// NAV/SPD, "speed: <u16> km/h".
const SPEED_FIBEX: &str = r#"<fx:FIBEX xmlns:fx="http://www.asam.net/xml/fbx" xmlns:ho="http://www.asam.net/xml">
  <fx:ELEMENTS>
    <fx:ECUS><fx:ECU ID="ECU1"/></fx:ECUS>
    <fx:PDUS>
      <fx:PDU ID="a"><ho:DESC>speed:</ho:DESC></fx:PDU>
      <fx:PDU ID="b"><fx:SIGNAL-INSTANCES><fx:SIGNAL-INSTANCE ID="s"><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER>
        <fx:SIGNAL-REF ID-REF="S_UINT16"/></fx:SIGNAL-INSTANCE></fx:SIGNAL-INSTANCES></fx:PDU>
      <fx:PDU ID="c"><ho:DESC>km/h</ho:DESC></fx:PDU>
    </fx:PDUS>
    <fx:FRAMES><fx:FRAME ID="ID_10"><fx:PDU-INSTANCES>
      <fx:PDU-INSTANCE ID="1"><fx:PDU-REF ID-REF="a"/><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
      <fx:PDU-INSTANCE ID="2"><fx:PDU-REF ID-REF="b"/><fx:SEQUENCE-NUMBER>1</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
      <fx:PDU-INSTANCE ID="3"><fx:PDU-REF ID-REF="c"/><fx:SEQUENCE-NUMBER>2</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
    </fx:PDU-INSTANCES><fx:MANUFACTURER-EXTENSION>
      <MESSAGE_TYPE>DLT_TYPE_LOG</MESSAGE_TYPE><MESSAGE_INFO>DLT_LOG_WARN</MESSAGE_INFO>
      <APPLICATION_ID>NAV</APPLICATION_ID><CONTEXT_ID>SPD</CONTEXT_ID>
    </fx:MANUFACTURER-EXTENSION></fx:FRAME></fx:FRAMES>
  </fx:ELEMENTS>
</fx:FIBEX>"#;

/// Non-verbose speed messages (without extended header), a verbose one, an
/// unknown non-verbose one, and one with its own extended header.
fn speed_file(fibex: bool) -> DltFile {
    let nv = |speed: u16, big_endian: bool| {
        let data = if big_endian { speed.to_be_bytes() } else { speed.to_le_bytes() }.to_vec();
        Msg {
            ext: None,
            big_endian,
            payload: Payload::NonVerbose { id: 10, data },
            ..Msg::log(Level::Info, "ECU1", "", "", vec![])
        }
    };
    let msgs = [
        nv(30, false),
        Msg::log(Level::Error, "ECU1", "SYS", "PWR", vec![Value::Str("low".into()).into()]),
        nv(130, true),
        Msg {
            ext: None,
            payload: Payload::NonVerbose { id: 99, data: vec![1] },
            ..Msg::log(Level::Info, "ECU1", "", "", vec![])
        },
        Msg {
            payload: Payload::NonVerbose { id: 10, data: vec![5, 0] },
            ..Msg::log(Level::Debug, "ECU1", "OWN", "CTX", vec![])
        },
    ];
    let mut f = Fibex::default();
    f.add("speed.xml", SPEED_FIBEX).unwrap();
    file(&msgs).with_fibex(fibex.then(|| Arc::new(f)))
}

#[test]
fn fibex_decodes_non_verbose_messages() {
    let f = speed_file(true);
    assert_eq!(payload(&f, 0), "speed: 30 km/h");
    assert_eq!(payload(&f, 2), "speed: 130 km/h", "big endian");
    assert_eq!(payload(&f, 3), "[99] 01", "not described");
    // The header columns come from FIBEX when the message has none.
    assert!(line(&f, 0).ends_with("\tECU1\tNAV\tSPD\twarn\tspeed: 30 km/h"), "{}", line(&f, 0));
    let field = |l, name| f.field(l, SCHEMA.iter().position(|d| d.name == name).unwrap());
    assert_eq!(field(0, "apid"), Some(FieldValue::Str(b"NAV"[..].into())));
    assert_eq!(field(0, "ctid"), Some(FieldValue::Str(b"SPD"[..].into())));
    assert_eq!(field(0, "type"), Some(FieldValue::Str(b"log"[..].into())));
    assert_eq!(f.supplied_level(0), Some(Some(Level::Warn)));
    // A message's own extended header wins; its payload is still decoded.
    assert!(line(&f, 4).ends_with("\tOWN\tCTX\tdebug\tspeed: 5 km/h"), "{}", line(&f, 4));
    assert_eq!(f.supplied_level(4), Some(Some(Level::Debug)));
    // Without FIBEX nothing changes.
    let raw = speed_file(false);
    assert_eq!(payload(&raw, 0), "[10] 1e 00");
    assert_eq!(raw.supplied_level(0), Some(None));
}

#[test]
fn fibex_fields_filter_on_both_paths() {
    let f = speed_file(true);
    let q = |s: &str| run_on(&f, &Filter::Query(Query::compile_for(s, SCHEMA).unwrap()));
    assert_eq!(q("apid == NAV"), [0, 2]);
    assert_eq!(q("level >= WARN"), [0, 1, 2]);
    assert_eq!(q(r"payload ~ /speed: 1\d\d/"), [2]);
    assert_eq!(q(r"/km\/h/ AND ctid == SPD"), [0, 2]);
    assert_eq!(q("mid == 10 AND apid != NAV"), [4]);
}

#[test]
fn fibex_arguments_are_signals() {
    let f = speed_file(true);
    let mut sig = crate::Signals::default();
    sig.set(
        crate::SignalDef::argument("speed", crate::SignalKind::Num, r#"mid == 10 AND ecu == "ECU1""#, 0, "m10")
            .unwrap(),
    );
    let q = |s: &str| run_on(&f, &Filter::Query(Query::compile(s).unwrap().with_signals(&sig).unwrap()));
    assert_eq!(q("@speed.event"), [0, 2, 4]);
    assert_eq!(q("@speed.event > 100"), [2]);
    // The state holds over line 3 (another message).
    assert_eq!(q("@speed > 100"), [2, 3]);
    assert_eq!(q("@speed.changed AND @speed < 50"), [0, 4]);
    // Without FIBEX the messages have no arguments.
    let raw = speed_file(false);
    assert!(
        run_on(&raw, &Filter::Query(Query::compile("@speed.event").unwrap().with_signals(&sig).unwrap())).is_empty()
    );
}

use super::*;

const DLT: &str = include_str!("../../../../tests/data/dlt.ksy");

fn load(text: &str) -> KsyProtocol {
    KsyProtocol::parse(text).unwrap_or_else(|e| panic!("{e}"))
}

fn error(text: &str) -> (usize, usize, String) {
    match KsyProtocol::parse(text) {
        Ok(_) => panic!("should fail"),
        Err(e) => (e.line, e.col, e.message),
    }
}

const AT: Origin<'static> = Origin { secs: 1, usecs: 2, channel: b"CH\0\0" };

fn field(p: &KsyProtocol, msg: &[u8], name: &str) -> Option<String> {
    let id = p.schema().iter().position(|f| f.name == name).unwrap_or_else(|| panic!("no field {name}"));
    p.field(msg, &AT, id).map(|v| match v {
        FieldValue::Str(s) => String::from_utf8_lossy(&s).into_owned(),
        FieldValue::U64(v) => v.to_string(),
        FieldValue::F64(v) => format!("{v}"),
    })
}

fn render(p: &KsyProtocol, msg: &[u8]) -> String {
    let mut out = Vec::new();
    p.render(msg, &AT, &mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// A DLT message: WEID, WTMS and an extended header (verbose log, warn).
fn dlt_message() -> Vec<u8> {
    let mut m = vec![0x20 | 0x01 | 0x04 | 0x10, 7, 0, 0];
    m.extend_from_slice(b"ECU1");
    m.extend_from_slice(&123_456u32.to_be_bytes());
    m.extend_from_slice(&[(3 << 4) | 0x01, 1]);
    m.extend_from_slice(b"NAV\0CTX1");
    m.extend_from_slice(&[0xde, 0xad]);
    let len = m.len() as u16;
    m[2..4].copy_from_slice(&len.to_be_bytes());
    m
}

#[test]
fn the_dlt_definition_reads_a_message() {
    let p = load(DLT);
    assert_eq!(p.name(), "DLTK");
    let names: Vec<&str> = p.schema().iter().map(|f| f.name.as_ref()).collect();
    assert_eq!(names, ["ecu", "apid", "ctid", "type", "mcnt", "tmsp", "seid", "mid", "mode", "payload"]);
    let m = dlt_message();
    assert_eq!(field(&p, &m, "ecu").as_deref(), Some("ECU1"));
    assert_eq!(field(&p, &m, "apid").as_deref(), Some("NAV"));
    assert_eq!(field(&p, &m, "ctid").as_deref(), Some("CTX1"));
    assert_eq!(field(&p, &m, "type").as_deref(), Some("log"));
    assert_eq!(field(&p, &m, "mcnt").as_deref(), Some("7"));
    assert_eq!(field(&p, &m, "tmsp").as_deref(), Some("123456"));
    assert_eq!(field(&p, &m, "seid"), None);
    assert_eq!(field(&p, &m, "mid"), None);
    assert_eq!(field(&p, &m, "mode").as_deref(), Some("verbose"));
    assert_eq!(field(&p, &m, "payload").as_deref(), Some("dead"));
    assert_eq!(p.level(&m, &AT), Some(Level::Warn));
    assert_eq!(p.uptime(&m, &AT), Some(12.3456));
    assert_eq!(render(&p, &m), "12.3456\tECU1\tNAV\tCTX1\twarn\tdead");
    let mut found = Vec::new();
    let two = [m.clone(), m.clone(), vec![1, 2]].concat();
    assert_eq!(p.split(&two, &mut found), 2);
    assert_eq!(found, [0, m.len()]);
}

#[test]
fn missing_fields_and_the_containers_ecu() {
    let p = load(DLT);
    // Non-verbose, no extended header, no ECU id, MSBF: a message id.
    let m = [0x20 | 0x02, 0, 0, 10, 0, 0, 0x01, 0x02, 0xaa, 0xbb];
    assert_eq!(field(&p, &m, "ecu").as_deref(), Some("CH"));
    assert_eq!(field(&p, &m, "apid"), None);
    assert_eq!(field(&p, &m, "type"), None);
    assert_eq!(field(&p, &m, "mid").as_deref(), Some("258"));
    assert_eq!(field(&p, &m, "mode").as_deref(), Some("non-verbose"));
    assert_eq!(p.level(&m, &AT), None);
    assert_eq!(render(&p, &m), "\tCH\t\t\t\t00000102aabb");
    // Little-endian payload: the id the other way round.
    let le = [0x20, 0, 0, 8, 0x02, 0x01, 0, 0];
    assert_eq!(field(&p, &le, "mid").as_deref(), Some("258"));
}

#[test]
fn split_stops_at_what_isnt_a_message() {
    let p = load(DLT);
    let mut found = Vec::new();
    // Version 2.
    assert_eq!(p.split(&[0x40, 0, 0, 4], &mut found), 4);
    // LEN too short for its own header (WEID wants 8 bytes).
    assert_eq!(p.split(&[0x24, 0, 0, 6, 0, 0], &mut found), 6);
    // LEN past the datagram.
    assert_eq!(p.split(&[0x20, 0, 0, 9, 0], &mut found), 5);
    assert!(found.is_empty());
    assert_eq!(p.split(&[0x20, 0, 0, 4], &mut found), 0);
    assert_eq!(found, [0]);
}

#[test]
fn positions_are_memoized_per_flag_byte_and_stay_right() {
    let p = load(DLT);
    // Every htyp with UEH, in a scrambled order, twice: the second round
    // reads from the tables the first one filled.
    for _ in 0..2 {
        for k in 0..32u8 {
            let flags = (k * 7) % 32;
            let htyp = 0x20 | 0x01 | (flags & 0x1e);
            let mut m = vec![htyp, 0, 0, 0];
            let ecu = htyp & 0x04 != 0;
            if ecu {
                m.extend_from_slice(b"E\0\0\0");
            }
            if htyp & 0x08 != 0 {
                m.extend_from_slice(&9u32.to_be_bytes());
            }
            if htyp & 0x10 != 0 {
                m.extend_from_slice(&5u32.to_be_bytes());
            }
            m.extend_from_slice(&[0x41, 0]);
            m.extend_from_slice(b"AP\0\0CT\0\0");
            let len = m.len() as u16;
            m[2..4].copy_from_slice(&len.to_be_bytes());
            assert_eq!(field(&p, &m, "apid").as_deref(), Some("AP"), "htyp {htyp:#x}");
            assert_eq!(field(&p, &m, "ecu").as_deref(), Some(if ecu { "E" } else { "CH" }));
            assert_eq!(field(&p, &m, "seid"), (htyp & 0x08 != 0).then(|| "9".to_owned()));
            assert_eq!(p.level(&m, &AT), Some(Level::Info));
        }
    }
}

/// A definition with only a structure and its own fields.
fn small(seq: &str, fields: &str) -> String {
    format!(
        "meta:\n  id: t\n  endian: le\nseq:\n{seq}\n-nerka:\n  name: T\n  framing: datagram\n  fields:\n{fields}\n  columns: [x]\n"
    )
}

#[test]
fn numbers_bits_and_strings() {
    let text = small(
        "  - { id: a, type: s2 }\n  - { id: b, type: u2be }\n  - { id: hi, type: b3 }\n  - { id: flag, type: b1 }\n  - { id: lo, type: b4 }\n  - { id: s, type: strz, encoding: ASCII }\n  - { id: rest, size-eos: true }",
        "    x: a\n    b: b\n    hi: hi\n    flag: flag\n    lo: lo\n    s: s\n    rest: rest\n    n: rest.length",
    );
    let p = load(&text);
    let m = [0xfe, 0xff, 0x01, 0x02, 0b1011_0110, b'h', b'i', 0, b'z', b'z'];
    assert_eq!(field(&p, &m, "x").as_deref(), Some(&*(-2i64 as u64).to_string()));
    assert_eq!(field(&p, &m, "b").as_deref(), Some("258"));
    assert_eq!(field(&p, &m, "hi").as_deref(), Some("5"));
    assert_eq!(field(&p, &m, "flag").as_deref(), Some("1"));
    assert_eq!(field(&p, &m, "lo").as_deref(), Some("6"));
    assert_eq!(field(&p, &m, "s").as_deref(), Some("hi"));
    assert_eq!(field(&p, &m, "rest").as_deref(), Some("zz"));
    assert_eq!(field(&p, &m, "n").as_deref(), Some("2"));
    let mut found = Vec::new();
    assert_eq!(p.split(&m, &mut found), 0);
    assert_eq!(found, [0]);
    // No terminator for the string: not a message.
    assert_eq!(p.split(&m[..7], &mut Vec::new()), 7);
}

#[test]
fn contents_valid_and_instances() {
    let text = small(
        "  - { id: magic, contents: [0xca, 0xfe] }\n  - { id: kind, type: u1, valid: { any-of: [1, 2] } }\n  - { id: off, type: u1 }",
        "    x: kind\n    at: at\n    ok: ok",
    )
    .replace("seq:", "instances:\n  at: { pos: off, type: u2be }\n  ok: { value: 'kind == 1 ? \"one\" : \"two\"' }\nseq:");
    let p = load(&text);
    let m = [0xca, 0xfe, 2, 4, 0x12, 0x34];
    assert_eq!(field(&p, &m, "at").as_deref(), Some("4660"));
    assert_eq!(field(&p, &m, "ok").as_deref(), Some("two"));
    assert_eq!(p.split(&m, &mut Vec::new()), 0);
    assert_eq!(p.split(&[0xca, 0xfe, 3, 0], &mut Vec::new()), 4, "kind 3 isn't valid");
    assert_eq!(p.split(&[0xca, 0xff, 1, 0], &mut Vec::new()), 4, "wrong magic");
    // An instance outside the message has no value.
    assert_eq!(field(&p, &[0xca, 0xfe, 1, 9], "at"), None);
}

#[test]
fn errors_say_where() {
    assert_eq!(error("meta: { id: x }\nseq: []\n").2, "a definition needs a `-nerka:` section");
    let base = small("  - { id: x, type: u1 }", "    x: x");
    assert_eq!(error(&base.replace("type: u1 }", "type: u1, repeat: eos }")).2, "repeat isn't read yet");
    assert_eq!(error(&base.replace("type: u1", "type: nope")), (5, 5, "there is no type `nope` here".into()));
    assert_eq!(
        error(&base.replace("type: u1", "type: u2").replace("  endian: le\n", "")).2.as_str(),
        "`x` needs an endianness: `meta: endian`, or a type like u2be"
    );
    assert_eq!(error(&base.replace("x: x", "x: x +")), (10, 11, "expected a value".into()));
    assert_eq!(error(&base.replace("x: x", "x: y")).2, "`y` isn't an attribute or instance of `root`");
    assert_eq!(error(&base.replace("name: T", "name: SomeIP")).2, "`SomeIP` is a native protocol's name");
    assert_eq!(error(&base.replace("    x: x", "    raw: x")).2, "`raw` is a name NFD has already");
    assert_eq!(error(&base.replace("columns: [x]", "columns: [y]")).2, "there is no field `y`; give `from:`");
    assert_eq!(error(&base.replace("seq:", "enums: {}\nseq:")).2, "enums aren't read yet");
    let recursive = "meta: { id: r }\nseq:\n  - { id: a, type: n }\ntypes:\n  n:\n    seq:\n      - { id: b, type: n }\n-nerka: { name: R, framing: datagram, columns: [{ name: c, from: a }] }\n";
    assert!(error(recursive).2.contains("contains itself"));
}

#[test]
fn circular_instances_have_no_value() {
    let text = small("  - { id: x, type: u1 }", "    x: x\n    a: a")
        .replace("seq:", "instances:\n  a: { value: b + 1 }\n  b: { value: a + 1 }\nseq:");
    let p = load(&text);
    assert_eq!(field(&p, &[1], "a"), None);
    assert_eq!(field(&p, &[1], "x").as_deref(), Some("1"));
}

#[test]
fn broken_definitions_and_messages_never_panic() {
    let mut seed = 0x0123_4567_89ab_cdefu64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let bytes = DLT.as_bytes();
    for _ in 0..400 {
        // Delete or repeat a stretch of the definition.
        let at = (next() % bytes.len() as u64) as usize;
        let n = (next() % 40) as usize;
        let mut text = bytes[..at].to_vec();
        if next() % 2 == 0 {
            text.extend_from_slice(&bytes[(at + n).min(bytes.len())..]);
        } else {
            text.extend_from_slice(&bytes[at..(at + n).min(bytes.len())]);
            text.extend_from_slice(&bytes[at..]);
        }
        let Ok(text) = String::from_utf8(text) else { continue };
        let Ok(p) = KsyProtocol::parse(&text) else { continue };
        for _ in 0..20 {
            let msg: Vec<u8> = (0..next() % 40).map(|_| next() as u8).collect();
            let mut found = Vec::new();
            p.split(&msg, &mut found);
            let mut out = Vec::new();
            p.render(&msg, &AT, &mut out);
            for id in 0..p.schema().len() {
                p.field(&msg, &AT, id);
            }
            p.level(&msg, &AT);
            p.detail(&msg, &AT);
        }
    }
}

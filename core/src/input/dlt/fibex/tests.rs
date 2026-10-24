use super::*;

/// Written for these tests in the convention of the COVESA examples: one
/// ECU, codings and signals for most arguments, conventional names for the
/// rest, text with entities.
const NAV: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<fx:FIBEX xmlns:fx="http://www.asam.net/xml/fbx" xmlns:ho="http://www.asam.net/xml" VERSION="3.1.0">
  <fx:PROJECT ID="p"><ho:SHORT-NAME>p</ho:SHORT-NAME></fx:PROJECT>
  <fx:ELEMENTS>
    <fx:ECUS><fx:ECU ID="NAV1"><ho:SHORT-NAME>NAV1</ho:SHORT-NAME></fx:ECU></fx:ECUS>
    <fx:PDUS>
      <fx:PDU ID="P10_0"><ho:SHORT-NAME>P10_0</ho:SHORT-NAME><ho:DESC>speed: </ho:DESC><fx:BYTE-LENGTH>0</fx:BYTE-LENGTH></fx:PDU>
      <fx:PDU ID="P10_1"><ho:SHORT-NAME>P10_1</ho:SHORT-NAME><fx:BYTE-LENGTH>2</fx:BYTE-LENGTH>
        <fx:SIGNAL-INSTANCES><fx:SIGNAL-INSTANCE ID="i1"><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_U16"/></fx:SIGNAL-INSTANCE></fx:SIGNAL-INSTANCES>
      </fx:PDU>
      <fx:PDU ID="P10_2"><ho:DESC>km/h &amp; &lt;ok&gt;&#33;</ho:DESC></fx:PDU>
      <fx:PDU ID="P11_0"><fx:SIGNAL-INSTANCES>
        <fx:SIGNAL-INSTANCE ID="a"><fx:SEQUENCE-NUMBER>1</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_BOOLEAN"/></fx:SIGNAL-INSTANCE>
        <fx:SIGNAL-INSTANCE ID="b"><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_TEXT"/></fx:SIGNAL-INSTANCE>
        <fx:SIGNAL-INSTANCE ID="c"><fx:SEQUENCE-NUMBER>2</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_F32"/></fx:SIGNAL-INSTANCE>
        <fx:SIGNAL-INSTANCE ID="d"><fx:SEQUENCE-NUMBER>3</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_I32"/></fx:SIGNAL-INSTANCE>
        <fx:SIGNAL-INSTANCE ID="e"><fx:SEQUENCE-NUMBER>4</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_UINT8"/></fx:SIGNAL-INSTANCE>
        <fx:SIGNAL-INSTANCE ID="f"><fx:SEQUENCE-NUMBER>5</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_RAWD"/></fx:SIGNAL-INSTANCE>
      </fx:SIGNAL-INSTANCES></fx:PDU>
      <fx:PDU ID="P12_0"><ho:DESC>odd</ho:DESC><fx:SIGNAL-INSTANCES>
        <fx:SIGNAL-INSTANCE ID="g"><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_WEIRD"/></fx:SIGNAL-INSTANCE>
      </fx:SIGNAL-INSTANCES></fx:PDU>
    </fx:PDUS>
    <fx:FRAMES>
      <fx:FRAME ID="ID_10"><ho:SHORT-NAME>ID_10</ho:SHORT-NAME><fx:FRAME-TYPE>OTHER</fx:FRAME-TYPE>
        <fx:PDU-INSTANCES>
          <fx:PDU-INSTANCE ID="x2"><fx:PDU-REF ID-REF="P10_2"/><fx:SEQUENCE-NUMBER>2</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
          <fx:PDU-INSTANCE ID="x0"><fx:PDU-REF ID-REF="P10_0"/><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
          <fx:PDU-INSTANCE ID="x1"><fx:PDU-REF ID-REF="P10_1"/><fx:SEQUENCE-NUMBER>1</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
        </fx:PDU-INSTANCES>
        <fx:MANUFACTURER-EXTENSION>
          <MESSAGE_TYPE>DLT_TYPE_LOG</MESSAGE_TYPE><MESSAGE_INFO>DLT_LOG_WARN</MESSAGE_INFO>
          <APPLICATION_ID>NAV</APPLICATION_ID><CONTEXT_ID>SPD</CONTEXT_ID>
          <MESSAGE_SOURCE_FILE>speed.c</MESSAGE_SOURCE_FILE><MESSAGE_LINE_NUMBER>42</MESSAGE_LINE_NUMBER>
        </fx:MANUFACTURER-EXTENSION>
      </fx:FRAME>
      <fx:FRAME ID="ID_0x0b"><fx:PDU-INSTANCES>
        <fx:PDU-INSTANCE ID="y"><fx:PDU-REF ID-REF="P11_0"/><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
      </fx:PDU-INSTANCES></fx:FRAME>
      <fx:FRAME ID="ID_12"><fx:PDU-INSTANCES>
        <fx:PDU-INSTANCE ID="z"><fx:PDU-REF ID-REF="P12_0"/><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
        <fx:PDU-INSTANCE ID="z2"><fx:PDU-REF ID-REF="P10_1"/><fx:SEQUENCE-NUMBER>1</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
      </fx:PDU-INSTANCES></fx:FRAME>
      <fx:FRAME ID="not a message"/>
    </fx:FRAMES>
    <fx:SIGNALS>
      <fx:SIGNAL ID="S_U16"><ho:SHORT-NAME>S_U16</ho:SHORT-NAME><fx:CODING-REF ID-REF="UINT16"/></fx:SIGNAL>
      <fx:SIGNAL ID="S_I32"><fx:CODING-REF ID-REF="SINT32"/></fx:SIGNAL>
      <fx:SIGNAL ID="S_F32"><fx:CODING-REF ID-REF="FLOA32"/></fx:SIGNAL>
      <fx:SIGNAL ID="S_TEXT"><fx:CODING-REF ID-REF="STRG"/></fx:SIGNAL>
      <fx:SIGNAL ID="S_BOOLEAN"><fx:CODING-REF ID-REF="BOOL"/></fx:SIGNAL>
    </fx:SIGNALS>
  </fx:ELEMENTS>
  <fx:PROCESSING-INFORMATION><fx:CODINGS>
    <fx:CODING ID="UINT16"><ho:SHORT-NAME>UINT16</ho:SHORT-NAME><ho:DESC>not a PDU text</ho:DESC>
      <ho:CODED-TYPE ho:BASE-DATA-TYPE="A_UINT16" CATEGORY="STANDARD-LENGTH-TYPE"><ho:BIT-LENGTH>16</ho:BIT-LENGTH></ho:CODED-TYPE></fx:CODING>
    <fx:CODING ID="SINT32"><ho:CODED-TYPE ho:BASE-DATA-TYPE="A_INT32" CATEGORY="STANDARD-LENGTH-TYPE"/></fx:CODING>
    <fx:CODING ID="FLOA32"><ho:CODED-TYPE ho:BASE-DATA-TYPE="A_FLOAT32" CATEGORY="STANDARD-LENGTH-TYPE"><ho:BIT-LENGTH>32</ho:BIT-LENGTH></ho:CODED-TYPE></fx:CODING>
    <fx:CODING ID="STRG"><ho:CODED-TYPE ho:BASE-DATA-TYPE="A_ASCIISTRING" CATEGORY="STANDARD-LENGTH-TYPE" TERMINATION="ZERO"/></fx:CODING>
    <fx:CODING ID="BOOL"><ho:SHORT-NAME>BOOL</ho:SHORT-NAME><ho:CODED-TYPE ho:BASE-DATA-TYPE="A_UINT8" CATEGORY="STANDARD-LENGTH-TYPE"><ho:BIT-LENGTH>8</ho:BIT-LENGTH></ho:CODED-TYPE></fx:CODING>
  </fx:CODINGS></fx:PROCESSING-INFORMATION>
</fx:FIBEX>
"#;

fn nav() -> Fibex {
    let mut f = Fibex::default();
    f.add("nav.xml", NAV).unwrap();
    f
}

fn text(f: &Fibex, mid: u32, data: &[u8], be: bool) -> String {
    let mut out = Vec::new();
    f.frame(mid, b"NAV1", None, None).unwrap().render(data, be, &mut out);
    String::from_utf8(out).unwrap()
}

#[test]
fn frames_with_their_header_fields() {
    let f = nav();
    assert_eq!(f.len(), 3, "the frame without a message id is skipped");
    let m = f.frame(10, b"NAV1", None, None).unwrap();
    assert_eq!((m.ecu.as_deref(), m.apid.as_deref(), m.ctid.as_deref()), (Some("NAV1"), Some("NAV"), Some("SPD")));
    assert_eq!((m.mstp, m.mtin, m.level()), (Some(0), Some(3), Some(Level::Warn)));
    assert_eq!(m.source, Some(("speed.c".to_owned(), 42)));
    assert_eq!(
        m.pieces,
        [Piece::Text("speed: ".into()), Piece::Arg(ArgType::Uint(2)), Piece::Text("km/h & <ok>!".into())]
    );
    assert!(f.frame(11, b"NAV1", None, None).is_some(), "ID_0x0b");
    assert!(f.frame(99, b"NAV1", None, None).is_none());
}

#[test]
fn payloads_render_as_text() {
    let f = nav();
    // In order of sequence numbers, little or big endian.
    assert_eq!(text(&f, 10, &[0x2c, 0x01], false), "speed: 300 km/h & <ok>!");
    assert_eq!(text(&f, 10, &[0x01, 0x2c], true), "speed: 300 km/h & <ok>!");
    // A string (16-bit length, NUL), a bool, a float, a signed, an 8-bit
    // unsigned by its conventional name, raw bytes.
    let mut d = vec![4, 0, b'a', b'b', b'c', 0, 1];
    d.extend(1.5f32.to_le_bytes());
    d.extend((-7i32).to_le_bytes());
    d.extend([200, 2, 0, 0xde, 0xad]);
    assert_eq!(text(&f, 11, &d, false), "abc true 1.5 -7 200 de ad");
    // Too short: what can be read, then the rest as hex.
    assert_eq!(text(&f, 11, &[4, 0, b'a', b'b', b'c', 0, 1, 0xff], false), "abc true <undecoded> ff");
    // Too long.
    assert_eq!(text(&f, 10, &[1, 0, 9], false), "speed: 1 km/h & <ok>! <undecoded> 09");
    // An argument of unknown type: nothing after it can be read.
    assert_eq!(text(&f, 12, &[1, 0], false), "odd <undecoded> 01 00");
}

#[test]
fn files_bind_frames_to_their_ecu() {
    let mut f = nav();
    let other = NAV.replace("NAV1", "GW").replace("<APPLICATION_ID>NAV<", "<APPLICATION_ID>GWAP<");
    f.add("gw.xml", &other).unwrap();
    assert_eq!(f.len(), 6);
    assert_eq!(f.frame(10, b"GW", None, None).unwrap().apid.as_deref(), Some("GWAP"));
    assert_eq!(f.frame(10, b"NAV1", None, None).unwrap().apid.as_deref(), Some("NAV"));
    // An unknown ECU: the one whose APID matches the message's.
    assert_eq!(f.frame(10, b"X", Some(b"GWAP"), None).unwrap().apid.as_deref(), Some("GWAP"));
    assert_eq!(f.files, ["nav.xml", "gw.xml"]);
}

#[test]
fn malformed_documents_say_where() {
    let mut f = Fibex::default();
    let e = f.add("bad.xml", "<fx:FIBEX><fx:FRAME></fx:FIBEX>").unwrap_err();
    assert!(e.to_string().starts_with("bad.xml: ill-formed document: expected `</fx:FRAME>`"), "{e}");
    let e = f.add("ent.xml", "<a>&nope;</a>").unwrap_err();
    assert!(e.to_string().contains("unknown entity '&nope;'"), "{e}");
    // Empty texts and codings nothing refers to are fine (as in dlt-core's
    // robustness test).
    f.add("ok.xml", "<fx:FIBEX><fx:ELEMENTS><fx:PDUS><fx:PDU ID=\"a\"><ho:DESC></ho:DESC></fx:PDU></fx:PDUS></fx:ELEMENTS></fx:FIBEX>")
        .unwrap();
    assert!(f.is_empty());
}

#[test]
fn half_floats() {
    assert_eq!(half(0x3c00), 1.0);
    assert_eq!(half(0xc000), -2.0);
    assert_eq!(half(0x0001), 2f32.powi(-24));
    assert!(half(0x7c00).is_infinite());
}

/// The open examples (COVESA dlt-daemon / dlt-viewer, esrlabs dlt-core),
/// downloaded into `NERKA_FIBEX_REF`: they all load, and describe messages.
#[test]
#[ignore = "needs NERKA_FIBEX_REF, a folder of reference FIBEX files"]
fn reference_files_load() {
    let dir = std::env::var("NERKA_FIBEX_REF").expect("NERKA_FIBEX_REF");
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        let f = Fibex::load(&[&p]).unwrap_or_else(|e| panic!("{e}"));
        let unknown =
            f.frames.values().flatten().flat_map(|m| &m.pieces).filter(|p| matches!(p, Piece::Unknown(_))).count();
        eprintln!("{}: {} messages, {unknown} unknown arguments", p.display(), f.len());
    }
}

#[test]
fn arguments_as_values_and_names() {
    let f = nav();
    let m = f.frame(11, b"NAV1", None, None).unwrap();
    let mut d = vec![4, 0, b'a', b'b', b'c', 0, 1];
    d.extend(1.5f32.to_le_bytes());
    d.extend((-7i32).to_le_bytes());
    d.extend([200, 2, 0, 0xde, 0xad]);
    let v: Vec<Option<SignalValue>> = (0..7).map(|i| m.arg(&d, false, i)).collect();
    assert_eq!(
        v,
        [
            Some(SignalValue::Enum(b"abc".to_vec())),
            Some(SignalValue::Enum(b"true".to_vec())),
            Some(SignalValue::Num(1.5)),
            Some(SignalValue::Num(-7.0)),
            Some(SignalValue::Num(200.0)),
            Some(SignalValue::Enum(b"de ad".to_vec())),
            None,
        ]
    );
    assert_eq!(m.arg(&d[..3], false, 2), None, "too short");
    assert_eq!(f.frame(10, b"NAV1", None, None).unwrap().template(), "speed: <u16> km/h & <ok>!");
    let args = f.arguments();
    let names: Vec<(u32, usize, &str, &str)> =
        args.iter().map(|a| (a.mid, a.index, a.name.as_str(), a.label.as_str())).collect();
    assert_eq!(
        names,
        // By ECU, APID, CTID, message id: 11 has no APID. Message 12 stops
        // at its unknown argument.
        [
            (11, 0, "m11_0", ""),
            (11, 1, "m11_1", ""),
            (11, 2, "m11_2", ""),
            (11, 3, "m11_3", ""),
            (11, 4, "m11_4", ""),
            (11, 5, "m11_5", ""),
            (10, 0, "speed", "speed")
        ]
    );
    assert_eq!((args[6].ty.name(), args[6].ty.kind()), ("u16".to_owned(), SignalKind::Num));
    assert_eq!(args[6].message, "speed: <u16> km/h & <ok>!");
    // An argument without text of its own takes the label before it: 12
    // becomes "odd <u8> speed: <u16> …", so its <u16> is "speed" (shared
    // with 10: the message id tells them apart) and its <u8> "odd".
    let mut t = Fibex::default();
    t.add("t.xml", &NAV.replace(r#"ID-REF="S_WEIRD""#, r#"ID-REF="S_UINT8""#).replace(
        r#"<fx:PDU-INSTANCE ID="z2"><fx:PDU-REF ID-REF="P10_1"/>"#,
        r#"<fx:PDU-INSTANCE ID="z2"><fx:PDU-REF ID-REF="P10_0"/></fx:PDU-INSTANCE><fx:PDU-INSTANCE ID="z3"><fx:PDU-REF ID-REF="P10_1"/>"#,
    ))
    .unwrap();
    let a12: Vec<(String, String)> =
        t.arguments().into_iter().filter(|a| a.mid == 12).map(|a| (a.name, a.label)).collect();
    assert_eq!(a12, [("odd".to_owned(), "odd".to_owned()), ("speed_m12_1".to_owned(), "speed".to_owned())]);
    let mut carry = Fibex::default();
    carry
        .add(
            "c.xml",
            &NAV.replace(r#"ID-REF="S_WEIRD""#, r#"ID-REF="S_UINT8""#).replace(
                r#"<fx:PDU-INSTANCE ID="z2"><fx:PDU-REF ID-REF="P10_1"/>"#,
                r#"<fx:PDU-INSTANCE ID="z2"><fx:PDU-REF ID-REF="P11_0"/>"#,
            ),
        )
        .unwrap();
    let c12: Vec<String> = carry.arguments().into_iter().filter(|a| a.mid == 12).map(|a| a.label).collect();
    assert_eq!(c12[..3], ["odd", "odd", "odd"], "12's arguments after 'odd' have no text of their own");
    // The same label in two messages: both names say which message.
    let mut two = nav();
    two.add("gw.xml", &NAV.replace("NAV1", "GW")).unwrap();
    let n: Vec<String> = two.arguments().iter().filter(|a| a.label == "speed").map(|a| a.name.clone()).collect();
    // Both are message 10, of two ECUs: the ECU tells them apart.
    assert_eq!(n, ["gw_speed_m10_0", "nav1_speed_m10_0"]);
    assert_eq!(snake("Free size:"), "free_size");
    assert_eq!(last_phrase("Buffer near limit. Free size: "), "Free size");
    assert_eq!(last_phrase("a, b; temp ="), "temp");
    assert_eq!(last_phrase("."), "");
    assert_eq!(snake("3 phases"), "_3_phases");
    assert_eq!(snake(" : "), "");
}

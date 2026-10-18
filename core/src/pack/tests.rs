use super::*;

const VISION: &str = "\
# Camera team's queries.
pack vision   # camera team

query cam_scope(ecu, cam)  labels: system/vision
    ecu == $ecu AND ctid == $cam

query frame_drops(cam)  parent: cam_scope  labels: system/vision, perf  color: violet
    payload ~ /$cam frame drop/

query noise
    \"heartbeat\"
    OR /^\\s*$/
";

fn pack(text: &str) -> Pack {
    parse_pack("t.nerka-pack", text).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn parses_the_example() {
    let p = pack(VISION);
    assert_eq!(p.name, "vision");
    let ids: Vec<&str> = p.queries.iter().map(|q| q.id.as_str()).collect();
    assert_eq!(ids, ["cam_scope", "frame_drops", "noise"]);
    let fd = &p.queries[1];
    assert_eq!(fd.parent.as_deref(), Some("cam_scope"));
    assert_eq!(fd.labels, ["system/vision", "perf"]);
    assert_eq!(fd.params, ["cam"]);
    assert_eq!(fd.color.as_deref(), Some("violet"));
    assert_eq!(fd.text, "payload ~ /$cam frame drop/");
    assert_eq!(fd.line, 6);
    assert_eq!(p.queries[0].params, ["ecu", "cam"]);
    // Multi-line bodies keep their lines; `$` before `/` is the anchor.
    assert_eq!(p.queries[2].text, "\"heartbeat\"\nOR /^\\s*$/");
    assert!(p.queries[2].params.is_empty() && p.queries[2].labels.is_empty());
    assert_eq!(p.parent_of(fd).as_deref(), Some("vision.cam_scope"));
}

#[test]
fn details() {
    let p = pack(
        "pack p\n\
         query a(v)  color: #FF8000  labels: x,x , y/z-1\n\
         \x20   /a$$b\\$c #x/\n\
         \n\
         \x20     AND \"${v}\"\n\
         \n\
         query b(v)   parent: other.a\n\
         \tapid == ${v}\n",
    );
    let a = &p.queries[0];
    assert_eq!(a.color.as_deref(), Some("#FF8000"));
    assert_eq!(a.labels, ["x", "y/z-1"]);
    // Blank lines inside a body are kept, trailing ones dropped, the
    // common indent removed. `$$` and `\$` aren't parameters, and `#`
    // isn't a comment inside a body.
    assert_eq!(a.text, "/a$$b\\$c #x/\n\n  AND \"${v}\"");
    assert_eq!(p.parent_of(&p.queries[1]).as_deref(), Some("other.a"));
}

/// `(line, column)` 1-based as shown to users, and the message.
fn error(text: &str) -> (usize, usize, String) {
    let e = parse_pack("t", text).unwrap_err();
    assert!(e.to_string().starts_with("t:"), "{e}");
    let (l, c) = e.pos.unwrap_or_else(|| panic!("no position: {e}"));
    (l + 1, c + 1, e.msg)
}

#[track_caller]
fn assert_error(text: &str, line: usize, col: usize, msg: &str) {
    let (l, c, m) = error(text);
    assert!(m.contains(msg), "message {m:?} doesn't contain {msg:?}");
    assert_eq!((l, c), (line, col), "position of {m:?}");
}

#[test]
fn file_level_errors() {
    assert_error("  x\npack p\n", 1, 3, "indented text outside a query");
    assert_error("pack\n", 1, 5, "expected a pack name");
    assert_error("pack a b\n", 1, 8, "single word");
    assert_error("pack a.b\n", 1, 6, "invalid pack name 'a.b'");
    assert_error("pack a\npack b\n", 2, 1, "second 'pack' line");
    assert_error("query a\n  /x/\n", 1, 1, "expected 'pack NAME'");
    assert_error("pack p\nqueries a\n", 2, 1, "expected 'query' but found 'queries'");
    assert_error("pack p\nquery a\n  /x/\nquery a\n  /y/\n", 4, 7, "already defined on line 2");
    let e = parse_pack("t", "# only a comment\n").unwrap_err();
    assert_eq!((e.pos, e.msg.as_str()), (None, "no 'pack NAME' line"));
    assert_eq!(e.to_string(), "t: no 'pack NAME' line");
}

#[test]
fn header_errors() {
    let q = |h: &str| format!("pack p\n{h}\n  /x/\n");
    assert_error(&q("query"), 2, 6, "expected a query name");
    assert_error(&q("query a-b"), 2, 7, "invalid query name 'a-b'");
    assert_error(&q("query \"a b"), 2, 7, "missing closing '\"'");
    assert_error(&q("query \"a.b\""), 2, 8, "invalid query name 'a.b'");
    assert_error(&q("query \" a\""), 2, 8, "invalid query name ' a'");
    assert_error(&q("query \"\""), 2, 8, "invalid query name ''");
    assert_error(&q("query a  parent: \"x.y.z\""), 2, 18, "invalid parent");
    assert_error(&q("query \"a b\"  parent: \"a b\""), 2, 22, "its own parent");
    let p = pack(&q("query \"xyz logs\"(cam)  parent: \"other.cam scope\"  labels: x"));
    assert_eq!(p.queries[0].id, "xyz logs");
    assert_eq!(p.queries[0].params, ["cam"]);
    assert_eq!(p.queries[0].parent.as_deref(), Some("other.cam scope"));
    assert_error(&q("query a(x, y"), 2, 8, "missing ')'");
    assert_error(&q("query a(x, 1y)"), 2, 12, "invalid parameter name '1y'");
    assert_error(&q("query a(x,)"), 2, 11, "invalid parameter name ''");
    assert_error(&q("query a(x, x)"), 2, 12, "declared twice");
    assert_error(&q("query a  labels: x  labels: y"), 2, 21, "'labels:' is given twice");
    assert_error(&q("query a  color:"), 2, 10, "'color:' needs a value");
    assert_error(&q("query a  parent: a.b.c"), 2, 18, "invalid parent 'a.b.c'");
    assert_error(&q("query a  parent: a"), 2, 18, "its own parent");
    assert_error(&q("query a  labels: x, y z"), 2, 21, "separated by commas: 'y z'");
    assert_error(&q("query a  labels: x, a//b"), 2, 21, "invalid label 'a//b'");
    assert_error(&q("query a  color: Violet"), 2, 17, "invalid colour 'Violet'");
    assert_error(&q("query a  color: #12345"), 2, 17, "invalid colour '#12345'");
    assert_error(&q("query a  colour: red"), 2, 10, "unknown attribute 'colour:'");
    assert_error(&q("query a(x)  red"), 2, 13, "expected parent:, labels: or color: but found 'red'");
    // Attribute values may follow the colon directly.
    let p = pack(&q("query a  labels:x,y  parent:b  color:red  # comment"));
    assert_eq!(p.queries[0].labels, ["x", "y"]);
    assert_eq!(p.queries[0].parent.as_deref(), Some("b"));
    assert_eq!(p.queries[0].color.as_deref(), Some("red"));
}

#[test]
fn body_errors() {
    assert_error("pack p\nquery a\nquery b\n  /x/\n", 2, 1, "query 'a' has no text");
    assert_error("pack p\nquery a\n\n", 2, 1, "query 'a' has no text");
    assert_error("pack p\nquery a(x)\n  /${x-y}/\n", 3, 4, "invalid parameter name 'x-y'");
    assert_error("pack p\nquery a(x)\n  ecu == X\n  AND /$cam/\n", 4, 8, "parameter '$cam' is not declared");
    assert_error("pack p\nquery a(x)\n  \"${x\"\n", 3, 4, "missing '}'");
    // NFD syntax errors point into the file, across lines and indents.
    assert_error("pack p\nquery a\n    level >= ERROR\n    AND level >= LOUD\n", 4, 18, "unknown level 'LOUD'");
    assert_error("pack p\nquery a(cam)\n    ctid == $cam AND\n", 3, 21, "expected a condition");
    assert_error("pack p\nquery a(c)\n  /${c}(/\n", 3, 3, "invalid regex");
    // A parameter that is a bare value is fine; one that is a field name isn't.
    pack("pack p\nquery a(v)\n  apid == $v\n");
    assert_error("pack p\nquery a(f)\n  $f == x\n", 3, 6, "unexpected");
}

#[test]
fn library_loads_files_and_dirs() {
    let dir = std::env::temp_dir().join(format!("nerka-packs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("b.nerka-pack"), "pack ota\nquery swup  parent: vision.cam_scope  labels: ota\n  /x/\n")
        .unwrap();
    std::fs::write(dir.join("a.nerka-pack"), VISION).unwrap();
    std::fs::write(dir.join("ignored.txt"), "not a pack").unwrap();
    let mut lib = Library::new();
    lib.load(&dir).unwrap();
    let names: Vec<&str> = lib.packs().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["vision", "ota"]);
    assert_eq!(lib.get("ota.swup").unwrap().1.id, "swup");
    assert!(lib.get("ota.nope").is_none() && lib.get("swup").is_none());
    assert_eq!(lib.with_label("system"), ["vision.cam_scope", "vision.frame_drops"]);
    assert_eq!(lib.with_label("system/vision/"), ["vision.cam_scope", "vision.frame_drops"]);
    assert!(lib.with_label("sys").is_empty());
    assert_eq!(lib.labels(), ["ota", "perf", "system/vision"]);

    let e = lib.load(&dir.join("a.nerka-pack")).unwrap_err();
    assert!(e.msg.contains("pack 'vision' is already loaded"), "{e}");
    std::fs::write(dir.join("c.nerka-pack"), "pack bad\nquery x\n  level >= LOUD\n").unwrap();
    let e = Library::new().load(&dir).unwrap_err();
    assert!(
        e.to_string().ends_with(
            "c.nerka-pack:3:12: unknown level 'LOUD' (TRACE or VERBOSE, DEBUG, INFO, NOTICE, WARN, ERROR, FATAL)"
        ),
        "{e}"
    );
    assert!(Library::new().load(&dir.join("missing.nerka-pack")).unwrap_err().pos.is_none());
    // Lenient loading keeps the good packs and lists the bad ones.
    let mut lenient = Library::new();
    let errs = lenient.load_lenient(&dir);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].file.ends_with("c.nerka-pack"));
    assert_eq!(lenient.packs().len(), 2);

    // Saving a query writes its pack file and reloads the pack.
    let mut q = lenient.get("ota.swup").unwrap().1.clone();
    q.text = "apid == SWUP2".into();
    lenient.save_query("ota", &q).unwrap();
    assert_eq!(lenient.get("ota.swup").unwrap().1.text, "apid == SWUP2");
    assert!(std::fs::read_to_string(dir.join("b.nerka-pack")).unwrap().contains(
        "    apid == SWUP2
"
    ));
    q.text = "level >= LOUD".into();
    assert!(lenient.save_query("ota", &q).is_err());
    assert!(lenient.save_query("nope", &q).is_err());
    assert_eq!(lenient.get("ota.swup").unwrap().1.text, "apid == SWUP2");
    std::fs::remove_dir_all(&dir).unwrap();
}

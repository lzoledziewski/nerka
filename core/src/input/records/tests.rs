use super::*;
use crate::engine::search::tests::run_on;
use crate::{Bookmarks, Filter, Matcher, Query, TextFile};

const LOG: &str = "\
preamble line
2025-10-18 10:00:00 INFO start
2025-10-18 10:00:01 ERROR request failed
java.lang.IllegalStateException: boom
    at a.b.C.run(C.java:10)
    at a.b.D.main(D.java:5)
2025-10-18 10:00:02 WARN slow
2025-10-18 10:00:03 INFO done
";

fn start() -> RecordSplit {
    RecordSplit::parse(r"start=/\d{4}-\d\d-\d\d /").unwrap()
}

fn rows(f: &RecordFile) -> Vec<String> {
    (0..f.line_count()).map(|l| f.line_text(l).unwrap().into_owned()).collect()
}

#[test]
fn a_start_pattern_begins_each_record() {
    let f = RecordFile::from_bytes("t", LOG.into(), start());
    assert_eq!(f.line_count(), 5);
    let r = rows(&f);
    assert_eq!(r[0], "preamble line", "lines before the first start are a record too");
    assert_eq!(r[2].lines().count(), 4);
    assert!(r[2].starts_with("2025-10-18 10:00:01 ERROR") && r[2].ends_with("(D.java:5)"));
    assert_eq!(r[4], "2025-10-18 10:00:03 INFO done");
    assert_eq!(f.line_detail(2).unwrap(), [r[2].clone()], "the line window has its line breaks");
}

#[test]
fn blank_lines_and_separators_split_too() {
    let text = "\n\nfirst a\r\nfirst b\r\n\r\n  \nsecond\n\n\nthird a\nthird b";
    let f = RecordFile::from_bytes("t", text.into(), RecordSplit::Blank);
    assert_eq!(rows(&f), ["first a\r\nfirst b", "second", "third a\nthird b"]);

    let text = "----\none\n----\n----\ntwo a\ntwo b\n---- end\nthree\n";
    let f = RecordFile::from_bytes("t", text.into(), RecordSplit::parse("sep=----").unwrap());
    assert_eq!(rows(&f), ["one", "two a\ntwo b", "three"]);
    // Lines is plain text.
    let f = RecordFile::from_bytes("t", "a\r\nb\n".into(), RecordSplit::Lines);
    assert_eq!(rows(&f), ["a", "b"]);
}

#[test]
fn queries_see_whole_records() {
    let f = RecordFile::from_bytes("t", LOG.into(), start());
    let q = |s: &str| run_on(&f, &Filter::Query(Query::compile(s).unwrap()));
    let re = |s: &str| run_on(&f, &Filter::Regex(Matcher::new(s, Default::default()).unwrap()));
    // A line of a record finds the record; the level is its first line's.
    assert_eq!(re("D.java"), [2]);
    assert_eq!(q("level >= ERROR"), [2]);
    assert_eq!(q("level >= WARN AND /Exception/"), [2]);
    // A regex across the record's lines.
    assert_eq!(re(r"boom\n\s+at a\.b\.C"), [2]);
    assert_eq!(q(r"/^\s+at /"), [2], "^ is every line's start");
    assert_eq!(re("INFO"), [1, 4]);

    // Text between records (separators, blank lines) belongs to none.
    let text = "---- sep a\nalpha\n---- sep b\nbeta\n\n";
    let f = RecordFile::from_bytes("t", text.into(), RecordSplit::parse("sep=/---- sep/").unwrap());
    let re = |s: &str| run_on(&f, &Filter::Regex(Matcher::new(s, Default::default()).unwrap()));
    assert!(re("sep").is_empty());
    assert_eq!(re("a"), [0, 1]);
    assert_eq!(re("beta"), [1]);
}

#[test]
fn specs_read_back() {
    for spec in ["lines", "blank", r"start=/\d{4}-/", "sep=/----/"] {
        assert_eq!(RecordSplit::parse(spec).unwrap().spec(), spec);
    }
    assert_eq!(RecordSplit::parse("start=x").unwrap().spec(), "start=/x/");
    for bad in ["start=", "start=/(/", "lines please", "nope=x"] {
        assert!(RecordSplit::parse(bad).is_err(), "{bad}");
    }
}

/// A bookmark made with one row per line is on the record that holds its
/// line; one made on a record is on its first line.
#[test]
fn bookmarks_move_between_lines_and_records() {
    let as_lines = TextFile::from_bytes("t", LOG.into());
    let as_records = RecordFile::from_bytes("t", LOG.into(), start());
    let mut b = Bookmarks::default();
    b.set_note(4, "the C frame");
    b.set_note(6, "slow");
    let saved = b.serialize_anchored(&as_lines);
    let mut r = Bookmarks::parse(&saved);
    r.resolve(&as_records);
    assert_eq!(r.iter().collect::<Vec<_>>(), [(2, "the C frame"), (3, "slow")]);
    assert!(!r.is_unsure(2) && !r.is_unsure(3));

    let mut b = Bookmarks::default();
    b.set_note(3, "slow");
    let mut l = Bookmarks::parse(&b.serialize_anchored(&as_records));
    l.resolve(&as_lines);
    assert_eq!(l.iter().collect::<Vec<_>>(), [(6, "slow")]);
}

/// Big enough for several chunks: the parallel split finds what a plain
/// one-line-at-a-time split does.
#[test]
fn the_parallel_split_matches_a_plain_one() {
    let mut text = String::from("preamble\n");
    for i in 0..400_000 {
        if i % 3 == 0 {
            text.push_str(&format!("2025-10-18 START {i}\r\n"));
        } else {
            text.push_str(&format!("  continued {i} {}\n", "x".repeat(i % 7)));
        }
    }
    assert!(text.len() > 2 * REPORT);
    let f = RecordFile::from_bytes("t", text.clone().into_bytes(), start());
    let (mut want, mut cur) = (Vec::new(), String::new());
    for line in text.lines() {
        if line.starts_with("2025-") && !cur.is_empty() {
            want.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(line);
    }
    want.push(cur);
    assert_eq!(f.line_count(), want.len() as u64);
    for (l, w) in want.iter().enumerate() {
        assert_eq!(f.line_text(l as u64).unwrap().replace("\r\n", "\n"), *w, "record {l}");
    }
}

const JSONL: &str = r#"{"ts": "10:00:00", "level": "info", "msg": "start", "ctx": {"user": "alice", "id": 7}, "latency": 1.25, "status": 200}
{"ts": "10:00:01", "level": "ERROR", "msg": "db \"down\"", "ctx": {"user": "bob", "id": 8}, "latency": 30, "status": 503, "tags": ["db", "retry"]}
{"ts": "10:00:02", "level": "warn", "msg": "slow", "ctx": {"user": "alice"}, "latency": 4.5, "status": 200}
not json at all
{"ts": "10:00:03", "msg": "no level", "status": "n/a"}
"#;

fn json(text: &str) -> RecordFile {
    RecordFile::from_bytes("t", text.into(), RecordSplit::Json)
}

#[test]
fn json_keys_are_fields() {
    let f = json(JSONL);
    assert_eq!(f.line_count(), 4, "the line that isn't JSON is skipped");
    let names: Vec<(&str, crate::FieldKind)> = f.schema().iter().map(|d| (d.name.as_ref(), d.kind)).collect();
    use crate::FieldKind::{F64, Str, U64};
    assert_eq!(
        names,
        [
            ("ts", Str),
            ("json.level", Str),
            ("msg", Str),
            ("ctx.user", Str),
            ("ctx.id", U64),
            ("latency", F64),
            ("status", Str),
            ("tags", Str)
        ],
        "the level key is the row's level, json.level its text; a key holding a number and text is text"
    );
    let q = |s: &str| run_on(&f, &Filter::Query(Query::compile(s).unwrap()));
    assert_eq!(q("level >= WARN"), [1, 2]);
    assert_eq!(q("ctx.user == alice"), [0, 2]);
    assert_eq!(q("latency > 2 AND latency < 10"), [2]);
    assert_eq!(q("ctx.id >= 8"), [1]);
    assert_eq!(q("msg ~ /\"down\"/"), [1], "strings are unescaped");
    assert_eq!(q("tags ~ /retry/"), [1], "arrays as their text");
    assert_eq!(q(r#"status == "n/a""#), [3]);
    assert_eq!(q("ctx.id != 7"), [1, 2, 3], "a missing key only satisfies !=");
    assert_eq!(q("/alice/ AND latency > 4"), [2], "raw is the record's text");
    assert!(Query::compile_for("nope == 1", f.schema()).is_err());

    // Pretty JSON, one array; numeric levels (bunyan / pino).
    let pretty = "[\n  {\n    \"level\": 30,\n    \"msg\": \"hi\"\n  },\n  {\"level\": 50, \"msg\": \"bad\"}\n]\n";
    let f = json(pretty);
    assert_eq!(f.line_count(), 2);
    assert_eq!(f.line_text(0).unwrap().lines().count(), 4, "a record keeps its lines");
    let q = |s: &str| run_on(&f, &Filter::Query(Query::compile(s).unwrap()));
    assert_eq!(q("level >= ERROR"), [1]);
    assert_eq!(q("level == INFO"), [0]);
}

/// A file opened without a choice is read as JSON if it looks like JSON;
/// a choice of lines reads it as lines.
#[test]
fn json_is_found_unless_lines_are_asked_for() {
    let dir = std::env::temp_dir().join(format!("nerka-json-auto-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (j, t) = (dir.join("a.log"), dir.join("b.log"));
    std::fs::write(&j, JSONL).unwrap();
    std::fs::write(&t, "[INFO] {\"a\": 1}\n[WARN] x\n").unwrap();
    let p = Progress::new();
    let auto = crate::OpenOptions::default();
    let lines = crate::OpenOptions { records: Some(RecordSplit::Lines), ..Default::default() };
    let open = |path: &Path, o: &crate::OpenOptions| crate::open_with(path, o, &p).unwrap();
    assert_eq!((open(&j, &auto).line_count(), open(&j, &auto).schema().len()), (4, 8));
    assert_eq!((open(&j, &lines).line_count(), open(&j, &lines).schema().len()), (5, 0));
    assert_eq!((open(&t, &auto).line_count(), open(&t, &auto).schema().len()), (2, 0), "[INFO] is text");
    assert_eq!(RecordSplit::parse_choice("auto").unwrap(), None);
    assert_eq!(RecordSplit::parse_choice("json").unwrap(), Some(RecordSplit::Json));
    std::fs::remove_dir_all(&dir).ok();
}

/// Keys named like NFD's own fields are `json.raw` (…); a plain `raw` or
/// `bookmark` is then ambiguous, and says how to write either.
#[test]
fn keys_named_like_nfd_fields_are_written_json_dot() {
    let text = r#"{"raw": "r1", "level": "warn", "bookmark": "b1", "msg": "hello"}
{"raw": "r2", "level": "error", "bookmark": "b2", "msg": "bye"}
"#;
    let f = json(text);
    let names: Vec<&str> = f.schema().iter().map(|d| d.name.as_ref()).collect();
    assert_eq!(names, ["json.raw", "json.level", "json.bookmark", "msg"]);
    let q = |s: &str| run_on(&f, &Filter::Query(Query::compile(s).unwrap()));
    assert_eq!(q("json.raw == r2"), [1]);
    assert_eq!(q("json.level == warn"), [0], "the key's text");
    assert_eq!(q("level >= ERROR"), [1], "level is the row's level: no clash");
    assert_eq!(q("json.msg == hello"), [0], "any key with the prefix");
    assert_eq!(q("nfd.raw ~ /bye/"), [1]);
    assert_eq!(q("/r1/"), [0], "a bare regex is the line's text, not ambiguous");
    assert_eq!(q("msg == raw"), Vec::<LineNo>::new(), "a value isn't a field");
    let e = Query::compile("level >= WARN AND raw ~ /x/").unwrap().bind(f.schema()).unwrap_err();
    assert_eq!(
        (e.pos, e.msg.as_str()),
        (18, "'raw' is ambiguous here: write nfd.raw (the line's text) or json.raw (the record's key)")
    );
    let e = Query::compile("NOT bookmark").unwrap().bind(f.schema()).unwrap_err();
    assert_eq!(e.pos, 4);
    assert!(e.msg.contains("nfd.bookmark (the line's bookmark) or json.bookmark"), "{}", e.msg);
}

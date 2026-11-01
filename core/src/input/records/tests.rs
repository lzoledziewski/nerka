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

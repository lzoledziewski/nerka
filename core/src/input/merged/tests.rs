use super::*;
use crate::engine::search::tests::run_on;
use crate::engine::within::{Path, SearchSet, search_within_path};
use crate::input::dlt::writer::{Msg, Value};
use crate::signal::{SignalDef, Signals};
use crate::{Bookmarks, DltFile, Filter, Matcher, MatcherOptions, Query};
use std::sync::Arc;

/// A DLT file of `(seconds, apid, text)` messages, in that order.
fn dlt(name: &str, msgs: &[(u32, &str, &str)]) -> Box<dyn LogSource> {
    let mut buf = Vec::new();
    for (secs, apid, text) in msgs {
        let mut m = Msg::log(crate::Level::Warn, "ECU1", apid, "CTX", vec![Value::Str((*text).into()).into()]);
        m.secs = *secs;
        m.encode(&mut buf);
    }
    Box::new(DltFile::from_bytes(name, buf))
}

/// File a: 10, 30, 50. File b: 20, 40, 35 (goes back), 60.
fn merged() -> Merged {
    let a = dlt("a.dlt", &[(10, "AAA", "a0 v=1"), (30, "AAA", "a1 v=3"), (50, "AAA", "a2 v=5")]);
    let b = dlt("b.dlt", &[(20, "BBB", "b0"), (40, "BBB", "b1 v=4"), (35, "BBB", "b2"), (60, "BBB", "b3 v=6")]);
    Merged::new(Concat::new(vec![a, b]).unwrap(), &Progress::new()).unwrap()
}

fn texts(s: &dyn LogSource) -> Vec<String> {
    (0..s.line_count())
        .map(|r| {
            let t = s.line_text(r).unwrap().into_owned();
            t.rsplit('\t').next().unwrap().split(' ').next().unwrap().to_owned()
        })
        .collect()
}

fn q(s: &dyn LogSource, nfd: &str) -> Filter {
    Filter::Query(Query::compile_for(nfd, s.schema()).unwrap_or_else(|e| panic!("{nfd}: {e}")))
}

#[test]
fn lines_interleave_by_time_and_each_file_keeps_its_order() {
    let m = merged();
    // b2 (35) stays after b1 (40): its file's order holds.
    assert_eq!(texts(&m), ["a0", "b0", "a1", "b1", "b2", "a2", "b3"]);
    assert_eq!(m.time(4), Some(35.0));
    assert!(m.format().ends_with("merged by time"), "{}", m.format());
    let r = m.reordered().unwrap();
    for row in 0..m.line_count() {
        assert_eq!(r.row_of[r.line_of[row as usize] as usize], row);
    }
    // Fields come from the line's own file.
    let file = m.schema().iter().position(|d| d.name == "file").unwrap();
    assert!(matches!(m.field(1, file), Some(FieldValue::Str(f)) if &*f == b"b.dlt"));
}

#[test]
fn filters_scan_the_files_and_map_the_rows_fast_and_naive_alike() {
    let m = merged();
    // A regex and a field query take the files' own path, mapped to rows.
    let re = Filter::Regex(Matcher::new("v=", MatcherOptions::default()).unwrap());
    assert_eq!(run_on(&m, &re), [0, 2, 3, 5, 6]);
    assert_eq!(run_on(&m, &q(&m, "apid == BBB AND NOT /b2/")), [1, 3, 6]);
    assert_eq!(run_on(&m, &q(&m, "file == a.dlt")), [0, 2, 5]);
    // A search inside a filter's rows, by both paths.
    let set = SearchSet { include: vec![q(&m, "/v=/"), q(&m, "apid == AAA")], exclude: vec![] };
    let within = [1, 2, 3, 4];
    let scan = search_within_path(&m, Some(&within), &set, &Progress::new(), Path::Scan).unwrap();
    let per = search_within_path(&m, Some(&within), &set, &Progress::new(), Path::PerLine).unwrap();
    assert_eq!(scan, per);
    assert_eq!(scan.lines, [2, 3]);
}

#[test]
fn signal_states_follow_the_rows_across_files() {
    let m = merged();
    let mut signals = Signals::default();
    signals.set(SignalDef::parse_spec("v=/v=(?<v>\\d+)/").unwrap());
    let f = |nfd: &str| Filter::Query(Query::compile_in(nfd, m.schema(), &signals).unwrap());
    // b0 (row 1) holds a0's v=1; b2 (row 4) holds b1's v=4.
    assert_eq!(run_on(&m, &f("@v == 1")), [0, 1]);
    assert_eq!(run_on(&m, &f("@v >= 4 AND apid == BBB")), [3, 4, 6]);
    assert_eq!(run_on(&m, &f("@v.changed")), [0, 2, 3, 5, 6]);
    // `@time` from the files, in row order.
    let mut t = Signals::default().for_source(&m);
    t.set(SignalDef::parse_spec("v=/v=(?<v>\\d+)/").unwrap());
    let g = Filter::Query(Query::compile_in("@time >= 35 AND @time < 50", m.schema(), &t).unwrap());
    assert_eq!(run_on(&m, &g), [3, 4]);
}

#[test]
fn bookmarks_are_rows_and_anchors_find_them_again() {
    let m = merged();
    let mut marks = Bookmarks::default();
    marks.toggle(4);
    let marks = Arc::new(marks);
    let f = Filter::Query(Query::compile_for("bookmark OR /a0/", m.schema()).unwrap().with_bookmarks(&marks));
    assert_eq!(run_on(&m, &f), [0, 4]);
    // Saved per file: through the files' lines (`reordered`), not rows.
    let r = m.reordered().unwrap();
    let per_file = marks.remapped(|row| r.line_of[row as usize]);
    assert!(per_file.contains(5), "b2 is line 2 of b, line 5 of the files");
    assert_eq!(per_file.remapped(|l| r.row_of[l as usize]), *marks);
}

#[test]
fn opening_merged_falls_back_where_times_are_missing() {
    let dir = std::env::temp_dir().join(format!("nerka-merged-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let write_dlt = |name: &str, msgs: &[(u32, &str)]| {
        let mut buf = Vec::new();
        for (secs, text) in msgs {
            let mut m = Msg::log(crate::Level::Warn, "ECU1", "APP", "CTX", vec![Value::Str((*text).into()).into()]);
            m.secs = *secs;
            m.encode(&mut buf);
        }
        let p = dir.join(name);
        std::fs::write(&p, buf).unwrap();
        p
    };
    let late = write_dlt("late.dlt", &[(20, "l0"), (40, "l1")]);
    let early = write_dlt("early.dlt", &[(10, "e0"), (30, "e1")]);
    let (p, o) = (Progress::new(), crate::OpenOptions::default());
    let (s, order) = crate::open_merged_by_time(&[late.clone(), early.clone()], &o, &p).unwrap();
    assert_eq!(order, [early.clone(), late.clone()]);
    assert_eq!(texts(&*s), ["e0", "l0", "e1", "l1"]);
    // One file: itself.
    let (one, _) = crate::open_merged_by_time(std::slice::from_ref(&late), &o, &p).unwrap();
    assert!(one.reordered().is_none());
    // Text files have no times: one after another.
    let (a, b) = (dir.join("a.log"), dir.join("b.log"));
    std::fs::write(&a, "one\n").unwrap();
    std::fs::write(&b, "two\n").unwrap();
    let (s, _) = crate::open_merged_by_time(&[b, a], &o, &p).unwrap();
    assert!(s.reordered().is_none());
    assert_eq!(s.line_text(0).unwrap(), "two");
    std::fs::remove_dir_all(&dir).ok();
}

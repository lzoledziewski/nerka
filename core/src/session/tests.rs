use super::*;
use crate::engine::search::tests::run_on;
use crate::signal::SignalDef;
use crate::{Matcher, MatcherOptions, TextFile};

const LOG: &str = "\
[INFO] v=1 start
[ERROR] v=5 disk
[WARN] v=20 slow
[ERROR] v=50 net
[INFO] v=2 done
";

fn source() -> Arc<dyn LogSource> {
    Arc::new(TextFile::from_bytes("t.log", LOG.as_bytes().to_vec()))
}

fn signals(spec: &str) -> Signals {
    let mut s = Signals::default();
    s.set(SignalDef::parse_spec(spec).unwrap());
    s
}

fn session() -> Session {
    let mut s = Session::new();
    s.set_source(source());
    s
}

fn q(s: &Session, nfd: &str) -> Filter {
    Filter::Query(s.compile(nfd).unwrap_or_else(|e| panic!("{nfd}: {e}")))
}

fn set(s: &Session, nfds: &[&str]) -> SearchSet {
    SearchSet { include: nfds.iter().map(|n| q(s, n)).collect(), exclude: vec![] }
}

fn run(s: &mut Session) {
    s.run_all(&Progress::new()).unwrap();
}

fn lines(s: &Session) -> Vec<LineNo> {
    s.result().map(|r| r.lines.clone()).unwrap_or_default()
}

fn hits(s: &Session) -> Vec<LineNo> {
    s.hits().map(|h| h.lines.clone()).unwrap_or_default()
}

/// The stages a round of tasks would start.
fn stages(s: &mut Session) -> Vec<Stage> {
    s.pending()
}

#[test]
fn search_and_colours_run_inside_the_filter_after_it() {
    let mut s = session();
    let f = q(&s, "level >= WARN");
    s.set_filter(Some(f.clone()));
    let search = set(&s, &["/v=5 |v=2 /"]);
    s.set_search(Some(search));
    let colors = set(&s, &["/net/"]);
    s.set_colors(Some(colors));
    // The filter alone first; the others wait for its result.
    assert_eq!(stages(&mut s), [Stage::Filter]);
    assert!(s.is_busy(Stage::Search));
    run(&mut s);
    assert_eq!(lines(&s), [1, 2, 3]);
    // Same lines as the engine finds directly (fast vs naive inside).
    assert_eq!(lines(&s), run_on(&*source(), &f));
    // v=2 is on line 4, not kept: the search only sees the filter's lines.
    assert_eq!(hits(&s), [1]);
    assert_eq!(s.colors().unwrap().lines, [3]);
    assert_eq!(s.rows(), 3);

    // No filter: every line, and what runs over the lines runs again.
    s.set_filter(None);
    assert_eq!(stages(&mut s), [Stage::Search, Stage::Colors]);
    s.finish(0, Ok(Output::Hits(SearchHits::default()))).unwrap(); // an unknown ticket: ignored
    run(&mut s);
    assert_eq!(s.rows(), 5);
    assert_eq!(hits(&s), [1, 4]);
}

#[test]
fn a_task_overtaken_by_a_change_is_ignored() {
    let mut s = session();
    let f = q(&s, "level >= ERROR");
    s.set_filter(Some(f));
    let old = s.tasks().pop().unwrap();
    // Changed while it ran: the old result doesn't count.
    let f = q(&s, "level >= WARN");
    s.set_filter(Some(f));
    let out = old.run(&Progress::new());
    assert_eq!(s.finish(old.ticket, out).unwrap(), Finished::Ignored);
    assert!(s.result().is_none());
    let new = s.tasks().pop().unwrap();
    assert!(s.tasks().is_empty(), "nothing new to start while it runs");
    let out = new.run(&Progress::new());
    assert_eq!(s.finish(new.ticket, out).unwrap(), Finished::Done(Stage::Filter));
    assert_eq!(lines(&s), [1, 2, 3]);
    // A cancelled task leaves the stage as it was.
    let f = q(&s, "level >= ERROR");
    s.set_filter(Some(f));
    let t = s.tasks().pop().unwrap();
    assert_eq!(s.finish(t.ticket, Err(crate::Error::Cancelled)).unwrap(), Finished::Ignored);
    assert_eq!(lines(&s), [1, 2, 3], "the last result stays shown");
}

#[test]
fn bookmarks_rerun_only_what_uses_them() {
    let mut s = session();
    let f = q(&s, "level >= ERROR OR bookmark");
    s.set_filter(Some(f));
    let search = set(&s, &["/v=/"]);
    s.set_search(Some(search));
    run(&mut s);
    assert_eq!(lines(&s), [1, 3]);
    let mut marks = Bookmarks::default();
    marks.toggle(4);
    s.set_bookmarks(marks.clone());
    // The filter uses them: it runs, then the search over its result.
    assert_eq!(stages(&mut s), [Stage::Filter]);
    run(&mut s);
    assert_eq!(lines(&s), [1, 3, 4]);
    assert_eq!(hits(&s), [1, 3, 4]);
    // The same bookmarks again: nothing to do.
    s.set_bookmarks(marks.clone());
    assert!(s.tasks().is_empty());

    // A filter without `bookmark`, a search with it: only the search runs.
    let f = q(&s, "level >= WARN");
    s.set_filter(Some(f));
    let search = set(&s, &["bookmark"]);
    s.set_search(Some(search));
    let colors = set(&s, &["/slow/"]);
    s.set_colors(Some(colors));
    run(&mut s);
    assert!(hits(&s).is_empty(), "line 4 isn't kept");
    marks.toggle(2);
    s.set_bookmarks(marks);
    assert_eq!(stages(&mut s), [Stage::Search]);
    run(&mut s);
    assert_eq!(hits(&s), [2]);
}

#[test]
fn signals_recompile_what_uses_them_and_drop_what_breaks() {
    let mut s = session();
    s.set_signals(signals("v=/v=(?<v>\\d+)/"));
    let f = q(&s, "@v > 4");
    s.set_filter(Some(f));
    let search = set(&s, &["/disk/", "@v.changed"]);
    s.set_search(Some(search));
    let colors = set(&s, &["/net/"]);
    s.set_colors(Some(colors));
    run(&mut s);
    assert_eq!(lines(&s), [1, 2, 3]);
    assert_eq!(hits(&s), [1, 2, 3]);
    // The same name from another capture: the filter and the search run
    // again with it, the colours (no signal) don't.
    assert!(s.set_signals(signals("v=/v=(?<v>\\d)\\d/")).is_empty());
    assert_eq!(stages(&mut s), [Stage::Filter]);
    run(&mut s);
    // Now only two-digit values: 20 -> 2, 50 -> 5 (states hold between).
    assert_eq!(lines(&s), [3, 4]);
    // The colours ran again too, over the new result (it changed).
    assert!(s.colors().unwrap().lines == [3]);

    // The signal is gone: the filter and the search can't compile.
    let errs = s.set_signals(Signals::default());
    let failed: Vec<Stage> = errs.iter().map(|(st, _)| *st).collect();
    assert_eq!(failed, [Stage::Filter, Stage::Search]);
    assert!(s.filter().is_none() && s.search_set().is_none());
    run(&mut s);
    assert_eq!(s.rows(), 5, "no filter: every line");
    assert_eq!(s.colors().unwrap().lines, [3]);
}

#[test]
fn a_new_source_runs_everything_again() {
    let mut s = session();
    let f = Filter::Regex(Matcher::new("ERROR", MatcherOptions::default()).unwrap());
    s.set_filter(Some(f));
    run(&mut s);
    assert_eq!(lines(&s), [1, 3]);
    s.set_source(Arc::new(TextFile::from_bytes("u.log", b"ERROR a\nok\nERROR b\nERROR c\n".to_vec())));
    assert!(s.result().is_none());
    run(&mut s);
    assert_eq!(lines(&s), [0, 2, 3]);
    // Without a source there is nothing to run.
    let mut empty = Session::new();
    empty.set_filter(Some(q(&empty, "level >= ERROR")));
    assert!(empty.tasks().is_empty());
    assert_eq!(empty.rows(), 0);
}

/// Review 2026-11-09, R5: a filter whose task fails leaves the session
/// without a filter result, as `finish` documents (before, the lines of the
/// filter before it stayed under the new filter's text); the search's hits,
/// found inside those lines, go too, and nothing runs again until a query
/// changes.
#[test]
fn a_failed_filter_has_no_result() {
    let mut s = session();
    let f = q(&s, "level >= ERROR");
    s.set_filter(Some(f));
    let search = set(&s, &["/v=/"]);
    s.set_search(Some(search));
    run(&mut s);
    assert_eq!(lines(&s), [1, 3]);
    assert!(!hits(&s).is_empty());
    // Another filter, whose task fails (an error while it runs).
    let next = q(&s, "level >= WARN");
    s.set_filter(Some(next));
    let t = s.tasks().into_iter().find(|t| t.stage == Stage::Filter).unwrap();
    assert!(s.finish(t.ticket, Err(crate::Error::Format("failed".into()))).is_err());
    assert!(s.result().is_none(), "the lines of `level >= ERROR` are still shown: {:?}", lines(&s));
    assert!(s.hits().is_none(), "hits found inside the old filter's lines");
    assert!(stages(&mut s).is_empty(), "nothing runs again by itself");
}

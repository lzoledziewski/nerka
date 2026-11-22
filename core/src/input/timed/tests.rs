use super::*;
use crate::engine::search::tests::run_on;
use crate::input::concat::Concat;
use crate::input::dlt::writer::{Msg, Value};
use crate::input::merged::Merged;
use crate::input::profile::Profiles;
use crate::signal::Signals;
use crate::{DltFile, Filter, Matcher, MatcherOptions, Query, TextFile};

/// 2025-10-17 10:00:00 UTC.
const T0: f64 = 1_760_695_200.0;

fn text(name: &str, s: &str) -> Box<dyn LogSource> {
    Box::new(TextFile::from_bytes(name, s.as_bytes().to_vec()))
}

fn timed(name: &str, s: &str, profile: &str, offset: f64) -> Timed {
    let p = Profile::parse_spec(profile).unwrap();
    Timed::text(text(name, s), &p, Why::Default, offset, &Progress::new()).unwrap()
}

/// A DLT file of `(seconds after T0, apid, text)` messages.
fn dlt(name: &str, msgs: &[(u32, &str, &str)]) -> Box<dyn LogSource> {
    let mut buf = Vec::new();
    for (secs, apid, text) in msgs {
        let mut m = Msg::log(crate::Level::Warn, "ECU1", apid, "CTX", vec![Value::Str((*text).into()).into()]);
        m.secs = T0 as u32 + secs;
        m.encode(&mut buf);
    }
    Box::new(DltFile::from_bytes(name, buf))
}

const LOG: &str = "header without a time\n\
                   2025-10-17 10:00:01.500 INFO start\n\
                   2025-10-17 10:00:03 ERROR failed\n\
                   \tat frame one\n\
                   \tat frame two\n\
                   [2025-10-17T10:00:05Z] WARN done\n";

#[test]
fn lines_carry_the_time_before_them() {
    let f = timed("a.log", LOG, "default: time auto", 0.0);
    assert!(f.has_time());
    let times: Vec<Option<f64>> = (0..f.line_count()).map(|l| f.time(l)).collect();
    let want = [1.5, 1.5, 3.0, 3.0, 3.0, 5.0].map(|d| Some(T0 + d));
    assert_eq!(times, want);
    // Out of order (the memo of the last line doesn't fit): the same.
    for l in [4, 0, 3, 5, 1] {
        assert_eq!(f.time(l), want[l as usize], "line {l}");
    }
    let own: Vec<Option<f64>> = (0..f.line_count()).map(|l| f.own_time(l)).collect();
    assert_eq!(own, [None, Some(T0 + 1.5), Some(T0 + 3.0), None, None, Some(T0 + 5.0)]);
    assert_eq!(f.time(6), None);
    let r = &f.info().reading[0];
    assert_eq!((r.profile.as_str(), r.time.as_str()), ("default", "the time each line starts with"));
}

#[test]
fn an_offset_moves_every_time() {
    let f = timed("a.log", LOG, "default: time auto", -3600.0);
    assert_eq!(f.time(2), Some(T0 + 3.0 - 3600.0));
    assert_eq!(f.own_time(3), None);
    // A table's time column shows the moved time; its bytes aren't the
    // file's any more.
    let d = Timed::shifted(dlt("d.dlt", &[(2, "APP", "x")]), 7200.0);
    assert_eq!(d.time(0), Some(T0 + 2.0 + 7200.0));
    assert!(d.line_text(0).unwrap().starts_with("2025-10-17 12:00:02.000000\t"), "{}", d.line_text(0).unwrap());
    assert!(d.line_detail(0).unwrap()[0].starts_with("2025-10-17 12:00:02"));
    let re = Filter::Regex(Matcher::new("12:00:02", MatcherOptions::default()).unwrap());
    assert_eq!(run_on(&d, &re), [0]);
}

#[test]
fn the_builtin_rule_needs_half_the_lines() {
    let few = "2025-10-17 10:00:01 a\nb\nc\nd\n";
    let f = timed("a.log", few, "default: time auto", 0.0);
    assert!(!f.has_time());
    assert_eq!(f.time(0), None);
    assert!(f.info().reading[0].time.contains("1 of the first 4"), "{}", f.info().reading[0].time);
    // A profile's own regex: one line is enough.
    let f = timed("a.log", few, r"p: time /^(?<time>\S+ \S+)/", 0.0);
    assert!(f.has_time());
    assert_eq!(f.time(3), Some(T0 + 1.0));
    // A regex that reads nothing: no time.
    let f = timed("a.log", few, r"p: time /never(?<time>x)/", 0.0);
    assert!(!f.has_time());
    let f = timed("a.log", few, "p: time none", 0.0);
    assert!(!f.has_time());
}

#[test]
fn zones_and_numbers_come_from_the_profile() {
    let f = timed("a.log", "2025-10-17 12:00:00 x\n", "p: time auto; zone +02:00", 0.0);
    assert_eq!(f.time(0), Some(T0));
    let k = timed("k.log", "[ 12.5] boot\n[ 13.0] up\n", r"k: time /\[\s*(?<time>[\d.]+)\]/; numbers s", 0.0);
    assert_eq!(k.time(1), Some(13.0));
    let ms = timed("m.log", "1760695200250 x\n", r"m: time /^(?<time>\d+)/; numbers ms", 0.0);
    assert_eq!(ms.time(0), Some(T0 + 0.25));
}

/// A text file with times shares a view with DLT (owner, 2026-11-14: text
/// logs merged with DLTs and captures).
#[test]
fn timed_text_joins_dlt_in_the_mixed_layout() {
    let p = Profile::builtin();
    let t = Timed::text(text("app.log", LOG), &p, Why::Default, 0.0, &Progress::new()).unwrap();
    let d = dlt("ecu.dlt", &[(2, "NAV", "nav up"), (4, "NAV", "nav ERROR seen")]);
    let c = Concat::new(vec![Box::new(t), d]).unwrap();
    assert!(c.is_mixed());
    assert!(c.info().protocols.iter().any(|p| p == "TEXT"), "{:?}", c.info().protocols);
    let cols: Vec<&str> = c.columns().iter().map(|c| c.name.as_ref()).collect();
    assert_eq!(cols, ["time", "proto", "info", "payload"]);
    // A text line: its own time, TEXT, no info, the line; a line carrying
    // the time before it shows none.
    assert_eq!(c.line_text(2).unwrap(), "2025-10-17 10:00:03.000000\tTEXT\t\t2025-10-17 10:00:03 ERROR failed");
    assert_eq!(c.line_text(3).unwrap(), "\tTEXT\t\t\tat frame one");
    assert_eq!(c.line_detail(3).unwrap(), ["", "TEXT", "", "\tat frame one"]);
    let m = Merged::new(c, &Progress::new()).unwrap();
    let payload = |r: LineNo| m.line_text(r).unwrap().rsplit('\t').next().unwrap().to_owned();
    let order: Vec<String> = (0..m.line_count()).map(payload).collect();
    assert_eq!(
        order,
        [
            "header without a time",
            "2025-10-17 10:00:01.500 INFO start",
            "nav up",
            "2025-10-17 10:00:03 ERROR failed",
            "at frame one",
            "at frame two",
            "nav ERROR seen",
            "[2025-10-17T10:00:05Z] WARN done",
        ]
    );
    // Filters: the files' own paths and the naive one alike.
    let q = |nfd: &str| Filter::Query(Query::compile_for(nfd, m.schema()).unwrap_or_else(|e| panic!("{nfd}: {e}")));
    assert_eq!(run_on(&m, &q("proto == TEXT AND level >= WARN")), [3, 7]);
    // The DLT's own level (WARN), the text's from its line.
    assert_eq!(run_on(&m, &q("level >= ERROR")), [3]);
    assert_eq!(run_on(&m, &q("level == WARN")), [2, 6, 7]);
    assert_eq!(run_on(&m, &q("file == app.log AND /frame/")), [4, 5]);
    assert_eq!(run_on(&m, &q("raw ~ /^\\tat/")), Vec::<LineNo>::new());
    let re = Filter::Regex(Matcher::new("ERROR", MatcherOptions::default()).unwrap());
    assert_eq!(run_on(&m, &re), [3, 6]);
    // `@time` on every row, the text's carried ones too.
    let s = Signals::default().for_source(&m);
    let t = Filter::Query(Query::compile_in("@time >= \"2025-10-17 10:00:03\"", m.schema(), &s).unwrap());
    assert_eq!(run_on(&m, &t), [3, 4, 5, 6, 7]);
}

/// Owner, 2026-11-22: a field `profile` on text files' lines, and a
/// profile's signals reading only its files.
#[test]
fn the_profile_is_a_field_and_its_signals_read_its_files_only() {
    let app = Profile::parse_spec(r"app: time auto; signal word:enum=/(?<word>ERROR|seen)/").unwrap();
    let t = Timed::text(text("app.log", LOG), &app, Why::Name, 0.0, &Progress::new()).unwrap();
    assert_eq!(t.schema().last().unwrap().name, "profile");
    let d = dlt("ecu.dlt", &[(2, "NAV", "nav up"), (4, "NAV", "nav ERROR seen")]);
    let m = Merged::new(Concat::new(vec![Box::new(t), d]).unwrap(), &Progress::new()).unwrap();
    let q = |nfd: &str, s: &Signals| {
        Filter::Query(Query::compile_in(nfd, m.schema(), s).unwrap_or_else(|e| panic!("{nfd}: {e}")))
    };
    let none = Signals::default();
    // Rows: header, start, nav up, failed, frame one, frame two, nav ERROR seen, done.
    assert_eq!(run_on(&m, &q("profile == app", &none)), [0, 1, 3, 4, 5, 7]);
    assert_eq!(run_on(&m, &q("profile == app AND /ERROR/", &none)), [3]);
    assert_eq!(run_on(&m, &q("profile != app", &none)), [2, 6]);
    // The DLT's "ERROR seen" isn't an event of the text's profile signal.
    let (ps, _) = Profiles::parse_text(&format!("{}\n", app.spec()));
    let reading = m.info().reading;
    let mut s = Signals::default().for_source(&m);
    for d in ps.signals_for(&reading).signals {
        s.set(d);
    }
    assert_eq!(run_on(&m, &q("@word.event", &s)), [3]);
    // A profile no file is read with: said.
    let w = crate::input::query_warnings(&Query::compile_for("profile == other", m.schema()).unwrap(), &m);
    assert_eq!(w, ["no file here is read with profile 'other' (there are: app)"]);
}

#[test]
fn text_without_a_time_is_refused_beside_dlt() {
    let p = Profile::parse_spec("p: time none").unwrap();
    let t = Timed::text(text("app.log", LOG), &p, Why::Default, 0.0, &Progress::new()).unwrap();
    let Err(crate::Error::MixedKinds(e)) = Concat::new(vec![Box::new(t), dlt("ecu.dlt", &[(1, "NAV", "x")])]) else {
        panic!("refused")
    };
    assert!(e.contains("app.log has no time"), "{e}");
    // Text with text stays one kind, without the mixed layout.
    let a = Box::new(timed("a.log", LOG, "default: time auto", 0.0));
    let b = Box::new(timed("b.log", LOG, "p: time none", 0.0));
    let c = Concat::new(vec![a, b]).unwrap();
    assert!(!c.is_mixed() && c.columns().is_empty());
}

#[test]
fn files_on_disk_get_their_profile_and_offset() {
    let dir = std::env::temp_dir().join(format!("nerka-timed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("app.log");
    std::fs::write(&log, LOG).unwrap();
    let ecu = dir.join("ecu.dlt");
    let mut buf = Vec::new();
    for (secs, text) in [(2u32, "nav up"), (4, "nav two")] {
        let mut m = Msg::log(crate::Level::Info, "ECU1", "NAV", "CTX", vec![Value::Str(text.into()).into()]);
        m.secs = T0 as u32 + secs;
        m.encode(&mut buf);
    }
    std::fs::write(&ecu, buf).unwrap();
    let p = Progress::new();
    let mut o = crate::OpenOptions::default();
    let (s, order) = crate::open_merged_by_time(&[ecu.clone(), log.clone()], &o, &p).unwrap();
    assert!(s.reordered().is_some(), "merged");
    // The text starts first (its header carries 10:00:01.5).
    assert_eq!(order, [log.clone(), ecu.clone()]);
    let reading = s.info().reading;
    assert_eq!(reading.len(), 2);
    assert_eq!((reading[0].file.as_str(), reading[0].profile.as_str()), ("app.log", "default"));
    assert_eq!(reading[1].time, "the file's own");
    // A saved profile chosen by name; the DLT moved by an offset.
    let (ps, _) = Profiles::parse_text("logs: name *.log; time none\n");
    o.profiles = std::sync::Arc::new(ps);
    o.offset_for = vec![(ecu.clone(), 3_000_000_000)];
    let one = crate::open_with(&log, &o, &p).unwrap();
    assert!(!one.has_time());
    assert_eq!(one.info().reading[0].why, "by the file's name");
    let moved = crate::open_with(&ecu, &o, &p).unwrap();
    assert_eq!(moved.time(0), Some(T0 + 5.0));
    // A split chosen for one file is that file's only; one that finds
    // nothing to split at says so.
    let other = dir.join("other.log");
    std::fs::write(&other, "x\ny\n").unwrap();
    let start = crate::input::records::RecordSplit::parse("start=/\\d{4}-/").unwrap();
    o.records_for = vec![(log.clone(), start.clone())];
    assert_eq!(crate::open_with(&log, &o, &p).unwrap().line_count(), 3, "the header, two records");
    let plain = crate::open_with(&other, &o, &p).unwrap();
    assert_eq!((plain.line_count(), plain.format()), (2, "text".to_owned()));
    o.records_for = vec![(other.clone(), start)];
    let one = crate::open_with(&other, &o, &p).unwrap();
    assert_eq!(one.line_count(), 1);
    assert!(one.info().notes[0].starts_with("start=/\\d{4}-/ matched no line: the whole file is one record"));
    o.records_for.clear();
    // Forced: the default, whatever the name says.
    o.profile_for = vec![(log.clone(), "default".into())];
    assert!(crate::open_with(&log, &o, &p).unwrap().has_time());
    std::fs::remove_dir_all(&dir).ok();
}

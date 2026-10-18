use super::*;
use crate::engine::search::tests::run_on;
use crate::input::mem::{MemSource, Row};
use crate::{Filter, Level, LogSource, Query, TextFile};

const NAV: &str = "\
pack nav
query all(app)  labels: sys/nav
    apid == $app
query slow  parent: all  labels: perf
    /slow/
query errs  parent: all  labels: sys/errors
    level >= ERROR
    OR /error/
query big  parent: slow
    mcnt >= 0x10
";

const TXT: &str = "\
pack txt
query cam(cam)  labels: vision
    raw ~ /$cam frame drop/
query fail(what)  labels: vision, errors
    level >= ERROR AND \"$what\"
";

const VISION: &str = "\
pack vision
query cam_scope(ecu, cam)  labels: system/vision
    ecu == $ecu AND ctid == $cam
";

fn pack(text: &str) -> Pack {
    parse_pack("t.nerka-pack", text).unwrap_or_else(|e| panic!("{e}"))
}

fn lib() -> Library {
    let mut lib = Library::new();
    lib.add(pack(NAV)).unwrap();
    lib.add(pack(TXT)).unwrap();
    lib
}

fn params(kv: &[(&str, &str)]) -> Params {
    kv.iter().map(|(k, v)| (k.to_string(), ParamValue::literal(*v))).collect()
}

/// Picks; `@label` picks a label.
fn sel(include: &[&str], exclude: &[&str]) -> Selection {
    let pick = |s: &&str| match s.strip_prefix('@') {
        Some(l) => Pick::Label(l.to_owned()),
        None => Pick::Query(s.to_string()),
    };
    Selection { include: include.iter().map(pick).collect(), exclude: exclude.iter().map(pick).collect() }
}

fn mem() -> MemSource {
    let row = |text, apid, mcnt, level| Row { text, apid, mcnt, level };
    MemSource::new(vec![
        row("nav start", Some("NAV"), Some(1), Some(Level::Info)),
        row("nav slow", Some("NAV"), Some(2), Some(Level::Warn)),
        row("nav error", Some("NAV"), Some(3), Some(Level::Debug)),
        row("stor slow", Some("STOR"), Some(4), Some(Level::Info)),
        row("no header slow", None, None, None),
        row("nav slow again", Some("NAV"), Some(0x10), Some(Level::Error)),
        row("stor down", Some("STOR"), Some(0x20), Some(Level::Fatal)),
    ])
}

/// The expansion finds the same lines as the hand-written query, both
/// checked fast path against naive per-line evaluation.
#[track_caller]
fn same_lines(src: &dyn LogSource, expanded: &str, hand: &str) -> Vec<crate::LineNo> {
    let q = |s: &str| Filter::Query(Query::compile(s).unwrap_or_else(|e| panic!("{s}: {e}")));
    let got = run_on(src, &q(expanded));
    assert_eq!(got, run_on(src, &q(hand)), "{expanded}  vs  {hand}");
    got
}

#[test]
fn parent_chain_and_provenance() {
    let e = expand(&sel(&["nav.big"], &[]), &lib(), &params(&[("app", "NAV")])).unwrap();
    assert_eq!(e.nfd, r#"(apid == "NAV") AND (/slow/) AND (mcnt >= 0x10)"#);
    let [p] = e.parts.as_slice() else { panic!("{:?}", e.parts) };
    assert_eq!((p.query.as_str(), p.role, e.part_nfd(p)), ("nav.big", Role::Include, e.nfd.as_str()));
    let chain: Vec<(&str, &str)> = p.chain.iter().map(|(q, r)| (q.as_str(), &e.nfd[r.clone()])).collect();
    assert_eq!(chain, [("nav.all", r#"(apid == "NAV")"#), ("nav.slow", "(/slow/)"), ("nav.big", "(mcnt >= 0x10)")]);
    assert_eq!(same_lines(&mem(), &e.nfd, "apid == NAV AND raw ~ /slow/ AND mcnt >= 16"), [5]);
}

#[test]
fn include_or_exclude_and_not() {
    let lib = lib();
    let p = params(&[("app", "NAV")]);
    let e = expand(&sel(&["nav.slow", "nav.errs"], &["nav.big"]), &lib, &p).unwrap();
    assert_eq!(
        e.nfd,
        r#"((apid == "NAV") AND (/slow/) OR (apid == "NAV") AND (level >= ERROR OR /error/)) AND NOT ((apid == "NAV") AND (/slow/) AND (mcnt >= 0x10))"#
    );
    let parts: Vec<(&str, Role, &str)> = e.parts.iter().map(|p| (p.query.as_str(), p.role, e.part_nfd(p))).collect();
    assert_eq!(parts[1], ("nav.errs", Role::Include, r#"(apid == "NAV") AND (level >= ERROR OR /error/)"#));
    assert_eq!(parts[2].1, Role::Exclude);
    // Every part is valid NFD on its own.
    for p in &e.parts {
        Query::compile(e.part_nfd(p)).unwrap();
    }
    let hand = "apid == NAV AND (/slow/ OR level >= ERROR OR /error/) AND NOT (/slow/ AND mcnt >= 16)";
    assert_eq!(same_lines(&mem(), &e.nfd, hand), [1, 2]);

    // One include: no group parentheses; a one-query exclude: none either.
    let e = expand(&sel(&["txt.cam"], &["txt.fail"]), &lib, &params(&[("cam", "c1"), ("what", "x")])).unwrap();
    assert_eq!(e.nfd, r#"(raw ~ /c1 frame drop/) AND NOT (level >= ERROR AND "x")"#);
    // Only excludes: every other line.
    let e = expand(&sel(&[], &["nav.slow", "txt.cam"]), &lib, &params(&[("app", "NAV"), ("cam", "c")])).unwrap();
    assert_eq!(e.nfd, r#"NOT ((apid == "NAV") AND (/slow/)) AND NOT (raw ~ /c frame drop/)"#);
    assert_eq!(same_lines(&mem(), &e.nfd, "NOT (apid == NAV AND /slow/)"), [0, 2, 3, 4, 6]);
}

#[test]
fn labels_resolve_to_a_snapshot() {
    let mut lib = lib();
    let s = sel(&["@sys"], &[]);
    let frozen = s.snapshot(&lib).unwrap();
    assert_eq!(frozen.include, [Pick::Query("nav.all".into()), Pick::Query("nav.errs".into())]);
    lib.add(pack("pack late\nquery x  labels: sys/late\n  /late/\n")).unwrap();
    let p = params(&[("app", "NAV")]);
    assert!(!expand(&frozen, &lib, &p).unwrap().nfd.contains("late"));
    assert!(expand(&s, &lib, &p).unwrap().nfd.contains("(/late/)"));
    // A label picked together with one of its queries doesn't repeat it.
    let p = params(&[("app", "NAV"), ("what", "x")]);
    let e = expand(&sel(&["nav.errs", "@sys/errors", "@errors"], &[]), &lib, &p).unwrap();
    let ids: Vec<&str> = e.parts.iter().map(|p| p.query.as_str()).collect();
    assert_eq!(ids, ["nav.errs", "txt.fail"]);
}

#[test]
fn parameters_are_regex_escaped() {
    let text = "\
cam(1).x frame drop\n\
cam11x frame drop\n\
a/b\\c frame drop\n\
[ERROR] lost \"q\\uote\" here\n\
[ERROR] lost q\\uote here\n";
    let src = TextFile::from_bytes("t", text.as_bytes().to_vec());
    let lib = lib();
    let one = |id: &str, v: ParamValue| {
        let name = if id == "txt.cam" { "cam" } else { "what" };
        expand(&sel(&[id], &[]), &lib, &[(name.to_string(), v)].into())
    };

    let e = one("txt.cam", ParamValue::literal("cam(1).x")).unwrap();
    assert_eq!(e.nfd, r"(raw ~ /cam\(1\)\.x frame drop/)");
    assert_eq!(same_lines(&src, &e.nfd, r#"raw ~ "cam(1).x frame drop""#), [0]);
    // `/` can't end the regex, and a trailing `\` can't escape its end.
    let e = one("txt.cam", ParamValue::literal(r"a/b\c")).unwrap();
    assert_eq!(same_lines(&src, &e.nfd, r#""a/b\\c frame drop""#), [2]);
    let e = one("txt.cam", ParamValue::literal(r"\")).unwrap();
    assert_eq!(e.nfd, r"(raw ~ /\x5c frame drop/)");
    // Raw: regex syntax when asked for; `/` is still escaped.
    let e = one("txt.cam", ParamValue::raw(r"cam\(1\)\.x|cam1+x")).unwrap();
    assert_eq!(same_lines(&src, &e.nfd, r"/cam\(1\)\.x frame drop|cam1+x frame drop/"), [0, 1]);
    let e = one("txt.cam", ParamValue::raw("a/b")).unwrap();
    assert_eq!(e.nfd, r"(raw ~ /a\x2fb frame drop/)");
    // Inside a string a value is a literal, raw or not.
    let e = one("txt.fail", ParamValue::raw(r#""q\uote""#)).unwrap();
    assert_eq!(e.nfd, r#"(level >= ERROR AND "\"q\\uote\"")"#);
    assert_eq!(same_lines(&src, &e.nfd, r#"level >= ERROR AND /"q\\uote"/"#), [3]);
    // A bare value becomes a string, whatever it contains.
    let e = expand(&sel(&["nav.all"], &[]), &lib, &params(&[("app", r#"N"A OR x"#)])).unwrap();
    assert_eq!(e.nfd, r#"(apid == "N\"A OR x")"#);
    assert!(same_lines(&mem(), &e.nfd, r#"apid == "N\"A OR x""#).is_empty());
}

#[test]
fn errors() {
    let lib = lib();
    let p = params(&[("app", "NAV")]);
    let err = |s: Selection, p: &Params| expand(&s, &lib, p).unwrap_err();
    assert_eq!(err(sel(&[], &[]), &p), ExpandError::Empty);
    assert_eq!(err(sel(&["nav.nope"], &[]), &p), ExpandError::UnknownQuery("nav.nope".into()));
    assert_eq!(err(sel(&["nope"], &[]), &p), ExpandError::UnknownQuery("nope".into()));
    assert_eq!(err(sel(&["nav.all"], &["@nope"]), &p), ExpandError::UnknownLabel("nope".into()));
    assert_eq!(
        err(sel(&["nav.big"], &[]), &Params::new()),
        ExpandError::MissingParam { query: "nav.all".into(), param: "app".into() }
    );
    // A raw value that breaks the regex is reported, not run.
    let bad: Params = [("cam".to_string(), ParamValue::raw("("))].into();
    let e = err(sel(&["txt.cam"], &[]), &bad);
    assert!(matches!(&e, ExpandError::Invalid { query, err } if query == "txt.cam" && err.pos == 6), "{e:?}");

    // A child whose parent is gone is an error, never "all lines".
    let mut lib = Library::new();
    lib.add(pack(
        "pack ota\nquery swup  parent: vision.cam_scope  labels: ota\n  apid == SWUP\nquery own  parent: gone\n  /x/\n",
    ))
    .unwrap();
    let orphan = |lib: &Library, id: &str| expand(&sel(&[id], &[]), lib, &Params::new()).unwrap_err();
    assert_eq!(
        orphan(&lib, "ota.swup"),
        ExpandError::OrphanedChild { query: "ota.swup".into(), parent: "vision.cam_scope".into() }
    );
    assert_eq!(
        orphan(&lib, "ota.own"),
        ExpandError::OrphanedChild { query: "ota.own".into(), parent: "ota.gone".into() }
    );
    assert!(matches!(orphan(&lib, "@ota"), ExpandError::OrphanedChild { .. }));
    // Once the parent's pack is loaded, the child refines it.
    lib.add(pack(VISION)).unwrap();
    let p = params(&[("ecu", "SOCB"), ("cam", "CAM1")]);
    assert_eq!(
        expand(&sel(&["@ota"], &[]), &lib, &p).unwrap().nfd,
        r#"(ecu == "SOCB" AND ctid == "CAM1") AND (apid == SWUP)"#
    );

    let mut lib = Library::new();
    lib.add(pack("pack c\nquery a  parent: b\n  /a/\nquery b  parent: a\n  /b/\n")).unwrap();
    assert_eq!(orphan(&lib, "c.a"), ExpandError::ParentCycle("c.a".into()));
}

#[test]
fn multi_line_bodies_become_one_line() {
    let e = expand(&sel(&["nav.errs"], &[]), &lib(), &params(&[("app", "NAV")])).unwrap();
    assert_eq!(e.nfd, r#"(apid == "NAV") AND (level >= ERROR OR /error/)"#);
    let mut lib = Library::new();
    lib.add(pack("pack w\nquery a\n  raw ~ /a  b/\n     AND\n\n   \"x   y\"\n")).unwrap();
    assert_eq!(expand(&sel(&["w.a"], &[]), &lib, &Params::new()).unwrap().nfd, r#"(raw ~ /a  b/ AND "x   y")"#);
}

#[test]
fn expansion_as_a_search_inside_a_filter() {
    let src = mem();
    let filter = expand(&sel(&["nav.all"], &[]), &lib(), &params(&[("app", "NAV")])).unwrap();
    let within = same_lines(&src, &filter.nfd, "apid == NAV");
    let search = expand(&sel(&["nav.slow", "nav.errs"], &["nav.big"]), &lib(), &params(&[("app", "NAV")])).unwrap();
    let set = search.search_set().unwrap();
    let hits = crate::search_within(&src, Some(&within), &set, &crate::Progress::new()).unwrap();
    let got: Vec<(crate::LineNo, Vec<usize>)> =
        (0..hits.len()).map(|i| (hits.lines[i], hits.matched(i).collect())).collect();
    // Line 5 is slow, an error and big: excluded.
    assert_eq!(got, [(1, vec![0]), (2, vec![1])]);
}

#[test]
fn missing_fields_follow_the_parent_chain() {
    use crate::input::mem::SCHEMA;
    let mut lib = lib();
    lib.add(pack(VISION)).unwrap();
    // nav.* use apid and mcnt, which the test schema has; text has none.
    assert!(lib.missing_fields("nav.big", SCHEMA).is_empty());
    assert_eq!(lib.missing_fields("nav.big", &[]), ["apid", "mcnt"]);
    assert!(lib.missing_fields("txt.cam", &[]).is_empty());
    // A child inherits its parent's fields; parameters don't matter.
    assert_eq!(lib.missing_fields("vision.cam_scope", SCHEMA), ["ecu", "ctid"]);
    assert!(lib.missing_fields("nope.x", &[]).is_empty());
}

#[test]
fn chains() {
    let lib = lib();
    assert_eq!(lib.chain("nav.big"), ["nav.all", "nav.slow", "nav.big"]);
    assert_eq!(lib.chain("nav.all"), ["nav.all"]);
    assert!(lib.chain("nav.nope").is_empty());
}

#[test]
fn params_needed_follow_parent_chains() {
    let mut lib = lib();
    lib.add(pack(VISION)).unwrap();
    assert_eq!(lib.params_needed(["nav.big", "txt.cam", "nav.all"]), ["app", "cam"]);
    assert_eq!(lib.params_needed(["vision.cam_scope", "nope.x"]), ["ecu", "cam"]);
    assert!(lib.params_needed(["nav.slow"; 0]).is_empty());
}

use super::*;
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue};
use std::borrow::Cow;

const SCHEMA: &[FieldDef] = &[FieldDef::new("ecu", FieldKind::Str, false)];

/// A row: (ecu, time, uptime, text).
type Row = (&'static str, f64, f64, &'static str);

struct Src(Vec<Row>);

impl LogSource for Src {
    fn name(&self) -> &str {
        "test"
    }
    fn line_count(&self) -> LineNo {
        self.0.len() as LineNo
    }
    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
        self.0.get(line as usize).map(|r| Cow::Borrowed(r.3.as_bytes()))
    }
    fn schema(&self) -> &[FieldDef] {
        SCHEMA
    }
    fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        let row = self.0.get(line as usize)?;
        (id == 0).then(|| FieldValue::Str(row.0.as_bytes().into()))
    }
    fn time(&self, line: LineNo) -> Option<f64> {
        Some(self.0.get(line as usize)?.1)
    }
    fn uptime(&self, line: LineNo) -> Option<f64> {
        Some(self.0.get(line as usize)?.2)
    }
}

/// ECU A boots twice (reboot at line 6); B is chatty. Times in seconds.
fn log() -> Src {
    Src(vec![
        ("A", 0.0, 0.1, "boot temp=40"),
        ("B", 0.5, 9.0, "retry"),
        ("A", 1.0, 1.1, "net down"),
        ("B", 1.1, 9.6, "retry"),
        ("B", 1.2, 9.7, "retry"),
        ("A", 40.0, 40.1, "temp=91 ota progress"),
        ("A", 50.0, 0.2, "boot temp=50"),
        ("A", 51.0, 1.2, "net down"),
        ("A", 52.0, 2.2, "ota progress temp=60"),
        ("B", 53.0, 60.0, "retry"),
    ])
}

fn signals() -> Signals {
    let mut s = Signals::default();
    s.set(SignalDef::parse_spec("lifecycle a: lines ecu == A; reset uptime").unwrap());
    s
}

fn check(spec: &str) -> CheckResult {
    let src = log();
    CheckDef::parse_spec(spec)
        .unwrap_or_else(|e| panic!("{spec}: {} at {}", e.msg, e.pos))
        .run(&src, &signals(), &Progress::new())
        .unwrap()
}

/// (scope, verdict) of each scope.
fn verdicts(r: &CheckResult) -> Vec<(String, Verdict)> {
    r.scopes.iter().map(|s| (s.scope.label.clone(), s.verdict.clone())).collect()
}

fn issue(n: u64) -> Verdict {
    Verdict::Issue(n)
}

fn fact(v: &str) -> Verdict {
    Verdict::Fact(Some(v.into()))
}

#[test]
fn exists_overall_and_per_lifecycle() {
    let r = check("down: lines raw ~ /net down/; per a");
    assert_eq!(r.kind, Kind::Issue);
    assert_eq!(verdicts(&r), [("overall".into(), issue(2)), ("a #1".into(), issue(1)), ("a #2".into(), issue(1))]);
    assert_eq!(r.scopes[2].evidence[0].rows, [7]);
    // A lifecycle's span holds B's lines too.
    let r = check("retry: lines raw == retry; per a");
    assert_eq!(verdicts(&r)[1..], [("a #1".into(), issue(3)), ("a #2".into(), issue(1))]);
    let r = check("none: lines raw == nothing");
    assert_eq!(verdicts(&r), [("overall".into(), Verdict::Ok)]);
}

#[test]
fn counts_are_facts_or_issues() {
    let r = check("n: lines raw == retry; count; per a");
    assert_eq!(r.kind, Kind::Fact);
    assert_eq!(verdicts(&r), [("overall".into(), fact("4")), ("a #1".into(), fact("3")), ("a #2".into(), fact("1"))]);
    // As an issue: as often as there are lines, where the condition holds.
    let r = check("many: lines raw == retry; count >= 3; per a");
    assert_eq!(verdicts(&r), [("overall".into(), issue(4)), ("a #1".into(), issue(3)), ("a #2".into(), Verdict::Ok)]);
    assert_eq!(r.scopes[1].evidence.iter().map(|e| e.rows.clone()).collect::<Vec<_>>(), [[1], [3], [4]]);
}

#[test]
fn rates_find_bursts() {
    // Three retries within 1 s: one burst.
    let r = check("storm: lines raw == retry; rate 2 per 1s");
    assert_eq!(verdicts(&r), [("overall".into(), issue(1))]);
    assert_eq!(r.overall().evidence[0].rows, [1, 4]);
    assert_eq!(check("storm: lines raw == retry; rate 3 per 1s").overall().verdict, Verdict::Ok);
}

/// Review finding R4: times that go back (two overlapping files one after
/// another) must not make a burst out of lines 10 s apart. Ignored until
/// the owner decides how checks treat times out of order.
#[test]
#[ignore = "R4: checks assume rows in time order (doc/review/2026-11-07-findings.md)"]
fn rates_with_times_going_back() {
    // File A: 100 s .. 130 s; then file B: 105 s .. 135 s. Every retry is
    // 10 s from the one before it in its own file.
    let src = Src(vec![
        ("A", 100.0, 0.0, "retry"),
        ("A", 110.0, 0.0, "retry"),
        ("A", 120.0, 0.0, "retry"),
        ("A", 130.0, 0.0, "retry"),
        ("B", 105.0, 0.0, "retry"),
        ("B", 115.0, 0.0, "retry"),
        ("B", 125.0, 0.0, "retry"),
        ("B", 135.0, 0.0, "retry"),
    ]);
    let r = CheckDef::parse_spec("storm: lines raw == retry; rate 2 per 1s")
        .unwrap()
        .run(&src, &Signals::default(), &Progress::new())
        .unwrap();
    assert_eq!(r.overall().verdict, Verdict::Ok, "{:?}", r.overall().evidence);
}

#[test]
fn gaps_between_its_lines() {
    let r = check("quiet: lines ecu == A; gap 30s; per a");
    // 1 s -> 40 s in the first lifecycle; the second is busy.
    assert_eq!(verdicts(&r), [("overall".into(), issue(1)), ("a #1".into(), issue(1)), ("a #2".into(), Verdict::Ok)]);
    assert_eq!(r.overall().evidence[0].rows, [2, 5]);
    assert_eq!(r.overall().evidence[0].note, "none for 39.000 s");
}

#[test]
fn a_cause_then_or_without_its_effect() {
    // Net down, OTA progress within 5 s: only in the second lifecycle.
    let r = check("ok: lines raw ~ /net down/; then raw ~ /ota progress/ within 5s; per a");
    assert_eq!(verdicts(&r), [("overall".into(), issue(1)), ("a #1".into(), Verdict::Ok), ("a #2".into(), issue(1))]);
    assert_eq!(r.scopes[2].evidence[0].rows, [7, 8]);
    let r = check("stall: lines raw ~ /net down/; missing raw ~ /ota progress/ within 5s; per a");
    assert_eq!(verdicts(&r), [("overall".into(), issue(1)), ("a #1".into(), issue(1)), ("a #2".into(), Verdict::Ok)]);
    // A follow-up in the next lifecycle doesn't count for this one.
    let r = check("stall: lines raw ~ /net down/; missing raw ~ /ota progress/ within 60s; per a");
    assert_eq!(verdicts(&r)[1], ("a #1".into(), Verdict::Ok), "the OTA line at 40 s is in a #1");
    // Not judged when the log ends first.
    let r = check("stall: lines raw ~ /net down/; missing raw ~ /never/ within 1h");
    assert_eq!(r.overall().verdict, Verdict::Ok);
}

#[test]
fn values_from_its_lines() {
    let r = check(r"temp: lines /temp=(?<t>\d+)/; value max; per a");
    assert_eq!(
        verdicts(&r),
        [("overall".into(), fact("91")), ("a #1".into(), fact("91")), ("a #2".into(), fact("60"))]
    );
    assert_eq!(r.scopes[1].evidence[0].rows, [5]);
    let r = check(r"t0: lines /temp=(?<t>\d+)/; value first; per a");
    assert_eq!(verdicts(&r)[1..], [("a #1".into(), fact("40")), ("a #2".into(), fact("50"))]);
    assert_eq!(check(r"lo: lines /temp=(?<t>\d+)/; value min").overall().verdict, fact("40"));
    let e = CheckDef::parse_spec("x: lines raw ~ /temp/; value max").unwrap().run(&log(), &signals(), &Progress::new());
    assert!(e.unwrap_err().to_string().contains("needs a named group"));
}

#[test]
fn every_test_can_be_a_fact_or_an_issue() {
    // As facts: the measure.
    assert_eq!(check("n: lines raw == retry; exists; fact").overall().verdict, fact("4"));
    let r = check("most: lines raw == retry; rate 1 per 1s; fact");
    assert_eq!(r.overall().verdict, fact("3"));
    assert_eq!(r.overall().evidence[0].rows, [1, 4]);
    let r = check("longest: lines ecu == A; gap 1s; fact; per a");
    assert_eq!(verdicts(&r)[1], ("a #1".into(), fact("39.000 s")));
    assert_eq!(r.scopes[1].evidence[0].rows, [2, 5]);
    let r = check("stalls: lines raw ~ /net down/; missing raw ~ /ota progress/ within 5s; fact; per a");
    assert_eq!(verdicts(&r), [("overall".into(), fact("1")), ("a #1".into(), fact("1")), ("a #2".into(), fact("0"))]);
    // As issues: a value with a condition, a count without one (any).
    let r = check(r"hot: lines /temp=(?<t>\d+)/; value max > 80; per a");
    assert_eq!(r.kind, Kind::Issue);
    assert_eq!(verdicts(&r), [("overall".into(), issue(1)), ("a #1".into(), issue(1)), ("a #2".into(), Verdict::Ok)]);
    assert_eq!(r.scopes[1].evidence[0].note, "the max: 91");
    assert_eq!(check("any: lines raw == retry; count; issue").overall().verdict, issue(4));
    assert_eq!(check("none: lines raw == never; count; issue").overall().verdict, Verdict::Ok);
    let e = CheckDef::parse_spec(r"x: lines /t=(?<t>\d+)/; value max; issue").unwrap_err();
    assert_eq!(e.msg, "an issue on a value needs a condition, e.g. value max > 90");
}

#[test]
fn names_and_library_queries_may_hold_spaces() {
    // Bug (owner, 2026-11-07): "Fusa Faults from aurix", {Aurix_logs.Fusa Fault}.
    let d = CheckDef::parse_spec("Fusa Faults from aurix: lines {Aurix_logs.Fusa Fault}; count").unwrap();
    assert_eq!(d.name(), "Fusa Faults from aurix");
    assert_eq!(d.unresolved(), ["Aurix_logs.Fusa Fault"]);
    assert_eq!(d.spec(), "Fusa Faults from aurix: lines {Aurix_logs.Fusa Fault}; count");
    let p = CheckDef::parse_spec("x: lines {sys.drops of cam cam=CAM1 n=2}; exists").unwrap();
    assert_eq!(p.unresolved(), ["sys.drops of cam"]);
    assert_eq!(p.spec(), "x: lines {sys.drops of cam cam=CAM1 n=2}; exists");
    let dir = std::env::temp_dir().join(format!("nerka-check-space-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pack = dir.join("Aurix_logs.nerka-pack");
    std::fs::write(
        &pack,
        "pack Aurix_logs

query \"Fusa Fault\"
  raw == retry
",
    )
    .unwrap();
    let mut lib = Library::new();
    lib.load(&pack).unwrap();
    let r = d.resolved(&lib).unwrap().run(&log(), &signals(), &Progress::new()).unwrap();
    assert_eq!(r.overall().verdict, fact("4"));
    std::fs::remove_dir_all(&dir).ok();
    // Checks are the same check whatever their names' case.
    let mut c = Checks::default();
    c.set(CheckDef::parse_spec("Big One: lines raw == a").unwrap());
    c.set(CheckDef::parse_spec("big one: lines raw == b").unwrap());
    assert_eq!(c.len(), 1);
}

#[test]
fn labels_group_checks() {
    let d = CheckDef::parse_spec("x: lines raw == y; labels ota, network").unwrap();
    assert_eq!(d.labels(), ["ota", "network"]);
    assert_eq!(d.spec(), "x: lines raw == y; exists; labels ota, network");
}

#[test]
fn errors_say_why() {
    let run = |spec: &str, sig: &Signals| {
        CheckDef::parse_spec(spec).unwrap().run(&log(), sig, &Progress::new()).unwrap_err().to_string()
    };
    assert_eq!(run("x: lines raw == retry; per nope", &signals()), "check 'x': no lifecycle 'nope' here");
    let e = run("x: lines {sys.retry}", &signals());
    assert_eq!(e, "check 'x': library query {sys.retry} isn't loaded (load its pack)");
    let e = run("x: lines @nope > 1; count", &signals());
    assert!(e.starts_with("check 'x': unknown signal '@nope'"), "{e}");
}

/// Owner, 2026-11-07: "can checks reference signals?" Its lines and the
/// test's other lines may use the tab's `@signals`, plain or bound to a
/// lifecycle, and find what the same filter finds.
#[test]
fn queries_may_use_signals() {
    use crate::engine::search::tests::run_on;
    let mut sig = signals();
    sig.set(SignalDef::parse_spec(r"t=/temp=(?<t>\d+)/").unwrap());
    sig.set(SignalDef::parse_spec(r"ota(s):enum:per(a):default(idle)=/ota (?<s>\w+)/").unwrap());
    let run = |spec: &str| CheckDef::parse_spec(spec).unwrap().run(&log(), &sig, &Progress::new()).unwrap();
    let rows = |r: &CheckResult| -> Vec<LineNo> {
        let mut v: Vec<LineNo> = r.scopes[0].evidence.iter().flat_map(|e| e.rows.clone()).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let filter = |q: &str| run_on(&log(), &Filter::Query(Query::compile(q).unwrap().with_signals(&sig).unwrap()));
    for q in ["@t > 45", "@t > 45 AND ecu == A", "@ota == idle", "@ota == progress OR @t.event"] {
        let r = run(&format!("x: lines {q}; exists"));
        assert_eq!(rows(&r), filter(q), "{q}");
        let n = filter(q).len().to_string();
        assert_eq!(run(&format!("x: lines {q}; count")).scopes[0].verdict, fact(&n), "{q}");
    }
    // Per lifecycle: the bound signal starts each lifecycle at its default.
    let r = run("x: lines ecu == A AND @ota == idle; count; per a");
    assert_eq!(verdicts(&r)[1..], [("a #1".into(), fact("2")), ("a #2".into(), fact("2"))]);
    // The test's other query too.
    let r = run("x: lines raw ~ /net down/; then @t > 55 within 2s; per a");
    assert_eq!(verdicts(&r)[1..], [("a #1".into(), Verdict::Ok), ("a #2".into(), issue(1))]);
}

#[test]
fn library_queries_are_expanded() {
    let dir = std::env::temp_dir().join(format!("nerka-check-lib-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pack = dir.join("sys.nerka-pack");
    std::fs::write(&pack, "pack sys\n\nquery retry\n  raw == retry\n").unwrap();
    let mut lib = Library::new();
    lib.load(&pack).unwrap();
    let d = CheckDef::parse_spec("n: lines {sys.retry}; count").unwrap();
    assert_eq!(d.unresolved(), ["sys.retry"]);
    let r = d.resolved(&lib).unwrap().run(&log(), &signals(), &Progress::new()).unwrap();
    assert_eq!(r.overall().verdict, fact("4"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn specs_round_trip() {
    for s in [
        "down: lines raw ~ /net down/; exists; per a",
        "n: lines {sys.retry}; count",
        "many: lines raw == retry; count >= 3",
        "storm: lines raw == retry; rate 20 per 10s",
        "quiet: lines apid == NAV; gap 5s; per ecu1",
        "ok: lines {net.down}; then {ota.progress cam=1} within 500ms",
        "stall: lines {net.down}; missing raw ~ /ota/ within 30s; per ecu1",
        r"temp: lines /temp=(?<t>\d+)/; value max",
        "off x: lines raw == y; exists",
        "f: lines raw == y; rate 3 per 1s; fact; per a; labels net",
        "g: lines raw == y; count; issue",
        r"hot: lines /t=(?<t>\d+)/; value max > 90",
        "flow: lines raw == a; then raw == b within 1s; then raw == c within 2s; missing raw == d within 3s",
        "both: lines raw == y; count >= 3; gap 30s; per a",
        "either: lines raw == y; rate 2 per 1s; gap 30s; any",
        "fed: lines @retries.event; rate 2 per 1s",
    ] {
        let d = CheckDef::parse_spec(s).unwrap_or_else(|e| panic!("{s}: {}", e.msg));
        assert_eq!(d.spec(), s);
    }
    let (back, errors) = Checks::parse_text("# c\nn: lines raw == x; count\nbad\n");
    assert_eq!(back.len(), 1);
    assert_eq!(errors, ["line 3: expected NAME: followed by clauses (lines, per, and a test) (at column 1)"]);
}

#[test]
fn spec_errors_say_where() {
    let bad = |s: &str, pos: usize, msg: &str| {
        let e = CheckDef::parse_spec(s).unwrap_err();
        assert_eq!(e.pos, pos, "{s}: {}", e.msg);
        assert!(e.msg.contains(msg), "{s}: {}", e.msg);
    };
    bad("x lines raw == y", 0, "expected NAME:");
    bad(": lines raw == y", 0, "needs a name");
    bad("x: exists", 9, "needs its lines");
    bad("x: lines", 8, "'lines' needs a query");
    bad("x: lines level >= LOUD", 18, "unknown level");
    bad("x: lines raw == y; count ~ 3", 25, "expected >, >=");
    bad("x: lines raw == y; count > many", 27, "expected a number");
    bad("x: lines raw == y; rate many per 1s", 24, "expected a count");
    bad("x: lines raw == y; rate 3 in 1s", 24, "expected 'rate COUNT per DURATION'");
    bad("x: lines raw == y; gap soon", 23, "expected a duration");
    bad("x: lines raw == y; gap 0s", 23, "expected a duration");
    bad("x: lines raw == y; then raw == z", 24, "expected 'then QUERY within DURATION'");
    bad("x: lines raw == y; missing raw == z within ever", 43, "expected a duration");
    bad("x: lines raw == y; value most", 25, "expected first, last, min or max");
    bad("x: lines raw == y; exists; count; fact", 38, "a fact has one test");
    bad("x: lines raw == y; missing raw == z within 1s; then raw == w within 1s", 47, "can't follow a 'missing'");
    bad("x: lines raw == y; per 1a", 23, "'per' needs a lifecycle's name");
    bad("x: lines raw == y; often", 19, "unknown clause 'often'");
    bad("off x: lines", 12, "'lines' needs a query");
}

/// Owner, 2026-11-07: "a sequence of steps". Boots (0, 6), net down (2, 7),
/// ota (5 at 40 s, 8 at 52 s).
#[test]
fn steps_make_a_sequence() {
    // Every `then` step happens: an occurrence, with each step's line.
    let r = check("x: lines raw ~ /boot/; then raw ~ /net down/ within 2s; then raw ~ /ota/ within 2s; per a");
    assert_eq!(verdicts(&r), [("overall".into(), issue(1)), ("a #1".into(), Verdict::Ok), ("a #2".into(), issue(1))]);
    let e = &r.scopes[2].evidence[0];
    assert_eq!((e.rows.as_slice(), e.note.as_str()), (&[6, 7, 8][..], "step 2 after 1.000 s, step 3 after 1.000 s"));
    assert_eq!(r.scopes[2].marks, [8], "known at the last step");
    // `missing` steps are expectations: the issue is where one breaks.
    let r = check("x: lines raw ~ /boot/; missing raw ~ /net down/ within 2s; missing raw ~ /ota/ within 2s; per a");
    assert_eq!(verdicts(&r), [("overall".into(), issue(1)), ("a #1".into(), issue(1)), ("a #2".into(), Verdict::Ok)]);
    let e = &r.scopes[1].evidence[0];
    assert_eq!((e.rows.as_slice(), e.note.as_str()), (&[0, 2][..], "no step 3 within 2.000 s"));
    // What must happen, then what is expected after it.
    let r = check("x: lines raw ~ /net down/; then raw ~ /ota/ within 2s; missing raw ~ /boot/ within 500ms");
    let e = &r.overall().evidence;
    assert_eq!(r.overall().verdict, issue(1));
    assert_eq!(
        (e[0].rows.as_slice(), e[0].note.as_str()),
        (&[7, 8][..], "step 2 after 1.000 s, no step 3 within 500 ms")
    );
    // An expectation the log ends before isn't judged.
    let r = check("x: lines raw ~ /net down/; then raw ~ /ota/ within 2s; missing raw ~ /boot/ within 5s");
    assert_eq!(r.overall().verdict, Verdict::Ok);
    // As a fact: how often.
    let r = check("x: lines raw ~ /boot/; then raw ~ /net down/ within 2s; then raw ~ /ota/ within 2s; fact");
    assert_eq!(r.overall().verdict, fact("1"));
}

/// Owner, 2026-11-07: "several tests in one check". Retries at 0.5, 1.1,
/// 1.2 s (lifecycle a #1) and 53 s (#2).
#[test]
fn several_tests_in_one_check() {
    // All of them (the default): as often as they find one together.
    let r = check("x: lines raw == retry; count >= 3; gap 30s");
    assert_eq!(r.overall().verdict, issue(5), "4 lines >= 3, and one stop");
    assert!(r.overall().evidence.iter().any(|e| e.note == "gap 30s: none for 51.800 s"), "{:?}", r.overall().evidence);
    let r = check("x: lines raw == retry; count >= 3; gap 30s; per a");
    assert_eq!(verdicts(&r)[1..], [("a #1".into(), Verdict::Ok), ("a #2".into(), Verdict::Ok)]);
    // Any of them.
    let r = check("x: lines raw == retry; count >= 3; gap 30s; any; per a");
    assert_eq!(verdicts(&r), [("overall".into(), issue(5)), ("a #1".into(), issue(3)), ("a #2".into(), Verdict::Ok)]);
    assert_eq!(r.scopes[1].marks, [1, 3, 4]);
}

/// Owner, 2026-11-07: checks cascade, "implicit (e.g. a signal with values,
/// boolean)": each check is a signal; other checks and filters use it.
mod as_signals {
    use super::*;
    use crate::engine::search::tests::run_on;

    fn checks(specs: &[&str]) -> Checks {
        let mut c = Checks::default();
        for s in specs {
            c.set(CheckDef::parse_spec(s).unwrap_or_else(|e| panic!("{s}: {}", e.msg)));
        }
        c
    }

    // One source: a check's signal belongs to the source it ran on.
    fn src() -> &'static Src {
        static SRC: std::sync::OnceLock<Src> = std::sync::OnceLock::new();
        SRC.get_or_init(log)
    }

    fn run(c: &Checks, sig: &Signals) -> CheckRun {
        c.run_all(src(), sig, &Library::default(), &Progress::new()).unwrap()
    }

    fn filter(q: &str, sig: &Signals) -> Vec<LineNo> {
        run_on(
            src(),
            &Filter::Query(Query::compile(q).unwrap().with_signals(sig).unwrap_or_else(|e| panic!("{q}: {e}"))),
        )
    }

    fn result<'a>(r: &'a CheckRun, name: &str) -> &'a CheckResult {
        match &r.results.iter().find(|(n, _)| n == name).expect("ran").1 {
            Ok(r) => r,
            Err(e) => panic!("{name}: {e}"),
        }
    }

    #[test]
    fn names() {
        assert_eq!(signal_name("Timeout unanswered"), "timeout_unanswered");
        assert_eq!(signal_name("Fusa Faults (aurix)!"), "fusa_faults_aurix");
        assert_eq!(signal_name("2nd boot"), "check_2nd_boot");
        assert_eq!(signal_name("—"), "check");
    }

    #[test]
    fn a_check_counts_its_occurrences_per_lifecycle() {
        let r = run(&checks(&["Retries: lines raw == retry; per a"]), &signals());
        let sig = &r.signals;
        assert_eq!(filter("@retries.event", sig), [1, 3, 4, 9]);
        // 1, 2, 3 in a #1; 0 again where a #2 starts, 1 at line 9.
        assert_eq!(filter("@retries >= 2", sig), [3, 4, 5]);
        assert_eq!(filter("@retries == 0", sig), [0, 6, 7, 8]);
        assert_eq!(filter("@retries == 1 AND ecu == B", sig), [1, 9]);
    }

    #[test]
    fn a_fact_on_a_value_carries_it() {
        let r = run(&checks(&[r"Temp: lines /temp=(?<t>\d+)/; value max; per a"]), &signals());
        assert_eq!(filter("@temp.event", &r.signals), [5, 8]);
        assert_eq!(filter("@temp > 80", &r.signals), [5]);
    }

    #[test]
    fn checks_feed_checks_in_any_order() {
        // `burst` is listed first and uses `retries`: it runs after it.
        let c = checks(&[
            "burst: lines @retries.event; rate 2 per 1s",
            "retries: lines raw == retry; per a",
            "after first: lines ecu == A AND @retries > 0; count",
        ]);
        let r = run(&c, &signals());
        assert_eq!(result(&r, "burst").overall().verdict, issue(1));
        assert_eq!(result(&r, "after first").overall().verdict, fact("2"), "lines 2 and 5: after a retry in a #1");
        assert_eq!(r.results.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["burst", "retries", "after first"]);
    }

    #[test]
    fn what_cant_run_says_why() {
        let c = checks(&[
            "p: lines @q.event",
            "q: lines @p.event",
            "me: lines @me.event",
            "bad: lines raw == retry; per nope",
            "uses: lines @bad.event",
            "ok: lines raw == retry",
        ]);
        let r = run(&c, &signals());
        let err = |n: &str| r.results.iter().find(|(m, _)| m == n).unwrap().1.clone().unwrap_err();
        assert!(err("p").contains("uses @q, which come back to it"), "{}", err("p"));
        assert!(err("q").contains("uses @p"), "{}", err("q"));
        assert!(err("me").contains("uses @me"), "{}", err("me"));
        assert_eq!(err("uses"), "check 'uses': uses @bad of check 'bad', which failed");
        assert!(result(&r, "ok").overall().verdict == issue(4));
    }

    #[test]
    fn a_signal_of_the_tab_keeps_its_name() {
        let mut sig = signals();
        sig.set(SignalDef::parse_spec(r"retries=/temp=(?<retries>\d+)/").unwrap());
        let c = checks(&["retries: lines raw == retry", "hot: lines @retries > 55; count"]);
        let r = run(&c, &sig);
        assert_eq!(
            result(&r, "hot").overall().verdict,
            fact("3"),
            "the tab's @retries (temps 91, 60, 60), not the check's"
        );
        assert!(!r.signals.get("retries").unwrap().is_check());
        // Said, not silent (owner, 2026-11-07).
        assert_eq!(
            result(&r, "retries").signal_note.as_deref(),
            Some("its signal @retries isn't made: a signal of the tab is named @retries (rename the check)")
        );
        assert_eq!(result(&r, "hot").signal_note, None);
    }

    /// Two checks whose names make the same signal: the first in the list
    /// makes it, the other says so.
    #[test]
    fn two_checks_of_one_signal_name() {
        let c = checks(&[
            "Net down: lines raw ~ /net down/",
            "net_down: lines raw == retry",
            "uses: lines @net_down.event",
        ]);
        let r = run(&c, &signals());
        assert_eq!(result(&r, "Net down").signal_note, None);
        assert_eq!(
            result(&r, "net_down").signal_note.as_deref(),
            Some("its signal @net_down isn't made: check 'Net down' makes @net_down already (rename the check)")
        );
        assert_eq!(filter("@net_down.event", &r.signals), [2, 7]);
        assert_eq!(result(&r, "uses").overall().verdict, issue(2));
    }

    #[test]
    fn steps_and_several_tests_as_signals() {
        let c = checks(&[
            "flow: lines raw ~ /boot/; then raw ~ /net down/ within 2s; then raw ~ /ota/ within 2s; per a",
            "both: lines raw == retry; count >= 3; gap 30s; any; per a",
        ]);
        let r = run(&c, &signals());
        assert_eq!(filter("@flow.event", &r.signals), [8]);
        assert_eq!(filter("@both.event", &r.signals), [1, 3, 4]);
    }
}

/// Every test as a fact and as an issue, on a random log: the two agree in
/// every scope (owner, 2026-11-07: "how many" as an issue didn't match
/// "how many" as a fact).
mod consistency {
    use super::*;

    type RandRow = (&'static str, f64, f64, String);

    struct Rand(Vec<RandRow>);

    impl LogSource for Rand {
        fn name(&self) -> &str {
            "rand"
        }
        fn line_count(&self) -> LineNo {
            self.0.len() as LineNo
        }
        fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
            self.0.get(line as usize).map(|r| Cow::Borrowed(r.3.as_bytes()))
        }
        fn schema(&self) -> &[FieldDef] {
            SCHEMA
        }
        fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
            let row = self.0.get(line as usize)?;
            (id == 0).then(|| FieldValue::Str(row.0.as_bytes().into()))
        }
        fn time(&self, line: LineNo) -> Option<f64> {
            Some(self.0.get(line as usize)?.1)
        }
        fn uptime(&self, line: LineNo) -> Option<f64> {
            Some(self.0.get(line as usize)?.2)
        }
    }

    /// ECU A rebooting every ~2000 lines; "err" lines in bursts, "req" /
    /// "ack" pairs (some acks late or missing), `v=` values, gaps.
    fn log(seed: u64) -> Rand {
        let mut r = seed;
        let mut next = move || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r
        };
        let (mut t, mut boot) = (0.0, 0.0);
        let mut rows = Vec::new();
        for i in 0..20_000 {
            t += (next() % 50) as f64 / 1000.0;
            if next() % 3000 == 0 {
                t += 5.0;
            }
            if i % 2000 == 1999 {
                boot = t;
            }
            let ecu = if next() % 2 == 0 { "A" } else { "B" };
            let text = match next() % 30 {
                0..=2 => "err".to_owned(),
                3 => "req".to_owned(),
                4 => "ack".to_owned(),
                5 => format!("v={}", next() % 100),
                _ => "x".to_owned(),
            };
            rows.push((ecu, t, t - boot, text));
        }
        Rand(rows)
    }

    fn run(src: &Rand, spec: &str) -> CheckResult {
        let mut sig = Signals::default();
        sig.set(SignalDef::parse_spec("lifecycle a: lines ecu == A; reset uptime").unwrap());
        CheckDef::parse_spec(spec)
            .unwrap_or_else(|e| panic!("{spec}: {}", e.msg))
            .run(src, &sig, &Progress::new())
            .unwrap()
    }

    fn num(v: &Verdict) -> f64 {
        match v {
            Verdict::Fact(Some(s)) => s.trim_end_matches(" s").parse().unwrap_or_else(|_| panic!("{s}")),
            Verdict::Fact(None) => 0.0,
            v => panic!("not a fact: {v:?}"),
        }
    }

    fn times(v: &Verdict) -> u64 {
        match v {
            Verdict::Ok => 0,
            Verdict::Issue(n) => *n,
            v => panic!("not an issue: {v:?}"),
        }
    }

    #[test]
    fn facts_and_issues_agree() {
        for seed in [3, 77, 0xfeed] {
            let src = log(seed);
            let both = |test: &str| {
                let f = run(&src, &format!("f: lines raw == err; {test}; fact; per a"));
                let i = run(&src, &format!("i: lines raw == err; {test}; issue; per a"));
                assert_eq!(f.scopes.len(), i.scopes.len());
                assert!(f.scopes.len() > 5, "lifecycles: {}", f.scopes.len());
                f.scopes.into_iter().zip(i.scopes).map(|(a, b)| (a.verdict, b)).collect::<Vec<_>>()
            };
            // Shows up / how many: the issue happens as often as the fact counts,
            // each line its own evidence.
            for test in ["exists", "count"] {
                for (f, i) in both(test) {
                    assert_eq!(times(&i.verdict), num(&f) as u64, "seed {seed} {test} {}", i.scope.label);
                    assert!(i.evidence.iter().all(|e| e.rows.len() == 1));
                }
            }
            // A condition on the count.
            let f = run(&src, "f: lines raw == err; count; per a");
            let i = run(&src, "i: lines raw == err; count >= 60; per a");
            for (f, i) in f.scopes.iter().zip(&i.scopes) {
                let n = num(&f.verdict);
                assert_eq!(times(&i.verdict), if n >= 60.0 { n as u64 } else { 0 }, "seed {seed} {}", i.scope.label);
            }
            // Too many at once: an issue exactly when the most in a window is above it.
            for (f, i) in both("rate 4 per 200ms") {
                assert_eq!(times(&i.verdict) > 0, num(&f) > 4.0, "seed {seed} rate {}", i.scope.label);
            }
            // Stops: an issue exactly when the longest stop is longer.
            for (f, i) in both("gap 1s") {
                let longest = match &f {
                    Verdict::Fact(Some(s)) => s.trim_end_matches(" s").parse::<f64>().unwrap_or(0.0),
                    _ => 0.0,
                };
                assert_eq!(times(&i.verdict) > 0, longest > 1.0, "seed {seed} gap {} ({f:?})", i.scope.label);
            }
            // Followed or not: the fact counts the issue's occurrences.
            for test in ["then raw == ack within 100ms", "missing raw == ack within 100ms"] {
                let f = run(&src, &format!("f: lines raw == req; {test}; fact; per a"));
                let i = run(&src, &format!("i: lines raw == req; {test}; per a"));
                for (f, i) in f.scopes.iter().zip(&i.scopes) {
                    assert_eq!(times(&i.verdict), num(&f.verdict) as u64, "seed {seed} {test} {}", i.scope.label);
                }
            }
            // A value: an issue exactly when the condition holds on it.
            let f = run(&src, r"f: lines /v=(?<v>\d+)/; value max; per a");
            let i = run(&src, r"i: lines /v=(?<v>\d+)/; value max > 97; per a");
            for (f, i) in f.scopes.iter().zip(&i.scopes) {
                assert_eq!(times(&i.verdict) > 0, num(&f.verdict) > 97.0, "seed {seed} value {}", i.scope.label);
            }
        }
    }

    /// Step sequences against a plain walk: for each start line, each step
    /// is the first line of its kind after the step before, within its
    /// time; the sequence counts when every `then` step happens and (with
    /// `missing` steps) one of those doesn't, the scope lasting long enough.
    #[test]
    fn steps_agree_with_a_plain_walk() {
        let kinds = ["err", "req", "ack", "x"];
        for seed in [5u64, 99, 0xbeef, 4242] {
            let src = log(seed);
            let n = src.0.len();
            let end = src.0[n - 1].1;
            let mut r = seed;
            let mut next = move || {
                r ^= r << 13;
                r ^= r >> 7;
                r ^= r << 17;
                r
            };
            let mut some = 0;
            for _ in 0..12 {
                // 1 to 3 steps: `then` ones first, then `missing` ones.
                let k = 1 + (next() % 3) as usize;
                let thens = (next() % (k as u64 + 1)) as usize;
                let steps: Vec<(&str, f64, bool)> = (0..k)
                    .map(|i| (kinds[(next() % 4) as usize], [0.05, 0.2, 1.0][(next() % 3) as usize], i >= thens))
                    .collect();
                let start = kinds[(next() % 3) as usize];
                let spec = format!(
                    "s: lines raw == {start}; {}",
                    steps
                        .iter()
                        .map(|(q, w, m)| format!(
                            "{} raw == {q} within {}ms",
                            if *m { "missing" } else { "then" },
                            w * 1000.0
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                );
                // The walk, over the whole log.
                let mut want = 0;
                for i in 0..n {
                    if src.0[i].3 != start {
                        continue;
                    }
                    let (mut cur, mut t) = (i, src.0[i].1);
                    let mut found = None;
                    for &(q, w, missing) in &steps {
                        let next_one = (cur + 1..n).find(|&j| src.0[j].3 == q).filter(|&j| src.0[j].1 - t <= w);
                        match (missing, next_one) {
                            (_, Some(j)) => (cur, t) = (j, src.0[j].1),
                            (false, None) => {
                                found = Some(false);
                                break;
                            }
                            (true, None) => {
                                found = Some(end - t >= w);
                                break;
                            }
                        }
                    }
                    if found.unwrap_or(!steps.iter().any(|s| s.2)) {
                        want += 1;
                    }
                }
                let got = run(&src, &spec).overall().verdict.clone();
                assert_eq!(times(&got), want, "seed {seed}: {spec}");
                some += usize::from(want > 0);
            }
            assert!(some > 0, "seed {seed}: no sequence found anything");
        }
    }
}

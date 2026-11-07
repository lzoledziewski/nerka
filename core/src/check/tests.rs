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
    let r = check("many: lines raw == retry; count >= 3; per a");
    assert_eq!(verdicts(&r), [("overall".into(), issue(1)), ("a #1".into(), issue(1)), ("a #2".into(), Verdict::Ok)]);
}

#[test]
fn rates_find_bursts() {
    // Three retries within 1 s: one burst.
    let r = check("storm: lines raw == retry; rate 2 per 1s");
    assert_eq!(verdicts(&r), [("overall".into(), issue(1))]);
    assert_eq!(r.overall().evidence[0].rows, [1, 4]);
    assert_eq!(check("storm: lines raw == retry; rate 3 per 1s").overall().verdict, Verdict::Ok);
}

#[test]
fn gaps_between_its_lines() {
    let r = check("quiet: lines ecu == A; gap 30s; per a");
    // 1 s -> 40 s in the first lifecycle; the second is busy.
    assert_eq!(verdicts(&r), [("overall".into(), issue(1)), ("a #1".into(), issue(1)), ("a #2".into(), Verdict::Ok)]);
    assert_eq!(r.overall().evidence[0].rows, [2, 5]);
    assert_eq!(r.overall().evidence[0].note, "none for 39.0 s");
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
fn errors_say_why() {
    let run = |spec: &str, sig: &Signals| {
        CheckDef::parse_spec(spec).unwrap().run(&log(), sig, &Progress::new()).unwrap_err().to_string()
    };
    assert_eq!(run("x: lines raw == retry; per nope", &signals()), "check 'x': no lifecycle 'nope' here");
    let e = run("x: lines {sys.retry}", &signals());
    assert_eq!(e, "check 'x': library query {sys.retry} isn't loaded (load its pack)");
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
    bad("1x: lines raw == y", 0, "isn't a name");
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
    bad("x: lines raw == y; exists; count", 27, "a check has one test");
    bad("x: lines raw == y; per 1a", 23, "'per' needs a lifecycle's name");
    bad("x: lines raw == y; often", 19, "unknown clause 'often'");
    bad("off x: lines", 12, "'lines' needs a query");
}

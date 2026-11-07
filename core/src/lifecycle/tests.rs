use super::*;
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue};
use crate::signal::SignalDef;
use std::borrow::Cow;

const SCHEMA: &[FieldDef] = &[FieldDef::new("ecu", FieldKind::Str, false)];

/// A row: (ecu, time, uptime, text).
type Row = (Option<&'static str>, Option<f64>, Option<f64>, &'static str);

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
        (id == 0).then(|| self.0.get(line as usize)?.0.map(|e| FieldValue::Str(e.as_bytes().into())))?
    }
    fn time(&self, line: LineNo) -> Option<f64> {
        self.0.get(line as usize)?.1
    }
    fn uptime(&self, line: LineNo) -> Option<f64> {
        self.0.get(line as usize)?.2
    }
}

fn def(spec: &str) -> LifecycleDef {
    LifecycleDef::parse_spec(spec).unwrap_or_else(|e| panic!("{spec}: {} at {}", e.msg, e.pos))
}

fn run(spec: &str, src: &Src) -> Arc<Lifecycles> {
    def(spec).linked(&Signals::default()).lifecycles(src, &Progress::new()).unwrap()
}

fn run_err(spec: &str, src: &Src) -> String {
    def(spec).linked(&Signals::default()).lifecycles(src, &Progress::new()).unwrap_err().to_string()
}

/// (number, first, outcome) of each lifecycle.
fn lives(l: &Lifecycles) -> Vec<(u32, LineNo, &'static str)> {
    l.lives.iter().map(|x| (x.number, x.first, x.outcome.as_str())).collect()
}

fn findings(l: &Lifecycles) -> Vec<(&'static str, LineNo)> {
    l.findings.iter().map(|f| (f.kind.as_str(), f.row)).collect()
}

/// Two ECUs: A reboots without a shutdown, B shuts down cleanly, then
/// boots again; times in seconds, uptimes from each ECU's boot.
fn two_ecus() -> Src {
    Src(vec![
        (Some("A"), Some(100.0), Some(1.0), "A boot"),
        (Some("B"), Some(100.5), Some(0.5), "B boot"),
        (Some("A"), Some(110.0), Some(11.0), "A running"),
        (Some("B"), Some(120.0), Some(20.0), "B shutdown requested"),
        (Some("A"), Some(130.0), Some(0.2), "A boot"),
        (None, Some(131.0), None, "no ecu"),
        (Some("B"), Some(140.0), Some(0.3), "B boot"),
        (Some("A"), Some(150.0), Some(20.2), "A running"),
    ])
}

#[test]
fn each_entity_has_its_lifecycles_from_its_own_lines() {
    let src = two_ecus();
    let a = run("a: lines ecu == A; reset uptime; planned raw ~ /shutdown/", &src);
    assert_eq!(lives(&a), [(1, 0, "unclean"), (2, 4, "open")]);
    assert_eq!(a.lives[1].started_by, Start::Reset);
    assert_eq!(a.lives[0].boot, Some(99.0));
    assert_eq!(a.lives[1].lines, 2);
    assert!(a.findings.is_empty(), "{:?}", a.findings);
    // B's own rules: the same uptime, its own marker.
    let b = run("b: lines ecu == B; reset uptime; planned raw ~ /shutdown/", &src);
    assert_eq!(lives(&b), [(1, 1, "planned"), (2, 6, "open")]);
    assert_eq!(b.lives[0].marker, Some(3));
    // A lifecycle lasts until the next one starts: every line in between
    // is in it, whoever wrote it; before its first line, none.
    assert_eq!(a.at(5).map(|(_, l)| l.number), Some(2));
    assert_eq!(b.at(4).map(|(_, l)| l.number), Some(1));
    assert_eq!(b.at(0), None);
    assert_eq!(a.summary(), "2 lifecycles, 1 unclean");
}

#[test]
fn the_uptime_of_all_lines_mixes_writers() {
    // Without `lines`, A's and B's uptimes interleave: B's small one after
    // A's large one looks like a reset. Its lines say whose uptime it is.
    let src = Src(vec![
        (Some("A"), None, Some(100.0), "a"),
        (Some("B"), None, Some(30.0), "b"),
        (Some("A"), None, Some(101.0), "a"),
        (Some("B"), None, Some(31.0), "b"),
    ]);
    assert_eq!(run("all: reset uptime", &src).lives.len(), 3);
    assert_eq!(run("a: lines ecu == A; reset uptime", &src).lives.len(), 1);
}

#[test]
fn a_query_starts_lifecycles_and_the_uptime_validates_them() {
    let src = Src(vec![
        (Some("A"), Some(100.0), Some(1.0), "boot"),
        (Some("A"), Some(105.0), Some(6.0), "x"),
        (Some("A"), Some(110.0), Some(11.0), "boot again?"),
        (Some("A"), Some(130.0), Some(0.5), "y"),
        (Some("A"), Some(140.0), Some(10.5), "z"),
    ]);
    let l = run("a: start raw ~ /^boot/", &src);
    assert_eq!(lives(&l), [(1, 0, "unclean"), (2, 2, "open")]);
    // Line 2 started one while the uptime kept counting; line 3's uptime
    // went back without a start.
    assert_eq!(findings(&l), [("false reset", 2), ("missed reset", 3)]);
}

#[test]
fn start_and_markers_count_on_its_lines_only() {
    // B's "boot" doesn't start one of A's.
    let l = run("a: lines ecu == A; start raw ~ /boot/", &two_ecus());
    assert_eq!(lives(&l), [(1, 0, "unclean"), (2, 4, "open")]);
}

#[test]
fn small_steps_back_are_within_the_tolerance() {
    let src = Src(vec![
        (Some("A"), None, Some(10.0), "a"),
        (Some("A"), None, Some(8.0), "b"),
        (Some("A"), None, Some(12.0), "c"),
        (Some("A"), None, Some(1.0), "d"),
    ]);
    assert_eq!(run("b: reset uptime", &src).lives.len(), 2);
    assert_eq!(run("b: reset uptime tolerance 1s", &src).lives.len(), 3);
}

#[test]
fn a_gap_ends_a_lifecycle_as_lost() {
    let src = Src(vec![
        (None, Some(0.0), None, "a"),
        (None, Some(10.0), None, "b"),
        (None, Some(100.0), None, "c"),
        (None, Some(101.0), None, "crash: watchdog"),
        (None, Some(200.0), None, "d"),
    ]);
    let l = run("run: gap 30s; crash raw ~ /crash/", &src);
    assert_eq!(lives(&l), [(1, 0, "lost"), (2, 2, "crash"), (3, 4, "open")]);
    assert_eq!(l.lives[0].duration(), Some(10.0));
}

#[test]
fn a_late_first_line_is_noted() {
    let src = Src(vec![(Some("A"), Some(100.0), Some(38.0), "a"), (Some("A"), Some(101.0), Some(0.1), "b")]);
    let l = run("a: reset uptime", &src);
    assert_eq!(findings(&l), [("late start", 0)]);
    assert!(l.findings[0].detail.contains("or the log started during it"));
    assert!(run("a: reset uptime; late 60s", &src).findings.is_empty());
}

#[test]
fn a_defined_signal_can_be_the_reset() {
    // Plain text: the uptime from a dmesg-like prefix.
    let src = Src(vec![
        (None, None, None, "[    1.0] a"),
        (None, None, None, "[   50.0] b"),
        (None, None, None, "no prefix"),
        (None, None, None, "[    0.5] c"),
    ]);
    let mut signals = Signals::default();
    signals.set(SignalDef::parse_spec(r"up=/^\[\s*(?<up>[\d.]+)\]/").unwrap());
    let l = def("boot: reset @up").linked(&signals).lifecycles(&src, &Progress::new()).unwrap();
    assert_eq!(lives(&l), [(1, 0, "unclean"), (2, 3, "open")]);
    // Without the signal: an error that says so.
    assert_eq!(run_err("boot: reset @up", &src), "lifecycle 'boot': no signal @up (define it, or reset on uptime)");
    // Lines without an uptime: resetting on it is an error too.
    let e = run_err("boot: reset uptime", &src);
    assert!(e.contains("its lines carry no uptime"), "{e}");
}

#[test]
fn library_queries_are_kept_by_name_and_expanded() {
    let dir = std::env::temp_dir().join(format!("nerka-lifecycle-lib-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pack = dir.join("sys.nerka-pack");
    std::fs::write(&pack, "pack sys\n\nquery shutdown\n  raw ~ /shutdown/\n\nquery of(ecu)\n  ecu == \"$ecu\"\n")
        .unwrap();
    let mut lib = Library::new();
    lib.load(&pack).unwrap();
    let d = def("b: lines {sys.of ecu=B}; reset uptime; planned {sys.shutdown}");
    // Kept by name: the spec says so, and it isn't usable before resolved.
    assert_eq!(d.spec(), "b: lines {sys.of ecu=B}; reset uptime; planned {sys.shutdown}");
    assert_eq!(d.unresolved(), ["sys.of", "sys.shutdown"]);
    let e = d.lifecycles(&two_ecus(), &Progress::new()).unwrap_err().to_string();
    assert!(e.contains("library query sys.of isn't loaded"), "{e}");
    let r = d.resolved(&lib).unwrap();
    assert!(r.unresolved().is_empty());
    assert_eq!(r.planned().and_then(Rule::query).map(|q| q.source()), Some("(raw ~ /shutdown/)"));
    let l = r.lifecycles(&two_ecus(), &Progress::new()).unwrap();
    assert_eq!(lives(&l), [(1, 1, "planned"), (2, 6, "open")]);
    let e = def("b: reset uptime; planned {sys.nope}").resolved(&lib).unwrap_err();
    assert!(e.contains("sys.nope"), "{e}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn results_are_cached_per_source() {
    let d = def("a: reset uptime");
    let src = two_ecus();
    assert!(d.cached(&src).is_none());
    let a = d.lifecycles(&src, &Progress::new()).unwrap();
    let b = d.clone().lifecycles(&src, &Progress::new()).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
}

#[test]
fn specs_round_trip() {
    for s in [
        "a: lines ecu == A; reset uptime; planned raw ~ /shutdown/",
        "run: start apid == SYS; gap 30s; crash raw ~ /crash/; late 1m",
        "b: reset @up tolerance 500ms",
        "b: lines {sys.of ecu=B}; start {sys.boot}; reset uptime tolerance 2s; planned raw ~ /bye/; late 20s",
    ] {
        assert_eq!(def(s).spec(), s.replace("1m", "60s"));
    }
}

#[test]
fn spec_errors_say_where() {
    let bad = |s: &str, pos: usize, msg: &str| {
        let e = LifecycleDef::parse_spec(s).unwrap_err();
        assert_eq!(e.pos, pos, "{s}: {}", e.msg);
        assert!(e.msg.contains(msg), "{s}: {}", e.msg);
    };
    bad("boot reset uptime", 0, "expected NAME:");
    bad("1boot: gap 1s", 0, "isn't a name");
    bad("boot: gap 1s; restart x", 14, "unknown clause 'restart'");
    bad("boot: gap soon", 10, "needs a duration");
    bad("boot: reset tmsp", 12, "needs uptime or a signal");
    bad("boot: reset uptime tolerance x", 29, "expected a duration");
    bad("boot: reset uptime later", 12, "expected 'reset uptime'");
    bad("boot: start", 11, "'start' needs a query");
    bad("boot: start level >= LOUD", 21, "unknown level");
    bad("boot: planned raw ~ /x/", 23, "needs a start, reset or gap");
    bad("boot: gap 1s; planned {sys.x", 28, "expected '}'");
    bad("boot: gap 1s; planned {shutdown}", 23, "expected a library query");
    bad("boot: gap 1s; planned {sys.x cam}", 29, "expected NAME=VALUE");
}

#[test]
fn durations() {
    assert_eq!(parse_duration("30s"), Some(30.0));
    assert_eq!(parse_duration("30"), Some(30.0));
    assert_eq!(parse_duration("500ms"), Some(0.5));
    assert_eq!(parse_duration("2m"), Some(120.0));
    assert_eq!(parse_duration("1h"), Some(3600.0));
    assert_eq!(parse_duration("1.5 s"), Some(1.5));
    assert_eq!(parse_duration("-1s"), None);
    assert_eq!(parse_duration("5d"), None);
}

mod signals {
    use super::*;
    use crate::engine::search::tests::run_on;
    use crate::input::concat::Concat;
    use crate::signal::SignalError;
    use crate::{Filter, TextFile};

    /// A's OTA job runs over A's reboot.
    fn ota() -> Src {
        Src(vec![
            (Some("A"), Some(100.0), Some(1.0), "boot"),
            (Some("B"), Some(100.5), Some(0.5), "boot"),
            (Some("A"), Some(111.0), Some(12.0), "OTA state=downloading"),
            (Some("B"), Some(101.5), Some(1.5), "OTA state=installing"),
            (Some("A"), Some(130.0), Some(0.2), "boot"),
            (Some("A"), Some(131.0), Some(1.2), "x"),
            (Some("B"), Some(132.0), Some(32.0), "y"),
            (Some("A"), Some(133.0), Some(3.2), "OTA state=done"),
        ])
    }

    fn signals(specs: &[&str]) -> Signals {
        let mut s = Signals::default();
        for spec in specs {
            s.set(SignalDef::parse_spec(spec).unwrap_or_else(|e| panic!("{spec}: {e}")));
        }
        s
    }

    fn filter(nfd: &str, sig: &Signals) -> Filter {
        Filter::Query(Query::compile(nfd).unwrap().with_signals(sig).unwrap_or_else(|e| panic!("{nfd}: {e}")))
    }

    const ECU_A: &str = "lifecycle a: lines ecu == A; reset uptime";
    /// A's OTA state: its query picks A's lines, it resets with A.
    const OTA: &str = r"ota(s):enum:per(a):default(idle)=ecu == A AND /OTA state=(?<s>\w+)/";

    #[test]
    fn a_bound_signal_resets_when_its_lifecycle_starts() {
        let src = ota();
        let sig = signals(&[ECU_A, OTA]);
        let d = sig.get("ota").unwrap();
        let series = d.series(&src, &Progress::new()).unwrap();
        let state: Vec<String> = (0..8)
            .map(|r| match series.state_at(r) {
                Some((Some(_), v)) => d.format(v),
                Some((None, v)) => format!("{} (default)", d.format(v)),
                None => "-".into(),
            })
            .collect();
        assert_eq!(
            state,
            [
                "idle (default)",
                "idle (default)",
                "downloading",
                "downloading",
                // A rebooted: the default again, on every line from there
                // (the lifecycle is a time span, B's lines are in it too).
                "idle (default)",
                "idle (default)",
                "idle (default)",
                "done",
            ]
        );
        // The same through filters, fast and naive paths agreeing.
        assert_eq!(run_on(&src, &filter("@ota == idle", &sig)), [0, 1, 4, 5, 6]);
        // A default isn't an event; the first event changes it only if it differs.
        assert_eq!(run_on(&src, &filter("@ota.event", &sig)), [2, 7]);
        assert_eq!(run_on(&src, &filter("@ota.changed", &sig)), [2, 7]);
        // The lifecycle itself: its number, events where they start.
        assert_eq!(run_on(&src, &filter("@a == 2", &sig)), [4, 5, 6, 7]);
        assert_eq!(run_on(&src, &filter("@a.event", &sig)), [0, 4]);
        assert_eq!(run_on(&src, &filter("@a == 1 AND ecu == B", &sig)), [1, 3]);
    }

    #[test]
    fn a_signal_resets_with_another_entitys_lifecycle() {
        // B's state reset when A (say, the gateway) reboots: allowed.
        let src = ota();
        let sig = signals(&[ECU_A, r"b:enum:per(a)=ecu == B AND /OTA state=(?<b>\w+)/"]);
        assert_eq!(run_on(&src, &filter("@b == installing", &sig)), [3]);
    }

    #[test]
    fn changed_compares_with_the_default() {
        let src = Src(vec![
            (Some("A"), None, Some(1.0), "s=idle"),
            (Some("A"), None, Some(12.0), "s=busy"),
            (Some("A"), None, Some(0.1), "s=busy"),
        ]);
        let sig = signals(&[ECU_A, r"s:enum:per(a):default(idle)=/s=(?<s>\w+)/"]);
        // Line 0 sets what the default said; line 2 is the first after a reboot.
        assert_eq!(run_on(&src, &filter("@s.changed", &sig)), [1, 2]);
    }

    #[test]
    fn unbound_defaults_apply_from_the_start_of_each_file() {
        let src = Concat::new(vec![
            Box::new(TextFile::from_bytes("a", b"x\nn=5\ny\n".to_vec())) as Box<dyn LogSource>,
            Box::new(TextFile::from_bytes("b", b"z\nn=7\n".to_vec())),
        ])
        .unwrap();
        let sig = signals(&[r"n:default(0)=/n=(?<n>\d+)/"]);
        assert_eq!(run_on(&src, &filter("@n == 0", &sig)), [0, 3]);
        assert_eq!(run_on(&src, &filter("@n > 0", &sig)), [1, 2, 4]);
    }

    #[test]
    fn a_lifecycle_spans_files() {
        // Log rotation: one boot over two files keeps its state; a new boot
        // in the second file resets it.
        let src = Concat::new(vec![
            Box::new(TextFile::from_bytes("a", b"boot\nmode=fast\nx\n".to_vec())) as Box<dyn LogSource>,
            Box::new(TextFile::from_bytes("b", b"y\nboot\nz\n".to_vec())),
        ])
        .unwrap();
        let sig = signals(&["lifecycle run: start raw == boot", r"mode:enum:per(run)=/mode=(?<mode>\w+)/"]);
        assert_eq!(run_on(&src, &filter("@mode == fast", &sig)), [1, 2, 3]);
        assert_eq!(run_on(&src, &filter("@run == 2", &sig)), [4, 5]);
    }

    #[test]
    fn specs_with_lifecycles_round_trip() {
        let sig = signals(&[ECU_A, OTA, r"n:num:default(-1.5)=/n=(?<n>\S+)/"]);
        let (back, errors) = Signals::parse_text(&sig.to_text());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            back.iter().map(|d| d.spec()).collect::<Vec<_>>(),
            [ECU_A, OTA, r"n:num:default(-1.5)=/n=(?<n>\S+)/"]
        );
        assert_eq!(back.lifecycles().count(), 1);
        assert!(back.get("ota").unwrap().is_linked());
    }

    #[test]
    fn spec_errors() {
        let e = |s: &str| SignalDef::parse_spec(s).unwrap_err().to_string();
        assert_eq!(e(r"n:default(x)=/n=(?<n>\d)/"), "default 'x' isn't a number");
        assert_eq!(e(r"t:time:default(1)=/t=(?<t>\S+)/"), "a time signal takes no default");
        assert_eq!(e(r"n:per(1x)=/n=(?<n>\d)/"), "per(1x): not a lifecycle's name");
        let gap = e("lifecycle boot: gap soon");
        assert!(gap.starts_with("lifecycle: 'gap' needs a duration"), "{gap}");
        assert!(
            matches!(SignalDef::parse_spec("x:sometimes=/(?<x>.)/"), Err(SignalError::Spec(m)) if m.contains("per(LIFECYCLE)"))
        );
        // Bound to a lifecycle that isn't defined: an error where it's used.
        let mut sig = signals(&[OTA]);
        let err = Query::compile("@ota == idle").unwrap().with_signals(&sig).unwrap_err();
        assert_eq!(err.msg, "'@ota' resets per lifecycle 'a', which isn't defined");
        // Defining it later links it.
        sig.set(SignalDef::parse_spec(ECU_A).unwrap());
        assert!(Query::compile("@ota == idle").unwrap().with_signals(&sig).is_ok());
    }

    #[test]
    fn switched_off_definitions_are_kept_but_not_used() {
        let sig = signals(&["off lifecycle a: lines ecu == A; reset uptime", r"off n:num=/n=(?<n>\d+)/", OTA]);
        // Saved as they are, off.
        let (back, errors) = Signals::parse_text(&sig.to_text());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(back.get("a").unwrap().spec(), "off lifecycle a: lines ecu == A; reset uptime");
        assert_eq!(back.get("n").unwrap().spec(), r"off n:num=/n=(?<n>\d+)/");
        // Only those on are used: a signal bound to an off lifecycle isn't linked.
        let on = sig.on();
        assert_eq!(on.iter().map(|d| d.name()).collect::<Vec<_>>(), ["ota"]);
        assert!(Query::compile("@n > 1").unwrap().with_signals(&on).is_err());
        let err = Query::compile("@ota == idle").unwrap().with_signals(&on).unwrap_err();
        assert!(err.msg.contains("which isn't defined"), "{}", err.msg);
    }

    #[test]
    fn why_no_lifecycles_were_found_is_kept() {
        let d = def("a: reset @nope").linked(&Signals::default());
        let src = ota();
        assert!(d.done(&src).is_none());
        assert!(d.lifecycles(&src, &Progress::new()).is_err());
        assert_eq!(
            d.done(&src).unwrap().unwrap_err(),
            "lifecycle 'a': no signal @nope (define it, or reset on uptime)"
        );
        assert!(d.cached(&src).is_none());
    }

    #[test]
    fn there_is_no_built_in_uptime_signal() {
        // Whose uptime would it be? Lifecycles read it from their own lines.
        let sig = Signals::default().for_source(&ota());
        assert_eq!(sig.iter().map(|d| d.name()).collect::<Vec<_>>(), ["time"]);
    }
}

/// Random logs against a plain reference: the lifecycles found (chunked,
/// parallel, through the engine's scans) are the ones a line-by-line walk
/// finds, and a bound signal's state is the last event since its
/// lifecycle started.
mod stress {
    use super::*;
    use crate::engine::search::tests::run_on;
    use crate::{Event, Filter};

    type RandRow = (Option<&'static str>, Option<f64>, Option<f64>, String);

    /// Rows: (ecu, time, uptime, text), the text owned.
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
            (id == 0).then(|| self.0.get(line as usize)?.0.map(|e| FieldValue::Str(e.as_bytes().into())))?
        }
        fn time(&self, line: LineNo) -> Option<f64> {
            self.0.get(line as usize)?.1
        }
        fn uptime(&self, line: LineNo) -> Option<f64> {
            self.0.get(line as usize)?.2
        }
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Two ECUs rebooting now and then (some after "shutdown", some with
    /// "panic"), jitter in the uptimes, gaps in time, lines without an ECU,
    /// a time or an uptime, "boot" lines, and `s=` values.
    fn log(seed: u64, n: usize) -> Rand {
        let mut r = Rng(seed);
        let (mut t, mut boot) = (1000.0, [990.0, 980.0]);
        let mut rows = Vec::with_capacity(n);
        for _ in 0..n {
            t += r.below(100) as f64 / 1000.0;
            if r.below(20_000) == 0 {
                t += 120.0;
            }
            let e = r.below(2) as usize;
            let ecu = if r.below(50) == 0 { None } else { Some(["A", "B"][e]) };
            let mut text = match r.below(40) {
                0 => "boot".to_owned(),
                1 => format!("s={}", ["idle", "busy", "done"][r.below(3) as usize]),
                2..=4 => format!("s={}", ["idle", "busy"][r.below(2) as usize]),
                _ => format!("noise {}", r.below(1000)),
            };
            if r.below(3000) == 0 {
                boot[e] = t;
                text = if r.below(2) == 0 { "shutdown".into() } else { "panic".into() };
            }
            // Writers' clocks jitter: an uptime a little behind now and then.
            let jitter = if r.below(10) == 0 { r.below(3000) as f64 / 1000.0 } else { 0.0 };
            let up = (r.below(30) != 0).then_some((t - boot[e] - jitter).max(0.0));
            let time = (r.below(100) != 0).then_some(t);
            rows.push((ecu, time, up, text));
        }
        Rand(rows)
    }

    type RefLife = (u32, LineNo, LineNo, u64, &'static str);

    /// The lifecycles a plain walk finds: (number, first, last, lines, outcome).
    fn reference(src: &Rand, d: &LifecycleDef) -> Vec<RefLife> {
        let matches = |q: Option<&Rule>, row: LineNo| {
            q.and_then(Rule::query)
                .is_some_and(|q| q.clone().bind(SCHEMA).unwrap().matches_event(&mut Event::from_source(src, row)))
        };
        let tol = d.reset().map_or(DEFAULT_TOLERANCE, |r| r.tolerance);
        let mut out: Vec<RefLife> = Vec::new();
        let (mut last_up, mut last_t): (Option<f64>, Option<f64>) = (None, None);
        let mut marker: Option<&'static str> = None;
        for row in 0..src.line_count() {
            if d.lines().is_some() && !matches(d.lines(), row) {
                continue;
            }
            let (t, up) = (src.time(row), src.uptime(row));
            let back = d.reset().is_some() && matches!((up, last_up), (Some(u), Some(p)) if u < p - tol);
            let gap = matches!((d.gap(), t, last_t), (Some(g), Some(t), Some(p)) if t - p > g);
            let by_start = matches(d.start(), row);
            if out.is_empty() || by_start || back || gap {
                if let Some(l) = out.last_mut() {
                    let ended = if !by_start && !back && gap { "lost" } else { "unclean" };
                    l.4 = marker.unwrap_or(ended);
                }
                out.push((out.len() as u32 + 1, row, row, 0, "open"));
                (last_up, last_t, marker) = (None, None, None);
            }
            let l = out.last_mut().unwrap();
            l.2 = row;
            l.3 += 1;
            if up.is_some() {
                last_up = up;
            }
            if t.is_some() {
                last_t = t;
            }
            if matches(d.planned(), row) {
                marker = Some("planned");
            }
            if matches(d.crash(), row) {
                marker = Some("crash");
            }
        }
        if let (Some(l), Some(m)) = (out.last_mut(), marker) {
            l.4 = m;
        }
        out
    }

    const SPECS: &[&str] = &[
        "a: lines ecu == A; reset uptime; planned raw == shutdown; crash raw == panic",
        "b: lines ecu == B; reset uptime tolerance 500ms; start raw == boot",
        "all: reset uptime tolerance 2s; gap 60s; crash raw == panic",
        "g: lines ecu == A; gap 30s; planned raw == shutdown",
        "s: start raw == boot",
        "z: lines ecu == A; reset uptime tolerance 0s",
    ];

    #[test]
    fn lifecycles_match_a_plain_walk() {
        for seed in [7, 0x5eed, 0xdead_beef] {
            // More lines than a chunk, so chunk boundaries are crossed.
            let src = log(seed, 150_000);
            for spec in SPECS {
                let d = def(spec).linked(&Signals::default());
                let l = d.lifecycles(&src, &Progress::new()).unwrap();
                let got: Vec<RefLife> =
                    l.lives.iter().map(|x| (x.number, x.first, x.last, x.lines, x.outcome.as_str())).collect();
                let want = reference(&src, &d);
                assert_eq!(got.len(), want.len(), "seed {seed} {spec}");
                for (g, w) in got.iter().zip(&want) {
                    assert_eq!(g, w, "seed {seed} {spec}");
                }
                assert!(l.lives.iter().all(|x| x.lines > 0 && x.first <= x.last));
                for f in &l.findings {
                    let x = &l.lives[f.life];
                    assert!(x.first <= f.row && f.row <= x.last, "seed {seed} {spec}: {f:?} outside {x:?}");
                }
            }
        }
    }

    fn bytes(v: ValueRef<'_>) -> Vec<u8> {
        match v {
            ValueRef::Enum(t) => t.to_vec(),
            ValueRef::Num(n) => n.to_string().into_bytes(),
        }
    }

    #[test]
    fn bound_signals_follow_their_lifecycle() {
        let src = log(42, 120_000);
        let mut sig = Signals::default();
        for s in [
            "lifecycle a: lines ecu == A; reset uptime; planned raw == shutdown",
            r"s:enum:per(a):default(none)=ecu == A AND /^s=(?<s>\w+)/",
            r"t:enum:per(a)=/^s=(?<t>\w+)/",
        ] {
            sig.set(SignalDef::parse_spec(s).unwrap());
        }
        let life = sig.lifecycles().next().unwrap().lifecycles(&src, &Progress::new()).unwrap();
        assert!(life.lives.len() > 5, "{}", life.summary());
        for name in ["s", "t"] {
            let d = sig.get(name).unwrap();
            let series = d.series(&src, &Progress::new()).unwrap();
            let events: Vec<(LineNo, Vec<u8>)> = series.iter().map(|(r, v)| (r, bytes(v))).collect();
            // The state is the last event since the row's lifecycle started
            // (before the first one: since the log's start), else the default.
            for row in (0..src.line_count()).step_by(37) {
                let since = life.at(row).map_or(0, |(_, l)| l.first);
                let want = events.iter().rev().find(|(r, _)| *r <= row && *r >= since).map(|(_, v)| v.clone());
                let want = want.or_else(|| d.default_value().map(|v| v.to_string().into_bytes()));
                let got = series.state_at(row).map(|(_, v)| bytes(v));
                assert_eq!(got, want, "@{name} at row {row}");
            }
        }
        // Filters agree on the fast and the naive paths.
        let f = |q: &str| Filter::Query(Query::compile(q).unwrap().with_signals(&sig).unwrap());
        for q in ["@s == none", "@s == busy AND @a == 2", "@s.changed", "@t == idle", "@a.event", "NOT @t == done"] {
            run_on(&src, &f(q));
        }
    }
}

/// Spec lines made of random pieces never panic: they parse, or they fail
/// with an error inside the text.
mod fuzz {
    use super::*;
    use crate::signal::SignalDef;

    const PIECES: &[&str] = &[
        "a",
        "ecu1",
        ":",
        ";",
        " ",
        "  ",
        "(",
        ")",
        "{",
        "}",
        "=",
        "@",
        "@up",
        "uptime",
        "reset",
        "tolerance",
        "5s",
        "500ms",
        "-1s",
        "x",
        "lines",
        "start",
        "gap",
        "planned",
        "crash",
        "late",
        "lifecycle ",
        "off ",
        "per(",
        "default(",
        "enum",
        "num",
        "time",
        "filtered",
        "ecu == A",
        "raw ~ /x/",
        "/(?<v>\\d+)/",
        "{sys.of ecu=A}",
        "{sys.x",
        "sys.x}",
        "é",
        "ż",
        "日本",
        "\t",
        "\"",
        "AND",
        "NOT",
        "$",
        "1e309",
        "0x10",
        "ms",
        "h",
    ];

    const CLAUSES: &[&str] = &[
        "lines ecu == A",
        "lines {sys.of ecu=é}",
        "start raw ~ /boot/",
        "reset uptime",
        "reset uptime tolerance 1.5s",
        "reset @up tolerance 250ms",
        "gap 30s",
        "gap 1h",
        "planned {sys.x}",
        "crash raw == \"日本\"",
        "late 2m",
        "late 0s",
        "reset",
        "gap",
        "start",
        "lines ecu ==",
        "reset uptime tolerance",
        "planned {sys",
        "crash {x}",
        "late -1s",
        "bogus 1",
        "",
        " ",
    ];
    const PARTS: &[&str] = &[
        "num",
        "enum",
        "time",
        "all",
        "filtered",
        "per(a)",
        "per(1)",
        "default(0)",
        "default(x)",
        "default()",
        "per()",
        "nope",
    ];

    #[test]
    fn random_specs_never_panic() {
        let mut r = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r
        };
        let (mut lives, mut signals) = (0, 0);
        for k in 0..50_000 {
            let n = 1 + next() % 12;
            let mut s: String = (0..n).map(|_| PIECES[(next() % PIECES.len() as u64) as usize]).collect();
            // Every other one is made of clauses, valid or broken, so more
            // of them parse and the round trip is tried.
            if k % 2 == 0 {
                let m = 1 + next() % 5;
                let cl: Vec<&str> = (0..m).map(|_| CLAUSES[(next() % CLAUSES.len() as u64) as usize]).collect();
                s = format!("n{k}: {}", cl.join("; "));
                if next() % 3 == 0 {
                    let parts: Vec<&str> =
                        (0..next() % 4).map(|_| PARTS[(next() % PARTS.len() as u64) as usize]).collect();
                    let head = if parts.is_empty() { String::new() } else { format!(":{}", parts.join(":")) };
                    s = format!(r"v{head}=/v=(?<v>\w+)/");
                }
            }
            match LifecycleDef::parse_spec(&s) {
                Ok(d) => {
                    lives += 1;
                    // What it writes back reads back the same.
                    let back = LifecycleDef::parse_spec(&d.spec())
                        .unwrap_or_else(|e| panic!("{s:?} -> {:?}: {}", d.spec(), e.msg));
                    assert_eq!(back.spec(), d.spec(), "{s:?}");
                }
                Err(e) => assert!(e.pos <= s.len(), "{s:?}: {} at {}", e.msg, e.pos),
            }
            if let Ok(d) = SignalDef::parse_spec(&s) {
                signals += 1;
                let back = SignalDef::parse_spec(&d.spec()).unwrap_or_else(|e| panic!("{s:?} -> {:?}: {e}", d.spec()));
                assert_eq!(back.spec(), d.spec(), "{s:?}");
            }
            if let Err(e) = Rule::parse(&s) {
                assert!(e.pos <= s.len(), "{s:?}: {} at {}", e.msg, e.pos);
            }
        }
        assert!(lives > 500 && signals > 10, "parsed {lives} lifecycles, {signals} signals");
    }
}

/// Edge cases found by stress testing (2026-11-07).
mod edges {
    use super::*;
    use crate::Filter;
    use crate::engine::search::tests::run_on;

    #[test]
    fn an_empty_log_and_lines_that_match_nothing() {
        let empty = Src(vec![]);
        assert!(run("a: reset uptime", &empty).lives.is_empty());
        let l = run("c: lines ecu == C; reset uptime", &two_ecus());
        assert!(l.lives.is_empty());
        assert_eq!(l.summary(), "0 lifecycles");
        assert_eq!(l.at(3), None);
    }

    #[test]
    fn a_first_line_without_an_uptime_is_no_reason_to_give_up() {
        // Its first line has none (a message without tmsp), later ones do.
        let src = Src(vec![
            (Some("A"), Some(1.0), None, "a"),
            (Some("A"), Some(2.0), Some(20.0), "b"),
            (Some("A"), Some(3.0), Some(0.5), "c"),
        ]);
        assert_eq!(lives(&run("a: reset uptime", &src)), [(1, 0, "unclean"), (2, 2, "open")]);
    }

    #[test]
    fn a_finding_that_repeats_is_counted_once_per_lifecycle() {
        // No lines: A's and B's uptimes interleave; with a start line the
        // reset isn't the rule, so every step back is a missed reset.
        let mut rows = Vec::new();
        for i in 0..300 {
            rows.push((Some("A"), None, Some(100.0 + i as f64), "a"));
            rows.push((Some("B"), None, Some(10.0 + i as f64), "b"));
        }
        let l = run("all: start raw == boot; late 1000s", &Src(rows));
        assert_eq!(l.findings.len(), 1, "{:?}", l.findings);
        let f = &l.findings[0];
        assert_eq!((f.kind, f.row, f.count), (FindingKind::MissedReset, 1, 300));
        assert!(f.detail.contains("the first of 300 times") && f.detail.contains("several writers"), "{}", f.detail);
    }

    #[test]
    fn a_single_line() {
        let src = Src(vec![(Some("A"), Some(1.0), Some(1.0), "shutdown")]);
        assert_eq!(lives(&run("a: reset uptime; planned raw == shutdown", &src)), [(1, 0, "planned")]);
    }

    #[test]
    fn a_start_line_that_is_also_a_marker_ends_nothing_before_it() {
        // A "crash" line that starts a lifecycle marks the new one.
        let src = Src(vec![(None, None, None, "x"), (None, None, None, "boot crash"), (None, None, None, "y")]);
        let l = run("a: start raw ~ /boot/; crash raw ~ /crash/", &src);
        assert_eq!(lives(&l), [(1, 0, "unclean"), (2, 1, "crash")]);
    }

    #[test]
    fn the_reset_signal_may_reset_with_the_lifecycle_itself() {
        // `up` is bound to `a`, and `a` resets on `up`: no loop.
        let src = Src(vec![
            (None, None, None, "up=10"),
            (None, None, None, "up=20"),
            (None, None, None, "up=1"),
            (None, None, None, "up=2"),
        ]);
        let mut sig = Signals::default();
        sig.set(SignalDef::parse_spec("lifecycle a: reset @up").unwrap());
        sig.set(SignalDef::parse_spec(r"up:per(a)=/up=(?<up>\d+)/").unwrap());
        let f = Filter::Query(Query::compile("@a == 2").unwrap().with_signals(&sig).unwrap());
        assert_eq!(run_on(&src, &f), [2, 3]);
    }

    #[test]
    fn a_filtered_signal_keeps_its_lifecycle() {
        let src = Src(vec![
            (Some("A"), None, Some(1.0), "s=x"),
            (Some("A"), None, Some(20.0), "s=y"),
            (Some("A"), None, Some(0.5), "z"),
        ]);
        let mut sig = Signals::default();
        sig.set(SignalDef::parse_spec("lifecycle a: reset uptime").unwrap());
        sig.set(SignalDef::parse_spec(r"s:enum:filtered:per(a):default(none)=/s=(?<s>\w+)/").unwrap());
        let d = sig.get("s").unwrap();
        let kept = d.series(&src, &Progress::new()).unwrap().within(&[0, 2]);
        // Line 2 is in A's second lifecycle: the default again.
        assert_eq!(kept.state_at(2).map(|(r, v)| (r, d.format(v))), Some((None, "none".into())));
        assert_eq!(kept.state_at(1).map(|(r, v)| (r, d.format(v))), Some((Some(0), "x".into())));
    }
}

/// Two DLT files, one per ECU, merged by time: a lifecycle per ECU over the
/// merged rows, a bound signal, and filters on both paths.
mod merged {
    use super::*;
    use crate::engine::search::tests::run_on;
    use crate::input::concat::Concat;
    use crate::input::dlt::writer::{Msg, Value};
    use crate::input::merged::Merged;
    use crate::{DltFile, Filter, Level};

    /// (seconds, uptime in s, text) per message of one ECU.
    fn dlt(ecu: &str, msgs: &[(u32, f64, &str)]) -> Box<dyn LogSource> {
        let mut bytes = Vec::new();
        for (secs, up, text) in msgs {
            let mut m = Msg::log(Level::Info, ecu, "APP", "CTX", vec![Value::Str((*text).into()).into()]);
            m.secs = *secs;
            m.timestamp = Some((up * 10_000.0) as u32);
            m.encode(&mut bytes);
        }
        Box::new(DltFile::from_bytes(format!("{ecu}.dlt"), bytes))
    }

    #[test]
    fn lifecycles_over_files_merged_by_time() {
        // A reboots at 30 (planned), B at 25 (unclean).
        let a = dlt(
            "A",
            &[(10, 1.0, "s=up"), (20, 11.0, "s=busy"), (29, 20.0, "shutdown"), (30, 0.5, "x"), (40, 10.5, "s=up")],
        );
        let b = dlt("B", &[(11, 2.0, "b"), (24, 15.0, "b"), (25, 0.2, "b"), (35, 10.2, "b")]);
        let src = Merged::new(Concat::new(vec![a, b]).unwrap(), &Progress::new()).unwrap();
        // Merged rows: 10A 11B 20A 24B 25B 29A 30A 35B 40A.
        let text = |r: LineNo| src.line_text(r).unwrap().into_owned();
        assert!(text(1).contains('b') && text(6).contains('x'), "{}", text(6));
        let mut sig = Signals::default();
        for s in [
            "lifecycle a: lines ecu == A; reset uptime; planned payload ~ /shutdown/",
            "lifecycle b: lines ecu == B; reset uptime",
            r"s:enum:per(a):default(off)=ecu == A AND payload ~ /s=(?<s>\w+)/",
        ] {
            sig.set(SignalDef::parse_spec(s).unwrap());
        }
        let la = sig.lifecycles().find(|l| l.name() == "a").unwrap().lifecycles(&src, &Progress::new()).unwrap();
        let lb = sig.lifecycles().find(|l| l.name() == "b").unwrap().lifecycles(&src, &Progress::new()).unwrap();
        assert_eq!(lives(&la), [(1, 0, "planned"), (2, 6, "open")]);
        assert_eq!(lives(&lb), [(1, 1, "unclean"), (2, 4, "open")]);
        let f = |q: &str| run_on(&src, &Filter::Query(Query::compile(q).unwrap().with_signals(&sig).unwrap()));
        // A's state resets at its reboot (row 6), on every line after.
        assert_eq!(f("@s == busy"), [2, 3, 4, 5]);
        assert_eq!(f("@s == off"), [6, 7]);
        assert_eq!(f("@a == 2 AND ecu == B"), [7]);
        assert_eq!(f("@b == 2"), [4, 5, 6, 7, 8]);
    }
}

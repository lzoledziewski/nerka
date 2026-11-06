use super::*;
use crate::event::{FieldDef, FieldId, FieldKind};
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

/// (label, first, outcome) of each lifecycle.
fn lives(l: &Lifecycles) -> Vec<(String, LineNo, &'static str)> {
    l.lives.iter().map(|x| (x.label(), x.first, x.outcome.as_str())).collect()
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
fn a_reset_of_the_uptime_starts_a_lifecycle_per_key() {
    let l = run("boot(ecu): reset @uptime; planned raw ~ /shutdown/", &two_ecus());
    assert_eq!(
        lives(&l),
        [
            ("A#1".into(), 0, "unclean"),
            ("B#1".into(), 1, "planned"),
            ("A#2".into(), 4, "open"),
            ("B#2".into(), 6, "open"),
        ]
    );
    assert_eq!(l.lives[2].started_by, Start::Reset);
    assert_eq!(l.lives[1].marker, Some(3));
    assert!(l.findings.is_empty(), "{:?}", l.findings);
    // Lines are of their key's lifecycle; a line without a key of none.
    let src = two_ecus();
    assert_eq!(l.at(&src, 3).map(|(_, x)| x.label()), Some("B#1".into()));
    assert_eq!(l.at(&src, 7).map(|(_, x)| x.label()), Some("A#2".into()));
    assert_eq!(l.at(&src, 5), None);
    // Boot time: the least time − uptime.
    assert_eq!(l.lives[0].boot, Some(99.0));
    assert_eq!(l.lives[2].lines, 2);
    assert_eq!(l.summary(), "4 lifecycles (2 keys), 1 unclean");
}

#[test]
fn a_query_starts_lifecycles_and_the_uptime_validates_them() {
    // `start` on "boot" lines only of ECU A's text: B's boot line 6 also
    // matches; line 2 says "boot" falsely.
    let src = Src(vec![
        (Some("A"), Some(100.0), Some(1.0), "boot"),
        (Some("A"), Some(105.0), Some(6.0), "x"),
        (Some("A"), Some(110.0), Some(11.0), "boot again?"),
        (Some("A"), Some(130.0), Some(0.5), "y"),
        (Some("A"), Some(140.0), Some(10.5), "z"),
    ]);
    let l = run("boot(ecu): start raw ~ /^boot/", &src);
    assert_eq!(lives(&l), [("A#1".into(), 0, "unclean"), ("A#2".into(), 2, "open")]);
    // Line 2 started one while the uptime kept counting; line 3's uptime
    // went back without a start.
    assert_eq!(findings(&l), [("false reset", 2), ("missed reset", 3)]);
}

#[test]
fn small_steps_back_are_within_the_tolerance() {
    let src = Src(vec![
        (Some("A"), None, Some(10.0), "a"),
        (Some("A"), None, Some(8.0), "b"),
        (Some("A"), None, Some(12.0), "c"),
        (Some("A"), None, Some(1.0), "d"),
    ]);
    assert_eq!(run("b(ecu): reset @uptime", &src).lives.len(), 2);
    assert_eq!(run("b(ecu): reset @uptime tolerance 1s", &src).lives.len(), 3);
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
    assert_eq!(lives(&l), [("#1".into(), 0, "lost"), ("#2".into(), 2, "crash"), ("#3".into(), 4, "open")]);
    assert_eq!(l.lives[0].duration(), Some(10.0));
}

#[test]
fn a_late_first_line_is_noted() {
    let src = Src(vec![(Some("A"), Some(100.0), Some(38.0), "a"), (Some("A"), Some(101.0), Some(0.1), "b")]);
    let l = run("boot(ecu): reset @uptime", &src);
    assert_eq!(findings(&l), [("late start", 0)]);
    assert!(l.findings[0].detail.contains("or the log started during it"));
    let l = run("boot(ecu): reset @uptime; late 60s", &src);
    assert!(l.findings.is_empty());
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
    signals.set(SignalDef::parse_spec(r"uptime=/^\[\s*(?<uptime>[\d.]+)\]/").unwrap());
    let l = def("boot: reset @uptime").linked(&signals).lifecycles(&src, &Progress::new()).unwrap();
    assert_eq!(lives(&l), [("#1".into(), 0, "unclean"), ("#2".into(), 3, "open")]);
    // Without the signal and without a source uptime: an error that says so.
    let e = def("boot: reset @uptime").linked(&Signals::default()).lifecycles(&src, &Progress::new()).unwrap_err();
    assert!(e.to_string().contains("no @uptime here"), "{e}");
}

#[test]
fn an_unknown_key_is_an_error() {
    let e = def("boot(node): reset @uptime").lifecycles(&two_ecus(), &Progress::new()).unwrap_err();
    assert_eq!(e.to_string(), "lifecycle 'boot': unknown key field 'node' (this source has: ecu)");
}

#[test]
fn results_are_cached_per_source() {
    let d = def("boot(ecu): reset @uptime");
    let src = two_ecus();
    assert!(d.cached(&src).is_none());
    let a = d.lifecycles(&src, &Progress::new()).unwrap();
    let b = d.clone().lifecycles(&src, &Progress::new()).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
}

#[test]
fn specs_round_trip() {
    for s in [
        "boot(ecu): reset @uptime; planned raw ~ /shutdown/",
        "run: start apid == SYS; gap 30s; crash raw ~ /crash/; late 1m",
        "b: reset @up tolerance 500ms",
        "b(ecu): start raw ~ /boot/; reset @uptime tolerance 2s; planned raw ~ /bye/; late 20s",
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
    bad("boot reset @uptime", 0, "expected NAME[(KEY)]:");
    bad("1boot: gap 1s", 0, "isn't a name");
    bad("boot(ecu: gap 1s", 8, "expected ')'");
    bad("boot(): gap 1s", 5, "the key is empty");
    bad("boot: gap 1s; restart x", 14, "unknown clause 'restart'");
    bad("boot: gap soon", 10, "needs a duration");
    bad("boot: reset uptime", 12, "needs a signal");
    bad("boot: reset @uptime tolerance x", 30, "expected a duration");
    bad("boot: reset @uptime later", 12, "expected 'reset @SIGNAL'");
    bad("boot: start", 11, "'start' needs a query");
    bad("boot: start level >= LOUD", 21, "unknown level");
    bad("boot: planned raw ~ /x/", 23, "needs a start, reset or gap");
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
    use crate::signal::{SignalError, SignalView};
    use crate::{Filter, TextFile};

    /// A's OTA job runs over A's reboot; B's isn't touched by it.
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

    const BOOT: &str = "lifecycle boot(ecu): reset @uptime";
    const OTA: &str = r"ota(s):enum:per(boot):default(idle)=/OTA state=(?<s>\w+)/";

    #[test]
    fn a_bound_signal_resets_per_key_at_each_lifecycle() {
        let src = ota();
        let sig = signals(&[BOOT, OTA]);
        let d = sig.get("ota").unwrap();
        let series = d.series(&src, &Progress::new()).unwrap();
        let state: Vec<String> = (0..8)
            .map(|r| match series.state_at(&src, r) {
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
                "installing",
                // A rebooted: its job's state is gone, B's stays.
                "idle (default)",
                "idle (default)",
                "installing",
                "done",
            ]
        );
        // The same through filters, fast and naive paths agreeing.
        assert_eq!(run_on(&src, &filter("@ota == idle", &sig)), [0, 1, 4, 5]);
        assert_eq!(run_on(&src, &filter("@ota == installing", &sig)), [3, 6]);
        // A default isn't an event; the first event changes it only if it differs.
        assert_eq!(run_on(&src, &filter("@ota.event", &sig)), [2, 3, 7]);
        assert_eq!(run_on(&src, &filter("@ota.changed", &sig)), [2, 3, 7]);
        // The lifecycle itself: its number for the line's key, events where they start.
        assert_eq!(run_on(&src, &filter("@boot == 2", &sig)), [4, 5, 7]);
        assert_eq!(run_on(&src, &filter("@boot.event", &sig)), [0, 1, 4]);
        assert_eq!(run_on(&src, &filter("@boot == 1 AND ecu == B", &sig)), [1, 3, 6]);
    }

    #[test]
    fn changed_compares_with_the_default() {
        let src = Src(vec![
            (Some("A"), None, Some(1.0), "s=idle"),
            (Some("A"), None, Some(12.0), "s=busy"),
            (Some("A"), None, Some(0.1), "s=busy"),
        ]);
        let sig = signals(&[BOOT, r"s:enum:per(boot):default(idle)=/s=(?<s>\w+)/"]);
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
        let sig = signals(&[BOOT, OTA, r"n:num:default(-1.5)=/n=(?<n>\S+)/"]);
        let text = sig.to_text();
        let (back, errors) = Signals::parse_text(&text);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            back.iter().map(|d| d.spec()).collect::<Vec<_>>(),
            [
                "lifecycle boot(ecu): reset @uptime",
                r"ota(s):enum:per(boot):default(idle)=/OTA state=(?<s>\w+)/",
                r"n:num:default(-1.5)=/n=(?<n>\S+)/",
            ]
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
        assert!(
            e("lifecycle boot: gap soon").starts_with("lifecycle: 'gap' needs a duration"),
            "{}",
            e("lifecycle boot: gap soon")
        );
        assert!(
            matches!(SignalDef::parse_spec("x:sometimes=/(?<x>.)/"), Err(SignalError::Spec(m)) if m.contains("per(LIFECYCLE)"))
        );
        // Bound to a lifecycle that isn't defined: an error where it's used.
        let sig = signals(&[OTA]);
        let err = Query::compile("@ota == idle").unwrap().with_signals(&sig).unwrap_err();
        assert_eq!(err.msg, "'@ota' resets per lifecycle 'boot', which isn't defined");
        // Defining it later links it.
        let mut sig = sig;
        sig.set(SignalDef::parse_spec(BOOT).unwrap());
        assert!(Query::compile("@ota == idle").unwrap().with_signals(&sig).is_ok());
    }

    #[test]
    fn the_source_uptime_is_a_built_in() {
        let src = ota();
        let sig = Signals::default().for_source(&src);
        assert_eq!(sig.iter().map(|d| d.name()).collect::<Vec<_>>(), ["time", "uptime"]);
        assert_eq!(run_on(&src, &filter("@uptime > 30", &sig)), [6]);
        assert!(!sig.to_text().contains("uptime"));
        let _ = SignalView::State;
    }
}

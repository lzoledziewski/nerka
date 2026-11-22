use super::*;

#[test]
fn spec_lines_read_back() {
    for spec in [
        "app: name *.log, app-*.txt; first /^\\d{4}-/; records start=/\\d{4}-/; time /^(?<time>\\S+ \\S+)/; zone local; numbers ms; signal thread=/\\[(?<thread>\\w+)\\]/",
        "kern: name dmesg*; time /^\\[\\s*(?<time>[\\d.]+)\\]/; numbers s",
        "plain: zone utc",
        "j: first /^\\{/; records json; zone +02:00",
        "bare:",
    ] {
        let p = Profile::parse_spec(spec).unwrap_or_else(|e| panic!("{spec}: {}", e.at(spec)));
        assert_eq!(p.spec(), spec);
        assert_eq!(Profile::parse_spec(&p.spec()).unwrap(), p);
    }
    let p = Profile::parse_spec("App :  time auto ;  zone UTC").unwrap();
    assert_eq!(p.spec(), "app: time auto; zone utc");
}

#[test]
fn errors_say_where() {
    for (spec, pos, msg) in [
        ("no colon", 0, "expected NAME:"),
        ("1x: time auto", 0, "isn't a name"),
        ("a: when x", 3, "unknown clause 'when'"),
        ("a: time /(/", 9, "regex"),
        ("a: first ^x", 9, "in slashes"),
        ("a: zone mars", 8, "'zone' needs"),
        ("a: numbers hours", 11, "'numbers' needs"),
        ("a: records rows", 11, "records"),
        ("a: signal =", 10, "signal"),
        ("a: time", 7, "'time' needs a value"),
    ] {
        let e = Profile::parse_spec(spec).expect_err(spec);
        assert_eq!(e.pos, pos, "{spec}: {}", e.msg);
        assert!(e.msg.contains(msg), "{spec}: {}", e.msg);
    }
}

#[test]
fn globs_match_names_in_any_case() {
    assert!(glob("*.log", "App.LOG"));
    assert!(glob("app-*.txt", "app-2025.txt"));
    assert!(glob("a?c", "abc"));
    assert!(glob("*", ""));
    assert!(glob("*x*y", "axbxy"));
    assert!(!glob("*.log", "app.log.1"));
    assert!(!glob("a?c", "ac"));
}

#[test]
fn chosen_forced_then_name_then_first_line_then_default() {
    let (ps, errors) = Profiles::parse_text(
        "# comment\n\
         sys: name *.syslog\n\
         json: first /^\\{/; records json\n\
         both: name *.log; first /^HEAD/\n\
         manual: time none\n\
         broken: when\n",
    );
    assert_eq!(errors.len(), 1);
    assert!(errors[0].starts_with("line 6:"), "{errors:?}");
    let pick = |forced: Option<&str>, path: &str, head: &str| {
        let (p, why) = ps.choose(forced, Path::new(path), head.as_bytes());
        (p.name().to_owned(), why)
    };
    assert_eq!(pick(None, "/x/a.syslog", "{}"), ("sys".into(), Why::Name));
    assert_eq!(pick(None, "a.txt", "\n\n  {\"a\": 1}"), ("json".into(), Why::FirstLine));
    assert_eq!(pick(None, "a.log", "HEAD 1\n"), ("both".into(), Why::Name));
    // `both` needs its first line too; then nothing else matches.
    assert_eq!(pick(None, "a.log", "other\n"), ("default".into(), Why::Default));
    assert_eq!(pick(Some("manual"), "a.syslog", ""), ("manual".into(), Why::Forced));
    assert_eq!(pick(Some("default"), "a.syslog", ""), ("default".into(), Why::Forced));
    // A forced profile that isn't there: as if none were forced.
    assert_eq!(pick(Some("gone"), "a.syslog", ""), ("sys".into(), Why::Name));
    // `manual` has no rule of its own: never chosen by itself.
    assert!(!ps.get("manual").unwrap().matches_by_itself());
    // The built-in default reads the time lines start with.
    assert!(matches!(ps.default_profile().time(), TimeRule::Leading));
}

#[test]
fn a_default_of_ones_own_replaces_the_builtin() {
    let (ps, _) = Profiles::parse_text("default: time none\n");
    let (p, why) = ps.choose(None, Path::new("a.log"), b"2025-10-17 10:00:00 x");
    assert_eq!((p.name(), why), ("default", Why::Default));
    assert!(matches!(p.time(), TimeRule::None));
}

#[test]
fn text_round_trips_and_a_session_profile_goes_first() {
    let (mut ps, _) = Profiles::parse_text("a: name *.a\nb: name *.b\n");
    ps.put_first(Profile::parse_spec("b: name *.log").unwrap());
    assert_eq!(ps.iter().map(Profile::name).collect::<Vec<_>>(), ["b", "a"]);
    let (again, errors) = Profiles::parse_text(&ps.to_text());
    assert!(errors.is_empty());
    assert_eq!(again, ps);
    assert!(ps.remove("A"));
    assert_eq!(ps.len(), 1);
}

#[test]
fn time_rules_read_lines() {
    let day = 20_378.0 * 86_400.0; // 2025-10-17
    let rule = TimeRule::Regex(Regex::new(r"ts=(?<time>\S+)").unwrap());
    assert_eq!(rule.read("a ts=2025-10-17T10:00:01Z b", Zone::Shown, Numbers::Auto), Some(day + 36_001.0));
    assert_eq!(rule.read("no time", Zone::Shown, Numbers::Auto), None);
    // Without a `time` group: the whole match.
    let whole = TimeRule::Regex(Regex::new(r"\d+\.\d+").unwrap());
    assert_eq!(whole.read("[   12.500] up", Zone::Shown, Numbers::S), Some(12.5));
    assert_eq!(whole.read("[1760695201.250] up", Zone::Shown, Numbers::Ms), Some(1_760_695.201_25));
    assert_eq!(
        TimeRule::Leading.read("2025-10-17 12:00:01 x", Zone::Fixed(7200.0), Numbers::Auto),
        Some(day + 36_001.0)
    );
}

fn reading(profiles: &[&str]) -> Vec<crate::input::timed::FileReading> {
    profiles
        .iter()
        .map(|p| crate::input::timed::FileReading { profile: (*p).to_owned(), ..Default::default() })
        .collect()
}

/// A profile's signal reads only its files (`profile == NAME`); two
/// profiles giving one name with the same options are one signal, each
/// file its own query; with other options the first file's wins, said.
#[test]
fn profile_signals_are_scoped_merged_and_clashes_said() {
    let (ps, errors) = Profiles::parse_text(concat!(
        r"a: name *.a; signal t:enum=/T=(?<t>\w+)/",
        "\n",
        r"b: name *.b; signal t:enum=/thread (?<t>\w+)/; signal n=/n=(?<n>\d+)/",
        "\n",
        r"c: name *.c; signal n:enum=/N(?<n>\w)/",
        "\n",
    ));
    assert!(errors.is_empty(), "{errors:?}");
    let got = ps.signals_for(&reading(&["a", "b", "a", "default"]));
    let specs: Vec<String> = got.signals.iter().map(|d| d.spec()).collect();
    assert_eq!(
        specs,
        [
            r"t:enum=(profile == a AND (/T=(?<t>\w+)/)) OR (profile == b AND (/thread (?<t>\w+)/))",
            r"n:num=(profile == b AND (/n=(?<n>\d+)/))",
        ]
    );
    assert_eq!(got.signals[0].profiles(), ["a", "b"]);
    assert!(got.clashes.is_empty());
    // c gives `n` as an enum: b's (the first file's) is used, and said.
    let got = ps.signals_for(&reading(&["b", "c"]));
    assert_eq!(got.signals.len(), 2);
    assert_eq!(got.signals[1].profiles(), ["b"]);
    assert_eq!(got.clashes.len(), 1);
    assert!(got.clashes[0].starts_with("@n: profile c defines it otherwise than profile b"), "{:?}", got.clashes);
}

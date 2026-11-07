use super::*;

const PACK: &str = "# Camera queries.\npack vision\n\nquery cam\n    apid == CAM\n";
const PRESET: &str = "preset \"cam triage\"\nnode include lit          query vision.cam\n";
const PLOT: &str = "plot speeds\nlane speed events\n";
const CHECK: &str = "storm: lines level >= ERROR; rate 25 per 1s";
const SIGNAL: &str = r"v=/v=(?<v>\d+)/";
const LIFECYCLE: &str = "lifecycle ecu1: lines ecu == ECU1; reset uptime";

/// A fresh folder for one test.
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nerka-bundle-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A config folder with one item of each kind (and a lifecycle).
fn filled(name: &str) -> Store {
    let d = dir(name);
    for (sub, file, text) in [
        ("packs", "vision.nerka-pack", PACK),
        ("presets", "cam triage.nerka-preset", PRESET),
        ("plots", "speeds.nerka-plot", PLOT),
    ] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
        std::fs::write(d.join(sub).join(file), text).unwrap();
    }
    std::fs::write(d.join("checks"), format!("# nerka checks\n{CHECK}\n")).unwrap();
    std::fs::write(d.join("signals"), format!("# nerka signals\n{SIGNAL}\n{LIFECYCLE}\n")).unwrap();
    Store::new(d)
}

fn item(kind: Kind, text: &str) -> Item {
    Item::new(kind, text).unwrap_or_else(|e| panic!("{e}"))
}

fn labels(plan: &Plan) -> Vec<String> {
    plan.items.iter().map(|p| format!("{} {} {}", p.status_text(), p.kind, p.name)).collect()
}

#[test]
fn items_are_named_from_their_text() {
    assert_eq!(item(Kind::Pack, PACK).name, "vision");
    assert_eq!(item(Kind::Preset, PRESET).name, "cam triage");
    assert_eq!(item(Kind::Plot, PLOT).name, "speeds");
    assert_eq!(item(Kind::Check, CHECK).name, "storm");
    assert_eq!(item(Kind::Signal, SIGNAL).name, "v");
    assert_eq!(item(Kind::Signal, LIFECYCLE).name, "ecu1");
    assert_eq!(item(Kind::Check, CHECK).text, format!("{CHECK}\n"));
    assert!(Item::new(Kind::Check, "a: lines x; rate 1 per 1s\nb: lines y; exists").unwrap_err().contains("one line"));
    assert!(Item::new(Kind::Check, "").unwrap_err().contains("empty"));
    assert!(Item::new(Kind::Pack, "query x\n  /x/\n").is_err());
}

#[test]
fn kinds_read_singular_and_plural() {
    assert_eq!(Kind::parse("pack"), Some(Kind::Pack));
    assert_eq!(Kind::parse("Signals"), Some(Kind::Signal));
    assert_eq!(Kind::parse("prefs"), None);
    assert!(Kind::ALL.iter().all(|k| k.format() == 1));
}

#[test]
fn bundle_text_round_trips_with_escapes() {
    let odd = Item { kind: Kind::Pack, name: "a b".into(), format: 1, text: "@@ x\n@@\n@@@y\nplain\n@ z\n".into() };
    let b = Bundle::new(vec![item(Kind::Pack, PACK), odd.clone(), item(Kind::Check, CHECK)]);
    let text = b.to_text();
    assert!(text.starts_with(&format!("nerka-bundle 1\nnerka {}\n", env!("CARGO_PKG_VERSION"))), "{text}");
    assert!(text.contains("@@ pack \"a b\" format 1\n@@@ x\n@@@\n@@@@y\nplain\n@ z\n"), "{text}");
    assert!(text.contains("@@ check storm format 1\nstorm: lines"), "{text}");
    let back = Bundle::parse("t", &text).unwrap();
    assert_eq!(back, b);
    assert_eq!(back.items[1], odd);
}

#[test]
fn bundle_reads_comments_crlf_and_unknown_kinds() {
    let text = "\r\nnerka-bundle 1\r\n# from a colleague\r\n\r\n@@ check storm format 1\r\nstorm: lines x; exists\r\n\
                @@ prefs mine format 1\r\nlocal_time=1\r\n@@ signal v format 1\r\nv=/(?<v>\\d)/\r\n";
    let b = Bundle::parse("t", text).unwrap();
    assert_eq!(b.nerka, None);
    assert_eq!(b.items.iter().map(Item::label).collect::<Vec<_>>(), ["check storm", "signal v"]);
    assert_eq!(b.items[0].text, "storm: lines x; exists\n");
    assert_eq!(b.unknown, [(6, "prefs".to_owned(), "mine".to_owned())]);
}

#[test]
fn bundle_errors_have_positions() {
    let e = |t: &str| Bundle::parse("b.nerka-bundle", t).unwrap_err().to_string();
    assert_eq!(e(""), "b.nerka-bundle: an empty file, not a nerka bundle");
    assert_eq!(e("pack vision\n"), "b.nerka-bundle:1:1: not a nerka bundle (its first line would be 'nerka-bundle 1')");
    assert_eq!(
        e("nerka-bundle 2\nnerka 9.0.0\n"),
        "b.nerka-bundle:1:14: bundle layout 2 (nerka 9.0.0): this nerka reads layout 1; update nerka to read it"
    );
    assert_eq!(e("nerka-bundle 1\nstray\n"), "b.nerka-bundle:2:1: expected an item: @@ KIND NAME format N");
    assert_eq!(e("nerka-bundle 1\n@@ check storm\n"), "b.nerka-bundle:2:15: expected @@ KIND NAME format N");
    assert_eq!(e("nerka-bundle 1\n@@ check storm format 1 x\n"), "b.nerka-bundle:2:25: expected @@ KIND NAME format N");
    assert_eq!(e("nerka-bundle 1\n@@ check storm fmt 1\n"), "b.nerka-bundle:2:16: expected 'format' but found 'fmt'");
    assert_eq!(
        e("nerka-bundle 1\n@@ check storm format x\n"),
        "b.nerka-bundle:2:23: expected a format number but found 'x'"
    );
    assert_eq!(e("nerka-bundle 1\n@@ check \"storm format 1\n"), "b.nerka-bundle:2:10: unclosed quote");
}

/// Format 2 of signals: a `#2` comment line in front; format 3: `#3`.
const STEPS_2: &[Step] = &[
    Step { kind: Kind::Signal, from: 1, run: |t| Ok(format!("#2\n{t}")) },
    Step { kind: Kind::Signal, from: 2, run: |t| Ok(format!("#3\n{t}")) },
];

#[test]
fn upgrades_run_their_steps_in_order() {
    assert_eq!(current(Kind::Signal, STEPS_2), 3);
    assert_eq!(current(Kind::Check, STEPS_2), 1);
    assert_eq!(upgrade_with(STEPS_2, Kind::Signal, 1, "v=/x/\n").unwrap(), "#3\n#2\nv=/x/\n");
    assert_eq!(upgrade_with(STEPS_2, Kind::Signal, 2, "v=/x/\n").unwrap(), "#3\nv=/x/\n");
    assert_eq!(upgrade_with(STEPS_2, Kind::Signal, 3, "v=/x/\n").unwrap(), "v=/x/\n");
    assert_eq!(
        upgrade_with(STEPS_2, Kind::Signal, 4, "v=/x/\n").unwrap_err(),
        "made by a newer nerka: signal format 4, this nerka reads up to 3; update nerka to import it"
    );
    assert_eq!(upgrade_with(STEPS_2, Kind::Signal, 0, "").unwrap_err(), "signal format 0 doesn't exist");
    let failing = &[Step { kind: Kind::Check, from: 1, run: |_| Err("no".into()) }];
    assert_eq!(upgrade_with(failing, Kind::Check, 1, "").unwrap_err(), "upgrading check format 1 to 2: no");
}

#[test]
fn renaming_changes_the_name_in_the_text() {
    let r = |k: Kind, t: &str, n: &str| renamed(k, &tidy(t), n).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r(Kind::Pack, PACK, "vision_imported"), PACK.replace("pack vision", "pack vision_imported"));
    assert_eq!(r(Kind::Pack, PACK, "my vision"), PACK.replace("pack vision", "pack \"my vision\""));
    assert_eq!(r(Kind::Preset, PRESET, "cam triage_imported"), PRESET.replace("cam triage", "cam triage_imported"));
    assert_eq!(r(Kind::Plot, PLOT, "speeds_imported"), "plot speeds_imported\nlane speed events\n");
    assert_eq!(r(Kind::Check, CHECK, "storm_imported"), format!("storm_imported{}\n", &CHECK[5..]));
    assert_eq!(r(Kind::Check, &format!("off {CHECK}"), "x y"), format!("off x y{}\n", &CHECK[5..]));
    assert_eq!(r(Kind::Signal, SIGNAL, "v_imported"), format!("v_imported(v){}\n", &SIGNAL[1..]));
    assert_eq!(r(Kind::Signal, "v:enum=/v=(?<v>\\w+)/", "w"), "w(v):enum=/v=(?<v>\\w+)/\n");
    assert_eq!(r(Kind::Signal, "off s(g):enum=/(?<g>\\w)/", "t"), "off t(g):enum=/(?<g>\\w)/\n");
    assert_eq!(r(Kind::Signal, LIFECYCLE, "ecu1_imported"), LIFECYCLE.replace("ecu1", "ecu1_imported") + "\n");
    assert!(renamed(Kind::Signal, SIGNAL, "not a name").is_err());
}

#[test]
fn a_folder_lists_its_items() {
    let s = filled("list");
    let (items, errors) = s.items();
    assert!(errors.is_empty(), "{errors:?}");
    let names: Vec<String> = items.iter().map(|s| s.item.label()).collect();
    assert_eq!(names, ["pack vision", "preset cam triage", "plot speeds", "check storm", "signal v", "signal ecu1"]);
    assert!(items[0].path.ends_with("vision.nerka-pack"));
    assert!(items[3].path.ends_with("checks"));
    assert_eq!(items[3].item.text, format!("{CHECK}\n"));
    // A broken file is said, the rest listed.
    std::fs::write(s.root().join("plots").join("bad.nerka-plot"), "lane x\n").unwrap();
    let (items, errors) = s.items();
    assert_eq!(items.len(), 6);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("bad.nerka-plot"), "{errors:?}");
}

#[test]
fn export_then_import_into_an_empty_folder() {
    let from = filled("export");
    let (b, errors) = from.export(&|_| true);
    assert!(errors.is_empty());
    let b = Bundle::parse("t", &b.to_text()).unwrap();
    assert_eq!(b.items.len(), 6);
    let to = Store::new(dir("import-empty"));
    let plan = to.plan(&b);
    assert_eq!(
        labels(&plan),
        [
            "new pack vision",
            "new preset cam triage",
            "new plot speeds",
            "new check storm",
            "new signal v",
            "new signal ecu1"
        ]
    );
    assert_eq!(plan.changes(), 6);
    let done = to.apply(&plan);
    assert!(done.errors.is_empty(), "{:?}", done.errors);
    assert_eq!(done.added.len(), 6);
    assert_eq!(done.summary(), "6 added");
    assert_eq!(done.backup, None);
    let root = to.root();
    assert_eq!(std::fs::read_to_string(root.join("packs/vision.nerka-pack")).unwrap(), PACK);
    assert_eq!(std::fs::read_to_string(root.join("presets/cam triage.nerka-preset")).unwrap(), PRESET);
    assert_eq!(std::fs::read_to_string(root.join("plots/speeds.nerka-plot")).unwrap(), PLOT);
    let checks = std::fs::read_to_string(root.join("checks")).unwrap();
    assert!(checks.starts_with("# nerka checks:") && checks.ends_with(&format!("\n{CHECK}\n")), "{checks}");
    let (signals, errors) = crate::Signals::parse_text(&std::fs::read_to_string(root.join("signals")).unwrap());
    assert!(errors.is_empty() && signals.get("v").is_some() && signals.get("ecu1").is_some());
    assert_eq!(
        std::fs::read_to_string(root.join(FORMAT_FILE)).unwrap().lines().filter(|l| l.ends_with(" 1")).count(),
        5
    );
    // Again: all the same, nothing to do.
    let again = to.plan(&b);
    assert!(again.items.iter().all(|p| p.status == Status::Same), "{:?}", labels(&again));
    assert_eq!(again.changes(), 0);
    assert_eq!(to.apply(&again).summary(), "nothing imported");
}

#[test]
fn same_ignores_line_ends_and_spelling() {
    let s = filled("same");
    let b = Bundle::new(vec![
        item(Kind::Pack, &PACK.replace('\n', "\r\n").replace("CAM\r\n", "CAM   \r\n\r\n")),
        // `rate 25 per 1s` as nerka writes it, spelled with more spaces.
        item(Kind::Check, "storm:  lines level >= ERROR ;rate 25 per 1s"),
    ]);
    assert_eq!(labels(&s.plan(&b)), ["same pack vision", "same check storm"]);
}

#[test]
fn conflicts_auto_keeps_both() {
    let s = filled("auto");
    let other_check = "storm: lines level >= WARN; rate 5 per 1s";
    let other_pack = "pack Vision\n\nquery cam\n    apid == CAM2\n";
    let b = Bundle::new(vec![item(Kind::Check, other_check), item(Kind::Pack, other_pack)]);
    let plan = s.plan(&b);
    assert_eq!(labels(&plan), ["different check storm", "different pack Vision"]);
    assert_eq!(plan.items[0].choice, Choice::Both);
    assert_eq!(plan.items[0].choice_text(Choice::Both), "keep both (as storm_imported)");
    assert_eq!(plan.items[1].both_name, "Vision_imported");
    let done = s.apply(&plan);
    assert!(done.errors.is_empty(), "{:?}", done.errors);
    assert_eq!(done.summary(), "2 kept both");
    let checks = std::fs::read_to_string(s.root().join("checks")).unwrap();
    assert_eq!(checks, format!("# nerka checks\n{CHECK}\nstorm_imported: lines level >= WARN; rate 5 per 1s\n"));
    let pack = std::fs::read_to_string(s.root().join("packs/Vision_imported.nerka-pack")).unwrap();
    assert!(pack.starts_with("pack Vision_imported\n"), "{pack}");
    assert_eq!(std::fs::read_to_string(s.root().join("packs/vision.nerka-pack")).unwrap(), PACK);
    // Importing the same bundle again: the next free names.
    let plan = s.plan(&b);
    assert_eq!(plan.items[0].both_name, "storm_imported_2");
    assert_eq!(plan.items[1].both_name, "Vision_imported_2");
}

#[test]
fn taking_theirs_backs_up_mine() {
    let s = filled("theirs");
    let other_check = "storm: lines level >= WARN; rate 5 per 1s";
    let other_preset = "preset \"cam triage\"\nnode exclude              query vision.cam\n";
    let b = Bundle::new(vec![item(Kind::Check, other_check), item(Kind::Preset, other_preset)]);
    let mut plan = s.plan(&b);
    plan.set_conflicts(Choice::Take);
    let done = s.apply(&plan);
    assert!(done.errors.is_empty(), "{:?}", done.errors);
    // Files as they're written, lines per kind after them.
    assert_eq!(done.replaced, ["preset cam triage", "check storm"]);
    let checks = std::fs::read_to_string(s.root().join("checks")).unwrap();
    assert_eq!(checks, format!("# nerka checks\n{other_check}\n"));
    let preset = s.root().join("presets/cam triage.nerka-preset");
    assert_eq!(std::fs::read_to_string(preset).unwrap(), other_preset);
    let backup = done.backup.expect("a backup");
    assert!(backup.starts_with(s.root().join(BACKUP_DIR)), "{}", backup.display());
    let old = Bundle::parse("b", &std::fs::read_to_string(&backup).unwrap()).unwrap();
    assert_eq!(old.items.iter().map(Item::label).collect::<Vec<_>>(), ["preset cam triage", "check storm"]);
    assert_eq!(old.items[1].text, format!("{CHECK}\n"));
    // Keeping mine writes nothing.
    let mut plan = s.plan(&Bundle::new(vec![item(Kind::Check, CHECK)]));
    plan.set_conflicts(Choice::Skip);
    assert_eq!(plan.items[0].choice_text(plan.items[0].choice), "keep mine");
    assert_eq!(s.apply(&plan).summary(), "nothing imported");
    assert_eq!(std::fs::read_to_string(s.root().join("checks")).unwrap(), format!("# nerka checks\n{other_check}\n"));
}

#[test]
fn invalid_items_are_skipped_and_said() {
    let s = Store::new(dir("invalid"));
    let mut b = Bundle::new(vec![
        item(Kind::Check, CHECK),
        Item {
            kind: Kind::Check,
            name: "broken".into(),
            format: 1,
            text: "broken: lines level >= LOUD; exists\n".into(),
        },
        Item { kind: Kind::Check, name: "storm".into(), format: 1, text: format!("{CHECK}\n") },
        Item { kind: Kind::Signal, name: "future".into(), format: 7, text: "whatever\n".into() },
    ]);
    b.unknown.push((9, "prefs".into(), "mine".into()));
    let plan = s.plan(&b);
    assert_eq!(
        labels(&plan),
        ["new check storm", "invalid check broken", "invalid check storm", "invalid signal future"]
    );
    let why = |i: usize| match &plan.items[i].status {
        Status::Invalid(w) => w.clone(),
        s => panic!("{s:?}"),
    };
    assert!(why(1).contains("LOUD"), "{}", why(1));
    assert_eq!(why(2), "twice in the bundle; the first one is imported");
    assert!(why(3).starts_with("made by a newer nerka: signal format 7"), "{}", why(3));
    assert_eq!(plan.invalid(), 3);
    assert_eq!(plan.notes, ["line 10: prefs mine: a kind this nerka doesn't know, skipped"]);
    assert_eq!(s.apply(&plan).summary(), "1 added");
}

#[test]
fn new_files_dont_overwrite_other_files() {
    let s = filled("files");
    // A file named like the pack, holding another pack.
    std::fs::write(s.root().join("packs/radar.nerka-pack"), "pack other\n").unwrap();
    let plan = s.plan(&Bundle::new(vec![item(Kind::Pack, "pack radar\n")]));
    assert_eq!(labels(&plan), ["new pack radar"]);
    s.apply(&plan);
    assert_eq!(std::fs::read_to_string(s.root().join("packs/radar.nerka-pack")).unwrap(), "pack other\n");
    assert_eq!(std::fs::read_to_string(s.root().join("packs/radar_2.nerka-pack")).unwrap(), "pack radar\n");
    assert_eq!(free_file(Path::new("d"), "a/b:c", "x"), Path::new("d").join("a_b_c.x"));
}

#[test]
fn a_folder_upgrades_with_a_backup() {
    let s = filled("upgrade");
    let up = s.upgrade_with(STEPS_2);
    assert!(up.errors.is_empty(), "{:?}", up.errors);
    assert_eq!(up.kinds, [(Kind::Signal, 1, 3)]);
    let signals = std::fs::read_to_string(s.root().join("signals")).unwrap();
    assert_eq!(signals, format!("# nerka signals\n#3\n#2\n{SIGNAL}\n#3\n#2\n{LIFECYCLE}\n"));
    let formats = std::fs::read_to_string(s.root().join(FORMAT_FILE)).unwrap();
    assert!(formats.contains("\nsignal 3\n") && formats.contains("\ncheck 1\n"), "{formats}");
    // The backup has every item as it was, in format 1.
    let backup = up.backup.clone().unwrap();
    let old = Bundle::parse("b", &std::fs::read_to_string(&backup).unwrap()).unwrap();
    assert_eq!(old.items.len(), 6);
    assert!(old.items.iter().all(|i| i.format == 1));
    assert_eq!(old.items[4].text, format!("{SIGNAL}\n"));
    assert!(up.message().unwrap().starts_with("settings upgraded: signals 1→3 (backup: "), "{:?}", up.message());
    // Done: nothing more.
    assert_eq!(s.upgrade_with(STEPS_2), Upgraded::default());
}

#[test]
fn a_folder_gets_its_format_file_and_newer_ones_are_left_alone() {
    let s = filled("formats");
    assert_eq!(s.upgrade(), Upgraded::default());
    let formats = std::fs::read_to_string(s.root().join(FORMAT_FILE)).unwrap();
    assert!(formats.lines().any(|l| l == "pack 1"), "{formats}");
    // A later nerka's folder.
    std::fs::write(s.root().join(FORMAT_FILE), "pack 1\ncheck 4\nprefs 2\n").unwrap();
    let up = s.upgrade();
    assert_eq!(up.kinds, []);
    assert_eq!(
        up.newer,
        ["checks here were saved by a newer nerka (check format 4, this nerka reads up to 1): update nerka"]
    );
    assert_eq!(std::fs::read_to_string(s.root().join("checks")).unwrap(), format!("# nerka checks\n{CHECK}\n"));
    // A folder that doesn't exist stays so.
    let none = Store::new(dir("formats-none").join("nerka"));
    assert_eq!(none.upgrade(), Upgraded::default());
    assert!(!none.root().exists());
}

#[test]
fn stamps_are_utc_times() {
    let s = stamp();
    assert_eq!(s.len(), 15, "{s}");
    assert_eq!(&s[8..9], "-");
    assert!(s.starts_with("20"), "{s}");
}

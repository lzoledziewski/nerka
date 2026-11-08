//! End-to-end tests of the `nerka` binary: what a script or an agent sees
//! (stdout, stderr, exit status) on generated files. Review finding R12
//! (doc/review/2026-11-07-findings.md). No dependency beyond std: the
//! binary is `CARGO_BIN_EXE_nerka`, the DLT file comes from core's writer.

use nerka_core::Level;
use nerka_core::input::dlt::writer::{Msg, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LOG: &str = "\
2026-11-07 10:00:00 [INFO] boot ok
2026-11-07 10:00:01 [WARN] disk 91% żółw
2026-11-07 10:00:02 [ERROR] net down: timeout
2026-11-07 10:00:03 [INFO] retry
2026-11-07 10:00:04 [ERROR] net down: timeout
";

/// A fresh folder for one test.
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nerka-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn log_file(d: &Path) -> PathBuf {
    let f = d.join("app.log");
    std::fs::write(&f, LOG).unwrap();
    f
}

/// Six DLT messages: NAV / COM, levels error to info, one second apart.
fn dlt_file(d: &Path) -> PathBuf {
    let mut buf = Vec::new();
    let rows = [
        (Level::Info, "NAV", "route ok"),
        (Level::Error, "NAV", "gps lost"),
        (Level::Warn, "COM", "retry 1"),
        (Level::Error, "COM", "link down"),
        (Level::Warn, "NAV", "gps weak"),
        (Level::Info, "COM", "link up"),
    ];
    for (i, (level, apid, text)) in rows.into_iter().enumerate() {
        let mut m = Msg::log(level, "ECU1", apid, "MAIN", vec![Value::Str(text.into()).into()]);
        m.secs += i as u32;
        m.mcnt = i as u8;
        m.encode(&mut buf);
    }
    let f = d.join("trace.dlt");
    std::fs::write(&f, buf).unwrap();
    f
}

fn nerka(args: &[&str]) -> Output {
    // Keep the user's config (packs, signals) out of it.
    let home = std::env::temp_dir().join(format!("nerka-cli-home-{}", std::process::id()));
    Command::new(env!("CARGO_BIN_EXE_nerka"))
        .args(args)
        .env("APPDATA", &home)
        .env("LOCALAPPDATA", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("XDG_CACHE_HOME", &home)
        .output()
        .expect("run nerka")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).replace("\r\n", "\n")
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn filter_prints_numbered_lines_and_counts() {
    let d = dir("filter");
    let f = log_file(&d);
    let o = nerka(&["filter", "level >= ERROR", s(&f)]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        out(&o),
        "3:2026-11-07 10:00:02 [ERROR] net down: timeout\n5:2026-11-07 10:00:04 [ERROR] net down: timeout\n"
    );
    let o = nerka(&["filter", "-c", "level >= WARN AND raw ~ /disk|timeout/", s(&f)]);
    assert_eq!(out(&o), "3\n");
    let o = nerka(&["filter", "-N", "raw ~ /żółw/", s(&f)]);
    assert_eq!(out(&o), "2026-11-07 10:00:01 [WARN] disk 91% żółw\n", "non-ASCII text comes out as it is");
}

#[test]
fn no_match_is_success_with_no_output() {
    // Today "no match" exits 0, unlike grep (finding R6); this pins it so a
    // change is deliberate.
    let d = dir("nomatch");
    let o = nerka(&["filter", "raw ~ /nothing here/", s(&log_file(&d))]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(out(&o), "");
}

#[test]
fn search_like_grep() {
    let d = dir("search");
    let f = log_file(&d);
    let o = nerka(&["search", "-i", "-c", "NET DOWN", s(&f)]);
    assert_eq!(out(&o), "2\n");
    let o = nerka(&["search", "-v", "-c", "INFO", s(&f)]);
    assert_eq!(out(&o), "3\n");
    let o = nerka(&["search", "-F", "-c", "91%", s(&f)]);
    assert_eq!(out(&o), "1\n");
    let o = nerka(&["search", "--in", "level >= ERROR", "-c", "timeout", s(&f)]);
    assert_eq!(out(&o), "2\n");
}

#[test]
fn lines_prints_a_one_based_range() {
    let d = dir("lines");
    let o = nerka(&["lines", s(&log_file(&d)), "2", "3"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("[WARN] disk") && out(&o).contains("[ERROR] net down"), "{}", out(&o));
    assert_eq!(out(&o).lines().count(), 2);
}

#[test]
fn query_errors_point_at_the_column_in_characters() {
    // Finding R5: the message's column and the caret agree after non-ASCII.
    let d = dir("errcol");
    let q = r#"raw ~ "żółw" AND lvl >= ERROR"#;
    let o = nerka(&["filter", q, s(&log_file(&d))]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(e.contains("unknown field 'lvl'") && e.contains("(at column 18)"), "{e}");
    let caret = e.lines().last().unwrap();
    assert_eq!(caret.find('^'), Some(2 + 17), "two spaces of indent, then the column: {e}");
}

#[test]
fn errors_exit_1_with_a_reason() {
    let d = dir("errors");
    let f = log_file(&d);
    let o = nerka(&["search", "(", s(&f)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("unclosed group"), "{}", err(&o));
    let missing = d.join("missing.log");
    let o = nerka(&["filter", "level >= ERROR", s(&missing)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("opening") && err(&o).contains("missing.log"), "{}", err(&o));
    // Bad usage is clap's: exit 2.
    let o = nerka(&["filter"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn dlt_fields_are_listed_and_filtered() {
    let d = dir("dlt");
    let f = dlt_file(&d);
    let o = nerka(&["open", s(&f)]);
    assert!(o.status.success(), "{}", err(&o));
    let fields = out(&o).lines().find(|l| l.starts_with("fields:")).map(str::to_owned).unwrap_or_default();
    for name in ["ecu", "apid", "ctid", "payload"] {
        assert!(fields.contains(name), "{fields}");
    }
    let o = nerka(&["filter", "-c", "apid == NAV AND level >= WARN", s(&f)]);
    assert_eq!(out(&o), "2\n");
    let o = nerka(&["filter", "-N", "apid == COM AND payload ~ /link/", s(&f)]);
    let rows: Vec<_> = out(&o).lines().map(str::to_owned).collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].contains("link down") && rows[1].contains("link up"), "{rows:?}");
}

/// The DLT file's messages (storage headers dropped) as UDP datagrams to
/// `port` in a pcap.
fn dlt_pcap(d: &Path, port: u16) -> PathBuf {
    let dlt = std::fs::read(dlt_file(d)).unwrap();
    let mut payloads = Vec::new();
    let mut pos = 0;
    while pos + 20 <= dlt.len() {
        let len = u16::from_be_bytes([dlt[pos + 18], dlt[pos + 19]]) as usize;
        payloads.push(dlt[pos + 16..pos + 16 + len].to_vec());
        pos += 16 + len;
    }
    udp_pcap(&d.join("trace.pcap"), port, &payloads)
}

/// A pcap of IPv4 / UDP datagrams to `port`, one per payload.
fn udp_pcap(p: &Path, port: u16, payloads: &[Vec<u8>]) -> PathBuf {
    let mut out = Vec::new();
    out.extend_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    for x in [0u32, 0, 262_144, 1] {
        out.extend_from_slice(&x.to_le_bytes());
    }
    for payload in payloads {
        let mut f = vec![2; 12];
        f.extend_from_slice(&[0x08, 0x00, 0x45, 0]);
        f.extend_from_slice(&((28 + payload.len()) as u16).to_be_bytes());
        f.extend_from_slice(&[0, 1, 0, 0, 64, 17, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2]);
        f.extend_from_slice(&50000u16.to_be_bytes());
        f.extend_from_slice(&port.to_be_bytes());
        f.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(payload);
        for x in [0u32, 0, f.len() as u32, f.len() as u32] {
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.extend_from_slice(&f);
    }
    std::fs::write(p, out).unwrap();
    p.to_path_buf()
}

/// The example definitions in `examples/protocols` stay valid: the sensor
/// one as README shows it, the DLT one in the test above.
#[test]
fn the_example_sensor_definition_reads_as_documented() {
    let d = dir("sensor");
    // sensor u2, seq u4, kind u1, value s2 (tenths), a note: little-endian.
    let reading = |sensor: u16, seq: u32, kind: u8, value: i16, note: &str| {
        let mut m = sensor.to_le_bytes().to_vec();
        m.extend_from_slice(&seq.to_le_bytes());
        m.push(kind);
        m.extend_from_slice(&value.to_le_bytes());
        m.extend_from_slice(note.as_bytes());
        m
    };
    let payloads = [
        reading(7, 1, 1, 215, "ok"),
        reading(7, 2, 2, 953, "ciśnienie"),
        reading(8, 3, 2, 412, ""),
        reading(8, 4, 1, -40, "cold"),
    ];
    let pcap = udp_pcap(&d.join("sensor.pcap"), 41000, &payloads);
    let ksy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/protocols/sensor.ksy");
    let o = nerka(&["--protocol", s(&ksy), "filter", "kind == pressure AND value > 90", s(&pcap)]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(lines[0].starts_with("2:") && lines[0].ends_with("\t7\tpressure\t95.3\tciśnienie"), "{}", lines[0]);
    let o = nerka(&["--protocol", s(&ksy), "filter", "-c", "level >= ERROR", s(&pcap)]);
    assert_eq!(out(&o), "1\n", "{}", err(&o));
    let o = nerka(&["--protocol", s(&ksy), "filter", "-c", "value < 0 AND note == cold", s(&pcap)]);
    assert_eq!(out(&o), "1\n", "{}", err(&o));
}

#[test]
fn a_protocol_definition_reads_a_capture() {
    let d = dir("ksy");
    let pcap = dlt_pcap(&d, 40000);
    let ksy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/protocols/dlt.ksy");
    let def = ["--protocol", s(&ksy), "--protocol-port", "DLTK=40000"];
    let o = nerka(&[&def[..], &["filter", "-c", "proto == DLTK AND apid == NAV AND level >= WARN", s(&pcap)]].concat());
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "2\n");
    // Without the definition the port carries nothing Nerka reads.
    let o = nerka(&["filter", "-c", "level >= WARN", s(&pcap)]);
    assert_eq!(out(&o), "0\n", "{}", err(&o));
    // Rules for native DLT too: the same datagrams, its reader.
    let o = nerka(&["--route", "dlt:port=40000,dst=10.0.0.0/8", "filter", "-c", "proto == DLT", s(&pcap)]);
    assert_eq!(out(&o), "6\n", "{}", err(&o));
    let o = nerka(&["--route", "DLT:port=40000,src=192.168.0.0/16", "filter", "-c", "level >= WARN", s(&pcap)]);
    assert_eq!(out(&o), "0\n", "a rule on another source matches nothing: {}", err(&o));
    let o = nerka(&[&def[..], &["--disable-protocol", "DLTK", "filter", "-c", "level >= WARN", s(&pcap)]].concat());
    assert_eq!(out(&o), "0\n", "{}", err(&o));
    // A rule for a name no protocol has, a bad rule, a broken definition:
    // exit 1 with why.
    let o = nerka(&["--protocol", s(&ksy), "--protocol-port", "OTHER=1", "open", s(&pcap)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("no protocol is named OTHER"), "{}", err(&o));
    let o = nerka(&["--route", "DLT:port=1,vlan=9999", "open", s(&pcap)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("--route DLT:port=1,vlan=9999: VLAN id 9999 isn't 1 to 4094"), "{}", err(&o));
    let broken = d.join("broken.ksy");
    std::fs::write(&broken, "meta: { id: x }\nseq:\n  - { id: a, type: u3 }\n").unwrap();
    let o = nerka(&["--protocol", s(&broken), "open", s(&pcap)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("broken.ksy: line 3, column 5: there is no type `u3` here"), "{}", err(&o));
}

#[test]
fn checks_report_issues_and_strict_fails() {
    let d = dir("check");
    let f = dlt_file(&d);
    let spec = "errs: lines level >= ERROR; exists";
    let o = nerka(&["--check", spec, "check", s(&f)]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("errs") && out(&o).contains("2x"), "{}", out(&o));
    let o = nerka(&["--check", spec, "check", "--strict", s(&f)]);
    assert_eq!(o.status.code(), Some(1), "--strict fails on an issue");
    let o = nerka(&["--check", "none: lines apid == XYZ; exists", "check", "--strict", s(&f)]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = nerka(&["--check", spec, "check", "--json", s(&f)]);
    assert!(out(&o).trim_start().starts_with(['{', '[']) && out(&o).contains("\"errs\""), "{}", out(&o));
}

/// `nerka` with its own config home, `home/nerka` (Windows and Unix).
fn nerka_at(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nerka"))
        .args(args)
        .env("APPDATA", home)
        .env("LOCALAPPDATA", home)
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_CACHE_HOME", home)
        .output()
        .expect("run nerka")
}

#[test]
fn export_and_import_a_settings_bundle() {
    let d = dir("bundle");
    let (mine, theirs) = (d.join("mine"), d.join("theirs"));
    let cfg = mine.join("nerka");
    std::fs::create_dir_all(cfg.join("packs")).unwrap();
    std::fs::write(cfg.join("packs/vision.nerka-pack"), "pack vision\n\nquery cam\n    apid == CAM\n").unwrap();
    std::fs::write(cfg.join("checks"), "storm: lines level >= ERROR; rate 25 per 1s\nlost: lines /x/; exists\n")
        .unwrap();
    let bundle = d.join("mine.nerka-bundle");
    let o = nerka_at(&mine, &["export", "-o", s(&bundle), "--only", "packs", "--only", "check:storm"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(err(&o), "nerka: exported 1 pack, 1 check\n");
    let text = std::fs::read_to_string(&bundle).unwrap();
    assert!(text.starts_with("nerka-bundle 1\n"), "{text}");
    assert!(text.contains("@@ check storm format 1\n") && !text.contains("lost"), "{text}");
    // An earlier nerka's folder got its format file.
    assert!(cfg.join("format").is_file());

    // Theirs has another storm.
    let tcfg = theirs.join("nerka");
    std::fs::create_dir_all(&tcfg).unwrap();
    std::fs::write(tcfg.join("checks"), "storm: lines level >= WARN; exists\n").unwrap();
    let o = nerka_at(&theirs, &["import", "--dry-run", s(&bundle)]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        out(&o),
        "new        pack vision: add\ndifferent  check storm: keep both (as storm_imported)\ndry run: 2 items would be written\n"
    );
    assert!(!tcfg.join("packs").exists());
    let o = nerka_at(&theirs, &["import", s(&bundle)]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).ends_with("imported: 1 added, 1 kept both\n"), "{}", out(&o));
    assert_eq!(
        std::fs::read_to_string(tcfg.join("checks")).unwrap(),
        "storm: lines level >= WARN; exists\nstorm_imported: lines level >= ERROR; rate 25 per 1s\n"
    );
    let o = nerka_at(&theirs, &["packs"]);
    assert!(out(&o).starts_with("vision  ("), "{}", out(&o));
    // Taking theirs replaces the line.
    let o = nerka_at(&theirs, &["import", "--conflicts", "theirs", s(&bundle)]);
    assert!(out(&o).contains("same       pack vision: —\n"), "{}", out(&o));
    assert!(out(&o).contains("imported: 1 replaced (replaced ones backed up to "), "{}", out(&o));
    let checks = std::fs::read_to_string(tcfg.join("checks")).unwrap();
    assert!(checks.starts_with("storm: lines level >= ERROR; rate 25 per 1s\n"), "{checks}");
}

#[test]
fn import_says_what_it_cant_read() {
    let d = dir("bundle-bad");
    let home = d.join("home");
    let bad = d.join("bad.nerka-bundle");
    std::fs::write(&bad, "nerka-bundle 1\n@@ check ok format 1\nok: lines /x/; exists\n@@ check no format 9\nno: x\n")
        .unwrap();
    let o = nerka_at(&home, &["import", s(&bad)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("new        check ok: add\n"), "{}", out(&o));
    assert!(out(&o).contains("invalid    check no: made by a newer nerka: check format 9"), "{}", out(&o));
    assert_eq!(err(&o), "nerka: 1 invalid item(s) not imported\n");
    assert!(std::fs::read_to_string(home.join("nerka/checks")).unwrap().contains("ok: lines /x/; exists"));
    std::fs::write(&bad, "pack vision\n").unwrap();
    let o = nerka_at(&home, &["import", s(&bad)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("bad.nerka-bundle:1:1: not a nerka bundle"), "{}", err(&o));
    let o = nerka_at(&home, &["export", "--only", "prefs"]);
    assert!(err(&o).contains("--only prefs: 'prefs' isn't pack, preset, plot, check or signal"), "{}", err(&o));
    let o = nerka_at(&home, &["export", "--only", "check:nope"]);
    assert!(err(&o).contains("--only check:nope: no such check"), "{}", err(&o));
}

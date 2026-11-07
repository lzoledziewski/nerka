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

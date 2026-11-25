//! 🤖 AI terminal (owner, 2026-11-24: the user's own terminal, not one in
//! the window): the AI command the user chose (Settings → AI command:
//! `claude`, `codex`, `gemini`, …) in a terminal of the system, in the
//! folder of the active tab's file, with `NERKA_WINDOW` set to this
//! window's process, so `nerka mcp` works on this window. For Claude Code
//! the command gets `--mcp-config` with a file that runs `nerka mcp` (next
//! to nerka-gui), so nothing has to be installed first; other clients need
//! `nerka mcp --install CLIENT` once.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The command line the terminal runs: `NERKA_WINDOW` set, then `command`
/// (with `--mcp-config` for Claude Code when `mcp_config` is given).
pub fn inner_command(command: &str, pid: u32, mcp_config: Option<&Path>, windows: bool) -> String {
    let mut cmd = command.trim().to_owned();
    if let Some(cfg) = mcp_config
        && is_claude(&cmd)
    {
        cmd += &format!(" --mcp-config \"{}\"", cfg.display());
    }
    if windows {
        format!("set NERKA_WINDOW={pid}&& {cmd}")
    } else {
        // The shell stays when the command ends, as a terminal would.
        format!("export NERKA_WINDOW={pid}; {cmd}; exec \"${{SHELL:-sh}}\"")
    }
}

/// Whether `command` starts Claude Code.
fn is_claude(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or_default();
    let name = Path::new(first).file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    name.eq_ignore_ascii_case("claude")
}

/// The `nerka` command line next to this `nerka-gui`, if it is there.
fn nerka_cli() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    let cli = me.with_file_name(if cfg!(windows) { "nerka.exe" } else { "nerka" });
    cli.is_file().then_some(cli)
}

/// An MCP configuration that runs `nerka mcp`, written to the config folder
/// (`<config>/nerka/mcp.json`) for `--mcp-config`.
fn mcp_config_file() -> Option<PathBuf> {
    let cli = nerka_cli()?;
    let server = nerka_core::json::Value::object([
        ("command", nerka_core::json::Value::from(cli.display().to_string())),
        ("args", vec!["mcp"].into()),
    ]);
    let v = nerka_core::json::Value::object([("mcpServers", nerka_core::json::Value::object([("nerka", server)]))]);
    let path = nerka_core::config_dir()?.join("mcp.json");
    nerka_core::write_atomic(&path, v.to_string()).ok()?;
    Some(path)
}

/// Open the system's terminal in `dir` running `command` for this window.
/// An error says why none opened.
pub fn open(dir: &Path, command: &str) -> Result<(), String> {
    if command.trim().is_empty() {
        return Err("no AI command: set one in Settings → AI command (e.g. claude)".into());
    }
    let cfg = if is_claude(command) { mcp_config_file() } else { None };
    let inner = inner_command(command, std::process::id(), cfg.as_deref(), cfg!(windows));
    launch(dir, &inner)
}

#[cfg(windows)]
fn launch(dir: &Path, inner: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // Windows Terminal if there is one, else a console window of its own.
    let wt = Command::new("wt.exe").arg("-d").arg(dir).arg("cmd").raw_arg(format!("/s /k \"{inner}\"")).spawn();
    if wt.is_ok() {
        return Ok(());
    }
    Command::new("cmd")
        .raw_arg(format!("/c start \"nerka AI\" /D \"{}\" cmd /s /k \"{inner}\"", dir.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("couldn't start a terminal: {e}"))
}

#[cfg(target_os = "macos")]
fn launch(dir: &Path, inner: &str) -> Result<(), String> {
    let line = format!("cd \"{}\"; {inner}", dir.display()).replace('\\', "\\\\").replace('"', "\\\"");
    Command::new("osascript")
        .args([
            "-e",
            &format!("tell application \"Terminal\" to do script \"{line}\""),
            "-e",
            "tell application \"Terminal\" to activate",
        ])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("couldn't start Terminal: {e}"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn launch(dir: &Path, inner: &str) -> Result<(), String> {
    // $TERMINAL, then the usual ones; each takes the command its own way.
    let mut tried = Vec::new();
    let user = std::env::var("TERMINAL").ok().filter(|t| !t.trim().is_empty());
    for term in user.iter().map(String::as_str).chain([
        "x-terminal-emulator",
        "gnome-terminal",
        "konsole",
        "xfce4-terminal",
        "xterm",
    ]) {
        let mut c = Command::new(term);
        c.current_dir(dir);
        match term {
            "gnome-terminal" => c.args(["--", "sh", "-c", inner]),
            "xfce4-terminal" => c.args(["-x", "sh", "-c", inner]),
            _ => c.args(["-e", "sh", "-c", inner]),
        };
        match c.spawn() {
            Ok(_) => return Ok(()),
            Err(_) => tried.push(term.to_owned()),
        }
    }
    Err(format!("no terminal found (tried {}): set TERMINAL", tried.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_gets_the_window_and_claude_its_config() {
        let cfg = Path::new("C:\\Users\\u\\nerka\\mcp.json");
        assert_eq!(
            inner_command("claude", 42, Some(cfg), true),
            "set NERKA_WINDOW=42&& claude --mcp-config \"C:\\Users\\u\\nerka\\mcp.json\""
        );
        assert_eq!(inner_command("  codex ", 42, Some(cfg), true), "set NERKA_WINDOW=42&& codex");
        assert_eq!(inner_command("gemini", 7, None, false), "export NERKA_WINDOW=7; gemini; exec \"${SHELL:-sh}\"");
        assert!(is_claude("/usr/local/bin/claude --model x") && is_claude("Claude.exe") && !is_claude("claudette"));
    }

    /// A real terminal opens (run by hand: `cargo test -p nerka-gui -- --ignored terminal`):
    /// its command sees NERKA_WINDOW and runs in the folder given.
    #[test]
    #[ignore = "opens a terminal on the desktop"]
    fn a_terminal_opens_with_the_window() {
        let dir = std::env::temp_dir().join(format!("nerka-terminal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("seen.txt");
        // The environment as the command sees it (cmd expands %X% when it
        // reads the line, before `set` has run: `set` lists it as it is).
        let cmd = if cfg!(windows) { "set > seen.txt&& exit" } else { "env > seen.txt; exit" };
        open(&dir, cmd).unwrap();
        let start = std::time::Instant::now();
        while !out.is_file() && start.elapsed() < std::time::Duration::from_secs(20) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let seen = String::from_utf8_lossy(&std::fs::read(&out).expect("the terminal ran the command")).into_owned();
        assert!(seen.contains(&format!("NERKA_WINDOW={}", std::process::id())), "{seen}");
        std::fs::remove_dir_all(&dir).ok();
    }
}

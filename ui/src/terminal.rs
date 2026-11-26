//! 🤖 AI terminal (owner, 2026-11-24: the user's own terminal, not one in
//! the window): the AI command the user chose (Settings → AI command:
//! `claude`, `codex`, `gemini`, …) in a terminal of the system, in the
//! folder of the active tab's file, with `NERKA_WINDOW` set to this
//! window's process, so `nerka mcp` works on this window. For Claude Code
//! the command gets `--mcp-config` with a file that runs `nerka mcp` (next
//! to nerka-gui), GitHub Copilot CLI `--additional-mcp-config`, so nothing
//! has to be installed first; other clients need `nerka mcp --install
//! CLIENT` once. A command that isn't installed is said before a terminal
//! opens (owner, 2026-11-25: the Claude desktop app has no `claude` on
//! PATH).

use std::path::{Path, PathBuf};
use std::process::Command;

/// AI clients to pick in Settings → AI command (owner, 2026-11-25: a list
/// and Custom): their name and command. Each speaks MCP, so it can work on
/// the window.
pub const PRESETS: &[(&str, &str)] =
    &[("Claude Code", "claude"), ("GitHub Copilot CLI", "copilot"), ("Codex", "codex"), ("Gemini CLI", "gemini")];

/// How to install each preset's command line, for when it isn't found.
fn install_hint(program: &str) -> &'static str {
    match program.to_ascii_lowercase().as_str() {
        "claude" => {
            "install Claude Code's command line (https://claude.com/claude-code: the desktop app doesn't put one \
             on PATH)"
        }
        "copilot" => "install GitHub Copilot CLI (npm install -g @github/copilot)",
        "codex" => "install Codex (npm install -g @openai/codex)",
        "gemini" => "install Gemini CLI (npm install -g @google/gemini-cli)",
        _ => "install it, or give its full path",
    }
}

/// Where `program` is: a path as given, else on PATH or in the folders
/// installers put command lines in (a window started from the desktop may
/// not have the shell's PATH). On Windows with the extensions it may have.
pub fn find_program(program: &str) -> Option<PathBuf> {
    let given = Path::new(program);
    let exts: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        std::iter::once(String::new()).chain(pathext.split(';').map(|e| e.to_ascii_lowercase())).collect()
    } else {
        vec![String::new()]
    };
    let with_ext = |p: &Path| exts.iter().map(|e| PathBuf::from(format!("{}{e}", p.display()))).find(|c| c.is_file());
    if given.components().count() > 1 {
        return with_ext(given);
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(h) = &home {
        dirs.extend([h.join(".local").join("bin"), h.join(".claude").join("local"), h.join(".npm-global").join("bin")]);
    }
    if let Some(a) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(a).join("npm"));
    }
    dirs.extend(["/usr/local/bin", "/opt/homebrew/bin"].map(PathBuf::from));
    dirs.iter().find_map(|d| with_ext(&d.join(program)))
}

/// The command line the terminal runs: `NERKA_WINDOW` set, then `command`
/// (with `--mcp-config` for Claude Code, `--additional-mcp-config` for
/// Copilot, when `mcp_config` is given).
pub fn inner_command(command: &str, pid: u32, mcp_config: Option<&Path>, windows: bool) -> String {
    let mut cmd = command.trim().to_owned();
    if let Some(cfg) = mcp_config {
        match client(&cmd) {
            Client::Claude => cmd += &format!(" --mcp-config \"{}\"", cfg.display()),
            Client::Copilot => cmd += &format!(" --additional-mcp-config \"@{}\"", cfg.display()),
            Client::Other => {}
        }
    }
    if windows {
        format!("set NERKA_WINDOW={pid}&& {cmd}")
    } else {
        // The shell stays when the command ends, as a terminal would.
        format!("export NERKA_WINDOW={pid}; {cmd}; exec \"${{SHELL:-sh}}\"")
    }
}

/// The AI client a command starts, for the MCP configuration it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Client {
    Claude,
    Copilot,
    Other,
}

fn client(command: &str) -> Client {
    let first = command.split_whitespace().next().unwrap_or_default();
    let name = Path::new(first).file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    match name.to_ascii_lowercase().as_str() {
        "claude" => Client::Claude,
        "copilot" => Client::Copilot,
        _ => Client::Other,
    }
}

/// The `nerka` command line next to this `nerka-gui`, if it is there.
fn nerka_cli() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    let cli = me.with_file_name(if cfg!(windows) { "nerka.exe" } else { "nerka" });
    cli.is_file().then_some(cli)
}

/// An MCP configuration that runs `nerka mcp`, written to the config folder
/// (`<config>/nerka/mcp.json`; Copilot's, which names its type and tools,
/// `mcp-copilot.json`) for the client's flag.
fn mcp_config_file(client: Client) -> Option<PathBuf> {
    use nerka_core::json::Value;
    let cli = nerka_cli()?;
    let mut server = Value::object([("command", Value::from(cli.display().to_string())), ("args", vec!["mcp"].into())]);
    let name = match client {
        Client::Copilot => {
            server.push("type", "local").push("tools", vec!["*"]);
            "mcp-copilot.json"
        }
        _ => "mcp.json",
    };
    let v = Value::object([("mcpServers", Value::object([("nerka", server)]))]);
    let path = nerka_core::config_dir()?.join(name);
    nerka_core::write_atomic(&path, v.to_string()).ok()?;
    Some(path)
}

/// Open the system's terminal in `dir` running `command` for this window.
/// An error says why none opened.
pub fn open(dir: &Path, command: &str) -> Result<(), String> {
    if command.trim().is_empty() {
        return Err("no AI command: set one in Settings → AI command (e.g. claude)".into());
    }
    let program = command.split_whitespace().next().unwrap_or_default();
    if find_program(program).is_none() {
        return Err(format!(
            "'{program}' isn't installed (not on PATH): {}; or choose another in ⚙ Settings → AI command",
            install_hint(Path::new(program).file_stem().and_then(|s| s.to_str()).unwrap_or(program))
        ));
    }
    let cfg = match client(command) {
        Client::Other => None,
        c => mcp_config_file(c),
    };
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
        assert_eq!(client("/usr/local/bin/claude --model x"), Client::Claude);
        assert_eq!((client("Claude.exe"), client("claudette")), (Client::Claude, Client::Other));
        // Copilot: its own flag, the file prefixed with @.
        assert_eq!(
            inner_command("copilot", 42, Some(Path::new("C:\\n\\mcp-copilot.json")), true),
            "set NERKA_WINDOW=42&& copilot --additional-mcp-config \"@C:\\n\\mcp-copilot.json\""
        );
    }

    /// A command that isn't installed is said before a terminal opens, with
    /// how to install it (the Claude desktop app has no `claude` on PATH).
    #[test]
    fn a_command_not_installed_is_said() {
        let shell = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(find_program(shell).is_some(), "{shell} on PATH");
        assert!(find_program("nerka-no-such-client").is_none());
        let e = open(Path::new("."), "nerka-no-such-client --x").unwrap_err();
        assert!(e.starts_with("'nerka-no-such-client' isn't installed (not on PATH)"), "{e}");
        assert!(install_hint("claude").contains("claude.com/claude-code"));
        assert!(install_hint("copilot").contains("@github/copilot"));
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
        let cmd = if cfg!(windows) { "cmd /c set > seen.txt&& exit" } else { "env > seen.txt; exit" };
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

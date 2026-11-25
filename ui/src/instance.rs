//! Windows as sessions: `nerka-gui FILE…` hands its files to the nerka
//! window focused last, which opens them as tabs and comes to the front;
//! with `--new-window` (or no files) a new window starts.
//!
//! Every window listens on a loopback TCP port and registers it, with a
//! random token, in `<config>/nerka/instances/<pid>`; it rewrites that file
//! when it gains focus, so the newest file is the window focused last. A
//! request is one connection: `nerka1 <token>`, then `open\t<path>` lines,
//! an optional `preset\t<path>` (a tab moved here: its filter, search and
//! selection), then `end`; the window answers `ok`. Only std, no dependency.
//!
//! Next to the registration, `<pid>.label` says what the window shows (its
//! name, its active tab), so a tab's menu can list the other windows to
//! move it to.
//!
//! Each connection is served on its own thread (at most [`MAX_SERVING`] at
//! once) and has [`REQUEST_TIME`] for its whole request, so a client that
//! connects and sends nothing, or a byte at a time, can't hold up the next
//! hand-off (review 2026-11-07 R17).
//!
//! **Agents** (`nerka mcp`, `docs/dev/ai.md` workflow B): after the token,
//! the line `agent` keeps the connection open; the client sends a JSON call
//! a line, `{"id": …, "tool": "count", "args": {…}}`, and the window
//! answers each with a line `{"id": …, "result": …}` or `{"id": …,
//! "error": "…"}`, in order. Only the hello has a deadline then: a call may
//! take as long as its work ([`AGENT_TIME`] at most).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime};

/// The hello of every connection (`nerka_core::link` speaks it too).
const MAGIC: &str = nerka_core::link::MAGIC;
/// Bytes a request may have at most.
const MAX_REQUEST: u64 = 1 << 20;
/// Time a request may take from connecting to `end`, however it trickles in.
const REQUEST_TIME: Duration = Duration::from_secs(2);
/// Connections served at once; more are closed at once.
const MAX_SERVING: usize = 8;
/// The longest an agent's call may take before the window is said not to
/// answer.
const AGENT_TIME: Duration = nerka_core::link::AGENT_TIME;

fn dir() -> Option<PathBuf> {
    nerka_core::link::instances_dir()
}

/// A token that other users' processes can't guess (they can't read the
/// per-user config folder).
fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..4u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos());
        h.write_u32(std::process::id());
        out += &format!("{:016x}", h.finish());
    }
    out
}

/// What another `nerka-gui` (or an agent) asked this window for.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Open(Vec<PathBuf>),
    /// A tab moved here: its files, and a preset file with its filter,
    /// search and selection (for the receiver to read and remove).
    Move {
        files: Vec<PathBuf>,
        preset: PathBuf,
    },
    /// An agent's call (`nerka mcp`).
    Agent(AgentCall),
}

/// A tool an agent calls on this window, and where its answer goes: the
/// result, or why there is none.
#[derive(Debug, Clone)]
pub struct AgentCall {
    pub tool: String,
    pub args: nerka_core::json::Value,
    pub reply: mpsc::Sender<Result<nerka_core::json::Value, String>>,
}

impl PartialEq for AgentCall {
    fn eq(&self, other: &Self) -> bool {
        (&self.tool, &self.args) == (&other.tool, &other.args)
    }
}

/// This window's listener and registration.
pub struct Server {
    rx: mpsc::Receiver<Request>,
    file: PathBuf,
    line: String,
    focused: bool,
    label: String,
}

/// Another running window, as its label file describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub label: String,
    registration: PathBuf,
}

impl Server {
    /// Listen and register. `wake` is called after a request arrives (to
    /// repaint). `None` if this system has no config folder or no loopback.
    pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Option<Server> {
        let dir = dir()?;
        std::fs::create_dir_all(&dir).ok()?;
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).ok()?;
        let port = listener.local_addr().ok()?.port();
        let token = new_token();
        let file = dir.join(std::process::id().to_string());
        let line = format!("{port} {token}\n");
        nerka_core::write_atomic(&file, &line).ok()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || listen(listener, token, tx, wake));
        Some(Server { rx, file, line, focused: true, label: String::new() })
    }

    /// Say what this window shows, for other windows' Move to window list.
    pub fn set_label(&mut self, label: &str) {
        let label: String = label.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        if label != self.label {
            let _ = nerka_core::write_atomic(label_file(&self.file), &label);
            self.label = label;
        }
    }

    /// Requests that arrived since the last call.
    pub fn poll(&self) -> Vec<Request> {
        self.rx.try_iter().collect()
    }

    /// Call every frame with whether the window has focus: gaining focus
    /// makes this the window that new files go to.
    pub fn set_focused(&mut self, focused: bool) {
        if focused && !self.focused {
            let _ = nerka_core::write_atomic(&self.file, &self.line);
        }
        self.focused = focused;
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
        let _ = std::fs::remove_file(label_file(&self.file));
    }
}

fn label_file(registration: &Path) -> PathBuf {
    registration.with_extension("label")
}

/// The other windows running, by label.
pub fn others() -> Vec<Window> {
    dir().map(|d| others_in(&d, std::process::id())).unwrap_or_default()
}

fn others_in(dir: &Path, own: u32) -> Vec<Window> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Window> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).and_then(|n| n.parse::<u32>().ok()).is_some_and(|n| n != own)
        })
        .map(|registration| {
            let label = std::fs::read_to_string(label_file(&registration)).unwrap_or_default();
            let label = if label.trim().is_empty() { "nerka".to_owned() } else { label.trim().to_owned() };
            Window { label, registration }
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Give a tab to `window`: its files and the preset file of its filter,
/// search and selection. An error says why it didn't take them; a window
/// that is gone is dropped from the list.
pub fn move_to(window: &Window, files: &[PathBuf], preset: &Path) -> Result<(), String> {
    match send(&window.registration, files, Some(preset)) {
        Some(true) => Ok(()),
        Some(false) => Err(format!("{} didn't take it (an older nerka?)", window.label)),
        None => {
            let _ = std::fs::remove_file(&window.registration);
            let _ = std::fs::remove_file(label_file(&window.registration));
            Err(format!("{} is gone", window.label))
        }
    }
}

/// Serve every connection to `listener` on a thread of its own (at most
/// [`MAX_SERVING`] at once); each request that arrives whole goes to `tx`,
/// then `wake` is called.
fn listen(listener: TcpListener, token: String, tx: mpsc::Sender<Request>, wake: impl Fn() + Send + Sync + 'static) {
    let (token, wake) = (Arc::new(token), Arc::new(wake));
    let serving = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        if serving.fetch_add(1, Ordering::SeqCst) >= MAX_SERVING {
            serving.fetch_sub(1, Ordering::SeqCst);
            continue; // dropped: closed
        }
        let (tx, token, wake, serving) = (tx.clone(), token.clone(), wake.clone(), serving.clone());
        std::thread::spawn(move || {
            let deliver = |req: Request| {
                if tx.send(req).is_ok() {
                    wake();
                }
            };
            serve(stream, &token, Instant::now() + REQUEST_TIME, &deliver);
            serving.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// A stream that may be read until a deadline (none: as long as it
/// takes): each read waits at most the time left, then reads fail.
struct Until {
    stream: TcpStream,
    deadline: Option<Instant>,
}

impl Read for Until {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.deadline.map(|d| d.saturating_duration_since(Instant::now()));
        if left.is_some_and(|l| l.is_zero()) {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(left)?;
        self.stream.read(buf)
    }
}

/// A line of at most [`MAX_REQUEST`] bytes, without its line end; `None`
/// at the end of the stream, on an error, or for a longer line.
fn read_line(r: &mut BufReader<Until>) -> Option<String> {
    let mut buf = Vec::new();
    let n = r.by_ref().take(MAX_REQUEST).read_until(b'\n', &mut buf).ok()?;
    if n == 0 || (buf.last() != Some(&b'\n') && n as u64 >= MAX_REQUEST) {
        return None;
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    String::from_utf8(buf).ok()
}

#[cfg(test)]
fn serve_one(stream: TcpStream, token: &str) -> Option<Request> {
    serve_until(stream, token, Instant::now() + REQUEST_TIME)
}

/// The one request a hand-off connection makes (tests).
#[cfg(test)]
fn serve_until(stream: TcpStream, token: &str, deadline: Instant) -> Option<Request> {
    let got = std::cell::RefCell::new(None);
    serve(stream, token, deadline, &|r| *got.borrow_mut() = Some(r));
    got.into_inner()
}

/// Serve one connection: a hand-off (`open` / `preset` lines, `end`) gives
/// one request; an agent's (`agent`) a request per call, each answered on
/// the connection once the window has answered it.
fn serve(stream: TcpStream, token: &str, deadline: Instant, deliver: &dyn Fn(Request)) {
    let Ok(mut out) = stream.try_clone() else { return };
    let mut r = BufReader::new(Until { stream, deadline: Some(deadline) });
    let Some(hello) = read_line(&mut r) else { return };
    let Some((magic, got)) = hello.split_once(' ') else { return };
    // Compare without stopping at the first difference.
    let same = got.len() == token.len() && got.bytes().zip(token.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0;
    if magic != MAGIC || !same {
        return;
    }
    let (mut files, mut preset) = (Vec::new(), None);
    while let Some(line) = read_line(&mut r) {
        match line.split_once('\t') {
            Some(("open", path)) if !path.is_empty() => files.push(PathBuf::from(path)),
            Some(("preset", path)) if !path.is_empty() && preset.is_none() => preset = Some(PathBuf::from(path)),
            _ if line == "agent" && files.is_empty() && preset.is_none() => {
                r.get_mut().deadline = None;
                if out.write_all(b"ok\n").is_ok() {
                    serve_agent(&mut r, &mut out, deliver);
                }
                return;
            }
            _ if line == "end" => {
                let _ = out.write_all(b"ok\n");
                deliver(match preset {
                    Some(preset) => Request::Move { files, preset },
                    None => Request::Open(files),
                });
                return;
            }
            _ => return,
        }
    }
}

/// An agent's calls, one a line, each answered before the next is read.
fn serve_agent(r: &mut BufReader<Until>, out: &mut TcpStream, deliver: &dyn Fn(Request)) {
    use nerka_core::json::{self, Value};
    while let Some(line) = read_line(r) {
        if line.trim().is_empty() {
            continue;
        }
        let (id, answer) = match json::parse(&line) {
            Err(e) => (Value::Null, Err(format!("not JSON: {e}"))),
            Ok(call) => {
                let id = call.get("id").cloned().unwrap_or(Value::Null);
                match call.get("tool").and_then(Value::as_str) {
                    None => (id, Err("no \"tool\" in the call".to_owned())),
                    Some(tool) => {
                        let args = call.get("args").cloned().unwrap_or_else(Value::new_object);
                        let (tx, rx) = mpsc::channel();
                        deliver(Request::Agent(AgentCall { tool: tool.to_owned(), args, reply: tx }));
                        let answer = rx.recv_timeout(AGENT_TIME).unwrap_or_else(|e| {
                            Err(match e {
                                mpsc::RecvTimeoutError::Timeout => "the window didn't answer in time".to_owned(),
                                mpsc::RecvTimeoutError::Disconnected => "the window dropped the call".to_owned(),
                            })
                        });
                        (id, answer)
                    }
                }
            }
        };
        let mut v = Value::object([("id", id)]);
        match answer {
            Ok(result) => v.push("result", result),
            Err(e) => v.push("error", e),
        };
        if writeln!(out, "{v}").is_err() {
            return;
        }
    }
}

/// Give `files` to the window focused last. Returns false if no window
/// took them (none running, or none answered): then start a new one.
pub fn hand_off(files: &[PathBuf]) -> bool {
    let Some(dir) = dir() else { return false };
    let Ok(rd) = std::fs::read_dir(&dir) else { return false };
    let mut entries: Vec<(SystemTime, PathBuf)> = rd
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(_, p)| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.parse::<u32>().is_ok()))
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.0));
    // Absolute paths: the window runs in another directory.
    let files: Vec<PathBuf> = files.iter().map(|f| std::path::absolute(f).unwrap_or_else(|_| f.clone())).collect();
    for (_, path) in entries {
        match send(&path, &files, None) {
            Some(true) => return true,
            Some(false) => {}
            // Nothing listens there: a window that didn't close cleanly.
            None => {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(label_file(&path));
            }
        }
    }
    false
}

/// `None`: nobody listens; `Some(false)`: it didn't accept.
fn send(registration: &Path, files: &[PathBuf], preset: Option<&Path>) -> Option<bool> {
    let text = std::fs::read_to_string(registration).ok()?;
    let (port, token) = text.lines().next()?.trim().split_once(' ')?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port.parse::<u16>().ok()?));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(300)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    let mut msg = format!("{MAGIC} {token}\n");
    for f in files {
        let p = f.display().to_string();
        if p.contains(['\n', '\r']) {
            return Some(false);
        }
        msg += &format!("open\t{p}\n");
    }
    if let Some(preset) = preset {
        let p = preset.display().to_string();
        if p.contains(['\n', '\r']) {
            return Some(false);
        }
        msg += &format!("preset\t{p}\n");
    }
    msg += "end\n";
    if s.write_all(msg.as_bytes()).is_err() {
        return Some(false);
    }
    let mut reply = String::new();
    let ok = BufReader::new(s).read_line(&mut reply).is_ok() && reply.trim() == "ok";
    Some(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_needs_the_token_and_ends_with_end() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let addr = listener.local_addr().unwrap();
        let ask = |msg: &'static str| {
            std::thread::spawn(move || {
                let mut s = TcpStream::connect(addr).unwrap();
                s.write_all(msg.as_bytes()).unwrap();
                s.shutdown(std::net::Shutdown::Write).unwrap();
                let mut reply = String::new();
                let _ = s.read_to_string(&mut reply);
                reply
            })
        };
        let cases = [
            (
                "nerka1 secret\nopen\t/a.log\nopen\t/b b.dlt\nend\n",
                Some(Request::Open(vec!["/a.log".into(), "/b b.dlt".into()])),
                "ok\n",
            ),
            (
                "nerka1 secret\nopen\t/a.log\npreset\t/tmp/m.nerka-preset\nend\n",
                Some(Request::Move { files: vec!["/a.log".into()], preset: "/tmp/m.nerka-preset".into() }),
                "ok\n",
            ),
            ("nerka1 secret\nopen\t/a.log\npreset\t/x\npreset\t/y\nend\n", None, ""),
            ("nerka1 wrong!\nopen\t/a.log\nend\n", None, ""),
            ("nerka1 secret\nopen\t/a.log\n", None, ""),
            ("nerka1 secret\nrm\t/a.log\nend\n", None, ""),
        ];
        for (msg, want, reply) in cases {
            let client = ask(msg);
            let (stream, _) = listener.accept().unwrap();
            assert_eq!(serve_one(stream, "secret"), want, "{msg:?}");
            assert_eq!(client.join().unwrap(), reply, "{msg:?}");
        }
    }

    /// Review 2026-11-07 R17: a request that trickles in a byte at a time
    /// is cut off at its deadline, not served for as long as it keeps going.
    #[test]
    fn a_slow_request_ends_at_its_deadline() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            for b in b"nerka1 secret\nopen\t/a.log\nend\n" {
                if s.write_all(&[*b]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let (stream, _) = listener.accept().unwrap();
        let start = Instant::now();
        assert_eq!(serve_until(stream, "secret", start + Duration::from_millis(300)), None);
        let took = start.elapsed();
        assert!(took < Duration::from_millis(1000), "served for {took:?}");
        client.join().unwrap();
    }

    /// Other windows are listed by their labels; this one and label files
    /// themselves are not windows.
    #[test]
    fn other_windows_by_label() {
        let dir = std::env::temp_dir().join(format!("nerka-instances-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (pid, label) in [("10", Some("OTA crash · a.dlt")), ("11", Some("b.log")), ("12", None), ("13", Some("me"))]
        {
            std::fs::write(dir.join(pid), "1 t\n").unwrap();
            if let Some(l) = label {
                std::fs::write(dir.join(format!("{pid}.label")), l).unwrap();
            }
        }
        let labels: Vec<String> = others_in(&dir, 13).into_iter().map(|w| w.label).collect();
        assert_eq!(labels, ["OTA crash · a.dlt", "b.log", "nerka"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An agent's connection stays open: a JSON call a line, each answered
    /// in order with its id; a bad line gets an error, not the end.
    #[test]
    fn an_agent_connection_answers_each_call() {
        use nerka_core::json::{Value, parse};
        let (tx, rx) = mpsc::channel();
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || listen(listener, "secret".into(), tx, || {}));
        // The window: answers each call with its tool's name.
        std::thread::spawn(move || {
            for r in rx {
                if let Request::Agent(call) = r {
                    let answer =
                        if call.tool == "fail" { Err("no".into()) } else { Ok(Value::from(call.tool.as_str())) };
                    let _ = call.reply.send(answer);
                }
            }
        });
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"nerka1 secret\nagent\n").unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert_eq!(line, "ok\n");
        // Slower than a hand-off's deadline: the connection stays.
        std::thread::sleep(REQUEST_TIME + Duration::from_millis(200));
        for (call, want) in [
            (r#"{"id":1,"tool":"count","args":{}}"#, r#"{"id":1,"result":"count"}"#),
            ("not json", r#"{"id":null,"error":"not JSON: not a JSON value at byte 0"}"#),
            (r#"{"id":"x","tool":"fail"}"#, r#"{"id":"x","error":"no"}"#),
            (r#"{"id":3}"#, r#"{"id":3,"error":"no \"tool\" in the call"}"#),
        ] {
            writeln!(s, "{call}").unwrap();
            line.clear();
            r.read_line(&mut line).unwrap();
            assert_eq!(parse(line.trim()).unwrap(), parse(want).unwrap(), "{call}");
        }
    }

    /// While one client holds a connection open, another is served.
    #[test]
    fn a_stalled_client_does_not_block_the_next() {
        let (tx, rx) = mpsc::channel();
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || listen(listener, "secret".into(), tx, || {}));
        let _stalled = TcpStream::connect(addr).unwrap(); // sends nothing
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"nerka1 secret\nopen\t/a.log\nend\n").unwrap();
        let got = rx.recv_timeout(Duration::from_millis(1000));
        assert_eq!(got.ok(), Some(Request::Open(vec!["/a.log".into()])));
    }
}

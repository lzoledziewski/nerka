//! Windows as sessions: `nerka-gui FILE…` hands its files to the nerka
//! window focused last, which opens them as tabs and comes to the front;
//! with `--new-window` (or no files) a new window starts.
//!
//! Every window listens on a loopback TCP port and registers it, with a
//! random token, in `<config>/nerka/instances/<pid>`; it rewrites that file
//! when it gains focus, so the newest file is the window focused last. A
//! request is one connection: `nerka1 <token>`, then `open\t<path>` lines,
//! then `end`; the window answers `ok`. Only std, no dependency.
//!
//! Each connection is served on its own thread (at most [`MAX_SERVING`] at
//! once) and has [`REQUEST_TIME`] for its whole request, so a client that
//! connects and sends nothing, or a byte at a time, can't hold up the next
//! hand-off (review 2026-11-07 R17).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime};

const MAGIC: &str = "nerka1";
/// Bytes a request may have at most.
const MAX_REQUEST: u64 = 1 << 20;
/// Time a request may take from connecting to `end`, however it trickles in.
const REQUEST_TIME: Duration = Duration::from_secs(2);
/// Connections served at once; more are closed at once.
const MAX_SERVING: usize = 8;

fn dir() -> Option<PathBuf> {
    nerka_core::config_dir().map(|d| d.join("instances"))
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

/// What another `nerka-gui` asked this window for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Open(Vec<PathBuf>),
}

/// This window's listener and registration.
pub struct Server {
    rx: mpsc::Receiver<Request>,
    file: PathBuf,
    line: String,
    focused: bool,
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
        Some(Server { rx, file, line, focused: true })
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
            if let Some(req) = serve_one(stream, &token)
                && tx.send(req).is_ok()
            {
                wake();
            }
            serving.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// A stream that may be read until a deadline: each read waits at most the
/// time left, then reads fail.
struct Until {
    stream: TcpStream,
    deadline: Instant,
}

impl Read for Until {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

fn serve_one(stream: TcpStream, token: &str) -> Option<Request> {
    serve_until(stream, token, Instant::now() + REQUEST_TIME)
}

fn serve_until(stream: TcpStream, token: &str, deadline: Instant) -> Option<Request> {
    let mut out = stream.try_clone().ok()?;
    let mut lines = BufReader::new(Until { stream, deadline }.take(MAX_REQUEST)).lines();
    let hello = lines.next()?.ok()?;
    let (magic, got) = hello.split_once(' ')?;
    // Compare without stopping at the first difference.
    let same = got.len() == token.len() && got.bytes().zip(token.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0;
    if magic != MAGIC || !same {
        return None;
    }
    let mut files = Vec::new();
    for line in lines {
        let line = line.ok()?;
        match line.split_once('\t') {
            Some(("open", path)) if !path.is_empty() => files.push(PathBuf::from(path)),
            _ if line == "end" => {
                let _ = out.write_all(b"ok\n");
                return Some(Request::Open(files));
            }
            _ => return None,
        }
    }
    None
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
        match send(&path, &files) {
            Some(true) => return true,
            Some(false) => {}
            // Nothing listens there: a window that didn't close cleanly.
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    false
}

/// `None`: nobody listens; `Some(false)`: it didn't accept.
fn send(registration: &Path, files: &[PathBuf]) -> Option<bool> {
    let text = std::fs::read_to_string(registration).ok()?;
    let (port, token) = text.trim().split_once(' ')?;
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

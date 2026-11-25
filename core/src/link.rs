//! An agent's link to a nerka window (`nerka mcp`; `docs/dev/ai.md`,
//! workflow B): the client side of the loopback protocol the window serves
//! (`ui/src/instance.rs`). Each window registers its port and a random
//! token in [`instances_dir`]`/<pid>` (readable by its user only); the link
//! says the token, then `agent`, and sends one JSON call a line.

use crate::json::{self, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The hello of every connection to a window.
pub const MAGIC: &str = "nerka1";

/// The longest a window's answer to one call may take.
pub const AGENT_TIME: Duration = Duration::from_secs(600);

/// Where windows register (`<config>/nerka/instances`).
pub fn instances_dir() -> Option<PathBuf> {
    crate::config_dir().map(|d| d.join("instances"))
}

/// An agent's connection to a window (`nerka mcp`): the window in
/// `NERKA_WINDOW` (a process id), else the one focused last.
pub struct AgentLink {
    r: BufReader<TcpStream>,
    w: TcpStream,
    next: u64,
    /// The window's process id.
    pub pid: u32,
}

impl AgentLink {
    /// Connect to `pid`'s window, or the one focused last (`None`). An
    /// error says why none answered.
    pub fn connect(pid: Option<u32>) -> Result<AgentLink, String> {
        let dir = instances_dir().ok_or("no per-user config folder")?;
        let mut regs: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(&dir)
            .map_err(|_| "no nerka window is open".to_owned())?
            .flatten()
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .filter(|(_, p)| {
                let n = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.parse::<u32>().ok());
                n.is_some() && pid.is_none_or(|want| n == Some(want))
            })
            .collect();
        regs.sort_by_key(|e| std::cmp::Reverse(e.0));
        for (_, reg) in regs {
            if let Some(link) = Self::to(&reg) {
                return Ok(link);
            }
        }
        Err(match pid {
            Some(p) => format!("the nerka window of process {p} isn't open"),
            None => "no nerka window is open".to_owned(),
        })
    }

    fn to(registration: &Path) -> Option<AgentLink> {
        let text = std::fs::read_to_string(registration).ok()?;
        let (port, token) = text.lines().next()?.trim().split_once(' ')?;
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port.parse::<u16>().ok()?));
        let mut w = TcpStream::connect_timeout(&addr, Duration::from_millis(300)).ok()?;
        w.write_all(format!("{MAGIC} {token}\nagent\n").as_bytes()).ok()?;
        let mut r = BufReader::new(w.try_clone().ok()?);
        r.get_mut().set_read_timeout(Some(Duration::from_secs(3))).ok()?;
        let mut ok = String::new();
        r.read_line(&mut ok).ok()?;
        if ok.trim() != "ok" {
            return None;
        }
        r.get_mut().set_read_timeout(Some(AGENT_TIME + Duration::from_secs(5))).ok()?;
        let pid = registration.file_name()?.to_str()?.parse().ok()?;
        Some(AgentLink { r, w, next: 1, pid })
    }

    /// Call `tool` with `args`; the window's result, or its error.
    pub fn call(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        let call = Value::object([("id", Value::from(id)), ("tool", tool.into()), ("args", args)]);
        writeln!(self.w, "{call}").map_err(|e| format!("the window went away: {e}"))?;
        let mut line = String::new();
        if self.r.read_line(&mut line).map_err(|e| format!("the window went away: {e}"))? == 0 {
            return Err("the window went away".into());
        }
        let v = json::parse(line.trim()).map_err(|e| format!("the window's answer isn't JSON: {e}"))?;
        if v.get("id").and_then(Value::as_u64) != Some(id) {
            return Err("the window answered another call".into());
        }
        match (v.get("result"), v.get("error").and_then(Value::as_str)) {
            (_, Some(e)) => Err(e.to_owned()),
            (Some(r), None) => Ok(r.clone()),
            (None, None) => Err("the window's answer has no result".into()),
        }
    }
}

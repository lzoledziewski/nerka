//! Syslog messages over UDP: RFC 5424 and the older BSD format (RFC 3164).
//!
//! ```text
//! <PRI>1 TIMESTAMP HOST APP PROCID MSGID [SD…] MSG      RFC 5424
//! <PRI>Mmm dd hh:mm:ss HOST TAG[PID]: MSG               RFC 3164
//! ```
//!
//! `PRI` is facility × 8 + severity; the severity is the line's level. One
//! datagram is one message; it carries no length of its own, so the
//! capture keeps a copy behind one ([`Protocol::delimited`]). The time shown
//! is the capture's; the message's own timestamp stays in the line window.

use super::{Origin, Protocol};
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue, Level};
use crate::input::ColumnDef;

const COLUMNS: [ColumnDef; 6] = [
    ColumnDef { name: "facility", width: 8 },
    ColumnDef { name: "severity", width: 8 },
    ColumnDef { name: "host", width: 14 },
    ColumnDef { name: "app", width: 12 },
    ColumnDef { name: "pid", width: 6 },
    ColumnDef { name: "payload", width: 0 },
];

const FACILITY: FieldId = 0;
const HOST: FieldId = 1;
const APP: FieldId = 2;
const PID: FieldId = 3;
const MSGID: FieldId = 4;
const STAMP: FieldId = 5;
const PAYLOAD: FieldId = 6;

const SCHEMA: &[FieldDef] = &[
    // kern, user, daemon, auth, local0, …
    FieldDef { name: "facility", kind: FieldKind::Str, costly: false },
    FieldDef { name: "host", kind: FieldKind::Str, costly: false },
    // APP-NAME (RFC 5424) or the tag (RFC 3164)
    FieldDef { name: "app", kind: FieldKind::Str, costly: false },
    // PROCID, or the tag's [pid]
    FieldDef { name: "pid", kind: FieldKind::Str, costly: false },
    // MSGID (RFC 5424)
    FieldDef { name: "msgid", kind: FieldKind::Str, costly: false },
    // the message's own timestamp, as written
    FieldDef { name: "stamp", kind: FieldKind::Str, costly: false },
    // the message text, i.e. the `payload` column
    FieldDef { name: "payload", kind: FieldKind::Str, costly: false },
];

const FACILITIES: [&str; 24] = [
    "kern", "user", "mail", "daemon", "auth", "syslog", "lpr", "news", "uucp", "cron", "authpriv", "ftp", "ntp",
    "audit", "alert", "clock", "local0", "local1", "local2", "local3", "local4", "local5", "local6", "local7",
];
const SEVERITIES: [&str; 8] = ["emerg", "alert", "crit", "err", "warning", "notice", "info", "debug"];

/// The syslog protocol.
pub struct Syslog;

/// The parts of one message; `None` where the message leaves one out.
#[derive(Debug, Default, PartialEq, Eq)]
struct Parts<'a> {
    pri: u8,
    stamp: Option<&'a [u8]>,
    host: Option<&'a [u8]>,
    app: Option<&'a [u8]>,
    pid: Option<&'a [u8]>,
    msgid: Option<&'a [u8]>,
    msg: &'a [u8],
}

/// `-` is RFC 5424's "no value".
fn value(v: &[u8]) -> Option<&[u8]> {
    (v != b"-" && !v.is_empty()).then_some(v)
}

/// The next space-separated token of `s` and what follows it.
fn token(s: &[u8]) -> (&[u8], &[u8]) {
    match s.iter().position(|&b| b == b' ') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, &[]),
    }
}

/// `<PRI>` at the start: its value and the rest.
fn pri(buf: &[u8]) -> Option<(u8, &[u8])> {
    let rest = buf.strip_prefix(b"<")?;
    let end = rest.iter().take(4).position(|&b| b == b'>')?;
    if end == 0 || !rest[..end].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let pri: u16 = std::str::from_utf8(&rest[..end]).ok()?.parse().ok()?;
    (pri < 192).then_some((pri as u8, &rest[end + 1..]))
}

/// `Mmm dd hh:mm:ss ` (RFC 3164's timestamp), if `s` starts with one.
fn bsd_stamp(s: &[u8]) -> Option<(&[u8], &[u8])> {
    let t = s.get(..16)?;
    let ok = t[..3].iter().all(u8::is_ascii_alphabetic)
        && t[3] == b' '
        && t[6] == b' '
        && t[9] == b':'
        && t[12] == b':'
        && t[15] == b' ';
    ok.then(|| (&s[..15], &s[16..]))
}

/// The message without trailing line breaks or NULs.
fn trim(buf: &[u8]) -> &[u8] {
    let end = buf.iter().rposition(|&b| !matches!(b, b'\n' | b'\r' | 0)).map_or(0, |i| i + 1);
    &buf[..end]
}

fn parse(buf: &[u8]) -> Option<Parts<'_>> {
    let (pri, rest) = pri(trim(buf))?;
    if let Some(rest) = rest.strip_prefix(b"1 ") {
        // RFC 5424.
        let (stamp, rest) = token(rest);
        let (host, rest) = token(rest);
        let (app, rest) = token(rest);
        let (pid, rest) = token(rest);
        let (msgid, mut rest) = token(rest);
        // Structured data: `-`, or `[…]` elements (`\]` inside is escaped).
        if let Some(r) = rest.strip_prefix(b"-") {
            rest = r.strip_prefix(b" ").unwrap_or(r);
        } else {
            while rest.first() == Some(&b'[') {
                let mut i = 1;
                while i < rest.len() && rest[i] != b']' {
                    i += if rest[i] == b'\\' { 2 } else { 1 };
                }
                rest = rest.get(i + 1..).unwrap_or_default();
            }
            rest = rest.strip_prefix(b" ").unwrap_or(rest);
        }
        let msg = rest.strip_prefix("\u{feff}".as_bytes()).unwrap_or(rest);
        return Some(Parts {
            pri,
            stamp: value(stamp),
            host: value(host),
            app: value(app),
            pid: value(pid),
            msgid: value(msgid),
            msg,
        });
    }
    // RFC 3164: a timestamp and host, then `TAG[PID]: ` before the text.
    let (stamp, host, rest) = match bsd_stamp(rest) {
        Some((stamp, rest)) => {
            let (host, rest) = token(rest);
            (Some(stamp), value(host), rest)
        }
        None => (None, None, rest),
    };
    let (mut app, mut pid, mut msg) = (None, None, rest);
    let (tag, after) = token(rest);
    if let Some(tag) = tag.strip_suffix(b":") {
        match tag.iter().position(|&b| b == b'[') {
            Some(i) if tag.ends_with(b"]") => {
                app = value(&tag[..i]);
                pid = value(&tag[i + 1..tag.len() - 1]);
            }
            _ => app = value(tag),
        }
        msg = after;
    }
    Some(Parts { pri, stamp, host, app, pid, msgid: None, msg })
}

fn level_of(severity: u8) -> Level {
    match severity {
        0..=2 => Level::Fatal,
        3 => Level::Error,
        4 => Level::Warn,
        5 => Level::Notice,
        6 => Level::Info,
        _ => Level::Debug,
    }
}

/// The columns' cells; the text with its line breaks if `keep`, else on
/// one line.
fn cells(p: &Parts<'_>, keep: bool) -> [String; 6] {
    let s = |v: Option<&[u8]>| String::from_utf8_lossy(v.unwrap_or_default()).into_owned();
    let text = String::from_utf8_lossy(p.msg);
    let text = if keep { text.into_owned() } else { text.replace(['\t', '\n', '\r'], " ") };
    [
        FACILITIES.get(usize::from(p.pri / 8)).copied().unwrap_or("").to_owned(),
        SEVERITIES[usize::from(p.pri % 8)].to_owned(),
        s(p.host),
        s(p.app),
        s(p.pid),
        text,
    ]
}

impl Protocol for Syslog {
    fn name(&self) -> &'static str {
        "syslog"
    }

    fn columns(&self) -> &'static [ColumnDef] {
        &COLUMNS
    }

    fn schema(&self) -> &'static [FieldDef] {
        SCHEMA
    }

    fn delimited(&self) -> bool {
        false
    }

    /// One message per datagram.
    fn split(&self, datagram: &[u8], found: &mut dyn FnMut(usize)) -> usize {
        if parse(datagram).is_some() {
            found(0);
            0
        } else {
            datagram.len()
        }
    }

    fn render(&self, msg: &[u8], _at: &Origin<'_>, out: &mut Vec<u8>) {
        let Some(p) = parse(msg) else { return };
        out.extend_from_slice(cells(&p, false).join("\t").as_bytes());
    }

    fn detail(&self, msg: &[u8], _at: &Origin<'_>) -> Vec<String> {
        parse(msg).map(|p| cells(&p, true).to_vec()).unwrap_or_default()
    }

    fn field<'m>(&self, msg: &'m [u8], _at: &Origin<'m>, id: FieldId) -> Option<FieldValue<'m>> {
        let p = parse(msg)?;
        let v = |b: Option<&'m [u8]>| b.map(|b| FieldValue::Str(b.into()));
        match id {
            FACILITY => FACILITIES.get(usize::from(p.pri / 8)).map(|f| FieldValue::Str(f.as_bytes().into())),
            HOST => v(p.host),
            APP => v(p.app),
            PID => v(p.pid),
            MSGID => v(p.msgid),
            STAMP => v(p.stamp),
            PAYLOAD => v(Some(p.msg)),
            _ => None,
        }
    }

    fn level(&self, msg: &[u8], _at: &Origin<'_>) -> Option<Level> {
        parse(msg).map(|p| level_of(p.pri % 8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc5424_and_rfc3164() {
        let p = parse(b"<165>1 2025-10-18T02:00:00.1Z gw nav 42 ID7 [x@1 a=\"b\\]\"][y@1] hello world\n").unwrap();
        let want = Parts {
            pri: 165,
            stamp: Some(b"2025-10-18T02:00:00.1Z"),
            host: Some(b"gw"),
            app: Some(b"nav"),
            pid: Some(b"42"),
            msgid: Some(b"ID7"),
            msg: b"hello world",
        };
        assert_eq!(p, want);
        let p = parse(b"<13>1 - - - - - - text").unwrap();
        assert_eq!((p.host, p.app, p.msgid, p.msg), (None, None, None, &b"text"[..]));

        let p = parse(b"<34>Oct 18 02:00:00 gw sshd[1234]: auth failed").unwrap();
        assert_eq!((p.pri, p.host, p.app, p.pid), (34, Some(&b"gw"[..]), Some(&b"sshd"[..]), Some(&b"1234"[..])));
        assert_eq!((p.stamp, p.msg), (Some(&b"Oct 18 02:00:00"[..]), &b"auth failed"[..]));
        let p = parse(b"<14>kernel: oops").unwrap();
        assert_eq!((p.host, p.app, p.msg), (None, Some(&b"kernel"[..]), &b"oops"[..]));
        let p = parse(b"<14>just text").unwrap();
        assert_eq!((p.app, p.msg), (None, &b"just text"[..]));

        for bad in [&b"no pri"[..], b"<>x", b"<192>x", b"<1a>x", b"<12345>x", b""] {
            assert!(parse(bad).is_none(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn severity_is_the_level() {
        let at = Origin { secs: 0, usecs: 0, channel: super::super::NO_CHANNEL };
        let level = |pri: u8| Syslog.level(format!("<{pri}>x").as_bytes(), &at);
        assert_eq!(level(8 + 3), Some(Level::Error));
        assert_eq!(level(4), Some(Level::Warn));
        assert_eq!(level(23 * 8 + 7), Some(Level::Debug));
        assert_eq!(level(2), Some(Level::Fatal));
    }
}

//! A small JSON reader for records, read in place: where the values of a
//! file are ([`records`]), the value at a key path in one of them
//! ([`get`]), and every key path with a scalar value ([`flatten`]). Nothing
//! is parsed beyond what is asked for, and malformed input just yields
//! nothing (no errors: a log is shown however broken it is).

use crate::{Error, Progress, Result};
use std::borrow::Cow;

/// A value in a record.
#[derive(Debug, Clone, PartialEq)]
pub enum Json<'a> {
    /// A string, unescaped.
    Str(Cow<'a, str>),
    /// A number, as written.
    Num(&'a [u8]),
    Bool(bool),
    Null,
    /// An object or array, as written.
    Raw(&'a [u8]),
}

impl Json<'_> {
    /// The value as text: a string's contents, anything else as written.
    pub fn text(&self) -> Cow<'_, [u8]> {
        match self {
            Json::Str(s) => Cow::Borrowed(s.as_bytes()),
            Json::Num(b) | Json::Raw(b) => Cow::Borrowed(b),
            Json::Bool(true) => Cow::Borrowed(b"true"),
            Json::Bool(false) => Cow::Borrowed(b"false"),
            Json::Null => Cow::Borrowed(b"null"),
        }
    }
}

fn ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

fn skip_ws(buf: &[u8], mut pos: usize) -> usize {
    while pos < buf.len() && ws(buf[pos]) {
        pos += 1;
    }
    pos
}

/// The end of the string whose opening quote is at `pos`, past its closing one.
fn skip_string(buf: &[u8], mut pos: usize) -> Option<usize> {
    pos += 1;
    while pos < buf.len() {
        match buf[pos] {
            b'"' => return Some(pos + 1),
            b'\\' => pos += 2,
            _ => pos += 1,
        }
    }
    None
}

/// The end of the value starting at `pos` (no leading whitespace).
fn skip_value(buf: &[u8], pos: usize) -> Option<usize> {
    match *buf.get(pos)? {
        b'"' => skip_string(buf, pos),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = pos;
            while i < buf.len() {
                match buf[i] {
                    b'"' => {
                        i = skip_string(buf, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        b't' => buf[pos..].starts_with(b"true").then_some(pos + 4),
        b'f' => buf[pos..].starts_with(b"false").then_some(pos + 5),
        b'n' => buf[pos..].starts_with(b"null").then_some(pos + 4),
        b'-' | b'0'..=b'9' => {
            let end = buf[pos..]
                .iter()
                .position(|b| !matches!(b, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
                .map_or(buf.len(), |i| pos + i);
            Some(end)
        }
        _ => None,
    }
}

/// A string's contents (between its quotes), unescaped.
fn unescape(raw: &[u8]) -> Cow<'_, str> {
    if !raw.contains(&b'\\') {
        return String::from_utf8_lossy(raw);
    }
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    let hex = |i: usize| std::str::from_utf8(raw.get(i..i + 4)?).ok().and_then(|h| u32::from_str_radix(h, 16).ok());
    while i < raw.len() {
        let rest = &raw[i..];
        let plain = rest.iter().position(|&b| b == b'\\').unwrap_or(rest.len());
        out.push_str(&String::from_utf8_lossy(&rest[..plain]));
        i += plain;
        if i + 1 >= raw.len() {
            break;
        }
        let c = raw[i + 1];
        i += 2;
        match c {
            b'n' => out.push('\n'),
            b't' => out.push('\t'),
            b'r' => out.push('\r'),
            b'b' => out.push('\u{8}'),
            b'f' => out.push('\u{c}'),
            b'u' => {
                let Some(mut code) = hex(i) else { break };
                i += 4;
                // A surrogate pair.
                if (0xD800..0xDC00).contains(&code)
                    && raw.get(i..i + 2) == Some(b"\\u")
                    && let Some(low) = hex(i + 2).filter(|l| (0xDC00..0xE000).contains(l))
                {
                    code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                    i += 6;
                }
                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
            other => out.push(other as char),
        }
    }
    Cow::Owned(out)
}

/// The value starting at `pos`, and where it ends.
fn value(buf: &[u8], pos: usize) -> Option<(Json<'_>, usize)> {
    let end = skip_value(buf, pos)?;
    let v = match buf[pos] {
        b'"' => Json::Str(unescape(&buf[pos + 1..end - 1])),
        b'{' | b'[' => Json::Raw(&buf[pos..end]),
        b't' => Json::Bool(true),
        b'f' => Json::Bool(false),
        b'n' => Json::Null,
        _ => Json::Num(&buf[pos..end]),
    };
    Some((v, end))
}

/// Calls `each(key, value start)` for the members of the object at `pos`.
fn members<'a>(buf: &'a [u8], pos: usize, each: &mut dyn FnMut(&[u8], usize) -> bool) -> Option<()> {
    if buf.get(pos) != Some(&b'{') {
        return None;
    }
    let mut i = skip_ws(buf, pos + 1);
    if buf.get(i) == Some(&b'}') {
        return Some(());
    }
    loop {
        if buf.get(i) != Some(&b'"') {
            return None;
        }
        let key_end = skip_string(buf, i)?;
        let key = &buf[i + 1..key_end - 1];
        i = skip_ws(buf, key_end);
        if buf.get(i) != Some(&b':') {
            return None;
        }
        i = skip_ws(buf, i + 1);
        if !each(key, i) {
            return Some(());
        }
        i = skip_ws(buf, skip_value(buf, i)?);
        match buf.get(i)? {
            b',' => i = skip_ws(buf, i + 1),
            b'}' => return Some(()),
            _ => return None,
        }
    }
}

/// Does key `raw` (as written, between quotes) name `want`?
fn key_is(raw: &[u8], want: &str) -> bool {
    if raw.contains(&b'\\') {
        unescape(raw).eq_ignore_ascii_case(want)
    } else {
        raw.eq_ignore_ascii_case(want.as_bytes())
    }
}

/// The value at `path` (keys, ignoring case) in the record `rec`.
pub fn get<'a>(rec: &'a [u8], path: &[String]) -> Option<Json<'a>> {
    let mut at = skip_ws(rec, 0);
    for key in path {
        let mut found = None;
        members(rec, at, &mut |k, v| {
            if key_is(k, key) {
                found = Some(v);
                return false;
            }
            true
        })?;
        at = found?;
    }
    value(rec, at).map(|(v, _)| v)
}

/// Every key path of `rec` with a value that isn't an object (arrays are
/// values, as text), lower case and joined by `.`, depth first; with the
/// end of each key in it (a key may hold a `.`: zeek's `id.orig_h`).
pub fn flatten<'a>(rec: &'a [u8], out: &mut dyn FnMut(&str, &[usize], &Json<'a>)) {
    type Out<'o, 'a> = &'o mut dyn FnMut(&str, &[usize], &Json<'a>);
    fn walk<'a>(rec: &'a [u8], at: usize, prefix: &mut String, ends: &mut Vec<usize>, out: Out<'_, 'a>) {
        let _ = members(rec, at, &mut |k, v| {
            let len = prefix.len();
            if !prefix.is_empty() {
                prefix.push('.');
            }
            if k.is_ascii() && !k.contains(&b'\\') {
                prefix.extend(k.iter().map(|b| b.to_ascii_lowercase() as char));
            } else {
                prefix.push_str(&unescape(k).to_lowercase());
            }
            ends.push(prefix.len());
            if rec[v] == b'{' {
                walk(rec, v, prefix, ends, out);
            } else if let Some((value, _)) = value(rec, v) {
                out(prefix, ends, &value);
            }
            ends.pop();
            prefix.truncate(len);
            true
        });
    }
    walk(rec, skip_ws(rec, 0), &mut String::new(), &mut Vec::new(), out);
}

/// The keys of a path [`flatten`] gives: `ends` are where each ends in it.
pub fn keys_of(path: &str, ends: &[usize]) -> Vec<String> {
    let mut from = 0;
    ends.iter()
        .map(|&end| {
            let key = path[from..end].to_owned();
            from = end + 1;
            key
        })
        .collect()
}

/// Where the records of a JSON file are: (start, end) of each top-level
/// value, or of each element if the file is one array. Text that isn't a
/// value is skipped to the next line. Reports progress and checks for a
/// cancel every few MiB.
pub fn records(buf: &[u8], progress: &Progress) -> Result<Vec<(u64, u64)>> {
    const REPORT: usize = 4 << 20;
    progress.set_total(buf.len() as u64);
    let mut reported = 0usize;
    let mut tick = |pos: usize| -> Result<()> {
        if pos - reported >= REPORT {
            if progress.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.add((pos - reported) as u64);
            reported = pos;
        }
        Ok(())
    };
    let mut out = Vec::new();
    let mut pos = skip_ws(buf, 0);
    if buf.get(pos) == Some(&b'[') && skip_value(buf, pos).is_some() {
        // One array: its elements.
        pos = skip_ws(buf, pos + 1);
        while pos < buf.len() && buf[pos] != b']' {
            let Some(end) = skip_value(buf, pos) else { break };
            out.push((pos as u64, end as u64));
            pos = skip_ws(buf, end);
            if buf.get(pos) == Some(&b',') {
                pos = skip_ws(buf, pos + 1);
            }
            tick(pos)?;
        }
        progress.add((buf.len() - reported) as u64);
        return Ok(out);
    }
    while pos < buf.len() {
        match skip_value(buf, pos) {
            Some(end) => {
                out.push((pos as u64, end as u64));
                pos = end;
            }
            None => pos = memchr::memchr(b'\n', &buf[pos..]).map_or(buf.len(), |i| pos + i + 1),
        }
        pos = skip_ws(buf, pos);
        tick(pos)?;
    }
    progress.add((buf.len() - reported) as u64);
    Ok(out)
}

/// Does `head` (a file's first bytes) start with a JSON object, or an array
/// whose first element is one? (`[INFO] …` is a text log.)
pub fn looks_like_json(head: &[u8]) -> bool {
    let pos = skip_ws(head, 0);
    match head.get(pos) {
        Some(b'{') => skip_value(head, pos).is_some() || head.len() >= 64 * 1024,
        Some(b'[') => {
            let first = skip_ws(head, pos + 1);
            head.get(first) == Some(&b'{') && (skip_value(head, first).is_some() || head.len() >= 64 * 1024)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &str) -> Vec<String> {
        p.split('.').map(str::to_owned).collect()
    }

    #[test]
    fn values_by_key_path() {
        let rec = br#"{"level": "ERROR", "msg": "disk \"full\"\n", "ctx": {"User": {"id": 42}, "tags": ["a", "b"]}, "t": -1.5e3, "ok": true, "x": null}"#;
        assert_eq!(get(rec, &path("level")), Some(Json::Str("ERROR".into())));
        assert_eq!(get(rec, &path("msg")), Some(Json::Str("disk \"full\"\n".into())));
        assert_eq!(get(rec, &path("ctx.user.id")), Some(Json::Num(b"42")), "keys ignore case");
        assert_eq!(get(rec, &path("ctx.tags")), Some(Json::Raw(br#"["a", "b"]"#)));
        assert_eq!(get(rec, &path("t")), Some(Json::Num(b"-1.5e3")));
        assert_eq!((get(rec, &path("ok")), get(rec, &path("x"))), (Some(Json::Bool(true)), Some(Json::Null)));
        assert_eq!(get(rec, &path("ctx.nope")), None);
        assert_eq!(get(rec, &path("level.deeper")), None);
        // Escapes, a surrogate pair among them.
        let escaped = format!("{{\"a\": \"{b}u00e9{b}ud83d{b}ude00{b}t\"}}", b = '\\');
        assert_eq!(get(escaped.as_bytes(), &path("a")), Some(Json::Str("\u{e9}\u{1f600}\t".into())));
        assert_eq!(get(b"{\"a\": 1", &path("a")), Some(Json::Num(b"1")), "what can be read is read");
        assert_eq!(get(b"not json", &path("a")), None);
    }

    #[test]
    fn flattened_keys() {
        let rec = br#"{"Level": "info", "ctx": {"user": {"id": 7}, "tags": [1]}, "n": 1}"#;
        let mut got = Vec::new();
        flatten(rec, &mut |k, _, v| got.push((k.to_owned(), v.text().into_owned())));
        let want = [("level", "info"), ("ctx.user.id", "7"), ("ctx.tags", "[1]"), ("n", "1")];
        assert_eq!(got, want.map(|(k, v)| (k.to_owned(), v.as_bytes().to_vec())));
    }

    #[test]
    fn records_of_lines_arrays_and_junk() {
        let lines = b"{\"a\":1}\n{\"a\": \"}\"}\n\n  {\"b\": [1, {\"c\": 2}]}\n";
        let at = |buf: &[u8]| records(buf, &Progress::new()).unwrap();
        let r: Vec<&[u8]> = at(lines).iter().map(|&(s, e)| &lines[s as usize..e as usize]).collect();
        assert_eq!(r, [&b"{\"a\":1}"[..], b"{\"a\": \"}\"}", b"{\"b\": [1, {\"c\": 2}]}"]);
        let pretty = b"[\n  {\n    \"a\": 1\n  },\n  {\"a\": 2}\n]\n";
        let r: Vec<&[u8]> = at(pretty).iter().map(|&(s, e)| &pretty[s as usize..e as usize]).collect();
        assert_eq!(r, [&b"{\n    \"a\": 1\n  }"[..], b"{\"a\": 2}"]);
        let junk = b"garbage line\n{\"a\": 1}\n{broken\n{\"a\": 2}";
        assert_eq!(at(junk).len(), 2);
    }

    #[test]
    fn json_is_told_from_text() {
        assert!(looks_like_json(b"  {\"a\": 1}\n"));
        assert!(looks_like_json(b"[{\"a\": 1}]"));
        assert!(!looks_like_json(b"[INFO] start\n"));
        assert!(!looks_like_json(b"[2025-10-18 10:00:00] start\n"));
        assert!(!looks_like_json(b"2025-10-18 {\"a\": 1}\n"));
        assert!(!looks_like_json(b"{ not json at all"));
    }
}

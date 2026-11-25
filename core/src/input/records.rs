//! Multi-line records in a text file: a row is a record of one or more
//! lines (a stack trace, a block between blank lines), found by a
//! [`RecordSplit`].
//!
//! Opening makes one pass that records where each record starts and ends
//! (16 bytes per record); the text stays in the mapped file. A row's bytes
//! are the record's, line breaks included, so a regex can span its lines
//! and the engine's buffer scan still works (lines between records, such as
//! separators and blank lines, belong to none). The level is detected from
//! the record's start, as for a line.

use super::json::{self, Json};
use super::{Anchor, Bytes, LogSource, new_data_id};
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue, Level};
use crate::{Error, LineNo, Progress, Result};
use regex::bytes::Regex;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// How a text file is split into rows.
#[derive(Debug, Clone, Default)]
pub enum RecordSplit {
    /// One row per line (plain text).
    #[default]
    Lines,
    /// Records bounded by a start line, an end line or both ([`Bounds`];
    /// `docs/dev/records.md`).
    Start(Bounds),
    /// Records are separated by blank lines (awk's `RS=""`).
    Blank,
    /// Records are separated by lines matching the regex (not part of
    /// either record).
    Separator(Regex),
    /// JSON: each top-level value is a record (JSON Lines, objects one
    /// after another), or each element if the file is one array. Keys are
    /// fields ([`JsonFields`]).
    Json,
}

/// What becomes of lines outside every record (before the first start,
/// between an end and the next start).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outside {
    /// Each run of them is one record (a session the capture began in: the
    /// owner's UART boots).
    One,
    /// Each is a row (the log around a dump).
    Rows,
}

/// Where records start and end. A line matching `start` begins a record;
/// one matching `end` ends it (that line included). Without `end` a record
/// runs to the next start; without `start` the line after an end begins
/// the next. Lines outside every record: `outside` (default: one record
/// for `start` alone, rows with `end`; owner, 2026-11-25).
#[derive(Debug, Clone)]
pub struct Bounds {
    pub start: Option<Regex>,
    pub end: Option<Regex>,
    pub outside: Outside,
}

impl Bounds {
    /// `outside` when the spec doesn't say.
    pub fn default_outside(end: bool) -> Outside {
        if end { Outside::Rows } else { Outside::One }
    }

    /// The start and end regexes as written (without the anchoring nerka
    /// adds), and whether `outside` is the default.
    pub fn texts(&self) -> (Option<String>, Option<String>, bool) {
        let re = |r: &Regex| {
            let s = r.as_str();
            s.strip_prefix("(?m)^(?:").and_then(|s| s.strip_suffix(')')).unwrap_or(s).to_owned()
        };
        let default = self.outside == Bounds::default_outside(self.end.is_some());
        (self.start.as_ref().map(re), self.end.as_ref().map(re), default)
    }
}

/// The field of a row outside every record (`outside`; NFD `NOT outside`
/// hides them).
pub const OUTSIDE_FIELD: &str = "outside";

impl PartialEq for RecordSplit {
    fn eq(&self, other: &Self) -> bool {
        self.spec() == other.spec()
    }
}

impl Eq for RecordSplit {}

impl RecordSplit {
    /// Read a spec: `lines`, `blank`, `json`, `sep=REGEX`, or bounds:
    /// `start=/RE/`, `end=/RE/`, `outside=one|rows`, separated by spaces
    /// (`start=/A/ end=/B/ outside=rows`). In a spec of one part a regex
    /// may be written without slashes (`start=\d{4}-`, as before).
    pub fn parse(spec: &str) -> Result<RecordSplit> {
        let spec = spec.trim();
        Ok(match spec {
            "" | "lines" => RecordSplit::Lines,
            "blank" => RecordSplit::Blank,
            "json" => RecordSplit::Json,
            _ => match spec.split_once('=') {
                Some(("sep", re)) => RecordSplit::Separator(regex(re)?),
                Some(("start" | "end" | "outside", _)) => RecordSplit::Start(bounds(spec)?),
                _ => {
                    let msg = format!(
                        "records: '{spec}' isn't lines, blank, json, sep=REGEX or start=REGEX / end=REGEX / outside=one|rows"
                    );
                    return Err(Error::Format(msg));
                }
            },
        })
    }

    /// A choice as the CLI and the Records window take it: a spec, or
    /// `auto` (`None`: JSON if the file looks like JSON, else lines).
    pub fn parse_choice(spec: &str) -> Result<Option<RecordSplit>> {
        if spec.trim() == "auto" { Ok(None) } else { RecordSplit::parse(spec).map(Some) }
    }

    /// The split for a file whose first bytes are `head`, given a choice
    /// (`None`: JSON if it looks like JSON, else lines).
    pub fn choose(choice: Option<&RecordSplit>, head: &[u8]) -> RecordSplit {
        match choice {
            Some(s) => s.clone(),
            None if json::looks_like_json(head) => RecordSplit::Json,
            None => RecordSplit::Lines,
        }
    }

    /// The spec [`Self::parse`] reads back.
    pub fn spec(&self) -> String {
        let re = |r: &Regex| {
            let s = r.as_str();
            s.strip_prefix("(?m)^(?:").and_then(|s| s.strip_suffix(')')).unwrap_or(s).to_owned()
        };
        match self {
            RecordSplit::Lines => "lines".into(),
            RecordSplit::Blank => "blank".into(),
            RecordSplit::Json => "json".into(),
            RecordSplit::Start(b) => {
                let mut parts = Vec::new();
                if let Some(r) = &b.start {
                    parts.push(format!("start=/{}/", re(r)));
                }
                if let Some(r) = &b.end {
                    parts.push(format!("end=/{}/", re(r)));
                }
                if b.outside != Bounds::default_outside(b.end.is_some()) {
                    parts.push(format!("outside={}", if b.outside == Outside::One { "one" } else { "rows" }));
                }
                parts.join(" ")
            }
            RecordSplit::Separator(r) => format!("sep=/{}/", re(r)),
        }
    }
}

/// A regex of a spec (`/…/` or bare), anchored at a line's start.
fn regex(s: &str) -> Result<Regex> {
    let s = s.trim();
    let s = s.strip_prefix('/').and_then(|s| s.strip_suffix('/')).unwrap_or(s);
    if s.is_empty() {
        return Err(Error::Format("records: the regex is empty".into()));
    }
    // Errors about the regex as typed, not the wrapped one.
    Regex::new(s).map_err(|e| Error::Format(format!("records: {e}")))?;
    Regex::new(&format!("(?m)^(?:{s})")).map_err(|e| Error::Format(format!("records: {e}")))
}

/// `start=… end=… outside=…`: each part `key=/regex/` (a `/` inside
/// written `\/` or in a class, `[/]`) or `key=word`; one part alone may
/// hold a bare regex with spaces (as specs did before `end=`).
fn bounds(spec: &str) -> Result<Bounds> {
    let bad = |m: String| Err(Error::Format(format!("records: {m}")));
    let mut parts: Vec<(&str, &str)> = Vec::new();
    let mut rest = spec.trim();
    while !rest.is_empty() {
        let Some((key, after)) = rest.split_once('=') else { return bad(format!("'{rest}' isn't KEY=VALUE")) };
        let key = key.trim();
        let (value, next) = if let Some(body) = after.strip_prefix('/') {
            // To the slash that closes it: not escaped, not in a class.
            let (mut esc, mut class) = (false, false);
            let close = body.char_indices().find(|&(_, c)| {
                let hit = c == '/' && !esc && !class;
                match c {
                    '\\' if !esc => esc = true,
                    '[' if !esc => (class, esc) = (true, false),
                    ']' if !esc => (class, esc) = (false, false),
                    _ => esc = false,
                }
                hit
            });
            let Some((i, _)) = close else { return bad(format!("{key}=/…: no closing /")) };
            (&after[..i + 2], &body[i + 1..])
        } else if parts.is_empty()
            && !after.contains(" end=")
            && !after.contains(" outside=")
            && !after.contains(" start=")
        {
            (after, "")
        } else {
            after.split_once(char::is_whitespace).unwrap_or((after, ""))
        };
        parts.push((key, value));
        rest = next.trim_start();
    }
    let (mut start, mut end, mut outside) = (None, None, None);
    for (key, value) in parts {
        match key {
            "start" if start.is_none() => start = Some(regex(value)?),
            "end" if end.is_none() => end = Some(regex(value)?),
            "outside" if outside.is_none() => {
                outside = Some(match value {
                    "one" => Outside::One,
                    "rows" => Outside::Rows,
                    v => return bad(format!("outside={v}: one or rows")),
                })
            }
            "start" | "end" | "outside" => return bad(format!("{key} given twice")),
            k => return bad(format!("'{k}': start, end or outside")),
        }
    }
    if start.is_none() && end.is_none() {
        return bad("start=/REGEX/ or end=/REGEX/ is needed".into());
    }
    let outside = outside.unwrap_or(Bounds::default_outside(end.is_some()));
    Ok(Bounds { start, end, outside })
}

/// A text file read as records ([`RecordSplit`] other than lines).
pub struct RecordFile {
    name: String,
    path: Option<PathBuf>,
    data: Bytes,
    /// Each record's start and end (without its last line break).
    starts: Vec<u64>,
    ends: Vec<u64>,
    split: RecordSplit,
    /// JSON records: the fields their keys give.
    json: Option<JsonFields>,
    /// Bounds: the rows outside every record (ascending), and the schema
    /// with the `outside` field.
    outside: Vec<LineNo>,
    bounds_schema: Vec<FieldDef>,
    data_id: usize,
    /// Each record's first line in the file (from 0), counted once when
    /// first asked ([`LogSource::file_line`]).
    file_lines: std::sync::OnceLock<Vec<LineNo>>,
}

/// The fields of JSON records: every key path with a value in any record
/// (lower case, nested keys joined by `.`: `ctx.user`),
/// typed by what they hold (whole numbers `U64`, other numbers `F64`, the
/// rest text; arrays and objects as their text). A `level`, `severity`,
/// `lvl` or `loglevel` key gives the row's level (names, or numbers on the
/// 10–60 scale of bunyan / pino). A key named like one of NFD's own fields
/// (`raw`, `level`, `bookmark`) is the field `json.raw` (…); see `dsl`.
#[derive(Debug, Clone, Default)]
pub struct JsonFields {
    schema: Vec<FieldDef>,
    paths: Vec<Vec<String>>,
    level: Option<Vec<String>>,
}

/// Records a thread reads for the JSON fields at a time.
const CHUNK: usize = 4096;

/// Key paths of JSON records, in the order first seen: name, keys, kind.
/// Two paths of one name (a key `id.x`, and `x` in `id`): the first one
/// seen is the field.
#[derive(Default)]
struct Keys {
    seen: Vec<(String, Vec<String>, FieldKind)>,
    at: std::collections::HashMap<String, usize>,
}

impl Keys {
    fn read(&mut self, rec: &[u8]) {
        json::flatten(rec, &mut |path, ends, v| {
            let kind = match v {
                Json::Null => return,
                Json::Num(n) if n.iter().all(u8::is_ascii_digit) => FieldKind::U64,
                Json::Num(_) => FieldKind::F64,
                _ => FieldKind::Str,
            };
            self.add(path, || json::keys_of(path, ends), kind);
        });
    }

    fn add(&mut self, path: &str, keys: impl FnOnce() -> Vec<String>, kind: FieldKind) {
        match self.at.get(path) {
            Some(&i) => {
                let k = &mut self.seen[i].2;
                *k = match (*k, kind) {
                    (a, b) if a == b => a,
                    (FieldKind::U64 | FieldKind::F64, FieldKind::U64 | FieldKind::F64) => FieldKind::F64,
                    _ => FieldKind::Str,
                }
            }
            None => {
                self.at.insert(path.to_owned(), self.seen.len());
                self.seen.push((path.to_owned(), keys(), kind));
            }
        }
    }

    /// `later`'s keys after these (it read the records after these).
    fn join(&mut self, later: Keys) {
        for (path, keys, kind) in later.seen {
            self.add(&path, || keys, kind);
        }
    }
}

/// Keys whose value is the level, in this order.
const LEVEL_KEYS: [&str; 4] = ["level", "severity", "lvl", "loglevel"];

impl JsonFields {
    fn of(buf: &[u8], starts: &[u64], ends: &[u64]) -> JsonFields {
        use rayon::prelude::*;
        // Every record, in chunks read in parallel and joined in order: a
        // key may first come late (one kind of event among thousands).
        let records: Vec<(u64, u64)> = starts.iter().copied().zip(ends.iter().copied()).collect();
        let seen = records
            .par_chunks(CHUNK)
            .map(|chunk| {
                let mut keys = Keys::default();
                for &(s, e) in chunk {
                    keys.read(&buf[s as usize..e as usize]);
                }
                keys
            })
            .reduce_with(|mut a, b| {
                a.join(b);
                a
            })
            .unwrap_or_default()
            .seen;
        let level = LEVEL_KEYS.iter().find(|k| seen.iter().any(|(p, _, _)| p == *k)).map(|k| vec![k.to_string()]);
        // Names a query can write (a WORD of NFD); a key named like one of
        // NFD's own fields is `json.raw` (`raw` stays the line's text).
        let usable = |p: &str| p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
        let (mut schema, mut paths) = (Vec::new(), Vec::new());
        for (p, keys, kind) in seen.into_iter().filter(|(p, _, _)| usable(p)) {
            paths.push(keys);
            // `profile`: the field every text file has (`input::timed`).
            let name =
                if matches!(p.as_str(), "raw" | "level" | "bookmark" | "profile") { format!("json.{p}") } else { p };
            schema.push(FieldDef::owned(name, kind, false));
        }
        JsonFields { schema, paths, level }
    }
}

/// A level written as a name (`warn`, `ERROR`) or a number (bunyan / pino:
/// 10 trace … 60 fatal).
fn level_of(v: &Json<'_>) -> Option<Level> {
    match v {
        Json::Num(n) => Some(match std::str::from_utf8(n).ok()?.parse::<f64>().ok()? as u32 {
            0..=19 => Level::Trace,
            20..=29 => Level::Debug,
            30..=39 => Level::Info,
            40..=49 => Level::Warn,
            50..=59 => Level::Error,
            _ => Level::Fatal,
        }),
        v => Level::parse(std::str::from_utf8(&v.text()).ok()?.trim()),
    }
}

/// Lines of `buf` as (start, end without its line break, next line's start),
/// from `from` on.
fn lines(buf: &[u8], from: usize) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
    let mut pos = from;
    std::iter::from_fn(move || {
        if pos >= buf.len() {
            return None;
        }
        let next = memchr::memchr(b'\n', &buf[pos..]).map_or(buf.len(), |i| pos + i + 1);
        let mut end = next;
        if end > pos && buf[end - 1] == b'\n' {
            end -= 1;
        }
        if end > pos && buf[end - 1] == b'\r' {
            end -= 1;
        }
        let line = (pos, end, next);
        pos = next;
        Some(line)
    })
}

/// Bytes between progress reports and cancel checks.
const REPORT: usize = 4 << 20;

/// The records of `buf`: (start, end) each, and the rows outside every
/// record (bounds).
type Split = (Vec<u64>, Vec<u64>, Vec<LineNo>);

/// The records of `buf`: (start, end) each.
fn split(buf: &[u8], how: &RecordSplit, progress: &Progress) -> Result<Split> {
    progress.set_total(buf.len() as u64);
    let (mut starts, mut ends) = (Vec::new(), Vec::new());
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
    // The record being collected: its start and the end of its last line.
    let mut open: Option<(usize, usize)> = None;
    fn close(open: &mut Option<(usize, usize)>, starts: &mut Vec<u64>, ends: &mut Vec<u64>) {
        if let Some((s, e)) = open.take() {
            starts.push(s as u64);
            ends.push(e as u64);
        }
    }
    match how {
        RecordSplit::Json => {
            let found = json::records(buf, progress)?;
            let (s, e) = found.into_iter().unzip();
            return Ok((s, e, Vec::new()));
        }
        RecordSplit::Lines => {
            for (s, e, next) in lines(buf, 0) {
                starts.push(s as u64);
                ends.push(e as u64);
                tick(next)?;
            }
        }
        RecordSplit::Blank | RecordSplit::Separator(_) => {
            for (s, e, next) in lines(buf, 0) {
                let gap = match how {
                    RecordSplit::Separator(re) => re.find(&buf[s..e]).is_some_and(|m| m.start() == 0),
                    _ => buf[s..e].iter().all(u8::is_ascii_whitespace),
                };
                if gap {
                    close(&mut open, &mut starts, &mut ends);
                } else {
                    open = Some((open.map_or(s, |(start, _)| start), e));
                }
                tick(next)?;
            }
        }
        RecordSplit::Start(Bounds { start: Some(re), end: None, outside }) => {
            return split_by_starts(buf, re, *outside, progress);
        }
        RecordSplit::Start(b) => return split_by_bounds(buf, b, progress),
    }
    close(&mut open, &mut starts, &mut ends);
    progress.add((buf.len() - reported) as u64);
    starts.shrink_to_fit();
    ends.shrink_to_fit();
    Ok((starts, ends, Vec::new()))
}

/// Whether `re` matches at the start of the line `buf[s..e]`.
fn at_line_start(re: &Regex, buf: &[u8], s: usize, e: usize) -> bool {
    re.find(&buf[s..e]).is_some_and(|m| m.start() == 0)
}

/// A byte range of the file: start, end.
type Span = (usize, usize);

/// Records between start and end lines, one pass ([`Bounds`]).
fn split_by_bounds(buf: &[u8], b: &Bounds, progress: &Progress) -> Result<Split> {
    let (mut starts, mut ends, mut outside) = (Vec::new(), Vec::new(), Vec::new());
    // The record, and the run of lines outside (`One`), being collected.
    let (mut open, mut out): (Option<Span>, Option<Span>) = (None, None);
    let push = |r: (usize, usize), starts: &mut Vec<u64>, ends: &mut Vec<u64>| {
        starts.push(r.0 as u64);
        ends.push(r.1 as u64);
    };
    let mut reported = 0usize;
    for (s, e, next) in lines(buf, 0) {
        if next - reported >= REPORT {
            if progress.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.add((next - reported) as u64);
            reported = next;
        }
        let begins = match &b.start {
            Some(re) => at_line_start(re, buf, s, e),
            // No start: a line outside a record begins one.
            None => open.is_none(),
        };
        if begins {
            if let Some(r) = open.take() {
                push(r, &mut starts, &mut ends);
            }
            if let Some(r) = out.take() {
                outside.push(starts.len() as LineNo);
                push(r, &mut starts, &mut ends);
            }
            open = Some((s, e));
        } else if let Some(r) = &mut open {
            r.1 = e;
        } else {
            match b.outside {
                Outside::Rows => {
                    outside.push(starts.len() as LineNo);
                    push((s, e), &mut starts, &mut ends);
                }
                Outside::One => out = Some((out.map_or(s, |o| o.0), e)),
            }
            continue;
        }
        if b.end.as_ref().is_some_and(|re| at_line_start(re, buf, s, e))
            && let Some(r) = open.take()
        {
            push(r, &mut starts, &mut ends);
        }
    }
    if let Some(r) = open.take() {
        push(r, &mut starts, &mut ends);
    }
    if let Some(r) = out.take() {
        outside.push(starts.len() as LineNo);
        push(r, &mut starts, &mut ends);
    }
    progress.add((buf.len() - reported) as u64);
    Ok((starts, ends, outside))
}

/// Records that begin at lines matching `re`: the lines are checked in
/// parallel, in chunks that start at line starts (as the line index is
/// built). A record ends where the next one starts, without that line
/// break.
fn split_by_starts(buf: &[u8], re: &Regex, how: Outside, progress: &Progress) -> Result<Split> {
    use rayon::prelude::*;
    let mut bounds = vec![0usize];
    while let Some(&last) = bounds.last() {
        let target = last + REPORT;
        if target >= buf.len() {
            break;
        }
        match memchr::memchr(b'\n', &buf[target..]) {
            Some(i) => bounds.push(target + i + 1),
            None => break,
        }
    }
    bounds.push(buf.len());
    let parts: Vec<Vec<u64>> = bounds
        .par_windows(2)
        .map(|w| {
            if progress.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let chunk = &buf[..w[1]];
            let found = lines(chunk, w[0])
                .filter(|&(s, e, _)| re.find(&chunk[s..e]).is_some_and(|m| m.start() == 0))
                .map(|(s, _, _)| s as u64)
                .collect();
            progress.add((w[1] - w[0]) as u64);
            Ok(found)
        })
        .collect::<Result<_>>()?;
    let mut starts: Vec<u64> = parts.concat();
    // Lines before the first start are outside every record: one record
    // (a session the capture began in), or a row each (the log before a
    // dump: `outside=rows`). No line matching: the whole file is one
    // record (and said).
    let mut outside = Vec::new();
    match starts.first().map(|&s| s as usize) {
        Some(first) if first > 0 => {
            let head: Vec<u64> = match how {
                Outside::One => vec![0],
                Outside::Rows => lines(&buf[..first], 0).map(|(s, _, _)| s as u64).collect(),
            };
            outside = (0..head.len() as LineNo).collect();
            starts.splice(0..0, head);
        }
        None if !buf.is_empty() => starts.push(0),
        _ => {}
    }
    let ends = (0..starts.len())
        .map(|k| {
            let mut end = starts.get(k + 1).map_or(buf.len(), |&s| s as usize);
            if end > starts[k] as usize && buf[end - 1] == b'\n' {
                end -= 1;
            }
            if end > starts[k] as usize && buf[end - 1] == b'\r' {
                end -= 1;
            }
            end as u64
        })
        .collect();
    Ok((starts, ends, outside))
}

impl RecordFile {
    /// Memory-map `path` and split it into records.
    pub fn open(path: impl AsRef<Path>, how: RecordSplit, progress: &Progress) -> Result<Self> {
        let path = path.as_ref();
        let data = Bytes::map(path)?;
        let (starts, ends, outside) = split(data.bytes(), &how, progress)?;
        let name = path.display().to_string();
        let json = (how == RecordSplit::Json).then(|| JsonFields::of(data.bytes(), &starts, &ends));
        let path = Some(path.to_path_buf());
        Ok(Self::of(name, path, data, (starts, ends, outside), how, json))
    }

    fn of(
        name: String,
        path: Option<PathBuf>,
        data: Bytes,
        split: Split,
        how: RecordSplit,
        json: Option<JsonFields>,
    ) -> Self {
        let (starts, ends, outside) = split;
        let bounds_schema = if matches!(how, RecordSplit::Start(_)) {
            vec![FieldDef::new(OUTSIDE_FIELD, FieldKind::U64, false)]
        } else {
            Vec::new()
        };
        Self {
            name,
            path,
            data,
            starts,
            ends,
            split: how,
            json,
            outside,
            bounds_schema,
            data_id: new_data_id(),
            file_lines: Default::default(),
        }
    }

    /// Rows outside every record (bounds), ascending.
    pub fn outside_rows(&self) -> &[LineNo] {
        &self.outside
    }

    /// In-memory text, handy for tests.
    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>, how: RecordSplit) -> Self {
        let found = split(&bytes, &how, &Progress::new()).expect("not cancelled");
        let json = (how == RecordSplit::Json).then(|| JsonFields::of(&bytes, &found.0, &found.1));
        Self::of(name.into(), None, Bytes::Owned(bytes), found, how, json)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// How the file is split.
    pub fn split(&self) -> &RecordSplit {
        &self.split
    }

    fn record(&self, line: LineNo) -> Option<&[u8]> {
        let i = usize::try_from(line).ok()?;
        Some(&self.data.bytes()[*self.starts.get(i)? as usize..self.ends[i] as usize])
    }
}

impl LogSource for RecordFile {
    fn data_id(&self) -> usize {
        self.data_id
    }

    fn file_line(&self, line: LineNo) -> Option<LineNo> {
        let lines = self.file_lines.get_or_init(|| {
            // One pass: the line breaks before each record's start.
            let buf = self.data.bytes();
            let (mut out, mut at, mut n) = (Vec::with_capacity(self.starts.len()), 0usize, 0u64);
            for &s in &self.starts {
                let s = (s as usize).min(buf.len());
                n += memchr::memchr_iter(b'\n', &buf[at..s]).count() as u64;
                at = s;
                out.push(n);
            }
            out
        });
        lines.get(line as usize).copied()
    }

    fn schema(&self) -> &[FieldDef] {
        match &self.json {
            Some(j) => &j.schema,
            None => &self.bounds_schema,
        }
    }

    fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        if self.json.is_none() {
            // Bounds: `outside` is 1 on a row outside every record.
            return (id == 0 && self.outside.binary_search(&line).is_ok()).then_some(FieldValue::U64(1));
        }
        let j = self.json.as_ref()?;
        let v = json::get(self.record(line)?, j.paths.get(id)?)?;
        let num = |v: &Json<'_>| match v {
            Json::Num(n) => std::str::from_utf8(n).ok().map(str::to_owned),
            _ => None,
        };
        match (j.schema[id].kind, v) {
            (_, Json::Null) => None,
            (FieldKind::U64, v) => num(&v)?.parse().ok().map(FieldValue::U64),
            (FieldKind::F64, v) => num(&v)?.parse().ok().map(FieldValue::F64),
            (_, Json::Str(std::borrow::Cow::Borrowed(s))) => Some(FieldValue::Str(s.as_bytes().into())),
            (_, v) => Some(FieldValue::Str(v.text().into_owned().into())),
        }
    }

    /// JSON records with a level key: its value (none if it has no level
    /// a query knows); other records: detected from the text.
    fn supplied_level(&self, line: LineNo) -> Option<Option<Level>> {
        let path = self.json.as_ref()?.level.as_ref()?;
        Some(json::get(self.record(line)?, path).as_ref().and_then(level_of))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn line_count(&self) -> LineNo {
        self.starts.len() as LineNo
    }

    fn size_bytes(&self) -> u64 {
        self.data.bytes().len() as u64
    }

    fn index_memory_bytes(&self) -> usize {
        (self.starts.capacity() + self.ends.capacity()) * size_of::<u64>()
            + self.json.as_ref().map_or(0, |j| j.paths.iter().flatten().map(|k| k.capacity()).sum())
    }

    fn format(&self) -> String {
        match &self.split {
            RecordSplit::Json => "JSON".into(),
            split => format!("text records ({})", split.spec()),
        }
    }

    /// A split that found nothing to split at says so: the whole file is
    /// one record (a `start=` regex chosen for another file).
    fn info(&self) -> super::SourceInfo {
        let mut info = super::SourceInfo::default();
        let one = self.starts.len() == 1
            && matches!(self.split, RecordSplit::Start(_) | RecordSplit::Separator(_))
            && self.line_bytes(0).is_some_and(|r| r.contains(&b'\n'));
        if one {
            info.notes.push(format!(
                "{} matched no line: the whole file is one record (Records…: one row per line, or automatic)",
                self.split.spec()
            ));
        } else if let RecordSplit::Start(b) = &self.split {
            // What the split did with the lines outside records.
            let first = self.outside.first() == Some(&0) && b.start.is_some();
            match b.outside {
                Outside::One if first => {
                    let lines = self.file_line(1).unwrap_or(1);
                    info.notes.push(format!(
                        "the first record (lines 1–{lines}) starts before the first match: a partial session \
                         (outside=rows: a row each)"
                    ));
                }
                Outside::Rows if !self.outside.is_empty() => {
                    info.notes.push(format!(
                        "{} rows outside every record, a line each (the field outside; NOT outside hides them)",
                        self.outside.len()
                    ));
                }
                _ => {}
            }
        }
        info
    }

    /// The record, its line breaks included.
    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
        let i = usize::try_from(line).ok()?;
        let (s, e) = (*self.starts.get(i)? as usize, self.ends[i] as usize);
        Some(Cow::Borrowed(&self.data.bytes()[s..e]))
    }

    fn contiguous(&self) -> Option<&[u8]> {
        Some(self.data.bytes())
    }

    fn has_time(&self) -> bool {
        false
    }

    /// The record an offset falls in, or the one before a gap.
    fn line_of_offset(&self, offset: u64) -> LineNo {
        self.starts.partition_point(|&s| s <= offset).saturating_sub(1) as LineNo
    }

    fn line_start(&self, line: LineNo) -> u64 {
        usize::try_from(line).ok().and_then(|i| self.starts.get(i)).copied().unwrap_or(self.data.bytes().len() as u64)
    }

    fn anchor(&self, line: LineNo) -> Option<Anchor> {
        let start = self.line_start(line);
        Some(Anchor::of_text(Some(start), self.data.bytes().get(start as usize..)?))
    }

    fn lines_at(&self, offsets: &[u64]) -> Vec<Option<LineNo>> {
        offsets.iter().map(|o| self.starts.binary_search(o).ok().map(|i| i as LineNo)).collect()
    }

    /// A line anchored in another view of the file (one row per line) is
    /// in the record that holds it.
    fn line_holding(&self, a: &Anchor) -> Option<LineNo> {
        let o = a.offset?;
        let l = self.line_of_offset(o);
        let i = usize::try_from(l).ok()?;
        let inside = *self.starts.get(i)? <= o && o < self.ends[i];
        let same = Anchor::of_text(Some(o), self.data.bytes().get(o as usize..)?).hash == a.hash;
        (inside && same).then_some(l)
    }
}

#[cfg(test)]
mod tests;

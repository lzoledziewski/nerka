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
    /// A line matching the regex starts a record; the lines after it, up
    /// to the next such line, belong to it (lines before the first one are
    /// a record too).
    Start(Regex),
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

impl PartialEq for RecordSplit {
    fn eq(&self, other: &Self) -> bool {
        self.spec() == other.spec()
    }
}

impl Eq for RecordSplit {}

impl RecordSplit {
    /// Read a spec: `lines`, `blank`, `json`, `start=REGEX`, `sep=REGEX`
    /// (a regex may be written `/…/`).
    pub fn parse(spec: &str) -> Result<RecordSplit> {
        let spec = spec.trim();
        let regex = |s: &str| {
            let s = s.trim();
            let s = s.strip_prefix('/').and_then(|s| s.strip_suffix('/')).unwrap_or(s);
            if s.is_empty() {
                return Err(Error::Format("records: the regex is empty".into()));
            }
            // Errors about the regex as typed, not the wrapped one.
            Regex::new(s).map_err(|e| Error::Format(format!("records: {e}")))?;
            Regex::new(&format!("(?m)^(?:{s})")).map_err(|e| Error::Format(format!("records: {e}")))
        };
        Ok(match spec {
            "" | "lines" => RecordSplit::Lines,
            "blank" => RecordSplit::Blank,
            "json" => RecordSplit::Json,
            _ => match spec.split_once('=') {
                Some(("start", re)) => RecordSplit::Start(regex(re)?),
                Some(("sep", re)) => RecordSplit::Separator(regex(re)?),
                _ => {
                    let msg = format!("records: '{spec}' isn't lines, blank, json, start=REGEX or sep=REGEX");
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
            RecordSplit::Start(r) => format!("start=/{}/", re(r)),
            RecordSplit::Separator(r) => format!("sep=/{}/", re(r)),
        }
    }
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
    data_id: usize,
}

/// The fields of JSON records: every key path with a value in the first
/// [`SAMPLE`] records (lower case, nested keys joined by `.`: `ctx.user`),
/// typed by what they hold (whole numbers `U64`, other numbers `F64`, the
/// rest text; arrays and objects as their text). A `level`, `severity`,
/// `lvl` or `loglevel` key gives the row's level (names, or numbers on the
/// 10–60 scale of bunyan / pino); `raw`, `level` and `bookmark` are NFD's own
/// and aren't fields.
#[derive(Debug, Clone, Default)]
pub struct JsonFields {
    schema: Vec<FieldDef>,
    paths: Vec<Vec<String>>,
    level: Option<Vec<String>>,
}

/// Records read for the JSON fields.
const SAMPLE: usize = 1000;

/// Keys whose value is the level, in this order.
const LEVEL_KEYS: [&str; 4] = ["level", "severity", "lvl", "loglevel"];

impl JsonFields {
    fn of(buf: &[u8], starts: &[u64], ends: &[u64]) -> JsonFields {
        let mut seen: Vec<(String, FieldKind)> = Vec::new();
        for (&s, &e) in starts.iter().zip(ends).take(SAMPLE) {
            json::flatten(&buf[s as usize..e as usize], &mut |path, v| {
                let kind = match v {
                    Json::Null => return,
                    Json::Num(n) if n.iter().all(u8::is_ascii_digit) => FieldKind::U64,
                    Json::Num(_) => FieldKind::F64,
                    _ => FieldKind::Str,
                };
                match seen.iter_mut().find(|(p, _)| p == path) {
                    Some((_, k)) => {
                        *k = match (*k, kind) {
                            (a, b) if a == b => a,
                            (FieldKind::U64 | FieldKind::F64, FieldKind::U64 | FieldKind::F64) => FieldKind::F64,
                            _ => FieldKind::Str,
                        }
                    }
                    None => seen.push((path.to_owned(), kind)),
                }
            });
        }
        let level = LEVEL_KEYS.iter().find(|k| seen.iter().any(|(p, _)| p == *k)).map(|k| vec![k.to_string()]);
        // Names a query can write (a WORD of NFD), and not NFD's own.
        let usable = |p: &str| {
            p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
                && !matches!(p, "raw" | "level" | "bookmark")
        };
        let (mut schema, mut paths) = (Vec::new(), Vec::new());
        for (p, kind) in seen.into_iter().filter(|(p, _)| usable(p)) {
            paths.push(p.split('.').map(str::to_owned).collect());
            schema.push(FieldDef::owned(p, kind, false));
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

/// The records of `buf`: (start, end) each.
fn split(buf: &[u8], how: &RecordSplit, progress: &Progress) -> Result<(Vec<u64>, Vec<u64>)> {
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
            return Ok(found.into_iter().unzip());
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
        RecordSplit::Start(re) => {
            let (s, e) = split_by_starts(buf, re, progress)?;
            return Ok((s, e));
        }
    }
    close(&mut open, &mut starts, &mut ends);
    progress.add((buf.len() - reported) as u64);
    starts.shrink_to_fit();
    ends.shrink_to_fit();
    Ok((starts, ends))
}

/// Records that begin at lines matching `re`: the lines are checked in
/// parallel, in chunks that start at line starts (as the line index is
/// built). A record ends where the next one starts, without that line
/// break.
fn split_by_starts(buf: &[u8], re: &Regex, progress: &Progress) -> Result<(Vec<u64>, Vec<u64>)> {
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
    // Lines before the first start are a record too.
    if !buf.is_empty() && starts.first() != Some(&0) {
        starts.insert(0, 0);
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
    Ok((starts, ends))
}

impl RecordFile {
    /// Memory-map `path` and split it into records.
    pub fn open(path: impl AsRef<Path>, how: RecordSplit, progress: &Progress) -> Result<Self> {
        let path = path.as_ref();
        let data = Bytes::map(path)?;
        let (starts, ends) = split(data.bytes(), &how, progress)?;
        let name = path.display().to_string();
        let json = (how == RecordSplit::Json).then(|| JsonFields::of(data.bytes(), &starts, &ends));
        let path = Some(path.to_path_buf());
        Ok(Self { name, path, data, starts, ends, split: how, json, data_id: new_data_id() })
    }

    /// In-memory text, handy for tests.
    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>, how: RecordSplit) -> Self {
        let (starts, ends) = split(&bytes, &how, &Progress::new()).expect("not cancelled");
        let json = (how == RecordSplit::Json).then(|| JsonFields::of(&bytes, &starts, &ends));
        Self {
            name: name.into(),
            path: None,
            data: Bytes::Owned(bytes),
            starts,
            ends,
            split: how,
            json,
            data_id: new_data_id(),
        }
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

    fn schema(&self) -> &[FieldDef] {
        self.json.as_ref().map_or(&[], |j| &j.schema)
    }

    fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
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

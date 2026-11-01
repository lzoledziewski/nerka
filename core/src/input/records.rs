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

use super::{Anchor, Bytes, LogSource, new_data_id};
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
}

impl PartialEq for RecordSplit {
    fn eq(&self, other: &Self) -> bool {
        self.spec() == other.spec()
    }
}

impl Eq for RecordSplit {}

impl RecordSplit {
    /// Read a spec: `lines`, `blank`, `start=REGEX`, `sep=REGEX` (a regex
    /// may be written `/…/`).
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
            _ => match spec.split_once('=') {
                Some(("start", re)) => RecordSplit::Start(regex(re)?),
                Some(("sep", re)) => RecordSplit::Separator(regex(re)?),
                _ => {
                    let msg = format!("records: '{spec}' isn't lines, blank, start=REGEX or sep=REGEX");
                    return Err(Error::Format(msg));
                }
            },
        })
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
    data_id: usize,
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
        Ok(Self { name, path: Some(path.to_path_buf()), data, starts, ends, split: how, data_id: new_data_id() })
    }

    /// In-memory text, handy for tests.
    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>, how: RecordSplit) -> Self {
        let (starts, ends) = split(&bytes, &how, &Progress::new()).expect("not cancelled");
        Self {
            name: name.into(),
            path: None,
            data: Bytes::Owned(bytes),
            starts,
            ends,
            split: how,
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
}

impl LogSource for RecordFile {
    fn data_id(&self) -> usize {
        self.data_id
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

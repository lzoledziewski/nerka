//! A file with its time as a profile and the file's own settings give it
//! (`text-profiles.md`, "Time per file"): a text file's time read from its
//! lines by its profile's rule, and any file's times moved by an offset.
//!
//! **Text** (owner, 2026-11-22: best effort): when the file opens, the rule
//! is tried on its first [`SAMPLE`] non-empty lines; the built-in rule
//! (`time auto`) is used only if at least half of them have a time, a
//! profile's own regex if any has one; otherwise the file has no time, as
//! before. A line whose time can't be read (a stack trace's next lines, a
//! wrapped message) has the time of the line before it in its file, and
//! lines before the first time have the first one, so they stay with their
//! neighbours when sorted or merged by time. [`LogSource::own_time`] is a
//! line's own time only (a merged view's time column shows that). A time
//! of day without a date stays on day 0 (1970-01-01): no midnight is
//! counted.
//!
//! **Any file**: the offset is added to every time (the time column of a
//! table shows the moved time).

use super::profile::{Profile, TimeRule, Why};
use super::{Anchor, ColumnDef, LogSource, Reorder, SourceInfo};
use crate::event::{FieldDef, FieldId, FieldValue, Level};
use crate::time::{Numbers, Zone};
use crate::{LineNo, Progress, Result};
use std::borrow::Cow;
use std::cell::Cell;

/// Non-empty lines the time rule is tried on when the file opens.
pub const SAMPLE: usize = 200;

/// Lines looked back for the time a line without one carries.
const CARRY_BACK: LineNo = 100_000;

/// How a file's time is read, for people (the Files window, `nerka open`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileReading {
    /// Its name in a view of several (empty: one file).
    pub file: String,
    /// The profile it was read with (empty: not text).
    pub profile: String,
    /// Why that one: [`Why::text`].
    pub why: String,
    /// Where its time comes from: `the line's start`, `/REGEX/`, `the
    /// file's own`, or why it has none.
    pub time: String,
    /// Seconds added to its times.
    pub offset: f64,
}

thread_local! {
    /// The last line a time was carried to: (source, line, time). Scans go
    /// line after line, so a line without a time looks back one line only.
    static LAST: Cell<(usize, LineNo, f64)> = const { Cell::new((0, LineNo::MAX, 0.0)) };
}

pub struct Timed {
    inner: Box<dyn LogSource>,
    /// The rule read from text, if it found times.
    rule: Option<TimeRule>,
    zone: Zone,
    numbers: Numbers,
    offset: f64,
    /// The first line with a time and its time (text).
    first: Option<(LineNo, f64)>,
    reading: FileReading,
    /// Its table's first column is the time (DLT, captures): moved too.
    time_column: bool,
    /// Text: the inner source's fields and [`PROFILE_FIELD`] last.
    schema: Vec<FieldDef>,
}

/// The field a text file's lines have: the name of the profile it is read
/// with (`profile == app`).
pub const PROFILE_FIELD: &str = "profile";

impl Timed {
    /// `inner` (a text file) read with `profile`, its times moved by
    /// `offset` seconds.
    pub fn text(
        inner: Box<dyn LogSource>,
        profile: &Profile,
        why: Why,
        offset: f64,
        progress: &Progress,
    ) -> Result<Timed> {
        let (zone, numbers) = (profile.zone(), profile.numbers());
        let mut reading = FileReading {
            profile: profile.name().to_owned(),
            why: why.text().to_owned(),
            offset,
            ..FileReading::default()
        };
        let rule = profile.time().clone();
        let (mut tried, mut found, mut first) = (0usize, 0usize, None);
        if !matches!(rule, TimeRule::None) {
            for l in 0..inner.line_count() {
                if tried == SAMPLE || (l % 4096 == 0 && progress.is_cancelled()) {
                    break;
                }
                let Some(text) = inner.line_text(l) else { break };
                if text.trim().is_empty() {
                    continue;
                }
                tried += 1;
                if let Some(t) = rule.read(&text, zone, numbers) {
                    found += 1;
                    first.get_or_insert((l, t));
                }
            }
        }
        let on = match &rule {
            TimeRule::None => {
                reading.time = "none (the profile reads no time)".into();
                false
            }
            _ if tried == 0 => {
                reading.time = "none (no lines with text)".into();
                false
            }
            TimeRule::Leading if found * 2 < tried => {
                reading.time =
                    format!("none found: {found} of the first {tried} lines start with a time (half are needed)");
                false
            }
            TimeRule::Regex(r) if found == 0 => {
                reading.time = format!("none found: /{}/ read no time in the first {tried} lines", r.as_str());
                false
            }
            TimeRule::Leading => {
                reading.time = "the time each line starts with".into();
                true
            }
            TimeRule::Regex(r) => {
                reading.time = format!("/{}/", r.as_str());
                true
            }
        };
        let mut schema = inner.schema().to_vec();
        schema.push(FieldDef::new(PROFILE_FIELD, crate::event::FieldKind::Str, false));
        Ok(Timed {
            inner,
            rule: on.then_some(rule),
            zone,
            numbers,
            offset,
            first: first.filter(|_| on),
            reading,
            time_column: false,
            schema,
        })
    }

    /// `inner` (a file that knows its times: DLT, a capture) with its
    /// times moved by `offset` seconds.
    pub fn shifted(inner: Box<dyn LogSource>, offset: f64) -> Timed {
        let time_column = inner.columns().first().is_some_and(|c| c.name == "time");
        let reading = FileReading { time: "the file's own".into(), offset, ..FileReading::default() };
        Timed {
            inner,
            rule: None,
            zone: Zone::Shown,
            numbers: Numbers::Auto,
            offset,
            first: None,
            reading,
            time_column,
            schema: Vec::new(),
        }
    }

    /// The line's own time, before the offset.
    fn own(&self, line: LineNo) -> Option<f64> {
        let rule = self.rule.as_ref()?;
        rule.read(&self.inner.line_text(line)?, self.zone, self.numbers)
    }

    /// The line's time, or the one it carries, before the offset.
    fn carried(&self, line: LineNo) -> Option<f64> {
        let id = self.inner.data_id();
        let keep = |t: f64| {
            LAST.with(|c| c.set((id, line, t)));
            Some(t)
        };
        if let Some(t) = self.own(line) {
            return keep(t);
        }
        if line >= self.inner.line_count() {
            return None;
        }
        let (last_id, last_line, last) = LAST.with(Cell::get);
        if last_id == id && line > 0 && last_line == line - 1 {
            return keep(last);
        }
        let (first_line, first) = self.first?;
        if line < first_line {
            return keep(first);
        }
        let from = line.saturating_sub(CARRY_BACK).max(first_line);
        (from..line).rev().find_map(|l| self.own(l)).and_then(keep)
    }

    /// The time cell of a table's row, moved by the offset.
    fn moved_row<'a>(&self, line: LineNo, row: Cow<'a, [u8]>) -> Cow<'a, [u8]> {
        if !self.time_column || self.offset == 0.0 {
            return row;
        }
        let Some(t) = self.time(line) else { return row };
        let cut = row.iter().position(|&b| b == b'\t').unwrap_or(row.len());
        let mut out = crate::time::format_time(t, 6).into_bytes();
        out.extend_from_slice(&row[cut..]);
        Cow::Owned(out)
    }
}

impl LogSource for Timed {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn line_count(&self) -> LineNo {
        self.inner.line_count()
    }

    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
        let row = self.inner.line_bytes(line)?;
        Some(self.moved_row(line, row))
    }

    fn line_text(&self, line: LineNo) -> Option<Cow<'_, str>> {
        if self.time_column && self.offset != 0.0 {
            return self.line_bytes(line).map(|b| Cow::Owned(String::from_utf8_lossy(&b).into_owned()));
        }
        self.inner.line_text(line)
    }

    fn file_line(&self, line: LineNo) -> Option<LineNo> {
        self.inner.file_line(line)
    }

    fn line_in_file(&self, line: LineNo) -> LineNo {
        self.inner.line_in_file(line)
    }

    fn line_detail(&self, line: LineNo) -> Option<Vec<String>> {
        let mut cells = self.inner.line_detail(line)?;
        if self.time_column
            && self.offset != 0.0
            && let (Some(c), Some(t)) = (cells.first_mut(), self.time(line))
        {
            *c = crate::time::format_time(t, 6);
        }
        Some(cells)
    }

    fn size_bytes(&self) -> u64 {
        self.inner.size_bytes()
    }

    fn index_memory_bytes(&self) -> usize {
        self.inner.index_memory_bytes()
    }

    fn info(&self) -> SourceInfo {
        let mut info = self.inner.info();
        info.reading = vec![self.reading.clone()];
        info
    }

    fn format(&self) -> String {
        self.inner.format()
    }

    fn columns(&self) -> &[ColumnDef] {
        self.inner.columns()
    }

    fn schema(&self) -> &[FieldDef] {
        if self.schema.is_empty() { self.inner.schema() } else { &self.schema }
    }

    fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        if !self.schema.is_empty() && id == self.schema.len() - 1 {
            return (line < self.inner.line_count()).then(|| FieldValue::Str(self.reading.profile.as_bytes().into()));
        }
        self.inner.field(line, id)
    }

    fn supplied_level(&self, line: LineNo) -> Option<Option<Level>> {
        self.inner.supplied_level(line)
    }

    fn contiguous(&self) -> Option<&[u8]> {
        // A table whose time cells are moved isn't its file's bytes.
        if self.time_column && self.offset != 0.0 { None } else { self.inner.contiguous() }
    }

    fn line_of_offset(&self, offset: u64) -> LineNo {
        self.inner.line_of_offset(offset)
    }

    fn line_start(&self, line: LineNo) -> u64 {
        self.inner.line_start(line)
    }

    fn reordered(&self) -> Option<Reorder<'_>> {
        None
    }

    fn time(&self, line: LineNo) -> Option<f64> {
        let t = if self.rule.is_some() { self.carried(line) } else { self.inner.time(line) };
        t.map(|t| t + self.offset)
    }

    fn own_time(&self, line: LineNo) -> Option<f64> {
        let t = if self.rule.is_some() { self.own(line) } else { self.inner.own_time(line) };
        t.map(|t| t + self.offset)
    }

    fn has_time(&self) -> bool {
        if self.rule.is_some() { self.first.is_some() } else { self.inner.has_time() }
    }

    fn argument(&self, line: LineNo, index: usize) -> Option<crate::signal::SignalValue> {
        self.inner.argument(line, index)
    }

    fn uptime(&self, line: LineNo) -> Option<f64> {
        self.inner.uptime(line)
    }

    fn anchor(&self, line: LineNo) -> Option<Anchor> {
        self.inner.anchor(line)
    }

    fn line_holding(&self, anchor: &Anchor) -> Option<LineNo> {
        self.inner.line_holding(anchor)
    }

    fn lines_at(&self, offsets: &[u64]) -> Vec<Option<LineNo>> {
        self.inner.lines_at(offsets)
    }

    fn data_id(&self) -> usize {
        self.inner.data_id()
    }
}

#[cfg(test)]
mod tests;

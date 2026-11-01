//! Event model: a log line plus optionally normalized fields.
//!
//! Per spec: `raw` is mandatory, parsing is optional, the original line is
//! never lost. Fields are extracted lazily — only when a filter asks for
//! them — so plain regex filtering pays nothing for normalization.

use crate::{LineNo, LogSource};
use std::borrow::Cow;

/// How far into a line we look for a level token. Levels live in the
/// prefix; scanning the whole message would mislabel lines that merely
/// mention "ERROR".
const LEVEL_SCAN_BYTES: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Notice,
    Warn,
    Error,
    Fatal,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        Some(match s.to_ascii_uppercase().as_str() {
            "TRACE" | "T" | "VERBOSE" | "V" => Level::Trace,
            "DEBUG" | "D" => Level::Debug,
            "INFO" | "I" => Level::Info,
            "NOTICE" | "N" => Level::Notice,
            "WARN" | "WARNING" | "W" => Level::Warn,
            "ERROR" | "ERR" | "E" => Level::Error,
            "FATAL" | "CRIT" | "CRITICAL" | "F" => Level::Fatal,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Notice => "NOTICE",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Fatal => "FATAL",
        }
    }
}

/// Detect the severity of a line from an upper-case level token in its
/// prefix (`[ERROR]`, ` WARN `, `level=INFO`, …).
///
/// This is the default normalizer for plain text. Format-specific input
/// adapters (DLT, syslog, logcat…) will provide exact levels instead.
pub fn detect_level(line: &[u8]) -> Option<Level> {
    // Hand-rolled scan for whole-word upper-case tokens: ~4x faster than a
    // regex here, and this runs once per line for level-only queries.
    let head = &line[..line.len().min(LEVEL_SCAN_BYTES)];
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i < head.len() {
        if !head[i].is_ascii_uppercase() || (i > 0 && word(head[i - 1])) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < head.len() && head[j].is_ascii_uppercase() {
            j += 1;
        }
        if j == head.len() || !word(head[j]) {
            let level = match &head[i..j] {
                b"TRACE" => Some(Level::Trace),
                b"DEBUG" => Some(Level::Debug),
                b"INFO" => Some(Level::Info),
                b"NOTICE" => Some(Level::Notice),
                b"WARN" | b"WARNING" => Some(Level::Warn),
                b"ERROR" | b"ERR" => Some(Level::Error),
                b"FATAL" | b"CRIT" | b"CRITICAL" => Some(Level::Fatal),
                _ => None,
            };
            if level.is_some() {
                return level;
            }
        }
        i = j.max(i + 1);
    }
    None
}

/// Type of a source-supplied field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Short byte string (DLT ECU / APID / CTID, …): `==`, `!=`, `~`, `!~`.
    Str,
    /// Unsigned number (counters, timestamps, ids): all comparisons, and
    /// `~`, `!~` on its decimal text (a signal can capture it).
    U64,
    /// A name (`proto`: `DLT`, `SOMEIP`, `syslog`): text whose `==` and `!=`
    /// ignore case; `~`, `!~` as for text.
    Name,
}

/// One field a source supplies for every line, e.g. DLT's `apid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDef {
    /// Lower-case name used in queries.
    pub name: &'static str,
    pub kind: FieldKind,
    /// Getting the value decodes the line (e.g. a DLT payload), so
    /// conditions on it are evaluated after the cheap ones.
    pub costly: bool,
}

/// Index into [`crate::LogSource::schema`].
pub type FieldId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue<'a> {
    /// Borrowed for header fields, owned for decoded ones.
    Str(Cow<'a, [u8]>),
    U64(u64),
}

/// Lazily-normalized view of one line, passed to filter evaluation.
///
/// Built over `(source, line)` so that conditions on source-supplied fields
/// (and source-supplied levels) never fetch or render the line text.
pub struct Event<'a> {
    source: Option<&'a dyn LogSource>,
    line: LineNo,
    raw: Option<Cow<'a, [u8]>>,
    level: Option<Option<Level>>,
}

impl<'a> Event<'a> {
    /// A bare line without a source: no fields, level detected from text.
    pub fn new(raw: &'a [u8]) -> Self {
        Self { source: None, line: 0, raw: Some(Cow::Borrowed(raw)), level: None }
    }

    /// Line `line` of `source`; the text is fetched only when needed.
    pub fn from_source(source: &'a dyn LogSource, line: LineNo) -> Self {
        Self { source: Some(source), line, raw: None, level: None }
    }

    /// Like [`Self::from_source`] when the caller already holds the text.
    pub fn with_raw(source: &'a dyn LogSource, line: LineNo, raw: &'a [u8]) -> Self {
        Self { source: Some(source), line, raw: Some(Cow::Borrowed(raw)), level: None }
    }

    pub fn raw(&mut self) -> &[u8] {
        let (source, line) = (self.source, self.line);
        self.raw.get_or_insert_with(|| source.and_then(|s| s.line_bytes(line)).unwrap_or_default())
    }

    pub fn level(&mut self) -> Option<Level> {
        if let Some(l) = self.level {
            return l;
        }
        let l = match self.source.and_then(|s| s.supplied_level(self.line)) {
            Some(l) => l,
            None => detect_level(self.raw()),
        };
        self.level = Some(l);
        l
    }

    /// The source the line is from (none for a bare line).
    pub fn source(&self) -> Option<&'a dyn LogSource> {
        self.source
    }

    pub fn line(&self) -> LineNo {
        self.line
    }

    pub fn field(&self, id: FieldId) -> Option<FieldValue<'a>> {
        self.source.and_then(|s| s.field(self.line, id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert_eq!(detect_level(b"2026-10-17 10:00:00 [ERROR] x"), Some(Level::Error));
        assert_eq!(detect_level(b"ts WARNING disk"), Some(Level::Warn));
        assert_eq!(detect_level(b"level=INFO msg=ok"), Some(Level::Info));
        assert_eq!(detect_level(b"no level here, ERRORS"), None);
        assert_eq!(detect_level(b"MY_ERROR x_INFO ERR"), Some(Level::Error));
        assert_eq!(detect_level(b"FATAL"), Some(Level::Fatal));
        assert_eq!(detect_level("żółw [WARN]".as_bytes()), Some(Level::Warn));
        assert!(Level::Fatal > Level::Error && Level::Error > Level::Warn);
    }
}

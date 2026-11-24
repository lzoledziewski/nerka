//! Structured results for scripts and agents (`docs/dev/ai.md`): a line
//! and a source's summary as [`Value`]s. The CLI's `--json` prints them;
//! the MCP bridge (workflow B) answers with the same, so what an agent got
//! there can be reproduced with `nerka … --json`.
//!
//! Times are RFC 3339 in UTC whatever the local-time switch says
//! ([`time_text`]); lines are numbered from 1, in their own file.

use crate::json::Value;
use crate::{Event, FieldId, FieldKind, FieldValue, Level, LineNo, LogSource, Progress, Result};
use rayon::prelude::*;

/// A time as RFC 3339 in UTC, `2026-11-07T10:00:00.250000Z`, whatever the
/// local-time switch says; a time of day without a date (a log whose
/// lines start with `10:00:01.250`) as `10:00:01.250000`.
pub fn time_text(t: f64) -> String {
    let s = crate::time::format_utc(t, 6);
    let b = s.as_bytes();
    if b.len() >= 19 && b[4] == b'-' && b[7] == b'-' && b[10] == b' ' {
        format!("{}T{}Z", &s[..10], &s[11..])
    } else {
        s
    }
}

/// A field kind as `open --json` names it: `text`, `number`, `name`.
pub fn kind_name(k: FieldKind) -> &'static str {
    match k {
        FieldKind::Str => "text",
        FieldKind::U64 | FieldKind::F64 => "number",
        FieldKind::Name => "name",
    }
}

/// The fields a line object carries besides its own (`--fields a,b`),
/// checked against the source's.
#[derive(Debug, Default)]
pub struct Fields(Vec<(String, FieldId)>);

impl Fields {
    /// `names` (any case) found in `source`'s fields; an unknown one is an
    /// error listing those it has.
    pub fn parse(names: &[String], source: &dyn LogSource) -> std::result::Result<Fields, String> {
        let schema = source.schema();
        let mut out = Vec::new();
        for n in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
            match schema.iter().position(|d| d.name.eq_ignore_ascii_case(n)) {
                Some(id) => out.push((schema[id].name.to_string(), id)),
                None if schema.is_empty() => {
                    return Err(format!("no field '{n}': this file has none (plain text: raw and level)"));
                }
                None => {
                    let known: Vec<&str> = schema.iter().map(|d| d.name.as_ref()).collect();
                    return Err(format!("no field '{n}' (fields: {})", known.join(", ")));
                }
            }
        }
        Ok(Fields(out))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A field's value, typed: text as a string, numbers as numbers.
pub fn field_value(v: FieldValue<'_>) -> Value {
    match v {
        FieldValue::Str(b) => Value::Str(String::from_utf8_lossy(&b).into_owned()),
        FieldValue::U64(n) => Value::UInt(n),
        FieldValue::F64(f) => Value::Float(f),
    }
}

/// Row `row` of `source` as `{"file", "line", "time", "level", "raw"}`
/// and, when asked for, `"fields": {…}`. `file` and `line` are where the
/// row is: its own file and its line there (0-based here, from 1 in the
/// object). `time` and `level` are null on a line without one.
pub fn line(source: &dyn LogSource, row: LineNo, file: &str, line: LineNo, fields: &Fields) -> Value {
    let mut v = Value::new_object();
    v.push("file", file)
        .push("line", line + 1)
        .push("time", source.time(row).map(time_text))
        .push("level", Event::from_source(source, row).level().map(Level::as_str))
        .push("raw", source.line_text(row).map(|t| t.into_owned()));
    if !fields.0.is_empty() {
        let f =
            fields.0.iter().map(|(name, id)| (name.clone(), source.field(row, *id).map_or(Value::Null, field_value)));
        v.push("fields", Value::Object(f.collect()));
    }
    v
}

/// Lines by level, in the order of [`Level`], and those without one.
pub fn level_counts(source: &dyn LogSource, progress: &Progress) -> Result<([u64; 7], u64)> {
    const CHUNK: u64 = 1 << 16;
    let n = source.line_count();
    let chunks: Vec<[u64; 8]> = (0..n.div_ceil(CHUNK))
        .into_par_iter()
        .map(|c| {
            let mut counts = [0u64; 8];
            if progress.is_cancelled() {
                return counts;
            }
            for row in c * CHUNK..((c + 1) * CHUNK).min(n) {
                let i = Event::from_source(source, row).level().map_or(7, |l| l as usize);
                counts[i] += 1;
            }
            counts
        })
        .collect();
    if progress.is_cancelled() {
        return Err(crate::Error::Cancelled);
    }
    let mut all = [0u64; 8];
    for c in chunks {
        for (a, b) in all.iter_mut().zip(c) {
            *a += b;
        }
    }
    Ok(([all[0], all[1], all[2], all[3], all[4], all[5], all[6]], all[7]))
}

/// Lines looked at from each end for the first and last time.
const TIME_LOOK: u64 = 10_000;

/// The first and last time among the first and last [`TIME_LOOK`] lines.
pub fn time_span(source: &dyn LogSource) -> Option<(f64, f64)> {
    let n = source.line_count();
    let first = (0..n.min(TIME_LOOK)).find_map(|l| source.time(l))?;
    let last = (n.saturating_sub(TIME_LOOK)..n).rev().find_map(|l| source.time(l))?;
    Some((first, last))
}

/// What `nerka open --json` says about a view: its files, format, size,
/// lines, index, columns, fields (name, kind, costly), protocols, first
/// and last time, lines per level, how each file was read, and the notes
/// (what was skipped or dropped).
pub fn summary(source: &dyn LogSource, files: &[String], index_seconds: f64, progress: &Progress) -> Result<Value> {
    let info = source.info();
    let mut v = Value::new_object();
    v.push("files", files.to_vec())
        .push("format", source.format())
        .push("size", source.size_bytes())
        .push("lines", source.line_count())
        .push("index_bytes", source.index_memory_bytes())
        .push("index_seconds", index_seconds);
    let columns: Vec<&str> = source.columns().iter().map(|c| c.name.as_ref()).collect();
    v.push("columns", columns);
    let fields = source.schema().iter().map(|d| {
        Value::object([
            ("name", Value::from(d.name.as_ref())),
            ("kind", kind_name(d.kind).into()),
            ("costly", d.costly.into()),
        ])
    });
    v.push("fields", Value::Array(fields.collect()));
    v.push("protocols", info.protocols.clone());
    v.push(
        "time",
        time_span(source).map_or(Value::Null, |(a, b)| {
            Value::object([("first", time_text(a).into()), ("last", time_text(b).into())])
        }),
    );
    let (counts, none) = level_counts(source, progress)?;
    let mut levels = Value::new_object();
    for (l, n) in [Level::Trace, Level::Debug, Level::Info, Level::Notice, Level::Warn, Level::Error, Level::Fatal]
        .into_iter()
        .zip(counts)
    {
        levels.push(l.as_str(), n);
    }
    levels.push("none", none);
    v.push("levels", levels);
    let reading = info.reading.iter().map(|r| {
        Value::object([
            ("file", Value::from(r.file.as_str())),
            ("profile", r.profile.as_str().into()),
            ("why", r.why.as_str().into()),
            ("time", r.time.as_str().into()),
            ("offset", r.offset.into()),
        ])
    });
    v.push("reading", Value::Array(reading.collect()));
    v.push("notes", info.notes.clone());
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TextFile;

    #[test]
    fn times_are_rfc3339_in_utc() {
        // 2026-11-07 10:00:00.25 UTC.
        assert_eq!(time_text(1_794_045_600.25), "2026-11-07T10:00:00.250000Z");
        assert_eq!(time_text(3601.5), "01:00:01.500000");
    }

    #[test]
    fn a_line_as_json() {
        let f = TextFile::from_bytes("app.log", b"boot\n2026 [ERROR] net down\n".to_vec());
        let v = line(&f, 1, "app.log", 1, &Fields::default());
        assert_eq!(
            v.to_string(),
            r#"{"file":"app.log","line":2,"time":null,"level":"ERROR","raw":"2026 [ERROR] net down"}"#
        );
        assert_eq!(
            Fields::parse(&["apid".into()], &f).unwrap_err(),
            "no field 'apid': this file has none (plain text: raw and level)"
        );
    }

    #[test]
    fn levels_are_counted() {
        let f = TextFile::from_bytes("app.log", b"[INFO] a\n[ERROR] b\nc\n[ERROR] d\n".to_vec());
        let (c, none) = level_counts(&f, &Progress::new()).unwrap();
        assert_eq!((c[Level::Info as usize], c[Level::Error as usize], none), (1, 2, 1));
    }
}

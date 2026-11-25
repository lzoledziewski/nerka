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
    v.push("file", file).push("line", line + 1);
    // A record: where it starts in its file (the evaluation).
    if let Some(f) = record_first_line(source, row) {
        v.push("first_line", f);
    }
    v.push("time", source.time(row).map(time_text))
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
    v.push("hints", hints(source));
    Ok(v)
}

/// Where each row of a view is: its file (an index into the view's
/// files) and its line there, merged by time or not.
pub fn locator(source: &dyn LogSource) -> impl Fn(LineNo) -> (usize, LineNo) + '_ {
    let (inner, line_of) = match source.reordered() {
        Some(r) => (r.inner, Some(r.line_of)),
        None => (source, None),
    };
    let starts: Vec<LineNo> = inner.part_files().iter().map(|&(first, _)| first).collect();
    move |row| {
        let l = line_of.map_or(row, |m| m[row as usize]);
        let k = starts.partition_point(|&s| s <= l).saturating_sub(1);
        (k, l - starts.get(k).copied().unwrap_or(0))
    }
}

/// A view: its source and its files' names, as JSON places and lines
/// name them.
pub struct View<'a> {
    pub source: &'a dyn LogSource,
    pub files: &'a [String],
    /// A view of one file names it once (a page's `file`), not on each
    /// line (the path was most of an answer).
    pub compact: bool,
    /// A line's `raw` at most this many characters, cut with `…` and its
    /// length said (`"raw_chars"`); 0: whole (a DLT
    /// payload of hex ran to thousands of characters a line).
    pub width: usize,
}

/// MCP's default [`View::width`].
pub const WIDTH: usize = 500;

/// `v`'s `raw` cut to `width` characters (0: whole), its length then said.
pub fn cut_raw(v: &mut Value, width: usize) {
    let Value::Object(pairs) = v else { return };
    let Some(at) = pairs.iter().position(|(k, _)| k == "raw") else { return };
    let Value::Str(raw) = &mut pairs[at].1 else { return };
    let chars = raw.chars().count();
    if width == 0 || chars <= width {
        return;
    }
    let end = raw.char_indices().nth(width).map_or(raw.len(), |(i, _)| i);
    raw.truncate(end);
    raw.push('…');
    pairs.insert(at + 1, ("raw_chars".into(), Value::from(chars)));
}

/// Where record `row` of a source read as records starts in its file
/// (line from 1), through views of several files; `None` when a row is a line.
pub fn record_first_line(source: &dyn LogSource, row: LineNo) -> Option<u64> {
    // Sorted by time: the row's line in the files, then where it starts.
    if let Some(r) = source.reordered() {
        return record_first_line(r.inner, *r.line_of.get(row as usize)?);
    }
    source.file_line(row).map(|l| l + 1)
}

impl View<'_> {
    /// Row `row`'s file name and line in it (0-based); compact, a view of
    /// several files names each by its file name (the paths are given
    /// once, in a page's `files`).
    pub fn at(&self, row: LineNo) -> (&str, LineNo) {
        let (k, line) = locator(self.source)(row);
        let file = self.files.get(k).or(self.files.first()).map_or("", String::as_str);
        if self.compact && self.files.len() > 1 {
            let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
            return (name, line);
        }
        (file, line)
    }

    /// One file named once: lines and places leave `file` out.
    fn one_file(&self) -> bool {
        self.compact && self.files.len() == 1
    }

    /// `{"file", "line"}` (from 1) of row `row`; a record's
    /// `first_line` in its file.
    pub fn place(&self, row: LineNo) -> Value {
        let (file, line) = self.at(row);
        let mut v = Value::new_object();
        if !self.one_file() {
            v.push("file", file);
        }
        v.push("line", line + 1);
        if let Some(f) = record_first_line(self.source, row) {
            v.push("first_line", f);
        }
        v
    }

    /// Row `row` as a line object ([`line`]).
    pub fn line(&self, row: LineNo, fields: &Fields) -> Value {
        let (file, line_no) = self.at(row);
        let mut v = line(self.source, row, file, line_no, fields);
        if let Value::Object(pairs) = &mut v {
            if self.one_file() {
                pairs.retain(|(k, _)| k != "file");
            }
            // Compact: a line without a time or a level doesn't say so on
            // each line (the evaluation).
            if self.compact {
                pairs.retain(|(k, v)| !(matches!(k.as_str(), "time" | "level") && v.is_null()));
            }
        }
        cut_raw(&mut v, self.width);
        v
    }

    /// A page of `rows`: `{"total", "lines": [...], "more", "skip"}`,
    /// `skip` rows left out first, at most `max` given (`more`: how many
    /// after them; `skip`: where the next page starts). `matched` marks a
    /// line with `"match"` (lines around matches).
    pub fn page(
        &self,
        rows: &[LineNo],
        skip: u64,
        max: u64,
        fields: &Fields,
        matched: Option<&dyn Fn(LineNo) -> bool>,
    ) -> Value {
        let from = (skip as usize).min(rows.len());
        let to = from.saturating_add(max as usize).min(rows.len());
        let lines = rows[from..to].iter().map(|&r| {
            let mut v = self.line(r, fields);
            if let Some(m) = matched {
                v.push("match", m(r));
            }
            v
        });
        let mut v = Value::object([("total", Value::from(rows.len()))]);
        if self.one_file() {
            v.push("file", self.files[0].as_str());
        } else if self.compact {
            v.push("files", self.files.to_vec());
        }
        v.push("lines", Value::Array(lines.collect())).push("more", rows.len() - to).push("skip", to);
        v
    }
}

/// What a source's lines suggest about reading it (the evaluation):
/// a stack dump read better as records, a log without times. Looks at
/// the first 200,000 lines.
pub fn hints(source: &dyn LogSource) -> Vec<String> {
    let n = source.line_count().min(200_000);
    let (mut go, mut go_first, mut java) = (0u64, 0u64, 0u64);
    // Go crashes (`panic: `, `fatal error: `, `SIGABRT: abort`): the first.
    let (mut crashes, mut crash_first) = (0u64, 0u64);
    for l in 0..n {
        let Some(t) = source.line_text(l) else { continue };
        let crash = t.starts_with("panic: ")
            || t.starts_with("fatal error: ")
            || t.strip_prefix("SIG")
                .is_some_and(|r| r.split_once(": ").is_some_and(|(s, _)| s.bytes().all(|b| b.is_ascii_uppercase())));
        if crash {
            if crashes == 0 {
                crash_first = l + 1;
            }
            crashes += 1;
        }
        if let Some(rest) = t.strip_prefix("goroutine ")
            && rest.starts_with(|c: char| c.is_ascii_digit())
        {
            if go == 0 {
                go_first = l + 1;
            }
            go += 1;
        } else if t.starts_with(char::is_whitespace) && t.trim_start().starts_with("at ") && t.ends_with(')') {
            java += 1;
        }
    }
    let mut out = Vec::new();
    // An uptime at the start of each line (`[ 2088.354600]`: dmesg, a
    // a daemon's own output): a profile reads it as the line's time
    // .
    let uptimes = (0..n.min(200))
        .filter_map(|l| source.line_text(l))
        .filter(|t| {
            let b = t.as_bytes();
            b.first() == Some(&b'[') && {
                let rest = t[1..].trim_start();
                let num = rest.split(']').next().unwrap_or("");
                !num.is_empty() && num.contains('.') && num.bytes().all(|c| c.is_ascii_digit() || c == b'.')
            }
        })
        .count() as u64;
    if uptimes >= 3 && uptimes * 2 >= n.min(200) && time_span(source).is_none() {
        let profile = "uptime: time /^.[ ]*(?<time>[0-9]+[.][0-9]+)/; numbers s";
        out.push(format!(
            "lines start with an uptime in seconds ([ 2088.354600]): read it as their time with a profile, \
             --profile '{profile}' --profile uptime (window: Settings → Profiles…); then before / after and \
             gaps work"
        ));
    }
    if go >= 3 && crashes >= 2 && !source.format().starts_with("text records") {
        // A server's log with crashes in it (a model server's
        // runner, five times): the dumps are what followed, not the story.
        out.push(format!(
            "{crashes} Go crashes with goroutine dumps (the first at line {crash_first}, {go} goroutines in all): \
             what led to each is usually in the lines just before it (filter 'level >= WARN', or lines around \
             it); to read a dump's stacks, read as records start=/^goroutine [0-9]/ (CLI: --records \
             'start=/^goroutine [0-9]/'; window: set_records)"
        ));
    } else if go >= 3 && !source.format().starts_with("text records") {
        out.push(format!(
            "a Go goroutine dump ({go} goroutines from line {go_first}): read as records start=/^goroutine [0-9]/ \
             (CLI: --records 'start=/^goroutine [0-9]/'; window: set_records), each goroutine is one record and \
             patterns groups identical stacks (lines before the dump stay rows: patterns --in 'raw ~ /^goroutine/' \
             keeps the goroutines only)"
        ));
    }
    if java >= 5 && !source.format().starts_with("text records") {
        out.push(
            "Java stack traces: read as records start=/^\\S/ (CLI: --records 'start=/^\\S/'; window: set_records), \
             each line keeps its indented `at` lines"
                .into(),
        );
    }
    let uptime_said = out.iter().any(|h| h.starts_with("lines start with an uptime"));
    if source.line_count() > 0 && time_span(source).is_none() && !uptime_said {
        // Many lines start with a time, but under half: entries that span
        // lines (a payload, a trace), read best as records starting at the
        // time (a server's trace log).
        let tried: Vec<String> = (0..source.line_count().min(200))
            .filter_map(|l| source.line_text(l).map(|t| t.into_owned()))
            .filter(|t| !t.trim().is_empty())
            .collect();
        let timed = tried.iter().filter(|t| crate::time::leading_time(t, crate::time::Zone::Utc).is_some()).count();
        // The first line timed is enough: an entry may be long (a Python
        // traceback of ~17 lines: 6 % of the lines start with a time).
        let first_timed = tried.first().is_some_and(|t| crate::time::leading_time(t, crate::time::Zone::Utc).is_some());
        if timed >= 3 && (first_timed || timed * 10 >= tried.len()) && !source.format().starts_with("text records") {
            let split = "start=/^[0-9]{4}[-/][0-9]{2}[-/][0-9]{2}.[0-9]/";
            out.push(format!(
                "{timed} of the first {} lines start with a time: its entries span several lines; read as records \
                 {split} (CLI: --records '{split}'; window: set_records), each entry is one record with its time",
                tried.len()
            ));
        } else {
            out.push(
                "no line has a time: before / after (context by time) can't work; read around a line with lines".into(),
            );
        }
    }
    out
}

/// What [`patterns`] adds to each pattern.
#[derive(Debug, Clone, Copy, Default)]
pub struct PatternDetail {
    /// Each slot's values: count, first and last line.
    pub values: bool,
    /// An NFD query finding its lines.
    pub queries: bool,
    /// Its first N lines (0: all).
    pub lines: Option<usize>,
}

/// Patterns as `{"total", "patterns": [...], "more"}`, the first `top`
/// (0: all) in their order.
pub fn patterns(ps: &crate::pattern::Patterns, view: &View, top: usize, detail: PatternDetail) -> Value {
    let shown = if top == 0 { ps.patterns.len() } else { top.min(ps.patterns.len()) };
    let none = Fields::default();
    let mut list = Vec::new();
    for p in &ps.patterns[..shown] {
        let mut v = Value::new_object();
        v.push("count", p.count())
            .push("share", p.count() as f64 / ps.total().max(1) as f64)
            .push("first", view.place(p.first()))
            .push("last", view.place(p.last()))
            .push("fields", Value::object(p.fields.iter().map(|(n, x)| (n.to_string(), Value::from(x.to_string())))))
            .push("template", p.template())
            .push("unsorted", p.unsorted);
        if detail.values {
            let slots = p.slots().enumerate().map(|(k, s)| {
                let vals = s.values.iter().map(|x| {
                    Value::object([
                        ("value", Value::from(x.value().to_string())),
                        ("count", x.count().into()),
                        ("first", view.place(x.first())),
                        ("last", view.place(x.last())),
                    ])
                });
                Value::object([
                    ("slot", Value::from(k + 1)),
                    ("kind", (if s.is_state() { "state" } else { "values" }).into()),
                    ("overflow", s.overflow.into()),
                    ("values", Value::Array(vals.collect())),
                ])
            });
            v.push("slots", Value::Array(slots.collect()));
        }
        if detail.queries {
            v.push("query", ps.query(p));
        }
        if let Some(n) = detail.lines {
            let n = if n == 0 { usize::MAX } else { n };
            v.push("lines", Value::Array(p.lines[0].iter().take(n).map(|&l| view.line(l, &none)).collect()));
            v.push("lines_more", p.lines[0].len().saturating_sub(n));
        }
        list.push(v);
    }
    let mut v = Value::object([("total", Value::from(ps.total()))]);
    v.push("patterns", Value::Array(list)).push("more", ps.patterns.len() - shown);
    v
}

/// The differences of a comparison as `{"a", "b", "min_score",
/// "differences": [...], "more"}`: `sides` describe A and B (file, filter,
/// lines), `views` place their lines.
pub fn compare(
    ps: &crate::pattern::Patterns,
    diffs: &[crate::pattern::Difference],
    views: [&View; 2],
    sides: [Value; 2],
    listed: &crate::pattern::Listed,
    top: usize,
    queries: bool,
) -> Value {
    use crate::pattern::Change;
    let shown = if top == 0 { diffs.len() } else { top.min(diffs.len()) };
    let mut list = Vec::new();
    for d in &diffs[..shown] {
        let p = &ps.patterns[d.pattern];
        let side = compare_side(d);
        let mut v = Value::new_object();
        v.push("change", d.change.name()).push("score", d.score).push("a", d.counts[0]).push("b", d.counts[1]);
        if matches!(d.change, Change::Changed | Change::Same) {
            v.push("ratio", d.ratio());
        }
        let at = match d.value {
            None => p.sides[side].first,
            Some((s, k)) => p.slots().nth(s).expect("a slot").values[k].sides[side].first,
        };
        v.push("first", views[side].place(at)).push("side", ["a", "b"][side]);
        v.push("fields", Value::object(p.fields.iter().map(|(n, x)| (n.to_string(), Value::from(x.to_string())))));
        v.push("template", p.template());
        if let Some((s, k)) = d.value {
            v.push("slot", s + 1).push("value", p.slots().nth(s).expect("a slot").values[k].value().to_string());
        }
        if queries {
            v.push("query", ps.query(p));
        }
        list.push(v);
    }
    let [a, b] = sides;
    let mut v = Value::object([("a", a), ("b", b)]);
    v.push("min_score", listed.min_score)
        .push("show", listed.changes_text())
        .push("rows", listed.rows.name())
        .push("differences", Value::Array(list))
        .push("more", diffs.len() - shown);
    v
}

/// Where to look for a difference: the side it's on, or grew on.
pub fn compare_side(d: &crate::pattern::Difference) -> usize {
    use crate::pattern::Change;
    match d.change {
        Change::New => 1,
        Change::Gone => 0,
        Change::Changed | Change::Same => usize::from(d.ratio() >= 1.0),
    }
}

/// A signal's values between rows `from` and `to` (inclusive): how many
/// events; a number's first, last, minimum and maximum; an enum's states
/// with their counts; and at most `points` events spread over the range
/// (an enum: its changes first), each with its place and time.
pub fn signal(
    def: &crate::SignalDef,
    series: &crate::signal::SignalSeries,
    view: &View,
    from: LineNo,
    to: LineNo,
    points: usize,
) -> Value {
    use crate::signal::ValueRef;
    let events: Vec<(LineNo, ValueRef)> = series.iter().filter(|(r, _)| (from..=to).contains(r)).collect();
    let mut v = Value::object([("name", Value::from(def.name())), ("kind", def.kind().as_str().into())]);
    v.push("events", events.len());
    let at = |(row, val): (LineNo, ValueRef)| {
        let mut p = view.place(row);
        p.push("time", view.source.time(row).map(time_text));
        p.push(
            "value",
            match val {
                ValueRef::Num(n) => Value::from(n),
                ValueRef::Enum(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
            },
        );
        p
    };
    if let (Some(&first), Some(&last)) = (events.first(), events.last()) {
        v.push("first", at(first)).push("last", at(last));
    }
    let nums: Vec<f64> =
        events.iter().filter_map(|(_, x)| if let ValueRef::Num(n) = x { Some(*n) } else { None }).collect();
    if !nums.is_empty() {
        v.push("min", nums.iter().copied().fold(f64::INFINITY, f64::min));
        v.push("max", nums.iter().copied().fold(f64::NEG_INFINITY, f64::max));
    }
    // An enum: its states and how often each came; its changes.
    let mut shown: Vec<(LineNo, ValueRef)> = Vec::new();
    if nums.is_empty() && !events.is_empty() {
        let mut states: Vec<(String, u64)> = Vec::new();
        let mut last: Option<&[u8]> = None;
        for &(row, x) in &events {
            if let ValueRef::Enum(t) = x {
                let name = String::from_utf8_lossy(t).into_owned();
                match states.iter_mut().find(|(n, _)| *n == name) {
                    Some(s) => s.1 += 1,
                    None => states.push((name, 1)),
                }
                if last != Some(t) {
                    shown.push((row, x));
                    last = Some(t);
                }
            }
        }
        v.push("states", Value::object(states.into_iter().map(|(n, c)| (n, Value::from(c)))));
        v.push("changes", shown.len());
    } else {
        shown = events.clone();
    }
    // At most `points`, spread evenly, the first and the last kept.
    let n = shown.len();
    let picked: Vec<(LineNo, ValueRef)> = if n <= points.max(2) {
        shown
    } else {
        let k = points.max(2);
        (0..k).map(|i| shown[i * (n - 1) / (k - 1)]).collect()
    };
    v.push("thinned", picked.len() < n);
    v.push("points", Value::Array(picked.into_iter().map(at).collect()));
    v
}

/// A lifecycle definition's runs, as `nerka lifecycles --json` gives them
/// (`lifecycles`, `issues`, `summary`), for one definition.
pub fn lifecycles(name: &str, l: &crate::lifecycle::Lifecycles, view: &View) -> Value {
    let lives = l.lives.iter().map(|x| {
        let mut v = Value::new_object();
        v.push("lifecycle", name)
            .push("number", x.number)
            .push("first", view.place(x.first))
            .push("last", view.place(x.last))
            .push("lines", x.lines)
            .push("started_by", x.started_by.as_str())
            .push("outcome", x.outcome.as_str())
            .push("marker", x.marker.map(|m| view.place(m)))
            .push("start", x.first_time.map(time_text))
            .push("end", x.last_time.map(time_text))
            .push("duration", x.duration())
            .push("first_uptime", x.first_uptime)
            .push("boot", x.boot);
        v
    });
    let issues = l.findings.iter().map(|f| {
        Value::object([
            ("kind", Value::from(f.kind.as_str())),
            ("at", view.place(f.row)),
            ("count", f.count.into()),
            ("lifecycle", format!("{name}#{}", l.lives.get(f.life).map_or(0, |x| x.number)).into()),
            ("detail", f.detail.as_str().into()),
        ])
    });
    let mut v = Value::object([("lifecycles", Value::Array(lives.collect()))]);
    v.push("issues", Value::Array(issues.collect())).push("summary", format!("{name}: {}", l.summary()));
    v
}

/// `bytes` in base64 (RFC 4648, padded): a screenshot's PNG in an answer.
pub fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    out
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
    fn a_stack_dump_is_hinted_and_records_say_where_they_start() {
        let dump = "SIGQUIT: quit\n\ngoroutine 1 gp=0x1 m=nil [select]:\nmain.f()\n\ngoroutine 2 [chan send]:\nmain.g()\n\ngoroutine 3 [chan send]:\nmain.g()\n";
        let f = TextFile::from_bytes("dump.txt", dump.as_bytes().to_vec());
        let h = hints(&f);
        assert!(
            h[0].starts_with(
                "a Go goroutine dump (3 goroutines from line 3): read as records start=/^goroutine [0-9]/"
            ),
            "{h:?}"
        );
        assert!(h.iter().any(|x| x.starts_with("no line has a time")), "{h:?}");
        let split = crate::input::records::RecordSplit::parse(r"start=/^goroutine \d/").unwrap();
        let r = crate::input::records::RecordFile::from_bytes("dump.txt", dump.as_bytes().to_vec(), split);
        // The two lines before the dump stay rows, then a record per goroutine.
        assert_eq!(r.line_count(), 5);
        let files = ["dump.txt".to_owned()];
        let v = View { source: &r, files: &files, compact: true, width: 0 };
        assert_eq!(v.place(3).to_string(), r#"{"line":4,"first_line":6}"#);
        assert!(hints(&r).iter().all(|x| !x.contains("goroutine dump")), "read as records already");
    }

    #[test]
    fn crashes_in_a_server_log_are_hinted_as_crashes() {
        let one = "time=2026-04-02T10:00:00Z level=INFO msg=load\nSIGABRT: abort\nPC=0x1\n\ngoroutine 1 gp=0x1 m=0 [running]:\nmain.f()\n\ngoroutine 2 [select]:\nmain.g()\n\ngoroutine 3 [select]:\nmain.g()\n";
        let f = TextFile::from_bytes("serve.log", one.repeat(2).into_bytes());
        let h = hints(&f);
        assert!(
            h[0].starts_with("2 Go crashes with goroutine dumps (the first at line 2, 6 goroutines in all)"),
            "{h:?}"
        );
        // One dump: read as records.
        let f = TextFile::from_bytes("serve.log", one.into());
        assert!(hints(&f)[0].starts_with("a Go goroutine dump"), "{:?}", hints(&f));
    }

    #[test]
    fn a_long_raw_is_cut_and_its_length_said() {
        let f = TextFile::from_bytes("a.log", format!("{}\nshort\n", "ab".repeat(400)).into_bytes());
        let files = ["a.log".to_owned()];
        let v = View { source: &f, files: &files, compact: true, width: 10 };
        let fields = Fields::default();
        assert_eq!(v.line(0, &fields).to_string(), r#"{"line":1,"raw":"ababababab…","raw_chars":800}"#);
        assert_eq!(v.line(1, &fields).to_string(), r#"{"line":2,"raw":"short"}"#);
        let v = View { width: 0, ..v };
        assert!(!v.line(0, &fields).to_string().contains("raw_chars"));
    }

    #[test]
    fn entries_over_several_lines_are_hinted_to_read_as_records() {
        let log = "2026/04/16 03:08:41 DEBUG : a\n<xml>\n  <b/>\n</xml>\n2026/04/16 03:08:42 DEBUG : c\n<xml>\n  <d/>\n</xml>\n2026/04/16 03:08:43 DEBUG : e\n<x/>\n<y/>\n";
        let f = TextFile::from_bytes("trace.log", log.as_bytes().to_vec());
        let h = hints(&f);
        assert!(h[0].starts_with("3 of the first 11 lines start with a time"), "{h:?}");
        let spec = h[0].split("records ").nth(1).unwrap().split(' ').next().unwrap();
        let split = crate::input::records::RecordSplit::parse(spec).unwrap();
        let r = crate::input::records::RecordFile::from_bytes("trace.log", log.as_bytes().to_vec(), split);
        assert_eq!(r.line_count(), 3, "the hint's split makes an entry a record");
    }

    /// `@line`: a row's line in its own file, in a view of several files.
    #[test]
    fn at_line_is_the_line_in_its_own_file() {
        let dir = std::env::temp_dir().join(format!("nerka-at-line-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.log"), dir.join("b.log"));
        std::fs::write(&a, "a1\na2\na3\n").unwrap();
        std::fs::write(&b, "b1\nb2\n").unwrap();
        let v = crate::open_many(&[a, b], &crate::OpenOptions::default(), &Progress::new()).unwrap();
        let signals = crate::Signals::default().for_source(&*v);
        let rows = |q: &str| {
            let q = crate::Query::compile_in(q, v.schema(), &signals).unwrap();
            crate::search(&*v, &crate::Filter::Query(q), &Progress::new()).unwrap().lines
        };
        assert_eq!(rows("@line == 2"), [1, 4], "a2 and b2");
        assert_eq!(rows("file == b.log AND @line >= 2"), [4]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_uptime_at_line_starts_is_hinted_with_its_profile() {
        let log = "[ 2088.354600]~DLT~  587~NOTICE   ~Starting
[ 2088.354705]~DLT~  587~INFO     ~main()
[ 2089.1]~DLT~  587~INFO     ~x
";
        let f = TextFile::from_bytes("gw.log", log.as_bytes().to_vec());
        let h = hints(&f);
        assert_eq!(h.len(), 1, "{h:?}");
        assert!(h[0].contains("--profile 'uptime: time /^.[ ]*(?<time>[0-9]+[.][0-9]+)/; numbers s'"), "{h:?}");
    }

    #[test]
    fn base64_as_rfc_4648() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (a, b) in cases {
            assert_eq!(base64(a.as_bytes()), b);
        }
    }

    #[test]
    fn levels_are_counted() {
        let f = TextFile::from_bytes("app.log", b"[INFO] a\n[ERROR] b\nc\n[ERROR] d\n".to_vec());
        let (c, none) = level_counts(&f, &Progress::new()).unwrap();
        assert_eq!((c[Level::Info as usize], c[Level::Error as usize], none), (1, 2, 1));
    }
}

//! Several sources as one, one after another (the owner, 2026-10-20: one
//! view of several files; [`super::merged`] interleaves them by time).
//!
//! Line `n` of the whole is line `n - start` of the part it falls in. Parts
//! with the same columns keep them, with every part's fields (plain text
//! with plain text, DLT with DLT, captures included: those add `net.*`
//! fields a `.dlt` file doesn't have). Messages of different protocols
//! (owner, 2026-11-08: DLT from one capture, a protocol definition's
//! messages from another) share the **mixed layout** of a capture holding
//! several protocols: columns `time`, `proto`, `info`, `payload`, and every
//! part's fields, a field a part doesn't have having no value there. Plain
//! text and JSON mix with nothing else. A `file` field (the part's file
//! name) is added, so `file == b.dlt` filters by file.
//!
//! The engine searches part by part ([`LogSource::parts`]), so each part
//! keeps its own fast path; a part's [`View`] answers with the whole's
//! fields, columns and line text, so `file` and `raw` read the same there.

use super::proto::{MIXED_COLUMNS, PROTO_FIELD, Records};
use super::{ColumnDef, LogSource};
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue, Level};
use crate::{Error, LineNo, Result};
use std::borrow::Cow;

/// The field added to the parts' own.
pub const FILE_FIELD: &str = "file";

pub struct Concat {
    name: String,
    parts: Vec<Box<dyn LogSource>>,
    /// First line of each part in the whole.
    starts: Vec<LineNo>,
    /// File name of each part (the `file` field).
    files: Vec<String>,
    total: LineNo,
    schema: Vec<FieldDef>,
    /// Parts of different protocols, in the mixed layout.
    mixed: bool,
    /// Parts whose fields differ: [`Concat::fields`] maps them.
    mapped: bool,
    columns: Vec<ColumnDef>,
    /// Per part, its field for each of the whole's (`None`: it has none;
    /// the `file` field is the whole's own).
    fields: Vec<Vec<Option<FieldId>>>,
    /// Per part, whether its lines are rendered into the mixed layout
    /// (they have columns of their own).
    convert: Vec<bool>,
}

/// What kind of source this is, for "same kind": columns and fields.
fn kind(s: &dyn LogSource) -> (Vec<&str>, Vec<(&str, FieldKind)>) {
    (
        s.columns().iter().map(|c| c.name.as_ref()).collect(),
        s.schema().iter().map(|f| (f.name.as_ref(), f.kind)).collect(),
    )
}

/// Whether `s` holds messages of protocols (a `.dlt` file, a capture):
/// those mix in one view.
fn has_proto(s: &dyn LogSource) -> bool {
    !s.columns().is_empty() && s.schema().iter().any(|f| f.name == PROTO_FIELD.name)
}

/// A readable kind for messages: plain text, text records, JSON, DLT, or
/// another table.
fn kind_name(s: &dyn LogSource) -> &'static str {
    match s.columns() {
        [] if s.format() == "JSON" => "JSON",
        [] if s.format().starts_with("text records") => "text records",
        [] => "plain text",
        c if c == super::dlt::COLUMNS => "DLT",
        _ => "a table of other columns",
    }
}

/// The last path component of a source's name.
fn file_name(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_owned()
}

impl Concat {
    /// `parts` one after another. Errors if they aren't all of one kind.
    pub fn new(parts: Vec<Box<dyn LogSource>>) -> Result<Concat> {
        let Some(first) = parts.first() else { return Err(Error::Format("no files to open".into())) };
        let k = kind(first.as_ref());
        let differ = parts.iter().any(|p| kind(p.as_ref()) != k);
        // Only messages of protocols mix; plain text or JSON with anything
        // of another kind doesn't.
        let lone = parts.iter().position(|p| !has_proto(p.as_ref()));
        if let (true, Some(i)) = (differ, lone) {
            let (first, k) = (&parts[i], kind(parts[i].as_ref()));
            let other = parts.iter().find(|p| kind(p.as_ref()) != k).expect("they differ");
            let (a, b) = (file_name(first.name()), file_name(other.name()));
            let (ka, kb) = (kind_name(first.as_ref()), kind_name(other.as_ref()));
            // Of one kind with other fields (JSON with other keys): say so
            // (review 2026-11-09, R17: "is JSON but … is JSON").
            return Err(Error::MixedKinds(if ka == kb {
                let what = if ka == "JSON" { "keys" } else { "fields" };
                format!(
                    "{a} and {b} are both {ka} but with other {what}: only files with the same {what} open as one view"
                )
            } else {
                format!("{a} is {ka} but {b} is {kb}: only files of one kind open as one view")
            }));
        }
        // Columns differ: the mixed layout. Only fields differ (a `.dlt`
        // file and a capture of DLT): the columns stay.
        let mixed = parts.iter().any(|p| p.columns() != first.columns());
        // The mixed layout puts `proto` first, so a part's field ids are
        // never the whole's there, even when every part has the same fields.
        let mapped = mixed || parts.iter().any(|p| p.schema() != first.schema());
        // As a capture of several protocols, `proto` first when mixed; then
        // each part's fields (the first part's keep their ids). One of a
        // name already there is the same field when its kind is the same,
        // and left out when it isn't.
        let mut schema: Vec<FieldDef> = if mixed { vec![PROTO_FIELD] } else { Vec::new() };
        for p in &parts {
            for def in p.schema() {
                if !schema.iter().any(|d| d.name == def.name) {
                    schema.push(def.clone());
                }
            }
        }
        let fields = parts
            .iter()
            .map(|p| {
                schema.iter().map(|d| p.schema().iter().position(|o| o.name == d.name && o.kind == d.kind)).collect()
            })
            .collect();
        let convert = parts.iter().map(|p| mixed && p.columns() != MIXED_COLUMNS).collect();
        let columns = if mixed { MIXED_COLUMNS.to_vec() } else { first.columns().to_vec() };
        schema.push(FieldDef::new(FILE_FIELD, FieldKind::Str, false));
        let (mut starts, mut total) = (Vec::with_capacity(parts.len()), 0);
        for p in &parts {
            starts.push(total);
            total += p.line_count();
        }
        let files = parts.iter().map(|p| file_name(p.name())).collect();
        let name = parts.iter().map(|p| p.name()).collect::<Vec<_>>().join(" + ");
        Ok(Concat { name, parts, starts, files, total, schema, mixed, mapped, columns, fields, convert })
    }

    /// Whether parts of different protocols share the mixed layout.
    pub fn is_mixed(&self) -> bool {
        self.mixed
    }

    /// Part `k`'s line `l` as the whole shows it: in the mixed layout, its
    /// protocol's cells before the payload become `info`, as in a capture
    /// of several protocols.
    fn part_bytes(&self, k: usize, l: LineNo) -> Option<Cow<'_, [u8]>> {
        let own = self.parts[k].line_bytes(l)?;
        if !self.convert[k] {
            return Some(own);
        }
        let own = String::from_utf8_lossy(&own);
        let (time, rest) = own.split_once('\t').unwrap_or((&own, ""));
        let (info, payload) = Records::mixed_cells(rest);
        let mut out = Vec::with_capacity(own.len() + 8);
        out.extend_from_slice(time.as_bytes());
        out.push(b'\t');
        out.extend_from_slice(&self.proto_of(k, l));
        out.push(b'\t');
        out.extend_from_slice(info.as_bytes());
        out.push(b'\t');
        out.extend_from_slice(payload.as_bytes());
        Some(Cow::Owned(out))
    }

    /// Part `k`'s detailed line, in the mixed layout when it needs one.
    fn part_detail(&self, k: usize, l: LineNo) -> Option<Vec<String>> {
        let mut cells = self.parts[k].line_detail(l)?;
        if !self.convert[k] || cells.is_empty() {
            return Some(cells);
        }
        let time = cells.remove(0);
        let payload = cells.pop().unwrap_or_default();
        let info = cells.into_iter().filter(|c| !c.is_empty()).collect::<Vec<_>>().join(" ");
        Some(vec![time, String::from_utf8_lossy(&self.proto_of(k, l)).into_owned(), info, payload])
    }

    /// The protocol of part `k`'s line `l` (its `proto` field).
    fn proto_of(&self, k: usize, l: LineNo) -> Vec<u8> {
        let p = self.parts[k].schema().iter().position(|d| d.name == PROTO_FIELD.name);
        match p.and_then(|p| self.parts[k].field(l, p)) {
            Some(FieldValue::Str(s)) => s.into_owned(),
            _ => Vec::new(),
        }
    }

    /// Part `k`'s value of the whole's field `id`.
    fn part_field(&self, k: usize, l: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        if id == self.file_id() {
            return Some(FieldValue::Str(self.files[k].as_bytes().into()));
        }
        if !self.mapped {
            // One kind: the parts' fields are the whole's.
            return self.parts[k].field(l, id);
        }
        self.parts[k].field(l, self.fields[k].get(id).copied().flatten()?)
    }

    /// The part `line` falls in, and its line there.
    fn locate(&self, line: LineNo) -> Option<(usize, LineNo)> {
        if line >= self.total {
            return None;
        }
        let k = self.starts.partition_point(|&s| s <= line) - 1;
        Some((k, line - self.starts[k]))
    }

    fn file_id(&self) -> FieldId {
        self.schema.len() - 1
    }
}

impl LogSource for Concat {
    fn name(&self) -> &str {
        &self.name
    }

    fn line_count(&self) -> LineNo {
        self.total
    }

    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
        let (k, l) = self.locate(line)?;
        self.part_bytes(k, l)
    }

    fn line_detail(&self, line: LineNo) -> Option<Vec<String>> {
        let (k, l) = self.locate(line)?;
        self.part_detail(k, l)
    }

    fn size_bytes(&self) -> u64 {
        self.parts.iter().map(|p| p.size_bytes()).sum()
    }

    fn index_memory_bytes(&self) -> usize {
        self.parts.iter().map(|p| p.index_memory_bytes()).sum::<usize>() + self.starts.capacity() * 8
    }

    fn format(&self) -> String {
        let mut formats: Vec<String> = Vec::new();
        for f in self.parts.iter().map(|p| p.format()) {
            if !formats.contains(&f) {
                formats.push(f);
            }
        }
        formats.join(" + ")
    }

    fn columns(&self) -> &[ColumnDef] {
        &self.columns
    }

    fn schema(&self) -> &[FieldDef] {
        &self.schema
    }

    fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        let (k, l) = self.locate(line)?;
        self.part_field(k, l, id)
    }

    fn supplied_level(&self, line: LineNo) -> Option<Option<Level>> {
        let (k, l) = self.locate(line)?;
        self.parts[k].supplied_level(l)
    }

    fn time(&self, line: LineNo) -> Option<f64> {
        let (k, l) = self.locate(line)?;
        self.parts[k].time(l)
    }

    fn argument(&self, line: LineNo, index: usize) -> Option<crate::signal::SignalValue> {
        let (k, l) = self.locate(line)?;
        self.parts[k].argument(l, index)
    }

    fn uptime(&self, line: LineNo) -> Option<f64> {
        let (k, l) = self.locate(line)?;
        self.parts[k].uptime(l)
    }

    fn parts(&self) -> Option<Vec<super::Part<'_>>> {
        Some(
            (0..self.parts.len())
                .map(|k| super::Part { first: self.starts[k], source: Box::new(View { whole: self, k }) })
                .collect(),
        )
    }

    fn part_files(&self) -> Vec<(LineNo, LineNo)> {
        self.starts.iter().zip(&self.parts).map(|(&s, p)| (s, p.line_count())).collect()
    }
}

/// Part `k` of a [`Concat`] as a source of its own, with the whole's fields
/// (so `file` is answered) and the part's buffer (so the fast path stays).
struct View<'a> {
    whole: &'a Concat,
    k: usize,
}

impl View<'_> {
    fn part(&self) -> &dyn LogSource {
        self.whole.parts[self.k].as_ref()
    }
}

impl LogSource for View<'_> {
    fn name(&self) -> &str {
        self.part().name()
    }

    fn line_count(&self) -> LineNo {
        self.part().line_count()
    }

    fn anchor(&self, line: LineNo) -> Option<super::Anchor> {
        self.part().anchor(line)
    }

    fn lines_at(&self, offsets: &[u64]) -> Vec<Option<LineNo>> {
        self.part().lines_at(offsets)
    }

    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
        self.whole.part_bytes(self.k, line)
    }

    fn line_detail(&self, line: LineNo) -> Option<Vec<String>> {
        self.whole.part_detail(self.k, line)
    }

    fn columns(&self) -> &[ColumnDef] {
        &self.whole.columns
    }

    fn schema(&self) -> &[FieldDef] {
        &self.whole.schema
    }

    fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        self.whole.part_field(self.k, line, id)
    }

    fn supplied_level(&self, line: LineNo) -> Option<Option<Level>> {
        self.part().supplied_level(line)
    }

    fn time(&self, line: LineNo) -> Option<f64> {
        self.part().time(line)
    }

    fn argument(&self, line: LineNo, index: usize) -> Option<crate::signal::SignalValue> {
        self.part().argument(line, index)
    }

    fn uptime(&self, line: LineNo) -> Option<f64> {
        self.part().uptime(line)
    }

    fn contiguous(&self) -> Option<&[u8]> {
        self.part().contiguous()
    }

    fn line_of_offset(&self, offset: u64) -> LineNo {
        self.part().line_of_offset(offset)
    }

    fn line_start(&self, line: LineNo) -> u64 {
        self.part().line_start(line)
    }

    fn data_id(&self) -> usize {
        self.part().data_id()
    }
}

/// Open `paths` as one source: one file as itself, several one after
/// another (the same kind only).
pub fn open_many(
    paths: &[std::path::PathBuf],
    options: &super::OpenOptions,
    progress: &crate::Progress,
) -> Result<Box<dyn LogSource>> {
    let mut parts = Vec::with_capacity(paths.len());
    for p in paths {
        parts.push(super::open_with(p, options, progress).map_err(|e| Error::opening(p, e))?);
    }
    if parts.len() == 1 {
        return Ok(parts.pop().expect("one"));
    }
    Ok(Box::new(Concat::new(parts)?))
}

/// Like [`open_many`], but files that know their lines' times (DLT,
/// captures) are put in the order they start: by the first time each
/// holds. If any file has no time (a text log), the given order stays.
/// Returns the paths in the order used.
pub fn open_many_by_time(
    paths: &[std::path::PathBuf],
    options: &super::OpenOptions,
    progress: &crate::Progress,
) -> Result<(Box<dyn LogSource>, Vec<std::path::PathBuf>)> {
    let (mut sources, paths, _) = open_sorted(paths, options, progress)?;
    let source = if sources.len() == 1 { sources.pop().expect("one") } else { Box::new(Concat::new(sources)?) };
    Ok((source, paths))
}

/// Like [`open_many_by_time`], with the files' lines interleaved by time
/// ([`super::merged::Merged`]) when there are several and each knows its
/// lines' times; otherwise as `open_many_by_time` opens them.
pub fn open_merged_by_time(
    paths: &[std::path::PathBuf],
    options: &super::OpenOptions,
    progress: &crate::Progress,
) -> Result<(Box<dyn LogSource>, Vec<std::path::PathBuf>)> {
    let (mut sources, paths, timed) = open_sorted(paths, options, progress)?;
    let source: Box<dyn LogSource> = match sources.len() {
        1 => sources.pop().expect("one"),
        _ if timed => Box::new(super::merged::Merged::new(Concat::new(sources)?, progress)?),
        _ => Box::new(Concat::new(sources)?),
    };
    Ok((source, paths))
}

/// Opened files, their paths in that order, and whether every file
/// knows its times.
type Sorted = (Vec<Box<dyn LogSource>>, Vec<std::path::PathBuf>, bool);

/// Open `paths`, in the order they start when every file knows its
/// times (then `true`), else as given.
fn open_sorted(
    paths: &[std::path::PathBuf],
    options: &super::OpenOptions,
    progress: &crate::Progress,
) -> Result<Sorted> {
    let mut parts = Vec::with_capacity(paths.len());
    for p in paths {
        parts.push((p.clone(), super::open_with(p, options, progress).map_err(|e| Error::opening(p, e))?));
    }
    let starts: Option<Vec<f64>> = parts.iter().map(|(_, s)| start_time(s.as_ref())).collect();
    let timed = starts.is_some();
    if let Some(starts) = starts {
        let mut order: Vec<usize> = (0..parts.len()).collect();
        order.sort_by(|&a, &b| starts[a].total_cmp(&starts[b])); // stable: ties keep their order
        let mut slots: Vec<Option<_>> = parts.into_iter().map(Some).collect();
        parts = order.iter().map(|&i| slots[i].take().expect("a permutation")).collect();
    }
    let paths = parts.iter().map(|(p, _)| p.clone()).collect();
    Ok((parts.into_iter().map(|(_, s)| s).collect(), paths, timed))
}

/// The first time a source holds (looking at its first lines only), if it
/// knows its lines' times.
pub fn start_time(s: &dyn LogSource) -> Option<f64> {
    (0..s.line_count().min(1000)).find_map(|l| s.time(l))
}

#[cfg(test)]
mod tests;

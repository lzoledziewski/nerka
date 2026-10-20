//! Several sources of the same kind as one, one after another (the owner,
//! 2026-10-20: one view of several files; merging by time waits until
//! "time" is defined).
//!
//! Line `n` of the whole is line `n - start` of the part it falls in. The
//! parts must have the same columns and fields (plain text with plain
//! text, DLT with DLT, captures included). A `file` field (the part's file
//! name) is added, so `file == b.dlt` filters by file.
//!
//! The engine searches part by part ([`LogSource::parts`]), so each part
//! keeps its own fast path; a part's [`View`] answers with the whole's
//! fields, so `file` works there too.

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
}

/// What kind of source this is, for "same kind": columns and fields.
fn kind(s: &dyn LogSource) -> (Vec<&'static str>, Vec<(&'static str, FieldKind)>) {
    (s.columns().iter().map(|c| c.name).collect(), s.schema().iter().map(|f| (f.name, f.kind)).collect())
}

/// A readable kind for messages: "DLT" for table sources, else "plain text".
fn kind_name(s: &dyn LogSource) -> &'static str {
    if s.columns().is_empty() { "plain text" } else { "DLT" }
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
        if let Some(other) = parts.iter().find(|p| kind(p.as_ref()) != k) {
            return Err(Error::Format(format!(
                "{} is {} but {} is {}: only files of one kind open as one view",
                file_name(first.name()),
                kind_name(first.as_ref()),
                file_name(other.name()),
                kind_name(other.as_ref())
            )));
        }
        let mut schema = first.schema().to_vec();
        schema.push(FieldDef { name: FILE_FIELD, kind: FieldKind::Str, costly: false });
        let (mut starts, mut total) = (Vec::with_capacity(parts.len()), 0);
        for p in &parts {
            starts.push(total);
            total += p.line_count();
        }
        let files = parts.iter().map(|p| file_name(p.name())).collect();
        let name = parts.iter().map(|p| p.name()).collect::<Vec<_>>().join(" + ");
        Ok(Concat { name, parts, starts, files, total, schema })
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
        self.parts[k].line_bytes(l)
    }

    fn line_detail(&self, line: LineNo) -> Option<Vec<String>> {
        let (k, l) = self.locate(line)?;
        self.parts[k].line_detail(l)
    }

    fn size_bytes(&self) -> u64 {
        self.parts.iter().map(|p| p.size_bytes()).sum()
    }

    fn index_memory_bytes(&self) -> usize {
        self.parts.iter().map(|p| p.index_memory_bytes()).sum::<usize>() + self.starts.capacity() * 8
    }

    fn columns(&self) -> &[ColumnDef] {
        self.parts[0].columns()
    }

    fn schema(&self) -> &[FieldDef] {
        &self.schema
    }

    fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        let (k, l) = self.locate(line)?;
        if id == self.file_id() {
            return Some(FieldValue::Str(self.files[k].as_bytes().into()));
        }
        self.parts[k].field(l, id)
    }

    fn supplied_level(&self, line: LineNo) -> Option<Option<Level>> {
        let (k, l) = self.locate(line)?;
        self.parts[k].supplied_level(l)
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

    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
        self.part().line_bytes(line)
    }

    fn columns(&self) -> &[ColumnDef] {
        self.part().columns()
    }

    fn schema(&self) -> &[FieldDef] {
        &self.whole.schema
    }

    fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        if id == self.whole.file_id() {
            return Some(FieldValue::Str(self.whole.files[self.k].as_bytes().into()));
        }
        self.part().field(line, id)
    }

    fn supplied_level(&self, line: LineNo) -> Option<Option<Level>> {
        self.part().supplied_level(line)
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
        parts.push(super::open_with(p, options, progress)?);
    }
    if parts.len() == 1 {
        return Ok(parts.pop().expect("one"));
    }
    Ok(Box::new(Concat::new(parts)?))
}

#[cfg(test)]
mod tests;

//! Several files' lines interleaved by time: a [`Concat`] read in the order
//! of the lines' times, each file's own order kept ([`Merged::new`]); or
//! one source's lines sorted by time ([`Merged::sorted`], [`sort_by_time`]).
//!
//! Rows are the concatenation's lines in another order, so the engine
//! scans the concatenation (each file on its fast path) and maps the hits
//! ([`LogSource::reordered`]). Queries that depend on row numbers (signals,
//! whose state follows the rows across files, and `bookmark`) are
//! evaluated on the rows. 16 bytes per line: both directions of the map.

use super::concat::Concat;
use super::{Anchor, ColumnDef, LogSource, Reorder};
use crate::event::{FieldDef, FieldId, FieldValue, Level};
use crate::{Error, LineNo, Progress, Result};
use std::borrow::Cow;
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

pub struct Merged {
    name: String,
    inner: Box<dyn LogSource>,
    /// Sorted by time ([`Merged::sorted`]), else files interleaved.
    sorted: bool,
    /// Row -> line of `inner`.
    line_of: Vec<LineNo>,
    /// Line of `inner` -> row.
    row_of: Vec<LineNo>,
    data_id: usize,
}

/// A line waiting in the merge: its time, file and line in the file.
#[derive(PartialEq)]
struct Next(f64, usize, LineNo);

impl Eq for Next {}

impl PartialOrd for Next {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Next {
    /// Earlier time first; on a tie the file given first, then its order.
    fn cmp(&self, o: &Self) -> Ordering {
        self.0.total_cmp(&o.0).then(self.1.cmp(&o.1)).then(self.2.cmp(&o.2))
    }
}

impl Merged {
    /// Interleave `inner`'s files by their lines' times. A line without a
    /// time goes with the line before it in its file (lines before a file's
    /// first time go first). Each file keeps its own order even where its
    /// times go back.
    pub fn new(inner: Concat, progress: &Progress) -> Result<Merged> {
        let parts = inner.part_files();
        let total = inner.line_count();
        progress.set_total(total);
        // Each file's lines' times, carried forward over lines without one,
        // and kept from going back (so the file's order holds).
        let mut times: Vec<Vec<f64>> = Vec::with_capacity(parts.len());
        for &(first, n) in &parts {
            let mut t = Vec::with_capacity(n as usize);
            let mut last = f64::NEG_INFINITY;
            for l in 0..n {
                if l % 65_536 == 0 && progress.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                if let Some(x) = inner.time(first + l)
                    && x > last
                {
                    last = x;
                }
                t.push(last);
            }
            times.push(t);
            progress.add(n);
        }
        let mut heap: BinaryHeap<Reverse<Next>> = BinaryHeap::with_capacity(parts.len());
        for (k, t) in times.iter().enumerate() {
            if let Some(&x) = t.first() {
                heap.push(Reverse(Next(x, k, 0)));
            }
        }
        let mut line_of = Vec::with_capacity(total as usize);
        while let Some(Reverse(Next(_, k, l))) = heap.pop() {
            line_of.push(parts[k].0 + l);
            if let Some(&x) = times[k].get(l as usize + 1) {
                heap.push(Reverse(Next(x, k, l + 1)));
            }
        }
        Ok(Self::of(Box::new(inner), line_of, false))
    }

    /// `inner`'s lines sorted by their times, lines of the same time in
    /// their order (a stable sort). A line without a time goes with the
    /// line before it; lines before the first time go first. Unlike
    /// [`Merged::new`], lines whose times go back move.
    pub fn sorted(inner: Box<dyn LogSource>, progress: &Progress) -> Result<Merged> {
        let total = inner.line_count();
        progress.set_total(total);
        let mut times = Vec::with_capacity(total as usize);
        let mut last = f64::NEG_INFINITY;
        for l in 0..total {
            if l % 65_536 == 0 && progress.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if let Some(x) = inner.time(l) {
                last = x;
            }
            times.push(last);
        }
        progress.add(total);
        let mut line_of: Vec<LineNo> = (0..total).collect();
        // Stable, and quick on runs already in order (most of a log).
        line_of.sort_by(|&a, &b| times[a as usize].total_cmp(&times[b as usize]));
        Ok(Self::of(inner, line_of, true))
    }

    fn of(inner: Box<dyn LogSource>, line_of: Vec<LineNo>, sorted: bool) -> Merged {
        let mut row_of = vec![0; line_of.len()];
        for (row, &line) in line_of.iter().enumerate() {
            row_of[line as usize] = row as LineNo;
        }
        let name = inner.name().to_owned();
        Merged { name, inner, sorted, line_of, row_of, data_id: super::new_data_id() }
    }

    fn line(&self, row: LineNo) -> Option<LineNo> {
        self.line_of.get(row as usize).copied()
    }
}

impl LogSource for Merged {
    fn name(&self) -> &str {
        &self.name
    }

    fn info(&self) -> super::SourceInfo {
        self.inner.info()
    }

    fn line_count(&self) -> LineNo {
        self.line_of.len() as LineNo
    }

    fn line_bytes(&self, row: LineNo) -> Option<Cow<'_, [u8]>> {
        self.inner.line_bytes(self.line(row)?)
    }

    fn line_detail(&self, row: LineNo) -> Option<Vec<String>> {
        self.inner.line_detail(self.line(row)?)
    }

    fn size_bytes(&self) -> u64 {
        self.inner.size_bytes()
    }

    fn index_memory_bytes(&self) -> usize {
        self.inner.index_memory_bytes() + (self.line_of.capacity() + self.row_of.capacity()) * 8
    }

    fn format(&self) -> String {
        let how = if self.sorted { "sorted" } else { "merged" };
        format!("{}, {how} by time", self.inner.format())
    }

    fn columns(&self) -> &[ColumnDef] {
        self.inner.columns()
    }

    fn schema(&self) -> &[FieldDef] {
        self.inner.schema()
    }

    fn field(&self, row: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        self.inner.field(self.line(row)?, id)
    }

    fn supplied_level(&self, row: LineNo) -> Option<Option<Level>> {
        self.inner.supplied_level(self.line(row)?)
    }

    fn time(&self, row: LineNo) -> Option<f64> {
        self.inner.time(self.line(row)?)
    }

    fn argument(&self, row: LineNo, index: usize) -> Option<crate::signal::SignalValue> {
        self.inner.argument(self.line(row)?, index)
    }

    fn uptime(&self, row: LineNo) -> Option<f64> {
        self.inner.uptime(self.line(row)?)
    }

    /// Bookmarks are kept per file: see [`Self::reordered`].
    fn anchor(&self, row: LineNo) -> Option<Anchor> {
        self.inner.anchor(self.line(row)?)
    }

    fn line_holding(&self, anchor: &Anchor) -> Option<LineNo> {
        self.inner.line_holding(anchor).map(|l| self.row_of[l as usize])
    }

    fn lines_at(&self, offsets: &[u64]) -> Vec<Option<LineNo>> {
        self.inner.lines_at(offsets).into_iter().map(|l| l.map(|l| self.row_of[l as usize])).collect()
    }

    fn reordered(&self) -> Option<Reorder<'_>> {
        Some(Reorder { inner: &*self.inner, line_of: &self.line_of, row_of: &self.row_of })
    }

    fn data_id(&self) -> usize {
        self.data_id
    }
}

/// `source` with its lines sorted by time ([`Merged::sorted`]), or as it is
/// if it doesn't know its lines' times (a text log).
pub fn sort_by_time(source: Box<dyn LogSource>, progress: &Progress) -> Result<Box<dyn LogSource>> {
    if super::concat::start_time(source.as_ref()).is_none() {
        return Ok(source);
    }
    Ok(Box::new(Merged::sorted(source, progress)?))
}

#[cfg(test)]
mod tests;

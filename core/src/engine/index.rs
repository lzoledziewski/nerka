//! Line index: byte offset of the start of every line.
//!
//! Built in parallel: the buffer is split into fixed-size chunks, each chunk
//! is scanned for `\n` with SIMD (`memchr`), and the per-chunk results are
//! concatenated. On a modern machine this runs at memory bandwidth.

use crate::{Error, LineNo, Progress, Result};
use rayon::prelude::*;
use std::ops::Range;

const CHUNK: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct LineIndex {
    /// `starts[i]` = byte offset where line `i` starts.
    starts: Vec<u64>,
    /// Total length of the indexed buffer.
    len: u64,
}

impl LineIndex {
    pub fn build(buf: &[u8], progress: &Progress) -> Result<Self> {
        progress.set_total(buf.len() as u64);
        if buf.is_empty() {
            return Ok(Self { starts: Vec::new(), len: 0 });
        }
        let chunks: Vec<Vec<u64>> = buf
            .par_chunks(CHUNK)
            .enumerate()
            .map(|(ci, chunk)| {
                if progress.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let base = (ci * CHUNK) as u64;
                let v: Vec<u64> = memchr::memchr_iter(b'\n', chunk).map(|p| base + p as u64 + 1).collect();
                progress.add(chunk.len() as u64);
                Ok(v)
            })
            .collect::<Result<_>>()?;

        let total: usize = chunks.iter().map(Vec::len).sum();
        let mut starts = Vec::with_capacity(total + 1);
        starts.push(0);
        for c in chunks {
            starts.extend(c);
        }
        // A trailing newline does not open a new (empty) line.
        if starts.last() == Some(&(buf.len() as u64)) {
            starts.pop();
        }
        Ok(Self { starts, len: buf.len() as u64 })
    }

    pub fn line_count(&self) -> LineNo {
        self.starts.len() as LineNo
    }

    /// Byte range of the line content, without the line terminator
    /// (`\n` or `\r\n`).
    pub fn line_range(&self, buf: &[u8], line: LineNo) -> Option<Range<usize>> {
        let i = usize::try_from(line).ok()?;
        let start = *self.starts.get(i)? as usize;
        let mut end = self.starts.get(i + 1).map_or(self.len as usize, |&e| e as usize);
        if end > start && buf[end - 1] == b'\n' {
            end -= 1;
        }
        if end > start && buf[end - 1] == b'\r' {
            end -= 1;
        }
        Some(start..end)
    }

    /// Line containing byte offset `pos`.
    pub fn line_of_offset(&self, pos: u64) -> LineNo {
        match self.starts.binary_search(&pos) {
            Ok(i) => i as LineNo,
            Err(i) => (i - 1) as LineNo,
        }
    }

    /// Byte offset where `line` starts (or end of buffer past the last line).
    pub fn line_start(&self, line: LineNo) -> u64 {
        self.starts.get(line as usize).copied().unwrap_or(self.len)
    }

    /// Approximate heap usage, for diagnostics.
    pub fn memory_bytes(&self) -> usize {
        self.starts.capacity() * std::mem::size_of::<u64>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(buf: &[u8]) -> Vec<String> {
        let idx = LineIndex::build(buf, &Progress::new()).unwrap();
        (0..idx.line_count())
            .map(|l| String::from_utf8_lossy(&buf[idx.line_range(buf, l).unwrap()]).into_owned())
            .collect()
    }

    #[test]
    fn basic() {
        assert_eq!(lines(b"a\nbb\nccc"), ["a", "bb", "ccc"]);
    }

    #[test]
    fn trailing_newline_and_crlf() {
        assert_eq!(lines(b"a\r\nb\r\n"), ["a", "b"]);
    }

    #[test]
    fn empty_lines() {
        assert_eq!(lines(b"\n\nx\n"), ["", "", "x"]);
        assert!(lines(b"").is_empty());
    }

    #[test]
    fn offset_lookup() {
        let buf = b"aa\nbbb\nc";
        let idx = LineIndex::build(buf, &Progress::new()).unwrap();
        assert_eq!(idx.line_of_offset(0), 0);
        assert_eq!(idx.line_of_offset(2), 0);
        assert_eq!(idx.line_of_offset(3), 1);
        assert_eq!(idx.line_of_offset(7), 2);
    }

    #[test]
    fn crosses_chunk_boundaries() {
        let line = "x".repeat(999) + "\n";
        let buf = line.repeat(20_000).into_bytes(); // ~20 MB, > 2 chunks
        let idx = LineIndex::build(&buf, &Progress::new()).unwrap();
        assert_eq!(idx.line_count(), 20_000);
        assert_eq!(idx.line_range(&buf, 19_999).unwrap().len(), 999);
    }
}

//! Format-independent access to log lines.
//!
//! Every input format (plain text, pcap, DLT, …) implements [`LogSource`].
//! Binary formats decode records into displayable text lines; the search
//! engine and UIs only ever see this trait.

pub mod dlt;

use crate::event::{FieldDef, FieldId, FieldValue, Level};
use crate::{LineNo, Progress, Result};
use memmap2::Mmap;
use std::borrow::Cow;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Open `path` with the reader its content calls for: DLT if it starts
/// with a DLT storage header, plain text otherwise.
pub fn open(path: impl AsRef<Path>, progress: &Progress) -> Result<Box<dyn LogSource>> {
    let path = path.as_ref();
    let mut head = [0u8; 4];
    let n = File::open(path)?.read(&mut head)?;
    Ok(if dlt::is_dlt(&head[..n]) {
        Box::new(dlt::DltFile::open(path, progress)?)
    } else {
        Box::new(text::TextFile::open(path, progress)?)
    })
}

/// File contents: a read-only mapping, or owned bytes (tests, empty files).
pub(crate) enum Bytes {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Bytes {
    /// Map `path` read-only. If another process truncates the file while
    /// it is mapped, reads may fault: the same trade-off klogg and ripgrep
    /// make for speed. Follow/tail mode will remap on growth (see roadmap).
    pub(crate) fn map(path: &Path) -> Result<Bytes> {
        let file = File::open(path)?;
        Ok(if file.metadata()?.len() == 0 {
            Bytes::Owned(Vec::new())
        } else {
            // SAFETY: read-only mapping; see doc comment above.
            Bytes::Mapped(unsafe { Mmap::map(&file)? })
        })
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Bytes::Mapped(m) => m,
            Bytes::Owned(v) => v,
        }
    }
}

pub trait LogSource: Send + Sync {
    /// Human readable name (usually the file path).
    fn name(&self) -> &str;

    fn line_count(&self) -> LineNo;

    /// Bytes of a line without its terminator. Text sources borrow them
    /// from the file; decoded formats render them.
    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>>;

    /// Line as text; invalid UTF-8 is replaced with U+FFFD.
    fn line_text(&self, line: LineNo) -> Option<Cow<'_, str>> {
        self.line_bytes(line).map(|b| match b {
            Cow::Borrowed(b) => String::from_utf8_lossy(b),
            Cow::Owned(v) => Cow::Owned(String::from_utf8_lossy(&v).into_owned()),
        })
    }

    /// Size of the underlying file or buffer.
    fn size_bytes(&self) -> u64 {
        0
    }

    /// Memory used by the source's line or message index.
    fn index_memory_bytes(&self) -> usize {
        0
    }

    /// Fields this source supplies for every line (DLT: `ecu`, `apid`, …).
    /// Queries refer to them by name; plain text has none.
    fn schema(&self) -> &[FieldDef] {
        &[]
    }

    /// Value of field `id` (an index into [`Self::schema`]) for `line`.
    /// `None` when the line has no value for it. Must not render the line:
    /// header-only filters rely on this being cheap.
    fn field(&self, _line: LineNo, _id: FieldId) -> Option<FieldValue<'_>> {
        None
    }

    /// The level the format itself records for `line`. The outer `None`
    /// means the source doesn't supply levels, so they're detected from
    /// the text instead.
    fn supplied_level(&self, _line: LineNo) -> Option<Option<Level>> {
        None
    }

    /// Contiguous byte buffer holding all lines separated by `\n`, if the
    /// source has one. Text files return their mapping, which lets the
    /// search engine run the regex over the whole buffer instead of
    /// line-by-line. Decoded formats may return `None`.
    fn contiguous(&self) -> Option<&[u8]> {
        None
    }

    /// Map a byte offset inside [`Self::contiguous`] to a line number.
    fn line_of_offset(&self, _offset: u64) -> LineNo {
        0
    }

    /// Byte offset of the start of `line` inside [`Self::contiguous`].
    fn line_start(&self, _line: LineNo) -> u64 {
        0
    }
}
pub mod text;

#[cfg(test)]
pub(crate) mod mem;

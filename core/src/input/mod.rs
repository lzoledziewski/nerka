//! Format-independent access to log lines.
//!
//! Every input format (plain text, pcap, DLT, …) implements [`LogSource`].
//! Binary formats decode records into displayable text lines; the search
//! engine and UIs only ever see this trait.

pub mod capture;
pub mod concat;
pub mod dlt;
pub mod proto;

use crate::event::{FieldDef, FieldId, FieldValue, Level};
use crate::{LineNo, Progress, Result};
use memmap2::Mmap;
use std::borrow::Cow;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// How files are opened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenOptions {
    /// How DLT is found in captures (pcap, pcapng, MF4).
    pub capture: capture::CaptureOptions,
    /// Descriptions of DLT non-verbose messages (FIBEX).
    pub fibex: Option<std::sync::Arc<dlt::fibex::Fibex>>,
}

/// Open `path` with the reader its content calls for (default
/// [`OpenOptions`]): see [`open_with`].
pub fn open(path: impl AsRef<Path>, progress: &Progress) -> Result<Box<dyn LogSource>> {
    open_with(path, &OpenOptions::default(), progress)
}

/// Open `path` with the reader its content calls for: DLT if it starts
/// with a DLT storage header, the DLT inside a pcap / pcapng / MF4 capture,
/// plain text otherwise.
pub fn open_with(path: impl AsRef<Path>, options: &OpenOptions, progress: &Progress) -> Result<Box<dyn LogSource>> {
    let path = path.as_ref();
    let mut head = [0u8; 16];
    let n = File::open(path)?.read(&mut head)?;
    let head = &head[..n];
    Ok(if dlt::is_dlt(head) {
        Box::new(dlt::DltFile::open(path, progress)?.with_fibex(options.fibex.clone()))
    } else if capture::is_capture(head) {
        Box::new(capture::open(path, &options.capture, progress)?.0.with_fibex(options.fibex.clone()))
    } else {
        Box::new(text::TextFile::open(path, progress)?)
    })
}

/// A [`LogSource::data_id`] no other source has had in this process.
pub(crate) fn new_data_id() -> usize {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

/// A column of a source whose lines are tab-separated cells (DLT).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: &'static str,
    /// Typical content width in characters; 0 for the last column, which
    /// takes the rest of the line.
    pub width: u16,
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

    /// The line's cells for a detailed view of one line: one per
    /// [`Self::columns`] entry (plain text: the whole line as one cell).
    /// Unlike [`Self::line_text`], which flattens a decoded message's own
    /// line breaks and tabs into spaces so it stays one row of cells, the
    /// values keep them (DLT: a payload logged as several lines).
    fn line_detail(&self, line: LineNo) -> Option<Vec<String>> {
        let text = self.line_text(line)?;
        Some(match self.columns().len() {
            0 => vec![text.into_owned()],
            n => text.splitn(n, '\t').map(str::to_owned).collect(),
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

    /// Columns of the rendered lines, if they are tab-separated cells. UIs
    /// show such sources as a table; plain text has none.
    fn columns(&self) -> &[ColumnDef] {
        &[]
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

    /// For a source made of several ([`concat::Concat`]): its parts, each
    /// with its first line in the whole. The engine searches them one by
    /// one, so each keeps its own fast path.
    fn parts(&self) -> Option<Vec<Part<'_>>> {
        None
    }

    /// `(first line, line count)` of each file the source shows: one for a
    /// plain file, one per part for [`concat::Concat`] (bookmarks are kept
    /// per file).
    fn part_files(&self) -> Vec<(LineNo, LineNo)> {
        vec![(0, self.line_count())]
    }

    /// The time of `line` as the source knows it, in seconds since the Unix
    /// epoch (UTC): DLT's storage (receive) time. Plain text has none: a
    /// tab captures it with a `time` signal instead.
    fn time(&self, _line: LineNo) -> Option<f64> {
        None
    }

    /// Argument `index` of `line`'s message, if the source knows the
    /// message's arguments (DLT non-verbose messages described by FIBEX):
    /// a value without a regex, for signals.
    fn argument(&self, _line: LineNo, _index: usize) -> Option<crate::signal::SignalValue> {
        None
    }

    /// Where `line` is, to find it again when row numbers change
    /// (bookmarks): see [`Anchor`]. `None` if the source can't say (a view
    /// of several files: ask its [`Self::parts`]).
    fn anchor(&self, _line: LineNo) -> Option<Anchor> {
        None
    }

    /// The lines that start at these byte offsets of the file (an
    /// [`Anchor::offset`]), in one pass; `None` where no line starts there.
    fn lines_at(&self, offsets: &[u64]) -> Vec<Option<LineNo>> {
        vec![None; offsets.len()]
    }

    /// Identifies the data the source reads, for caches (signal events): a
    /// part of a [`concat::Concat`] answers its file's, so the part made
    /// for each scan is the same one. Files take a [`new_data_id`] when
    /// opened (an address could be reused by the next file); the default,
    /// the address, is for sources that live as long as their caches (tests).
    fn data_id(&self) -> usize {
        self as *const Self as *const () as usize
    }
}

/// Where a line is in its file, to find it again when row numbers change
/// (a capture indexed with other ports, a file that changed): the byte
/// offset it starts at (`None` if its bytes aren't contiguous in the file,
/// e.g. a reassembled datagram) and a hash of its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub offset: Option<u64>,
    pub hash: u32,
}

/// Bytes of a line that its [`Anchor::hash`] covers.
pub const ANCHOR_BYTES: usize = 64;

impl Anchor {
    /// The anchor of a line starting at `offset` whose bytes are `bytes`.
    pub fn of(offset: Option<u64>, bytes: &[u8]) -> Anchor {
        // FNV-1a, 32 bits.
        let hash = bytes[..bytes.len().min(ANCHOR_BYTES)]
            .iter()
            .fold(0x811c_9dc5u32, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
        Anchor { offset, hash }
    }
}

/// One part of a source made of several.
pub struct Part<'a> {
    /// Its first line in the whole.
    pub first: LineNo,
    pub source: Box<dyn LogSource + 'a>,
}
pub mod text;

#[cfg(test)]
pub(crate) mod mem;

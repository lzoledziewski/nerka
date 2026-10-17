//! Format-independent access to log lines.
//!
//! Every input format (plain text, pcap, DLT, …) implements [`LogSource`].
//! Binary formats decode records into displayable text lines; the search
//! engine and UIs only ever see this trait.

use crate::LineNo;
use crate::event::{FieldDef, FieldId, FieldValue, Level};
use std::borrow::Cow;

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

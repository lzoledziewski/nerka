//! Format-independent access to log lines.
//!
//! Every input format (plain text, pcap, DLT, …) implements [`LogSource`].
//! Binary formats decode records into displayable text lines; the search
//! engine and UIs only ever see this trait.

use crate::LineNo;
use std::borrow::Cow;

pub trait LogSource: Send + Sync {
    /// Human readable name (usually the file path).
    fn name(&self) -> &str;

    fn line_count(&self) -> LineNo;

    /// Raw bytes of a line without its terminator.
    fn line_bytes(&self, line: LineNo) -> Option<&[u8]>;

    /// Line as text; invalid UTF-8 is replaced with U+FFFD.
    fn line_text(&self, line: LineNo) -> Option<Cow<'_, str>> {
        self.line_bytes(line).map(String::from_utf8_lossy)
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

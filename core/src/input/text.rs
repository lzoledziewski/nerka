//! Plain-text log files, memory-mapped.

use super::Bytes;
use crate::{LineIndex, LineNo, LogSource, Progress, Result};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

pub struct TextFile {
    name: String,
    path: Option<PathBuf>,
    data: Bytes,
    index: LineIndex,
}

impl TextFile {
    /// Memory-map `path` (see [`Bytes::map`]) and build its line index.
    pub fn open(path: impl AsRef<Path>, progress: &Progress) -> Result<Self> {
        let path = path.as_ref();
        let data = Bytes::map(path)?;
        let index = LineIndex::build(data.bytes(), progress)?;
        Ok(Self { name: path.display().to_string(), path: Some(path.to_path_buf()), data, index })
    }

    /// In-memory source, handy for tests and stdin.
    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        let index = LineIndex::build(&bytes, &Progress::new()).expect("not cancelled");
        Self { name: name.into(), path: None, data: Bytes::Owned(bytes), index }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn index(&self) -> &LineIndex {
        &self.index
    }
}

impl LogSource for TextFile {
    fn name(&self) -> &str {
        &self.name
    }

    fn line_count(&self) -> LineNo {
        self.index.line_count()
    }

    fn size_bytes(&self) -> u64 {
        self.data.bytes().len() as u64
    }

    fn index_memory_bytes(&self) -> usize {
        self.index.memory_bytes()
    }

    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
        let buf = self.data.bytes();
        self.index.line_range(buf, line).map(|r| Cow::Borrowed(&buf[r]))
    }

    fn contiguous(&self) -> Option<&[u8]> {
        Some(self.data.bytes())
    }

    fn line_of_offset(&self, offset: u64) -> LineNo {
        self.index.line_of_offset(offset)
    }

    fn line_start(&self, line: LineNo) -> u64 {
        self.index.line_start(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_detail_is_the_whole_line() {
        let f = TextFile::from_bytes("t", b"a\tb c\r\nnext\n".to_vec());
        assert_eq!(f.line_detail(0), Some(vec!["a\tb c".to_owned()]));
        assert_eq!(f.line_detail(1), Some(vec!["next".to_owned()]));
        assert_eq!(f.line_detail(2), None);
    }
}

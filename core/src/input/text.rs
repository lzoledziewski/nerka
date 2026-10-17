//! Plain-text log files, memory-mapped.

use crate::{LineIndex, LineNo, LogSource, Progress, Result};
use memmap2::Mmap;
use std::borrow::Cow;
use std::fs::File;
use std::path::{Path, PathBuf};

enum Storage {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Storage {
    fn bytes(&self) -> &[u8] {
        match self {
            Storage::Mapped(m) => m,
            Storage::Owned(v) => v,
        }
    }
}

pub struct TextFile {
    name: String,
    path: Option<PathBuf>,
    data: Storage,
    index: LineIndex,
}

impl TextFile {
    /// Memory-map `path` and build its line index.
    ///
    /// The file is mapped read-only. If another process truncates it while
    /// mapped, reads may fault — the same trade-off klogg and ripgrep make
    /// for speed. Follow/tail mode will remap on growth (see roadmap).
    pub fn open(path: impl AsRef<Path>, progress: &Progress) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let data = if len == 0 {
            Storage::Owned(Vec::new())
        } else {
            // SAFETY: read-only mapping; see doc comment above.
            Storage::Mapped(unsafe { Mmap::map(&file)? })
        };
        let index = LineIndex::build(data.bytes(), progress)?;
        Ok(Self { name: path.display().to_string(), path: Some(path.to_path_buf()), data, index })
    }

    /// In-memory source, handy for tests and stdin.
    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        let index = LineIndex::build(&bytes, &Progress::new()).expect("not cancelled");
        Self { name: name.into(), path: None, data: Storage::Owned(bytes), index }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn size_bytes(&self) -> u64 {
        self.data.bytes().len() as u64
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

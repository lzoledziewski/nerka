//! Bookmarks with optional notes, persisted next to the log.
//!
//! Storage is a plain-text sidecar `<log>.nerka-marks`, one bookmark per
//! line: `<1-based line number>\t<note>`. Human-readable, diff-able, and
//! trivially shareable — the first step toward the shared annotations
//! described in the roadmap (a server-backed store will speak the same
//! model).

use crate::LineNo;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bookmarks {
    marks: BTreeMap<LineNo, String>,
}

impl Bookmarks {
    pub fn sidecar_path(log: &Path) -> PathBuf {
        let mut s = log.as_os_str().to_owned();
        s.push(".nerka-marks");
        PathBuf::from(s)
    }

    /// Load bookmarks for `log`; a missing sidecar means no bookmarks.
    pub fn load_for(log: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(Self::sidecar_path(log)) {
            Ok(s) => Ok(Self::parse(&s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Save next to `log`; an empty set removes the sidecar.
    pub fn save_for(&self, log: &Path) -> std::io::Result<()> {
        let path = Self::sidecar_path(log);
        if self.marks.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                r => r,
            };
        }
        std::fs::write(path, self.serialize())
    }

    /// Bookmarks of a view of several files, one after another: each
    /// file's sidecar, its lines shifted to where the file starts in the
    /// view. `files` is `(log, first line)` per file.
    pub fn load_for_parts(files: &[(&Path, LineNo)]) -> std::io::Result<Self> {
        let mut marks = BTreeMap::new();
        for (log, first) in files {
            marks.extend(Self::load_for(log)?.marks.into_iter().map(|(l, n)| (l + first, n)));
        }
        Ok(Self { marks })
    }

    /// Save a view of several files back to each file's sidecar, its lines
    /// as that file numbers them. `files` is `(log, first line, line count)`.
    pub fn save_for_parts(&self, files: &[(&Path, LineNo, LineNo)]) -> std::io::Result<()> {
        for (log, first, count) in files {
            let marks = self.marks.range(first..&(first + count)).map(|(l, n)| (l - first, n.clone())).collect();
            Self { marks }.save_for(log)?;
        }
        Ok(())
    }

    pub fn parse(s: &str) -> Self {
        let marks = s
            .lines()
            .filter_map(|l| {
                let (n, note) = l.split_once('\t').unwrap_or((l, ""));
                let n: LineNo = n.trim().parse().ok()?;
                Some((n.checked_sub(1)?, note.to_owned()))
            })
            .collect();
        Self { marks }
    }

    pub fn serialize(&self) -> String {
        self.marks.iter().map(|(l, n)| format!("{}\t{}\n", l + 1, n.replace(['\n', '\t'], " "))).collect()
    }

    /// Add or remove a bookmark; returns whether the line is now marked.
    pub fn toggle(&mut self, line: LineNo) -> bool {
        if self.marks.remove(&line).is_some() {
            false
        } else {
            self.marks.insert(line, String::new());
            true
        }
    }

    pub fn set_note(&mut self, line: LineNo, note: impl Into<String>) {
        self.marks.insert(line, note.into());
    }

    pub fn contains(&self, line: LineNo) -> bool {
        self.marks.contains_key(&line)
    }

    pub fn note(&self, line: LineNo) -> Option<&str> {
        self.marks.get(&line).map(String::as_str)
    }

    pub fn next_after(&self, line: LineNo) -> Option<LineNo> {
        self.marks.range(line + 1..).next().map(|(l, _)| *l)
    }

    pub fn prev_before(&self, line: LineNo) -> Option<LineNo> {
        self.marks.range(..line).next_back().map(|(l, _)| *l)
    }

    pub fn iter(&self) -> impl Iterator<Item = (LineNo, &str)> {
        self.marks.iter().map(|(l, n)| (*l, n.as_str()))
    }

    pub fn len(&self) -> usize {
        self.marks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn several_files_keep_their_own_sidecars() {
        let dir = std::env::temp_dir().join(format!("nerka-marks-parts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.log"), dir.join("b.log"));
        let mut m = Bookmarks::default();
        m.toggle(1);
        m.set_note(1, "in a");
        m.toggle(10);
        m.toggle(12);
        m.set_note(12, "in b");
        // a has lines 0..10, b 10..20 in the view.
        m.save_for_parts(&[(&a, 0, 10), (&b, 10, 10)]).unwrap();
        assert_eq!(
            std::fs::read_to_string(Bookmarks::sidecar_path(&a)).unwrap(),
            "2	in a
"
        );
        assert_eq!(
            std::fs::read_to_string(Bookmarks::sidecar_path(&b)).unwrap(),
            "1	
3	in b
"
        );
        let back = Bookmarks::load_for_parts(&[(&a, 0), (&b, 10)]).unwrap();
        assert_eq!(back, m);
        // Each file alone sees its own.
        assert_eq!(Bookmarks::load_for(&b).unwrap().iter().map(|(l, _)| l).collect::<Vec<_>>(), [0, 2]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn roundtrip_and_navigation() {
        let mut b = Bookmarks::default();
        assert!(b.toggle(9));
        b.set_note(41, "root cause\there");
        assert!(b.toggle(4));
        assert!(!b.toggle(4));
        assert_eq!(b.next_after(9), Some(41));
        assert_eq!(b.prev_before(41), Some(9));
        assert_eq!(b.next_after(41), None);
        let s = b.serialize();
        assert_eq!(s, "10\t\n42\troot cause here\n");
        assert_eq!(Bookmarks::parse(&s), {
            let mut e = Bookmarks::default();
            e.toggle(9);
            e.set_note(41, "root cause here");
            e
        });
    }

    #[test]
    fn sidecar_file() {
        let dir = std::env::temp_dir().join(format!("nerka-bm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        assert!(Bookmarks::load_for(&log).unwrap().is_empty());
        let mut b = Bookmarks::default();
        b.toggle(0);
        b.save_for(&log).unwrap();
        assert_eq!(Bookmarks::load_for(&log).unwrap(), b);
        b.toggle(0);
        b.save_for(&log).unwrap();
        assert!(!Bookmarks::sidecar_path(&log).exists());
        std::fs::remove_dir_all(dir).ok();
    }
}

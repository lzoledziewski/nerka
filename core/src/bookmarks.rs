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

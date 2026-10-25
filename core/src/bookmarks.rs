//! Bookmarks with optional notes, persisted next to the log.
//!
//! Storage is a plain-text sidecar `<log>.nerka-marks`, one bookmark per
//! line: `<1-based line number>\t<note>\t<name>`. A bookmark has a short
//! name (`B1`, `B2`… given when it is made, renamable) that the GUI shows
//! instead of its line number; a sidecar without names (two columns) gets
//! them when read. Human-readable, diff-able, and
//! trivially shareable — the first step toward the shared annotations
//! described in the roadmap (a server-backed store will speak the same
//! model).

use crate::LineNo;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bookmarks {
    marks: BTreeMap<LineNo, Mark>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Mark {
    name: String,
    note: String,
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
            marks.extend(Self::load_for(log)?.marks.into_iter().map(|(l, m)| (l + first, m)));
        }
        Ok(Self { marks })
    }

    /// Save a view of several files back to each file's sidecar, its lines
    /// as that file numbers them. `files` is `(log, first line, line count)`.
    pub fn save_for_parts(&self, files: &[(&Path, LineNo, LineNo)]) -> std::io::Result<()> {
        for (log, first, count) in files {
            let marks = self.marks.range(first..&(first + count)).map(|(l, m)| (l - first, m.clone())).collect();
            Self { marks }.save_for(log)?;
        }
        Ok(())
    }

    pub fn parse(s: &str) -> Self {
        let mut marks: BTreeMap<LineNo, Mark> = s
            .lines()
            .filter_map(|l| {
                let mut cols = l.splitn(3, '\t');
                let n: LineNo = cols.next()?.trim().parse().ok()?;
                let note = cols.next().unwrap_or("").to_owned();
                let name = cols.next().unwrap_or("").trim().to_owned();
                Some((n.checked_sub(1)?, Mark { name, note }))
            })
            .collect();
        // Unnamed ones (an older sidecar) get names in line order.
        let unnamed: Vec<LineNo> = marks.iter().filter(|(_, m)| m.name.is_empty()).map(|(l, _)| *l).collect();
        for l in unnamed {
            let name = free_name(&marks);
            marks.get_mut(&l).expect("listed").name = name;
        }
        Self { marks }
    }

    pub fn serialize(&self) -> String {
        let clean = |s: &str| s.replace(['\n', '\t'], " ");
        self.marks.iter().map(|(l, m)| format!("{}\t{}\t{}\n", l + 1, clean(&m.note), clean(&m.name))).collect()
    }

    /// Add or remove a bookmark; returns whether the line is now marked. A
    /// new one gets the first free name (`B1`, `B2`…).
    pub fn toggle(&mut self, line: LineNo) -> bool {
        if self.marks.remove(&line).is_some() {
            false
        } else {
            let name = free_name(&self.marks);
            self.marks.insert(line, Mark { name, note: String::new() });
            true
        }
    }

    /// Set the note (bookmarking the line if it isn't yet).
    pub fn set_note(&mut self, line: LineNo, note: impl Into<String>) {
        if !self.contains(line) {
            self.toggle(line);
        }
        self.marks.get_mut(&line).expect("just made").note = note.into();
    }

    /// Rename a bookmark (an empty name gets a free one back).
    pub fn set_name(&mut self, line: LineNo, name: impl Into<String>) {
        let name = name.into().replace(['\n', '\t'], " ").trim().to_owned();
        let Some(m) = self.marks.get_mut(&line) else { return };
        m.name = name;
        if m.name.is_empty() {
            let free = free_name(&self.marks);
            self.marks.get_mut(&line).expect("there").name = free;
        }
    }

    /// Its name (`B1`, or what it was renamed to).
    pub fn name(&self, line: LineNo) -> Option<&str> {
        self.marks.get(&line).map(|m| m.name.as_str())
    }

    /// Bookmarks with their names and notes, in line order.
    pub fn iter_named(&self) -> impl Iterator<Item = (LineNo, &str, &str)> {
        self.marks.iter().map(|(l, m)| (*l, m.name.as_str(), m.note.as_str()))
    }

    pub fn contains(&self, line: LineNo) -> bool {
        self.marks.contains_key(&line)
    }

    pub fn note(&self, line: LineNo) -> Option<&str> {
        self.marks.get(&line).map(|m| m.note.as_str())
    }

    pub fn next_after(&self, line: LineNo) -> Option<LineNo> {
        self.marks.range(line + 1..).next().map(|(l, _)| *l)
    }

    pub fn prev_before(&self, line: LineNo) -> Option<LineNo> {
        self.marks.range(..line).next_back().map(|(l, _)| *l)
    }

    pub fn iter(&self) -> impl Iterator<Item = (LineNo, &str)> {
        self.marks.iter().map(|(l, m)| (*l, m.note.as_str()))
    }

    pub fn len(&self) -> usize {
        self.marks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }
}

/// The first of `B1`, `B2`… no bookmark in `marks` is called.
fn free_name(marks: &BTreeMap<LineNo, Mark>) -> String {
    (1..).map(|n| format!("B{n}")).find(|c| !marks.values().any(|m| m.name == *c)).expect("unbounded")
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
        assert_eq!(std::fs::read_to_string(Bookmarks::sidecar_path(&a)).unwrap(), "2\tin a\tB1\n");
        assert_eq!(std::fs::read_to_string(Bookmarks::sidecar_path(&b)).unwrap(), "1\t\tB2\n3\tin b\tB3\n");
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
        assert_eq!(s, "10\t\tB1\n42\troot cause here\tB2\n");
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

    #[test]
    fn names_are_given_kept_and_renamed() {
        let mut b = Bookmarks::default();
        b.toggle(5);
        b.toggle(2);
        assert_eq!((b.name(5), b.name(2)), (Some("B1"), Some("B2")), "in the order made");
        b.set_name(5, "  boot ");
        assert_eq!(b.name(5), Some("boot"));
        b.toggle(7);
        assert_eq!(b.name(7), Some("B1"), "the first free name");
        b.set_name(7, "");
        assert_eq!(b.name(7), Some("B1"), "an empty name gets a free one");
        let back = Bookmarks::parse(&b.serialize());
        assert_eq!(back, b);
        // An older sidecar: names given in line order, notes kept.
        let old = Bookmarks::parse("9\tsecond\n3\t\n");
        assert_eq!(old.iter_named().collect::<Vec<_>>(), [(2, "B1", ""), (8, "B2", "second")]);
    }
}

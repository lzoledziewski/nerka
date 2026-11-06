//! Bookmarks with optional notes, persisted next to the log.
//!
//! Storage is a plain-text sidecar `<log>.nerka-marks`, one bookmark per
//! line: `<1-based line number>\t<note>\t<name>\t@<offset>:<hash>`. A
//! bookmark has a short name (`B1`, `B2`… given when it is made,
//! renamable) that the GUI shows instead of its line number; a sidecar
//! without names (two columns) gets them when read. Human-readable,
//! diff-able, and trivially shareable — the first step toward the shared
//! annotations described in the roadmap (a server-backed store will speak
//! the same model).
//!
//! **Anchors** (the last column, [`Anchor`]: where the line starts in the
//! file, `-` if its bytes aren't contiguous there, and a hash of its first
//! bytes) keep a bookmark on its line when row numbers change: a capture
//! indexed with other UDP ports, a file that changed. Loading
//! ([`Bookmarks::resolve`]) puts each bookmark on the line its anchor finds
//! (the line at that offset with that hash, else the nearest line with that
//! hash around its old row); one that isn't found stays on its old row,
//! **unsure** ([`Bookmarks::is_unsure`]), until it is edited. A sidecar
//! without anchors (older ones) is taken as it is.

use crate::input::Anchor;
use crate::{LineNo, LogSource};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// How far from its old row a bookmark looks for its line (rows each way).
const NEAR_ROWS: u64 = 5000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bookmarks {
    marks: BTreeMap<LineNo, Mark>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Mark {
    name: String,
    note: String,
    /// As read or last saved; an unsure one keeps the one it was saved with.
    anchor: Option<Anchor>,
    /// Its anchor didn't find the line: it stays on its old row.
    unsure: bool,
}

impl Bookmarks {
    pub fn sidecar_path(log: &Path) -> PathBuf {
        let mut s = log.as_os_str().to_owned();
        s.push(".nerka-marks");
        PathBuf::from(s)
    }

    /// The sidecar of `log` as written; a missing one means no bookmarks.
    pub fn read(log: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(Self::sidecar_path(log)) {
            Ok(s) => Ok(Self::parse(&s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Load bookmarks for `log`, shown by `source`: each on the line its
    /// anchor finds ([`Self::resolve`]).
    pub fn load_for(log: &Path, source: &dyn LogSource) -> std::io::Result<Self> {
        let mut b = Self::read(log)?;
        b.resolve(source);
        Ok(b)
    }

    /// Save next to `log` (anchors from `source`); an empty set removes the
    /// sidecar.
    pub fn save_for(&self, log: &Path, source: &dyn LogSource) -> std::io::Result<()> {
        let path = Self::sidecar_path(log);
        if self.marks.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                r => r,
            };
        }
        std::fs::write(path, self.anchored(source).serialize())
    }

    /// Bookmarks of a view of several files, one after another: each
    /// file's sidecar, resolved against that file, its lines shifted to
    /// where the file starts in the view. `files` is `(log, first line,
    /// the file's source)` per file.
    pub fn load_for_parts(files: &[(&Path, LineNo, &dyn LogSource)]) -> std::io::Result<Self> {
        let mut marks = BTreeMap::new();
        for (log, first, source) in files {
            marks.extend(Self::load_for(log, *source)?.marks.into_iter().map(|(l, m)| (l + first, m)));
        }
        Ok(Self { marks })
    }

    /// Save a view of several files back to each file's sidecar, its lines
    /// as that file numbers them. `files` is `(log, first line, line count,
    /// the file's source)`.
    pub fn save_for_parts(&self, files: &[(&Path, LineNo, LineNo, &dyn LogSource)]) -> std::io::Result<()> {
        for (log, first, count, source) in files {
            let marks = self.marks.range(first..&(first + count)).map(|(l, m)| (l - first, m.clone())).collect();
            Self { marks }.save_for(log, *source)?;
        }
        Ok(())
    }

    /// The sidecar's text, each bookmark (but the unsure ones) anchored on
    /// its line of `source`.
    pub fn serialize_anchored(&self, source: &dyn LogSource) -> String {
        self.anchored(source).serialize()
    }

    /// The same bookmarks, each (but the unsure ones) anchored on its line
    /// of `source`.
    fn anchored(&self, source: &dyn LogSource) -> Self {
        let mut b = self.clone();
        for (&l, m) in b.marks.iter_mut().filter(|(_, m)| !m.unsure) {
            m.anchor = source.anchor(l).or(m.anchor);
        }
        b
    }

    /// Put each bookmark on the line its anchor finds in `source`: the
    /// line at the anchor's offset if its hash matches, else the nearest
    /// line with that hash within [`NEAR_ROWS`] of its old row. One that
    /// isn't found stays on its old row (or the next free one), unsure.
    /// Bookmarks without anchors, and sources without them, stay as they are.
    pub fn resolve(&mut self, source: &dyn LogSource) {
        let n = source.line_count();
        if n == 0 || source.anchor(0).is_none() {
            return;
        }
        let offsets: Vec<u64> = self.marks.values().filter_map(|m| m.anchor?.offset).collect();
        let at: HashMap<u64, LineNo> =
            offsets.iter().zip(source.lines_at(&offsets)).filter_map(|(&o, l)| Some((o, l?))).collect();
        let hash = |l: LineNo| source.anchor(l).map(|a| a.hash);
        let old = std::mem::take(&mut self.marks);
        let mut lost = Vec::new();
        for (row, mut m) in old {
            let Some(a) = m.anchor else {
                self.marks.insert(row, m);
                continue;
            };
            let exact = a.offset.and_then(|o| at.get(&o).copied()).filter(|&l| hash(l) == Some(a.hash));
            // Made in another view of the file (lines, records).
            let exact = exact.or_else(|| source.line_holding(&a));
            let near = || {
                let rows = (1..=NEAR_ROWS).flat_map(|d| [row.checked_sub(d), row.checked_add(d)]).flatten();
                std::iter::once(row).chain(rows).filter(|&l| l < n).find(|&l| hash(l) == Some(a.hash))
            };
            match exact.or_else(near).filter(|l| !self.marks.contains_key(l)) {
                Some(l) => {
                    m.unsure = false;
                    self.marks.insert(l, m);
                }
                None => lost.push((row, m)),
            }
        }
        for (row, mut m) in lost {
            m.unsure = true;
            let free = (row..).find(|l| !self.marks.contains_key(l)).expect("unbounded");
            self.marks.insert(free, m);
        }
    }

    pub fn parse(s: &str) -> Self {
        let mut marks: BTreeMap<LineNo, Mark> = s
            .lines()
            .filter_map(|l| {
                let mut cols = l.splitn(4, '\t');
                let n: LineNo = cols.next()?.trim().parse().ok()?;
                let note = cols.next().unwrap_or("").to_owned();
                let name = cols.next().unwrap_or("").trim().to_owned();
                let anchor = cols.next().and_then(parse_anchor);
                Some((n.checked_sub(1)?, Mark { name, note, anchor, unsure: false }))
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
        self.marks
            .iter()
            .map(|(l, m)| {
                let anchor = match m.anchor {
                    Some(Anchor { offset: Some(o), hash }) => format!("\t@{o}:{hash:08x}"),
                    Some(Anchor { offset: None, hash }) => format!("\t@-:{hash:08x}"),
                    None => String::new(),
                };
                format!("{}\t{}\t{}{anchor}\n", l + 1, clean(&m.note), clean(&m.name))
            })
            .collect()
    }

    /// The same bookmarks on other line numbers (`map` must not merge two
    /// lines): between a view's rows and its files' lines when the rows are
    /// those lines in another order (files merged by time).
    pub fn remapped(&self, map: impl Fn(LineNo) -> LineNo) -> Bookmarks {
        Bookmarks { marks: self.marks.iter().map(|(&l, m)| (map(l), m.clone())).collect() }
    }

    /// Add or remove a bookmark; returns whether the line is now marked. A
    /// new one gets the first free name (`B1`, `B2`…).
    pub fn toggle(&mut self, line: LineNo) -> bool {
        if self.marks.remove(&line).is_some() {
            false
        } else {
            let name = free_name(&self.marks);
            self.marks.insert(line, Mark { name, ..Mark::default() });
            true
        }
    }

    /// Set the note (bookmarking the line if it isn't yet). An unsure
    /// bookmark is sure from now on: it stays on this line.
    pub fn set_note(&mut self, line: LineNo, note: impl Into<String>) {
        if !self.contains(line) {
            self.toggle(line);
        }
        let m = self.marks.get_mut(&line).expect("just made");
        m.note = note.into();
        m.unsure = false;
    }

    /// Rename a bookmark (an empty name gets a free one back). An unsure
    /// bookmark is sure from now on.
    pub fn set_name(&mut self, line: LineNo, name: impl Into<String>) {
        let name = name.into().replace(['\n', '\t'], " ").trim().to_owned();
        let Some(m) = self.marks.get_mut(&line) else { return };
        m.name = name;
        m.unsure = false;
        if m.name.is_empty() {
            let free = free_name(&self.marks);
            self.marks.get_mut(&line).expect("there").name = free;
        }
    }

    /// Whether the bookmark's anchor didn't find its line when it was
    /// loaded: it may be on another line than the one it was made on.
    pub fn is_unsure(&self, line: LineNo) -> bool {
        self.marks.get(&line).is_some_and(|m| m.unsure)
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

/// `@<offset>:<hash>` or `@-:<hash>` (hash in hex).
fn parse_anchor(s: &str) -> Option<Anchor> {
    let (offset, hash) = s.trim().strip_prefix('@')?.split_once(':')?;
    let offset = if offset == "-" { None } else { Some(offset.parse().ok()?) };
    Some(Anchor { offset, hash: u32::from_str_radix(hash, 16).ok()? })
}

/// The first of `B1`, `B2`… no bookmark in `marks` is called.
fn free_name(marks: &BTreeMap<LineNo, Mark>) -> String {
    (1..).map(|n| format!("B{n}")).find(|c| !marks.values().any(|m| m.name == *c)).expect("unbounded")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TextFile;

    /// A text source of `n` lines `line 0`, `line 1`, …
    fn lines(n: usize) -> TextFile {
        TextFile::from_bytes("t", (0..n).map(|i| format!("line {i}\n")).collect::<String>().into_bytes())
    }

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
        let (fa, fb) = (lines(10), lines(10));
        m.save_for_parts(&[(&a, 0, 10, &fa), (&b, 10, 10, &fb)]).unwrap();
        let sidecar = |log: &Path| std::fs::read_to_string(Bookmarks::sidecar_path(log)).unwrap();
        assert_eq!(sidecar(&a), format!("2\tin a\tB1\t@7:{:08x}\n", fa.anchor(1).unwrap().hash));
        let sb = sidecar(&b);
        let rows: Vec<&str> = sb.lines().map(|l| l.rsplit_once('\t').unwrap().0).collect();
        assert_eq!(rows, ["1\t\tB2", "3\tin b\tB3"]);
        let back = Bookmarks::load_for_parts(&[(&a, 0, &fa), (&b, 10, &fb)]).unwrap();
        assert_eq!(back.iter_named().collect::<Vec<_>>(), m.iter_named().collect::<Vec<_>>());
        // Each file alone sees its own.
        assert_eq!(Bookmarks::load_for(&b, &fb).unwrap().iter().map(|(l, _)| l).collect::<Vec<_>>(), [0, 2]);
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
        let f = lines(3);
        assert!(Bookmarks::load_for(&log, &f).unwrap().is_empty());
        let mut b = Bookmarks::default();
        b.toggle(0);
        b.save_for(&log, &f).unwrap();
        assert_eq!(Bookmarks::load_for(&log, &f).unwrap().iter_named().collect::<Vec<_>>(), [(0, "B1", "")]);
        b.toggle(0);
        b.save_for(&log, &f).unwrap();
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

    /// Rows move (lines added before, a capture indexed with other ports):
    /// a bookmark follows its line by its anchor.
    #[test]
    fn bookmarks_follow_their_lines() {
        let before = lines(100);
        let mut b = Bookmarks::default();
        b.set_note(10, "ten");
        b.set_note(50, "fifty");
        let saved = b.anchored(&before).serialize();

        // Three lines put in front: the same lines, three rows later.
        let text = format!("x\ny\nz\n{}", (0..100).map(|i| format!("line {i}\n")).collect::<String>());
        let after = TextFile::from_bytes("t", text.into_bytes());
        let mut back = Bookmarks::parse(&saved);
        back.resolve(&after);
        assert_eq!(back.iter().collect::<Vec<_>>(), [(13, "ten"), (53, "fifty")]);
        assert!(!back.is_unsure(13) && !back.is_unsure(53));

        // A line that changed: not found, so it stays, unsure, until edited.
        let text: String = (0..100).map(|i| if i == 50 { "changed\n".into() } else { format!("line {i}\n") }).collect();
        let changed = TextFile::from_bytes("t", text.into_bytes());
        let mut back = Bookmarks::parse(&saved);
        back.resolve(&changed);
        assert_eq!(back.iter().collect::<Vec<_>>(), [(10, "ten"), (50, "fifty")]);
        assert!(!back.is_unsure(10) && back.is_unsure(50));
        // Saved again, it keeps the anchor it had: found once the line is back.
        let again = back.anchored(&changed).serialize();
        assert_eq!(
            again.lines().nth(1).unwrap().rsplit_once('\t').unwrap().1,
            saved.lines().nth(1).unwrap().rsplit_once('\t').unwrap().1
        );
        back.set_note(50, "fifty, checked");
        assert!(!back.is_unsure(50));

        // An older sidecar without anchors: rows as they are.
        let mut old = Bookmarks::parse("11\tten\tB1\n");
        old.resolve(&after);
        assert_eq!(old.iter().collect::<Vec<_>>(), [(10, "ten")]);
        assert!(!old.is_unsure(10));
    }

    /// A capture indexed again with another protocol's port: the DLT
    /// messages move down, their bookmarks with them.
    #[test]
    fn bookmarks_follow_messages_when_a_capture_changes() {
        use crate::input::capture::{CaptureOptions, from_bytes};
        use crate::input::proto::someip::encode;
        let mut pcap = vec![0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 1, 0, 0, 0];
        let mut frame = |port: u16, payload: &[u8], t: u32| {
            let mut udp = 40000u16.to_be_bytes().to_vec();
            udp.extend_from_slice(&port.to_be_bytes());
            udp.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
            udp.extend_from_slice(&[0, 0]);
            udp.extend_from_slice(payload);
            let mut ip = vec![0x45, 0];
            ip.extend_from_slice(&((20 + udp.len()) as u16).to_be_bytes());
            ip.extend_from_slice(&[0, 1, 0, 0, 64, 17, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2]);
            ip.extend_from_slice(&udp);
            let mut eth = vec![2; 12];
            eth.extend_from_slice(&[8, 0]);
            eth.extend_from_slice(&ip);
            for v in [t, 0, eth.len() as u32, eth.len() as u32] {
                pcap.extend_from_slice(&v.to_le_bytes());
            }
            pcap.extend_from_slice(&eth);
        };
        let dlt = |text: &str| {
            let mut buf = Vec::new();
            let arg = crate::input::dlt::writer::Value::Str(text.into()).into();
            crate::input::dlt::writer::Msg::log(crate::Level::Info, "ECU1", "NAV", "CTX", vec![arg]).encode(&mut buf);
            buf.split_off(16)
        };
        for i in 0..20u32 {
            frame(30501, &encode(0x1234, 1, 0, i as u16, 0, 0, &[]), i);
            frame(3490, &dlt(&format!("message {i}")), i);
        }
        let dlt_only = CaptureOptions::default();
        let both = CaptureOptions { someip_udp_ports: vec![30501], ..Default::default() };
        let (a, _) = from_bytes("t.pcap", pcap.clone(), &dlt_only).unwrap();
        let (b, _) = from_bytes("t.pcap", pcap, &both).unwrap();
        assert_eq!((a.line_count(), b.line_count()), (20, 40));
        let mut m = Bookmarks::default();
        m.set_note(7, "seventh");
        let mut back = Bookmarks::parse(&m.anchored(&a).serialize());
        back.resolve(&b);
        assert_eq!(back.iter().collect::<Vec<_>>(), [(15, "seventh")]);
        assert!(b.line_text(15).unwrap().contains("message 7"));
    }
}

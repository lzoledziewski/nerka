//! How often each library query was used, per user: the library's "Most
//! used" list. Kept out of the pack files, so shared packs don't change
//! with every use.
//!
//! File format (`<config dir>/query-usage`, UTF-8), one query per line:
//!
//! ```text
//! 12<TAB>vision.frame_drops
//! 3<TAB>"camera team.noise"
//! ```
//!
//! The count, a tab (`<TAB>` above), then the qualified id as written (no quoting needed:
//! the id is the rest of the line). Lines that don't read are skipped.

use super::Library;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The usage file in the per-user config directory.
pub fn default_usage_path() -> Option<PathBuf> {
    crate::config_dir().map(|d| d.join("query-usage"))
}

/// Use counts by qualified query id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    counts: BTreeMap<String, u64>,
}

impl Usage {
    pub fn parse(text: &str) -> Usage {
        let counts = text
            .lines()
            .filter_map(|l| {
                let (n, id) = l.split_once('\t')?;
                Some((id.trim_end().to_owned(), n.trim().parse().ok()?))
            })
            .filter(|(id, _): &(String, u64)| !id.is_empty())
            .collect();
        Usage { counts }
    }

    pub fn serialize(&self) -> String {
        self.counts.iter().map(|(id, n)| format!("{n}\t{id}\n")).collect()
    }

    /// Read the file; a missing file is no usage yet.
    pub fn load(path: &Path) -> std::io::Result<Usage> {
        match std::fs::read_to_string(path) {
            Ok(t) => Ok(Usage::parse(&t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Usage::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::write_atomic(path, self.serialize())
    }

    pub fn count(&self, id: &str) -> u64 {
        self.counts.get(id).copied().unwrap_or(0)
    }

    /// Count one use of each query (each once, however often it is listed).
    pub fn record<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>) {
        let mut seen: Vec<&str> = Vec::new();
        for id in ids {
            if !seen.contains(&id) {
                seen.push(id);
                *self.counts.entry(id.to_owned()).or_default() += 1;
            }
        }
    }

    /// A query was renamed: its count follows (added to one the new name
    /// may already have).
    pub fn renamed(&mut self, old: &str, new: &str) {
        if let Some(n) = self.counts.remove(old) {
            *self.counts.entry(new.to_owned()).or_default() += n;
        }
    }

    pub fn removed(&mut self, id: &str) {
        self.counts.remove(id);
    }

    /// The loaded queries used at least once, most used first (ties in
    /// library order), at most `max`.
    pub fn most_used(&self, lib: &Library, max: usize) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = lib
            .packs()
            .iter()
            .flat_map(|p| p.queries.iter().map(move |q| format!("{}.{}", p.name, q.id)))
            .map(|id| {
                let n = self.count(&id);
                (id, n)
            })
            .filter(|(_, n)| *n > 0)
            .collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n)); // stable: library order among ties
        v.truncate(max);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::parse_pack;

    #[test]
    fn counts_round_trip_follow_renames_and_rank_loaded_queries() {
        let mut u = Usage::parse("2\tp.a\nnot a line\n5\t\"camera team.noise\"\nx\tp.b\n");
        assert_eq!((u.count("p.a"), u.count("\"camera team.noise\""), u.count("p.b")), (2, 5, 0));
        u.record(["p.b", "p.a", "p.b"]);
        assert_eq!((u.count("p.a"), u.count("p.b")), (3, 1));
        assert_eq!(Usage::parse(&u.serialize()), u);

        u.renamed("p.b", "p.c");
        assert_eq!((u.count("p.b"), u.count("p.c")), (0, 1));
        u.removed("p.a");
        assert_eq!(u.count("p.a"), 0);

        let mut lib = Library::new();
        lib.add(parse_pack("t", "pack p\nquery a\n  /a/\nquery c\n  /c/\nquery d\n  /d/\n").unwrap()).unwrap();
        u.record(["p.d", "p.d", "p.a"]);
        u.record(["p.d"]);
        // Not loaded ("camera team.noise") and unused (none) aren't listed.
        assert_eq!(u.most_used(&lib, 10), [("p.d".to_owned(), 2), ("p.a".to_owned(), 1), ("p.c".to_owned(), 1)]);
        assert_eq!(u.most_used(&lib, 1).len(), 1);

        let dir = std::env::temp_dir().join(format!("nerka-usage-{}", std::process::id()));
        let path = dir.join("query-usage");
        assert_eq!(Usage::load(&path).unwrap(), Usage::default());
        u.save(&path).unwrap();
        assert_eq!(Usage::load(&path).unwrap(), u);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

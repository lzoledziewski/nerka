//! Recently used filters, most recent first.
//!
//! Stored as plain text, one entry per line: `<kind>\t<pattern>`, where
//! `kind` is a short tag chosen by the caller (the GUI uses `regex` and
//! `query`). Unknown or malformed lines are skipped.

use std::path::Path;

/// Entries kept by default.
pub const DEFAULT_CAPACITY: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHistory {
    entries: Vec<(String, String)>,
    capacity: usize,
}

impl Default for SearchHistory {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl SearchHistory {
    pub fn with_capacity(capacity: usize) -> Self {
        Self { entries: Vec::new(), capacity: capacity.max(1) }
    }

    /// Record a search. A repeated entry moves to the front; blank patterns
    /// are ignored. Returns whether the history changed.
    pub fn push(&mut self, kind: &str, pattern: &str) -> bool {
        let pattern = pattern.trim();
        if pattern.is_empty() || pattern.contains(['\n', '\r']) || kind.contains(['\t', '\n']) {
            return false;
        }
        if self.entries.first().is_some_and(|(k, p)| k == kind && p == pattern) {
            return false;
        }
        self.entries.retain(|(k, p)| !(k == kind && p == pattern));
        self.entries.insert(0, (kind.to_owned(), pattern.to_owned()));
        self.entries.truncate(self.capacity);
        true
    }

    /// Entries, most recent first.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, p)| (k.as_str(), p.as_str()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn parse(s: &str) -> Self {
        let mut h = Self::default();
        for (k, p) in s.lines().filter_map(|l| l.split_once('\t')) {
            if !p.trim().is_empty() && !h.entries.iter().any(|(ek, ep)| ek == k && ep == p) {
                h.entries.push((k.to_owned(), p.to_owned()));
            }
        }
        h.entries.truncate(h.capacity);
        h
    }

    pub fn serialize(&self) -> String {
        self.entries.iter().map(|(k, p)| format!("{k}\t{p}\n")).collect()
    }

    /// Load from `path`; a missing file means an empty history.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(Self::parse(&s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.serialize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(h: &SearchHistory) -> Vec<(&str, &str)> {
        h.iter().collect()
    }

    #[test]
    fn most_recent_first_without_duplicates() {
        let mut h = SearchHistory::with_capacity(3);
        assert!(h.push("regex", "ERROR"));
        assert!(h.push("query", "level >= WARN"));
        assert!(!h.push("query", "  level >= WARN "));
        assert!(h.push("regex", "ERROR"));
        assert_eq!(entries(&h), [("regex", "ERROR"), ("query", "level >= WARN")]);
        // Same pattern, different kind: separate entries.
        assert!(h.push("query", "ERROR"));
        assert!(h.push("regex", "timeout"));
        assert_eq!(entries(&h), [("regex", "timeout"), ("query", "ERROR"), ("regex", "ERROR")]);
        assert!(!h.push("regex", "   "));
        assert!(!h.push("regex", "a\nb"));
    }

    #[test]
    fn roundtrip_keeps_tabs_in_pattern() {
        let mut h = SearchHistory::default();
        h.push("regex", "a\tb");
        h.push("query", "raw ~ /x/");
        let s = h.serialize();
        assert_eq!(s, "query\traw ~ /x/\nregex\ta\tb\n");
        assert_eq!(SearchHistory::parse(&s), h);
        assert!(SearchHistory::parse("garbage\nregex\t\n").is_empty());
    }

    #[test]
    fn file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("nerka-hist-{}", std::process::id()));
        let path = dir.join("sub").join("history");
        assert!(SearchHistory::load(&path).unwrap().is_empty());
        let mut h = SearchHistory::default();
        h.push("regex", "ERROR");
        h.save(&path).unwrap();
        assert_eq!(SearchHistory::load(&path).unwrap(), h);
        std::fs::remove_dir_all(dir).ok();
    }
}

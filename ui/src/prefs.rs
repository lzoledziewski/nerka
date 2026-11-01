//! Options remembered between runs: the last search mode and checkboxes
//! the user picked become the default for new tabs; the UDP ports that
//! carry DLT in captures; the unpack cache's size limit; local time or UTC.
//!
//! Stored as `key=value` lines in `<config>/nerka/prefs`. Unknown keys and
//! bad values are ignored, so an old or hand-edited file never fails.

use crate::Mode;
use crate::view::NoteMode;
use nerka_core::MatcherOptions;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct SearchPrefs {
    /// The Filter box's mode and checkboxes.
    pub mode: Mode,
    pub options: MatcherOptions,
    /// The Search box's mode and checkboxes.
    pub search_mode: Mode,
    pub search_regex: bool,
    pub search_ignore_case: bool,
    /// UDP destination ports that carry DLT in captures (pcap, MF4).
    pub dlt_ports: Vec<u16>,
    /// The unpack cache's size limit (compressed MF4), in GiB.
    pub unpack_limit_gib: u64,
    /// Times shown (and typed) in this computer's local time, else UTC.
    pub local_time: bool,
    /// An MF4 start time marked as local time is read in this computer's
    /// zone, else as UTC.
    pub mf4_local_time: bool,
    /// Where bookmark notes show in the views.
    pub bookmark_notes: NoteMode,
}

impl Default for SearchPrefs {
    /// Auto mode; Text mode searches literally until "Regex" is ticked.
    fn default() -> Self {
        Self {
            mode: Mode::Auto,
            options: MatcherOptions { fixed_string: true, ..Default::default() },
            search_mode: Mode::Auto,
            search_regex: false,
            search_ignore_case: false,
            dlt_ports: nerka_core::input::capture::DEFAULT_DLT_PORTS.to_vec(),
            unpack_limit_gib: nerka_core::input::capture::unpack::DEFAULT_UNPACK_LIMIT >> 30,
            // As DLT Viewer and Wireshark show them (owner, 2026-10-26).
            local_time: true,
            mf4_local_time: true,
            bookmark_notes: NoteMode::default(),
        }
    }
}

impl SearchPrefs {
    pub fn parse(s: &str) -> Self {
        let mut p = Self::default();
        for (key, value) in s.lines().filter_map(|l| l.split_once('=')) {
            let flag = match value.trim() {
                "true" => true,
                "false" => false,
                v => {
                    if key.trim() == "dlt_ports"
                        && let Some(ports) = parse_ports(v)
                    {
                        p.dlt_ports = ports;
                    }
                    if key.trim() == "unpack_limit_gib"
                        && let Some(n) = parse_gib(v)
                    {
                        p.unpack_limit_gib = n;
                    }
                    if key.trim() == "bookmark_notes"
                        && let Some(m) = NoteMode::parse(v)
                    {
                        p.bookmark_notes = m;
                    }
                    let mode = match v {
                        "auto" => Some(Mode::Auto),
                        "text" => Some(Mode::Text),
                        "query" => Some(Mode::Query),
                        _ => None,
                    };
                    match (key.trim(), mode) {
                        ("mode", Some(m)) => p.mode = m,
                        ("search_mode", Some(m)) => p.search_mode = m,
                        _ => {}
                    }
                    continue;
                }
            };
            match key.trim() {
                "regex" => p.options.fixed_string = !flag,
                "ignore_case" => p.options.case_insensitive = flag,
                "invert" => p.options.invert = flag,
                "search_regex" => p.search_regex = flag,
                "search_ignore_case" => p.search_ignore_case = flag,
                "local_time" => p.local_time = flag,
                "mf4_local_time" => p.mf4_local_time = flag,
                _ => {}
            }
        }
        p
    }

    pub fn serialize(&self) -> String {
        let name = |m: Mode| match m {
            Mode::Auto => "auto",
            Mode::Text => "text",
            Mode::Query => "query",
        };
        format!(
            "mode={}\nregex={}\nignore_case={}\ninvert={}\nsearch_mode={}\nsearch_regex={}\nsearch_ignore_case={}\ndlt_ports={}\nunpack_limit_gib={}\nlocal_time={}\nmf4_local_time={}\nbookmark_notes={}\n",
            name(self.mode),
            !self.options.fixed_string,
            self.options.case_insensitive,
            self.options.invert,
            name(self.search_mode),
            self.search_regex,
            self.search_ignore_case,
            ports_text(&self.dlt_ports),
            self.unpack_limit_gib,
            self.local_time,
            self.mf4_local_time,
            self.bookmark_notes.key()
        )
    }

    /// How captures are opened with these prefs.
    pub fn open_options(&self) -> nerka_core::OpenOptions {
        let mut o = nerka_core::OpenOptions::default();
        o.capture.dlt_udp_ports = self.dlt_ports.clone();
        o.capture.unpack.dir = nerka_core::cache_dir();
        o.capture.unpack.limit = self.unpack_limit_gib << 30;
        o.capture.mf4_local_time = self.mf4_local_time;
        o
    }

    /// A missing file means defaults.
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

/// Ports as typed: numbers separated by spaces or commas; at least one,
/// none zero.
pub fn parse_ports(s: &str) -> Option<Vec<u16>> {
    let mut v = Vec::new();
    for w in s.split([' ', ',']).filter(|w| !w.is_empty()) {
        match w.parse::<u16>() {
            Ok(p) if p != 0 => {
                if !v.contains(&p) {
                    v.push(p);
                }
            }
            _ => return None,
        }
    }
    (!v.is_empty()).then_some(v)
}

/// A size limit as typed, in GiB: a whole number from 1 to 1024.
pub fn parse_gib(s: &str) -> Option<u64> {
    s.trim().parse().ok().filter(|n| (1..=1024).contains(n))
}

pub fn ports_text(ports: &[u16]) -> String {
    ports.iter().map(u16::to_string).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let p = SearchPrefs {
            mode: Mode::Query,
            options: MatcherOptions { fixed_string: false, case_insensitive: true, invert: true },
            search_mode: Mode::Text,
            search_regex: true,
            search_ignore_case: true,
            dlt_ports: vec![3490, 3489],
            unpack_limit_gib: 20,
            local_time: false,
            mf4_local_time: false,
            bookmark_notes: NoteMode::Column,
        };
        assert_eq!(SearchPrefs::parse(&p.serialize()), p);
        assert_eq!(SearchPrefs::parse(&SearchPrefs::default().serialize()), SearchPrefs::default());
    }

    #[test]
    fn bad_input_falls_back_to_defaults() {
        assert_eq!(SearchPrefs::parse(""), SearchPrefs::default());
        let p = SearchPrefs::parse("mode=weird\nregex=maybe\nfoo=true\nignore_case = true\n");
        assert_eq!(p.mode, Mode::Auto);
        assert!(p.options.fixed_string);
        assert!(p.options.case_insensitive);
        assert_eq!(p.dlt_ports, [3490]);
        assert_eq!(SearchPrefs::parse("bookmark_notes=sideways\n").bookmark_notes, NoteMode::LineBefore);
        assert_eq!(SearchPrefs::parse("bookmark_notes=end\n").bookmark_notes, NoteMode::EndOfLine);
        // Bad ports keep the default.
        assert_eq!(SearchPrefs::parse("dlt_ports=3490 x\n").dlt_ports, [3490]);
        assert_eq!(SearchPrefs::parse("dlt_ports=13400, 3489\n").dlt_ports, [13400, 3489]);
        assert_eq!(parse_ports(""), None);
        assert_eq!(parse_ports("0"), None);
        assert_eq!(parse_ports("70000"), None);
        assert_eq!(SearchPrefs::parse("unpack_limit_gib=0\n").unpack_limit_gib, 8);
        assert_eq!(SearchPrefs::parse("unpack_limit_gib= 2\n").unpack_limit_gib, 2);
    }
}

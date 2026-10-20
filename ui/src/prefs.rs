//! Options remembered between runs: the last search mode and checkboxes
//! the user picked become the default for new tabs; the UDP ports that
//! carry DLT in captures.
//!
//! Stored as `key=value` lines in `<config>/nerka/prefs`. Unknown keys and
//! bad values are ignored, so an old or hand-edited file never fails.

use crate::Mode;
use nerka_core::MatcherOptions;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct SearchPrefs {
    pub mode: Mode,
    pub options: MatcherOptions,
    /// UDP destination ports that carry DLT in captures (pcap, MF4).
    pub dlt_ports: Vec<u16>,
}

impl Default for SearchPrefs {
    /// Auto mode; Text mode searches literally until "Regex" is ticked.
    fn default() -> Self {
        Self {
            mode: Mode::Auto,
            options: MatcherOptions { fixed_string: true, ..Default::default() },
            dlt_ports: nerka_core::input::capture::DEFAULT_DLT_PORTS.to_vec(),
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
                    if key.trim() == "mode" {
                        match v {
                            "auto" => p.mode = Mode::Auto,
                            "text" => p.mode = Mode::Text,
                            "query" => p.mode = Mode::Query,
                            _ => {}
                        }
                    }
                    continue;
                }
            };
            match key.trim() {
                "regex" => p.options.fixed_string = !flag,
                "ignore_case" => p.options.case_insensitive = flag,
                "invert" => p.options.invert = flag,
                _ => {}
            }
        }
        p
    }

    pub fn serialize(&self) -> String {
        let mode = match self.mode {
            Mode::Auto => "auto",
            Mode::Text => "text",
            Mode::Query => "query",
        };
        format!(
            "mode={mode}\nregex={}\nignore_case={}\ninvert={}\ndlt_ports={}\n",
            !self.options.fixed_string,
            self.options.case_insensitive,
            self.options.invert,
            ports_text(&self.dlt_ports)
        )
    }

    /// How captures are opened with these prefs.
    pub fn open_options(&self) -> nerka_core::OpenOptions {
        let mut o = nerka_core::OpenOptions::default();
        o.capture.dlt_udp_ports = self.dlt_ports.clone();
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
            dlt_ports: vec![3490, 3489],
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
        // Bad ports keep the default.
        assert_eq!(SearchPrefs::parse("dlt_ports=3490 x\n").dlt_ports, [3490]);
        assert_eq!(SearchPrefs::parse("dlt_ports=13400, 3489\n").dlt_ports, [13400, 3489]);
        assert_eq!(parse_ports(""), None);
        assert_eq!(parse_ports("0"), None);
        assert_eq!(parse_ports("70000"), None);
    }
}

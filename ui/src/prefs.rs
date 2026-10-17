//! Search options remembered between runs: the last combination the user
//! picked becomes the default for new tabs.
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
}

impl Default for SearchPrefs {
    /// Text mode searches literally until "Regex" is ticked.
    fn default() -> Self {
        Self { mode: Mode::Text, options: MatcherOptions { fixed_string: true, ..Default::default() } }
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
                    if key.trim() == "mode" {
                        match v {
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
            Mode::Text => "text",
            Mode::Query => "query",
        };
        format!(
            "mode={mode}\nregex={}\nignore_case={}\ninvert={}\n",
            !self.options.fixed_string, self.options.case_insensitive, self.options.invert
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let p = SearchPrefs {
            mode: Mode::Query,
            options: MatcherOptions { fixed_string: false, case_insensitive: true, invert: true },
        };
        assert_eq!(SearchPrefs::parse(&p.serialize()), p);
        assert_eq!(SearchPrefs::parse(&SearchPrefs::default().serialize()), SearchPrefs::default());
    }

    #[test]
    fn bad_input_falls_back_to_defaults() {
        assert_eq!(SearchPrefs::parse(""), SearchPrefs::default());
        let p = SearchPrefs::parse("mode=weird\nregex=maybe\nfoo=true\nignore_case = true\n");
        assert_eq!(p.mode, Mode::Text);
        assert!(p.options.fixed_string);
        assert!(p.options.case_insensitive);
    }
}

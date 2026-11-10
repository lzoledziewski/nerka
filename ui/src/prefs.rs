//! Options remembered between runs: the last search mode and checkboxes
//! the user picked become the default for new tabs; which protocols are
//! read in captures and the rules saying which datagrams are theirs; the
//! unpack cache's size limit; local time or UTC; whether tabs of several
//! files are merged by time.
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
    /// UDP destination ports that carry SOME/IP in captures.
    pub someip_ports: Vec<u16>,
    /// UDP destination ports that carry syslog in captures.
    pub syslog_ports: Vec<u16>,
    /// The unpack cache's size limit (compressed MF4), in GiB.
    pub unpack_limit_gib: u64,
    /// Times shown (and typed) in this computer's local time, else UTC.
    pub local_time: bool,
    /// An MF4 start time marked as local time is read in this computer's
    /// zone, else as UTC.
    pub mf4_local_time: bool,
    /// Where bookmark notes show in the views.
    pub bookmark_notes: NoteMode,
    /// Tabs of several files that know their times interleave their lines
    /// by time (the last choice in a tab's menu).
    pub merge_by_time: bool,
    /// New tabs run their checks by themselves when something changes, or
    /// only on ▶ Run (the last choice in a tab's Checks pane; owner,
    /// 2026-11-09, U2).
    pub checks_auto: bool,
    /// Rules for protocols in captures (`NAME:RULE`, as `--route` takes
    /// them): a protocol given rules uses these instead of its defaults.
    pub routes: Vec<String>,
    /// Protocols not read in captures, by name.
    pub disabled_protocols: Vec<String>,
    /// The protocol definitions in `<config>/nerka/protocols` (not saved
    /// here: read from their files at startup).
    pub protocols: Vec<nerka_core::input::proto::LoadedProtocol>,
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
            someip_ports: nerka_core::input::capture::DEFAULT_SOMEIP_PORTS.to_vec(),
            syslog_ports: nerka_core::input::capture::DEFAULT_SYSLOG_PORTS.to_vec(),
            unpack_limit_gib: nerka_core::input::capture::unpack::DEFAULT_UNPACK_LIMIT >> 30,
            // As DLT Viewer and Wireshark show them (owner, 2026-10-26).
            local_time: true,
            mf4_local_time: true,
            bookmark_notes: NoteMode::default(),
            merge_by_time: false,
            checks_auto: true,
            routes: Vec::new(),
            disabled_protocols: Vec::new(),
            protocols: Vec::new(),
        }
    }
}

impl SearchPrefs {
    pub fn parse(s: &str) -> Self {
        let mut p = Self::default();
        for (key, value) in s.lines().filter_map(|l| l.split_once('=')) {
            match key.trim() {
                "route" => {
                    if value.trim().parse::<nerka_core::input::capture::route::Route>().is_ok() {
                        p.routes.push(value.trim().to_owned());
                    }
                    continue;
                }
                "disabled_protocols" => {
                    p.disabled_protocols = value.split_whitespace().map(str::to_owned).collect();
                    continue;
                }
                _ => {}
            }
            let flag = match value.trim() {
                "true" => true,
                "false" => false,
                v => {
                    if let Some(ports) = parse_ports(v) {
                        match key.trim() {
                            "dlt_ports" => p.dlt_ports = ports,
                            "someip_ports" => p.someip_ports = ports,
                            "syslog_ports" => p.syslog_ports = ports,
                            _ => {}
                        }
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
                "merge_by_time" => p.merge_by_time = flag,
                "checks_auto" => p.checks_auto = flag,
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
        let mut out = format!(
            "mode={}\nregex={}\nignore_case={}\ninvert={}\nsearch_mode={}\nsearch_regex={}\nsearch_ignore_case={}\ndlt_ports={}\nsomeip_ports={}\nsyslog_ports={}\nunpack_limit_gib={}\nlocal_time={}\nmf4_local_time={}\nbookmark_notes={}\nmerge_by_time={}\n",
            name(self.mode),
            !self.options.fixed_string,
            self.options.case_insensitive,
            self.options.invert,
            name(self.search_mode),
            self.search_regex,
            self.search_ignore_case,
            ports_text(&self.dlt_ports),
            ports_text(&self.someip_ports),
            ports_text(&self.syslog_ports),
            self.unpack_limit_gib,
            self.local_time,
            self.mf4_local_time,
            self.bookmark_notes.key(),
            self.merge_by_time
        );
        out.push_str(&format!("checks_auto={}\n", self.checks_auto));
        for r in &self.routes {
            out.push_str(&format!("route={r}\n"));
        }
        out.push_str(&format!("disabled_protocols={}\n", self.disabled_protocols.join(" ")));
        out
    }

    /// How captures are opened with these prefs.
    pub fn open_options(&self) -> nerka_core::OpenOptions {
        let mut o = nerka_core::OpenOptions::default();
        o.capture.dlt_udp_ports = self.dlt_ports.clone();
        o.capture.someip_udp_ports = self.someip_ports.clone();
        o.capture.syslog_udp_ports = self.syslog_ports.clone();
        o.capture.unpack.dir = nerka_core::cache_dir();
        o.capture.unpack.limit = self.unpack_limit_gib << 30;
        o.capture.mf4_local_time = self.mf4_local_time;
        o.capture.protocols = self.protocols.clone();
        o.capture.routes = self.routes.iter().filter_map(|r| r.parse().ok()).collect();
        o.capture.disabled = self.disabled_protocols.clone();
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
            someip_ports: vec![30490, 30501],
            syslog_ports: vec![514, 1514],
            unpack_limit_gib: 20,
            local_time: false,
            mf4_local_time: false,
            bookmark_notes: NoteMode::Column,
            merge_by_time: true,
            checks_auto: false,
            routes: vec!["DLT:port=3490,vlan=5".into()],
            disabled_protocols: vec!["syslog".into()],
            protocols: Vec::new(),
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

#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[test]
    fn protocol_settings_are_saved_and_read_back() {
        let p = SearchPrefs {
            routes: vec!["DLTK:port=3491,src=10.0.0.0/8".into(), "DLT:vlan=5".into()],
            disabled_protocols: vec!["syslog".into(), "SOMEIP".into()],
            ..SearchPrefs::default()
        };
        let back = SearchPrefs::parse(&p.serialize());
        assert_eq!((back.routes, back.disabled_protocols), (p.routes, p.disabled_protocols));
        // A bad rule in the file is left out; the rest is read.
        let back = SearchPrefs::parse("route=DLT:3490\nroute=DLT:port=x\nroute=nothing\n");
        assert_eq!(back.routes, ["DLT:3490"]);
        let o = SearchPrefs::parse("route=DLT:3490,vlan=5\ndisabled_protocols=syslog\n").open_options();
        assert_eq!((o.capture.routes.len(), o.capture.disabled.as_slice()), (1, ["syslog".to_owned()].as_slice()));
    }
}

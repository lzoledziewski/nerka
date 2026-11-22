//! Profiles: how a text file is read, chosen per file (`text-profiles.md`).
//!
//! A profile is one spec line, `NAME: CLAUSE; CLAUSE; …` (the CLI's
//! `--profile` takes the same text; saved ones are lines of
//! `<config>/nerka/profiles`):
//!
//! | Clause | Meaning |
//! |---|---|
//! | `name GLOB[, GLOB…]` | files whose name matches (`*`, `?`; any case) |
//! | `first /REGEX/` | files whose first non-empty line matches |
//! | `records SPEC` | how lines group into rows: `lines`, `start=/…/`, `sep=/…/`, `blank`, `json`, `auto` |
//! | `time auto` | the time a line starts with ([`crate::time::leading_time`]) |
//! | `time /REGEX/` | the regex's group `time` (or its whole match), read by `parse_time` |
//! | `time none` | no time |
//! | `zone Z` | zone of times written without one: `utc`, `local`, `+02:00` (default: as shown) |
//! | `numbers N` | what a plain number counts: `s`, `ms`, `us`, `ns`, `auto` |
//! | `signal SPEC` | a signal switched on for its files (a `--signal` spec; repeatable) |
//!
//! **Choosing** ([`Profiles::choose`]): a profile forced for the file, else
//! the first (in list order) whose `name` matches (and whose `first`
//! matches too, if it has one), else the first whose `first` alone
//! matches, else the default. The default is the profile named `default`
//! if there is one, else the built-in [`Profile::builtin`] (`time auto`).
//! A profile with neither `name` nor `first` is used only when forced.

use super::records::RecordSplit;
use crate::dsl::DslError;
use crate::signal::SignalDef;
use crate::time::{Numbers, Zone};
use regex::Regex;
use std::path::Path;

/// The default profile's name.
pub const DEFAULT: &str = "default";

/// How a profile finds a line's time.
#[derive(Debug, Clone, Default)]
pub enum TimeRule {
    #[default]
    None,
    /// The time the line starts with.
    Leading,
    /// A regex's group `time`, or its whole match.
    Regex(Regex),
}

impl TimeRule {
    pub fn spec(&self) -> String {
        match self {
            TimeRule::None => "none".into(),
            TimeRule::Leading => "auto".into(),
            TimeRule::Regex(r) => format!("/{}/", r.as_str()),
        }
    }

    /// The time in `text` (a line, a record), as the rule reads it.
    pub fn read(&self, text: &str, zone: Zone, numbers: Numbers) -> Option<f64> {
        match self {
            TimeRule::None => None,
            TimeRule::Leading => crate::time::leading_time(text, zone),
            TimeRule::Regex(r) => {
                let c = r.captures(text)?;
                let m = c.name("time").or_else(|| c.get(0))?;
                crate::time::parse_time_as(m.as_str(), zone, numbers)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Profile {
    name: String,
    names: Vec<String>,
    first: Option<Regex>,
    /// `None`: automatic (JSON if it looks like JSON, else lines).
    records: Option<RecordSplit>,
    time: TimeRule,
    zone: Zone,
    numbers: Numbers,
    /// Specs, each a valid [`SignalDef`].
    signals: Vec<String>,
}

impl PartialEq for Profile {
    fn eq(&self, other: &Self) -> bool {
        self.spec() == other.spec()
    }
}

impl Eq for Profile {}

/// `pattern` (`*`, `?`) matches `name`, any case.
fn glob(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) =
        (pattern.to_lowercase().chars().collect(), name.to_lowercase().chars().collect());
    // Iterative matching with one backtrack point for the last `*`.
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < n.len() {
        if i < p.len() && (p[i] == '?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(s) = star {
            i = s + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == '*')
}

impl Profile {
    /// The built-in default: lines or JSON as the file looks, the time a
    /// line starts with.
    pub fn builtin() -> Profile {
        Profile::new(DEFAULT).expect("a name").with_time(TimeRule::Leading)
    }

    /// A profile named `name` with nothing set (no time).
    pub fn new(name: &str) -> Result<Profile, String> {
        let name = name.trim();
        if !crate::signal::is_name(name) {
            return Err(format!("'{name}' isn't a name (letters, digits and _, not first a digit)"));
        }
        Ok(Profile {
            name: name.to_ascii_lowercase(),
            names: Vec::new(),
            first: None,
            records: None,
            time: TimeRule::None,
            zone: Zone::Shown,
            numbers: Numbers::Auto,
            signals: Vec::new(),
        })
    }

    pub fn with_time(mut self, time: TimeRule) -> Profile {
        self.time = time;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn records(&self) -> Option<&RecordSplit> {
        self.records.as_ref()
    }

    pub fn time(&self) -> &TimeRule {
        &self.time
    }

    pub fn zone(&self) -> Zone {
        self.zone
    }

    pub fn numbers(&self) -> Numbers {
        self.numbers
    }

    /// The signals it switches on for its files.
    pub fn signals(&self) -> Vec<SignalDef> {
        self.signals.iter().filter_map(|s| SignalDef::parse_spec(s).ok()).collect()
    }

    /// Whether it is chosen by name or first line at all (else only when
    /// forced, or as the default).
    pub fn matches_by_itself(&self) -> bool {
        !self.names.is_empty() || self.first.is_some()
    }

    fn name_matches(&self, file: &str) -> bool {
        self.names.iter().any(|g| glob(g, file))
    }

    fn first_matches(&self, first: Option<&str>) -> bool {
        self.first.as_ref().is_some_and(|r| first.is_some_and(|l| r.is_match(l)))
    }

    /// Parse a spec line (module docs). Error positions are byte offsets
    /// in `spec`.
    pub fn parse_spec(spec: &str) -> Result<Profile, DslError> {
        let err = |msg: String, pos: usize| DslError { msg, pos };
        let Some(colon) = spec.find(':') else {
            return Err(err(
                "expected NAME: followed by clauses (name, first, records, time, zone, numbers, signal)".into(),
                0,
            ));
        };
        let lead = spec.len() - spec.trim_start().len();
        let mut p = Profile::new(&spec[..colon]).map_err(|m| err(m, lead))?;
        let mut at = colon + 1;
        for clause in crate::lifecycle::clauses(&spec[colon + 1..]) {
            let start = at + clause.len() - clause.trim_start().len();
            at += clause.len() + 1;
            let c = clause.trim();
            if c.is_empty() {
                continue;
            }
            let (word, rest) = c.split_once(char::is_whitespace).unwrap_or((c, ""));
            let rest_at = start + word.len() + (rest.len() - rest.trim_start().len()) + usize::from(!rest.is_empty());
            let rest = rest.trim();
            let need = |what: &str| -> Result<(), DslError> {
                if rest.is_empty() { Err(err(format!("'{what}' needs a value"), start + word.len())) } else { Ok(()) }
            };
            let regex = |s: &str| -> Result<Regex, DslError> {
                let inner = s
                    .strip_prefix('/')
                    .and_then(|s| s.strip_suffix('/'))
                    .ok_or_else(|| err(format!("expected a regex in slashes (/…/) but found '{s}'"), rest_at))?;
                Regex::new(inner).map_err(|e| err(format!("regex: {e}"), rest_at + 1))
            };
            match word.to_ascii_lowercase().as_str() {
                "name" => {
                    need("name")?;
                    p.names = rest.split(',').map(str::trim).filter(|g| !g.is_empty()).map(str::to_owned).collect();
                }
                "first" => {
                    need("first")?;
                    p.first = Some(regex(rest)?);
                }
                "records" => {
                    need("records")?;
                    p.records = RecordSplit::parse_choice(rest).map_err(|e| err(e.to_string(), rest_at))?;
                }
                "time" => {
                    need("time")?;
                    p.time = match rest.to_ascii_lowercase().as_str() {
                        "auto" => TimeRule::Leading,
                        "none" => TimeRule::None,
                        _ => TimeRule::Regex(regex(rest)?),
                    };
                }
                "zone" => {
                    need("zone")?;
                    p.zone = Zone::parse(rest).ok_or_else(|| {
                        err(format!("'zone' needs utc, local or an offset (+02:00) but found '{rest}'"), rest_at)
                    })?;
                }
                "numbers" => {
                    need("numbers")?;
                    p.numbers = Numbers::parse(rest).ok_or_else(|| {
                        err(format!("'numbers' needs s, ms, us, ns or auto but found '{rest}'"), rest_at)
                    })?;
                }
                "signal" => {
                    need("signal")?;
                    SignalDef::parse_spec(rest).map_err(|e| err(format!("signal: {e}"), rest_at))?;
                    p.signals.push(rest.to_owned());
                }
                w => {
                    return Err(err(
                        format!("unknown clause '{w}' (name, first, records, time, zone, numbers, signal)"),
                        start,
                    ));
                }
            }
        }
        Ok(p)
    }

    /// The spec line [`Self::parse_spec`] reads back.
    pub fn spec(&self) -> String {
        let mut parts = Vec::new();
        if !self.names.is_empty() {
            parts.push(format!("name {}", self.names.join(", ")));
        }
        if let Some(r) = &self.first {
            parts.push(format!("first /{}/", r.as_str()));
        }
        if let Some(r) = &self.records {
            parts.push(format!("records {}", r.spec()));
        }
        if !matches!(self.time, TimeRule::None) {
            parts.push(format!("time {}", self.time.spec()));
        }
        if self.zone != Zone::Shown {
            parts.push(format!("zone {}", self.zone.spec()));
        }
        if self.numbers != Numbers::Auto {
            parts.push(format!("numbers {}", self.numbers.spec()));
        }
        parts.extend(self.signals.iter().map(|s| format!("signal {s}")));
        if parts.is_empty() { format!("{}:", self.name) } else { format!("{}: {}", self.name, parts.join("; ")) }
    }
}

/// The profiles to choose from, in order (saved ones, those given for a
/// session first).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profiles(Vec<Profile>);

/// Why a profile was chosen, for people.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Forced,
    Name,
    FirstLine,
    Default,
}

impl Why {
    pub fn text(self) -> &'static str {
        match self {
            Why::Forced => "chosen for the file",
            Why::Name => "by the file's name",
            Why::FirstLine => "by its first line",
            Why::Default => "the default",
        }
    }
}

impl Profiles {
    /// The first line of a `profiles` file.
    pub const HEADER: &str =
        "# nerka profiles: one per line, NAME: CLAUSES (name, first, records, time, zone, numbers, signal)";

    pub fn iter(&self) -> impl Iterator<Item = &Profile> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.0.iter().find(|p| p.name.eq_ignore_ascii_case(name.trim()))
    }

    /// Add `p`, or replace the one of its name (in its place).
    pub fn set(&mut self, p: Profile) {
        match self.0.iter_mut().find(|q| q.name == p.name) {
            Some(q) => *q = p,
            None => self.0.push(p),
        }
    }

    /// Put `p` first (a session's), replacing one of its name.
    pub fn put_first(&mut self, p: Profile) {
        self.0.retain(|q| q.name != p.name);
        self.0.insert(0, p);
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let n = self.0.len();
        self.0.retain(|p| !p.name.eq_ignore_ascii_case(name));
        self.0.len() != n
    }

    /// The default: the one named `default`, else the built-in.
    pub fn default_profile(&self) -> Profile {
        self.get(DEFAULT).cloned().unwrap_or_else(Profile::builtin)
    }

    /// The profile for a file named `path` whose first bytes are `head`, and
    /// why. `forced`: a profile's name chosen for the file (one not here is
    /// ignored: the file is read as if none were forced).
    pub fn choose(&self, forced: Option<&str>, path: &Path, head: &[u8]) -> (Profile, Why) {
        if let Some(p) = forced.and_then(|f| self.get(f)) {
            return (p.clone(), Why::Forced);
        }
        if forced.is_some_and(|f| f.eq_ignore_ascii_case(DEFAULT)) {
            return (self.default_profile(), Why::Forced);
        }
        let file = path.file_name().map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy());
        let first = first_line(head);
        let first = first.as_deref();
        if let Some(p) = self.0.iter().find(|p| p.name_matches(&file) && (p.first.is_none() || p.first_matches(first)))
        {
            return (p.clone(), Why::Name);
        }
        if let Some(p) = self.0.iter().find(|p| p.names.is_empty() && p.first_matches(first)) {
            return (p.clone(), Why::FirstLine);
        }
        (self.default_profile(), Why::Default)
    }

    /// The signals the profiles of a view's files switch on
    /// ([`super::SourceInfo::reading`]), each name once (the first file's).
    pub fn signals_for(&self, reading: &[super::timed::FileReading]) -> Vec<SignalDef> {
        let mut out: Vec<SignalDef> = Vec::new();
        for r in reading {
            let Some(p) = self.get(&r.profile) else { continue };
            for d in p.signals() {
                if !out.iter().any(|o| o.name() == d.name()) {
                    out.push(d);
                }
            }
        }
        out
    }

    /// A `profiles` file's text: the profiles, and one error per line that
    /// isn't one (`line N: …`).
    pub fn parse_text(text: &str) -> (Profiles, Vec<String>) {
        let (mut out, mut errors) = (Profiles::default(), Vec::new());
        for (i, line) in text.lines().enumerate() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            match Profile::parse_spec(l) {
                Ok(p) => out.set(p),
                Err(e) => errors.push(format!("line {}: {}", i + 1, e.at(l))),
            }
        }
        (out, errors)
    }

    /// As a `profiles` file holds them.
    pub fn to_text(&self) -> String {
        let mut out = format!("{}\n", Self::HEADER);
        for p in &self.0 {
            out.push_str(&p.spec());
            out.push('\n');
        }
        out
    }

    /// The saved ones (`<config>/nerka/profiles`); none if there is no
    /// file. Lines that aren't profiles are the errors.
    pub fn load_saved() -> (Profiles, Vec<String>) {
        match crate::config_dir().map(|d| std::fs::read_to_string(d.join("profiles"))) {
            Some(Ok(text)) => Self::parse_text(&text),
            Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => {
                (Profiles::default(), vec![format!("profiles: {e}")])
            }
            _ => (Profiles::default(), Vec::new()),
        }
    }

    /// Write them to `<config>/nerka/profiles`.
    pub fn save(&self) -> std::io::Result<()> {
        let dir = crate::config_dir().ok_or_else(|| std::io::Error::other("no configuration folder"))?;
        std::fs::create_dir_all(&dir)?;
        crate::atomic::write_atomic(dir.join("profiles"), self.to_text())
    }
}

/// The first non-empty line of `head` (a file's first bytes), BOM, leading
/// white space and line break aside.
fn first_line(head: &[u8]) -> Option<String> {
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let line = head
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .find(|l| l.iter().any(|b| !b.is_ascii_whitespace()))?;
    Some(String::from_utf8_lossy(line).trim_start().to_owned())
}

#[cfg(test)]
mod tests;

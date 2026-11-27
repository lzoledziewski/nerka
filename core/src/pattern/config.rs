//! The pattern configuration: the settings of [`super::detect`] and
//! [`super::compare`] and rules scoped by the fields lines are grouped by
//! (DLT `ecu`, `apid`, `ctid`), so the engine can be tuned without a
//! rebuild. One global file, `<config>/nerka/patterns`; design:
//! `docs/dev/patterns.md`.
//!
//! ```text
//! # Settings (the defaults)
//! similarity 0.5            # stage 1: share of words alike to join a template
//! new-wildcards 1           # stage 1: words a line may turn into wildcards at once
//! new-slots 1               # stage 2: slots one merge may make
//! side 4                    # stage 2: tokens on the longer side of a slot
//! state-values 16           # a slot with more values is <*>, not a state
//! header sample 32 min 8 share 0.5 places 3   # header/footer detection
//! merge on                  # stage 2 at all
//! lone-word on              # a message's only word is never a slot
//!
//! # Rules: SCOPE: ACTION
//! apid == NAV: keep Elapsed
//! ecu == ECU1 AND apid == GPS*: max-slots 2
//! *: value /[a-z][\w-]*(\.[a-z][\w-]*)+/
//! ```
//!
//! ```text
//! line    := setting | rule                 -- `#` to the end: a comment (not in /regex/ or "…")
//! setting := "similarity" F | "new-wildcards" N | "new-slots" N | "side" N
//!          | "state-values" N | "header" ("sample" N | "min" N | "share" F | "places" N)+
//!          | "merge" ("on" | "off") | "lone-word" ("on" | "off")
//! rule    := scope ":" action
//! scope   := "*" | FIELD "==" VALUE ("AND" FIELD "==" VALUE)*   -- VALUE: a word or "…", `*` any text
//! action  := "keep" WORD+ | "value" REGEX | "literal" REGEX | "max-slots" N
//!          | "apart" WORD WORD+ | "header" WORD+ | "no-merge" | "ignore" [REGEX]
//!          | "set" setting | "prefix" REGEX   -- prefix: not the message, at its start
//! REGEX   := "/" … "/"                      -- `\/` for a slash
//! ```
//!
//! A rule applies to the lines whose fields match its scope; fields a
//! source doesn't have are empty. Of rules setting one thing (`max-slots`,
//! `no-merge`, `prefix`, each setting of `set`) the first matching one
//! applies; the others (`keep`, `value`, `literal`, `apart`, `header`,
//! `ignore`) add up. A `prefix` rule's named groups are fields of the lines
//! it cuts (`super::Cut`): later rules can be scoped by them.

use regex::Regex;

/// The settings: the engine's tunable numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Stage 1: how alike (0–1, the share of a template's literal tokens) a
    /// message must be to join a template.
    pub similarity: f32,
    /// Stage 1: words a message may turn into wildcards at once.
    pub new_wildcards: u32,
    /// Stage 2: slots one merge may make.
    pub new_slots: u32,
    /// Stage 2: tokens (words, values, slots) on the longer side of a place
    /// two templates differ in, at most.
    pub side: usize,
    /// A slot with more values is `<*>`, not a state (2–16).
    pub state_values: usize,
    /// Header and footer detection: lines of a context held back per chunk.
    pub header_sample: u32,
    /// … the fewest lines it is detected on.
    pub header_min: u32,
    /// … the share of lines a header or footer word is on, more than.
    pub header_share: f32,
    /// … tokens from the start and from the end it may be at.
    pub header_places: usize,
    /// Stage 2 at all.
    pub merge: bool,
    /// A message's only word is never a slot: level words near the start
    /// (`INFO`, `587~DEBUG`) are header, and two messages differing in
    /// their one other word (`dlt_message_init()`, `dlt_message_free()`)
    /// stay two patterns, not a state.
    pub lone_word: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            similarity: 0.5,
            new_wildcards: 1,
            new_slots: 1,
            side: 4,
            state_values: super::MAX_VALUES,
            header_sample: 32,
            header_min: 8,
            header_share: 0.5,
            header_places: 3,
            merge: true,
            lone_word: true,
        }
    }
}

/// One setting with its value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Setting {
    Similarity(f32),
    NewWildcards(u32),
    NewSlots(u32),
    Side(usize),
    StateValues(usize),
    HeaderSample(u32),
    HeaderMin(u32),
    HeaderShare(f32),
    HeaderPlaces(usize),
    Merge(bool),
    LoneWord(bool),
}

impl Setting {
    /// Which setting it is (for "the first one applies").
    fn key(self) -> usize {
        match self {
            Setting::Similarity(_) => 0,
            Setting::NewWildcards(_) => 1,
            Setting::NewSlots(_) => 2,
            Setting::Side(_) => 3,
            Setting::StateValues(_) => 4,
            Setting::HeaderSample(_) => 5,
            Setting::HeaderMin(_) => 6,
            Setting::HeaderShare(_) => 7,
            Setting::HeaderPlaces(_) => 8,
            Setting::Merge(_) => 9,
            Setting::LoneWord(_) => 10,
        }
    }
}

const KEYS: usize = 11;

impl Settings {
    pub fn apply(&mut self, s: Setting) {
        match s {
            Setting::Similarity(v) => self.similarity = v,
            Setting::NewWildcards(v) => self.new_wildcards = v,
            Setting::NewSlots(v) => self.new_slots = v,
            Setting::Side(v) => self.side = v,
            Setting::StateValues(v) => self.state_values = v,
            Setting::HeaderSample(v) => self.header_sample = v,
            Setting::HeaderMin(v) => self.header_min = v,
            Setting::HeaderShare(v) => self.header_share = v,
            Setting::HeaderPlaces(v) => self.header_places = v,
            Setting::Merge(v) => self.merge = v,
            Setting::LoneWord(v) => self.lone_word = v,
        }
    }
}

/// The lines a rule applies to: each field's value matching (`*`: any
/// text); none, every line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    /// (field, value pattern).
    pub conds: Vec<(String, String)>,
}

impl Scope {
    /// Whether a context matches: `value(field)` its fields' values (`None`:
    /// the source has no such field).
    pub fn matches(&self, value: &dyn Fn(&str) -> Option<String>) -> bool {
        self.conds.iter().all(|(f, pat)| glob(pat, value(f).as_deref().unwrap_or("")))
    }
}

/// `text` matches `pat`, where `*` is any text.
fn glob(pat: &str, text: &str) -> bool {
    let Some((head, rest)) = pat.split_once('*') else { return pat == text };
    let Some(mut text) = text.strip_prefix(head) else { return false };
    let parts: Vec<&str> = rest.split('*').collect();
    let (last, middle) = parts.split_last().expect("split gives one at least");
    for p in middle {
        match text.find(p) {
            Some(i) => text = &text[i + p.len()..],
            None => return false,
        }
    }
    text.len() >= last.len() && text.ends_with(last)
}

/// What a rule does.
#[derive(Debug, Clone)]
pub enum Action {
    /// Words never in a slot.
    Keep(Vec<String>),
    /// Text a token holds that is a value (`<*>`), as a digit makes one.
    Value(Regex),
    /// Text a token holds that is never a value, though it holds a digit.
    Literal(Regex),
    /// Slots a pattern has at most.
    MaxSlots(usize),
    /// Words (or atoms: `voltage=`) never in one slot together.
    Apart(Vec<String>),
    /// Header or footer tokens, whatever the detection says.
    Header(Vec<String>),
    /// Stage 2 doesn't merge these lines' templates.
    NoMerge,
    /// Lines left out: all, or those whose message matches.
    Ignore(Option<Regex>),
    /// Settings for these lines.
    Set(Vec<Setting>),
    /// What it matches at the start of a message isn't the message (a
    /// time and a source, `[00:01.2] [uart0:APP] …`); its named groups are
    /// fields of the line (`uart`), as `apid` is. Anchored at the start.
    Prefix(Regex),
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub scope: Scope,
    pub action: Action,
    /// As written.
    pub text: String,
}

/// Settings and rules: what [`super::PatternOptions::config`] holds.
#[derive(Debug, Clone, Default)]
pub struct PatternConfig {
    pub settings: Settings,
    pub rules: Vec<Rule>,
}

/// The settings and rules of one context (resolved by
/// [`PatternConfig::local`]).
#[derive(Debug, Clone, Default)]
pub(super) struct Local {
    pub s: Settings,
    pub keep: Vec<Box<str>>,
    /// Groups of items, each item its atoms.
    pub apart: Vec<Vec<Vec<Box<str>>>>,
    /// The words of `keep` and `apart`: no wildcard in stage 1 (stage 2
    /// tells the cases apart).
    pub guard: Vec<Box<str>>,
    pub values: Vec<Regex>,
    pub literals: Vec<Regex>,
    pub max_slots: Option<usize>,
    pub header: Vec<Box<str>>,
    pub ignore_all: bool,
    pub ignore: Vec<Regex>,
    /// Its `prefix` rule: an index into [`PatternConfig::prefixes`].
    pub prefix: Option<usize>,
    /// Filled by the engine for stage 2 (`lone-word`): the context's header
    /// and footer words, as detected and as `header` rules say …
    pub header_words: std::collections::HashSet<Box<str>>,
    /// … and whether its text holds levels (raw text, not a field).
    pub levels: bool,
}

impl Local {
    /// Whether it leaves `text` (a message) out.
    pub fn ignores(&self, text: &str) -> bool {
        self.ignore_all || self.ignore.iter().any(|r| r.is_match(text))
    }

    /// Tokens are masked as without rules.
    pub fn plain_mask(&self) -> bool {
        self.values.is_empty() && self.literals.is_empty()
    }

    /// `tok` holds a word of `keep` or `apart`.
    pub fn guards(&self, tok: &str) -> bool {
        !self.guard.is_empty() && words(tok).any(|w| self.guard.iter().any(|g| **g == *w))
    }
}

/// The word runs (letters, digits, `_`) of a text.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !super::is_word_char(c)).filter(|w| !w.is_empty())
}

impl PatternConfig {
    /// Parse a `patterns` file; an error names its line (1-based).
    pub fn parse(text: &str) -> Result<PatternConfig, String> {
        let mut c = PatternConfig::default();
        for (i, line) in text.lines().enumerate() {
            c.add_line(line).map_err(|e| format!("line {}: {e}", i + 1))?;
        }
        Ok(c)
    }

    /// A line of the file: a setting, a rule, a comment or blank.
    pub fn add_line(&mut self, line: &str) -> Result<(), String> {
        let toks = tokens(line)?;
        if toks.is_empty() {
            return Ok(());
        }
        if is_rule(&toks) {
            self.add_rule_tokens(toks, line)
        } else {
            for s in setting(&toks)? {
                self.settings.apply(s);
            }
            Ok(())
        }
    }

    /// A setting for all lines: `similarity 0.7`, or `similarity=0.7`.
    pub fn set(&mut self, line: &str) -> Result<(), String> {
        let line = line.replacen('=', " ", 1);
        let toks = tokens(&line)?;
        for s in setting(&toks)? {
            self.settings.apply(s);
        }
        Ok(())
    }

    /// A rule: `apid == NAV: keep Elapsed`.
    pub fn add_rule(&mut self, line: &str) -> Result<(), String> {
        let toks = tokens(line)?;
        if !is_rule(&toks) {
            return Err(format!("'{}' isn't a rule (SCOPE: ACTION, e.g. '*: keep WORD')", line.trim()));
        }
        self.add_rule_tokens(toks, line)
    }

    fn add_rule_tokens(&mut self, toks: Vec<Token>, line: &str) -> Result<(), String> {
        let (scope, rest) = scope(&toks)?;
        let action = action(rest)?;
        let text = strip_comment(line).trim().to_owned();
        self.rules.push(Rule { scope, action, text });
        Ok(())
    }

    /// The configuration saved (`<config>/nerka/patterns`); the defaults if
    /// there is none.
    pub fn load_saved() -> Result<PatternConfig, String> {
        let Some(path) = Self::saved_path() else { return Ok(PatternConfig::default()) };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PatternConfig::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Add `line` (a setting or rule, checked first) at the end of the
    /// saved configuration; returns its file.
    pub fn append_saved(line: &str) -> Result<std::path::PathBuf, String> {
        PatternConfig::default().add_line(line)?;
        let path = Self::saved_path().ok_or("no configuration folder (APPDATA, XDG_CONFIG_HOME or HOME)")?;
        Self::append_to(&path, line)?;
        Ok(path)
    }

    fn append_to(path: &std::path::Path, line: &str) -> Result<(), String> {
        let mut text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                "# nerka pattern configuration: settings and rules (docs: cli.md)\n".to_owned()
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(line.trim());
        text.push('\n');
        // The whole file still reads: a line can't break it.
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        path.parent().map_or(Ok(()), std::fs::create_dir_all).map_err(|e| format!("{}: {e}", path.display()))?;
        crate::write_atomic(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// As a `patterns` file: the settings that aren't the defaults, then
    /// the rules as written. [`Self::parse`] reads it back the same.
    pub fn to_text(&self) -> String {
        let (s, d) = (&self.settings, Settings::default());
        let mut out = String::from("# nerka pattern configuration: settings and rules (docs: cli.md)\n");
        let mut line = |differs: bool, text: String| {
            if differs {
                out.push_str(&text);
                out.push('\n');
            }
        };
        line(s.similarity != d.similarity, format!("similarity {}", s.similarity));
        line(s.new_wildcards != d.new_wildcards, format!("new-wildcards {}", s.new_wildcards));
        line(s.new_slots != d.new_slots, format!("new-slots {}", s.new_slots));
        line(s.side != d.side, format!("side {}", s.side));
        line(s.state_values != d.state_values, format!("state-values {}", s.state_values));
        let mut header = String::new();
        for (differs, text) in [
            (s.header_sample != d.header_sample, format!(" sample {}", s.header_sample)),
            (s.header_min != d.header_min, format!(" min {}", s.header_min)),
            (s.header_share != d.header_share, format!(" share {}", s.header_share)),
            (s.header_places != d.header_places, format!(" places {}", s.header_places)),
        ] {
            if differs {
                header += &text;
            }
        }
        line(!header.is_empty(), format!("header{header}"));
        line(s.merge != d.merge, format!("merge {}", if s.merge { "on" } else { "off" }));
        line(s.lone_word != d.lone_word, format!("lone-word {}", if s.lone_word { "on" } else { "off" }));
        for r in &self.rules {
            out.push_str(&r.text);
            out.push('\n');
        }
        out
    }

    /// Where it is saved.
    pub fn saved_path() -> Option<std::path::PathBuf> {
        crate::config_dir().map(|d| d.join("patterns"))
    }

    /// The `prefix` rules' regexes, in order ([`Local::prefix`] indexes them).
    pub(super) fn prefixes(&self) -> Vec<&Regex> {
        self.rules
            .iter()
            .filter_map(|r| match &r.action {
                Action::Prefix(re) => Some(re),
                _ => None,
            })
            .collect()
    }

    /// The settings and rules for lines whose fields `names` have `vals`.
    pub(super) fn local(&self, names: &[String], vals: &[Option<&[u8]>]) -> Local {
        let mut l = Local { s: self.settings.clone(), ..Local::default() };
        let mut set = [false; KEYS];
        let value = |f: &str| {
            names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(f))
                .and_then(|i| vals.get(i).copied().flatten())
                .map(|b| String::from_utf8_lossy(b).into_owned())
        };
        let mut prefixes = 0;
        for r in &self.rules {
            let prefix = matches!(r.action, Action::Prefix(_)).then(|| {
                prefixes += 1;
                prefixes - 1
            });
            if !r.scope.matches(&value) {
                continue;
            }
            let mut apply = |s: Setting| {
                if !std::mem::replace(&mut set[s.key()], true) {
                    l.s.apply(s);
                }
            };
            match &r.action {
                Action::Keep(ws) => l.keep.extend(ws.iter().map(|w| w.as_str().into())),
                Action::Value(re) => l.values.push(re.clone()),
                Action::Literal(re) => l.literals.push(re.clone()),
                Action::MaxSlots(n) => {
                    l.max_slots.get_or_insert(*n);
                }
                Action::Apart(items) => l.apart.push(
                    items.iter().map(|i| super::atomize(i).into_iter().map(|(_, a)| a.into()).collect()).collect(),
                ),
                Action::Header(ws) => l.header.extend(ws.iter().map(|w| w.as_str().into())),
                Action::NoMerge => apply(Setting::Merge(false)),
                Action::Ignore(None) => l.ignore_all = true,
                Action::Ignore(Some(re)) => l.ignore.push(re.clone()),
                Action::Set(ss) => ss.iter().for_each(|&s| apply(s)),
                Action::Prefix(_) => {
                    l.prefix = l.prefix.or(prefix);
                }
            }
        }
        let mut guard: Vec<Box<str>> = l.keep.clone();
        for group in &l.apart {
            for item in group {
                guard.extend(item.iter().filter(|a| super::is_word(a)).cloned());
            }
        }
        guard.sort();
        guard.dedup();
        l.guard = guard;
        l
    }
}

/// A token of a line: a word (`"…"` unquoted), or a `/regex/`.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    /// Quoted: never a keyword or `*`.
    Quoted(String),
    Regex(String),
}

impl Token {
    fn text(&self) -> &str {
        match self {
            Token::Word(s) | Token::Quoted(s) | Token::Regex(s) => s,
        }
    }
    fn is(&self, w: &str) -> bool {
        matches!(self, Token::Word(s) if s == w)
    }
}

/// `line` up to a `#` comment outside quotes and regexes.
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut prev_space = true;
    let mut esc = false;
    for (i, c) in line.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match (quote, c) {
            (Some(_), '\\') => esc = true,
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"') => quote = Some('"'),
            (None, '/') if prev_space => quote = Some('/'),
            (None, '#') if prev_space => return &line[..i],
            _ => {}
        }
        prev_space = c.is_whitespace();
    }
    line
}

/// The tokens of a line, its comment left out.
fn tokens(line: &str) -> Result<Vec<Token>, String> {
    let line = strip_comment(line);
    let mut out = Vec::new();
    let mut cs = line.chars().peekable();
    while let Some(&c) = cs.peek() {
        if c.is_whitespace() {
            cs.next();
            continue;
        }
        if c == '"' || c == '/' {
            cs.next();
            let mut s = String::new();
            let mut closed = false;
            while let Some(d) = cs.next() {
                if d == '\\' {
                    match cs.next() {
                        // In a regex, `\/` is a slash; other escapes stay for the regex.
                        Some('/') if c == '/' => s.push('/'),
                        Some(e) if c == '/' => {
                            s.push('\\');
                            s.push(e);
                        }
                        Some(e) => s.push(e),
                        None => break,
                    }
                } else if d == c {
                    closed = true;
                    break;
                } else {
                    s.push(d);
                }
            }
            if !closed {
                return Err(format!("{c} without its closing {c}"));
            }
            out.push(if c == '"' { Token::Quoted(s) } else { Token::Regex(s) });
            // `"NAV":` — the colon after a quoted value.
            if cs.peek() == Some(&':') {
                cs.next();
                out.push(Token::Word(":".into()));
            }
            continue;
        }
        let mut s = String::new();
        while let Some(&d) = cs.peek() {
            if d.is_whitespace() {
                break;
            }
            s.push(d);
            cs.next();
        }
        out.push(Token::Word(s));
    }
    Ok(out)
}

/// A rule's tokens start with a scope: `*` or `*:`, or `FIELD ==`.
fn is_rule(toks: &[Token]) -> bool {
    toks[0].is("*") || toks[0].is("*:") || toks.get(1).is_some_and(|t| t.is("=="))
}

/// The scope, and the tokens after its `:`.
fn scope(toks: &[Token]) -> Result<(Scope, &[Token]), String> {
    let mut i = 0;
    let colon = |t: &Token| matches!(t, Token::Word(w) if w == ":");
    if toks[0].is("*:") {
        return Ok((Scope::default(), &toks[1..]));
    }
    if toks[0].is("*") {
        return match toks.get(1) {
            Some(t) if colon(t) => Ok((Scope::default(), &toks[2..])),
            _ => Err("'*' and then ':' (e.g. '*: keep WORD')".into()),
        };
    }
    let mut s = Scope::default();
    loop {
        let field = match toks.get(i) {
            Some(Token::Word(w)) if w.chars().all(|c| c.is_alphanumeric() || c == '_') && !w.is_empty() => w.clone(),
            Some(t) => return Err(format!("'{}' isn't a field name", t.text())),
            None => return Err("a scope without ':'".into()),
        };
        if !toks.get(i + 1).is_some_and(|t| t.is("==")) {
            return Err(format!("'{field}' and then '==' (a scope: FIELD == VALUE AND …)"));
        }
        let (value, mut end) = match toks.get(i + 2) {
            Some(Token::Word(w)) => match w.strip_suffix(':') {
                Some(v) if !v.is_empty() => (v.to_owned(), true),
                _ if w == ":" => return Err(format!("'{field} ==' without a value")),
                _ => (w.clone(), false),
            },
            Some(Token::Quoted(q)) => (q.clone(), false),
            Some(Token::Regex(_)) => return Err("a scope's value is a word, \"…\" or with *, not a regex".into()),
            None => return Err(format!("'{field} ==' without a value")),
        };
        s.conds.push((field, value));
        i += 3;
        if !end && toks.get(i).is_some_and(colon) {
            end = true;
            i += 1;
        }
        if end {
            return Ok((s, &toks[i..]));
        }
        match toks.get(i) {
            Some(t) if t.is("AND") => i += 1,
            Some(t) => return Err(format!("'{}': AND or ':' after a scope's value", t.text())),
            None => return Err("a scope without ':'".into()),
        }
    }
}

fn regex(t: Option<&Token>, what: &str) -> Result<Regex, String> {
    match t {
        Some(Token::Regex(r)) => Regex::new(r).map_err(|e| format!("{what} /{r}/: {e}")),
        Some(t) => Err(format!("{what}: '{}' isn't a /regex/", t.text())),
        None => Err(format!("{what} and then a /regex/")),
    }
}

/// A `prefix` rule's regex, anchored at the start; one that matches empty
/// text would cut nothing from some lines: an error.
fn prefix(t: Option<&Token>) -> Result<Regex, String> {
    let re = regex(t, "prefix")?;
    let anchored = Regex::new(&format!("^(?:{})", re.as_str())).map_err(|e| format!("prefix /{re}/: {e}"))?;
    if anchored.is_match("") {
        return Err(format!("prefix /{re}/ matches empty text: it would cut nothing"));
    }
    Ok(anchored)
}

fn number<T: std::str::FromStr>(t: Option<&Token>, what: &str) -> Result<T, String> {
    match t {
        Some(t) => t.text().parse().map_err(|_| format!("{what}: '{}' isn't a number", t.text())),
        None => Err(format!("{what} and then a number")),
    }
}

fn action(toks: &[Token]) -> Result<Action, String> {
    let Some(first) = toks.first() else { return Err("a rule without an action after ':'".into()) };
    let rest = &toks[1..];
    let words = |at_least: usize, what: &str| -> Result<Vec<String>, String> {
        if let Some(t) = rest.iter().find(|t| matches!(t, Token::Regex(_))) {
            return Err(format!("{what}: '/{}/': words, not a regex", t.text()));
        }
        if rest.len() < at_least {
            return Err(format!("{what} and then {}", if at_least == 1 { "a word" } else { "two words at least" }));
        }
        Ok(rest.iter().map(|t| t.text().to_owned()).collect())
    };
    let none = |a: Action| match rest.first() {
        Some(t) => Err(format!("'{}' after {}", t.text(), first.text())),
        None => Ok(a),
    };
    let one = |a: Action| match rest.get(1) {
        Some(t) => Err(format!("'{}' after {} …", t.text(), first.text())),
        None => Ok(a),
    };
    match first.text() {
        _ if !matches!(first, Token::Word(_)) => Err(format!("'{}' isn't an action", first.text())),
        "keep" => Ok(Action::Keep(words(1, "keep")?)),
        "value" => one(Action::Value(regex(rest.first(), "value")?)),
        "literal" => one(Action::Literal(regex(rest.first(), "literal")?)),
        "max-slots" => one(Action::MaxSlots(number(rest.first(), "max-slots")?)),
        "apart" => Ok(Action::Apart(words(2, "apart")?)),
        "header" => Ok(Action::Header(words(1, "header")?)),
        "no-merge" => none(Action::NoMerge),
        "ignore" => match rest.first() {
            None => Ok(Action::Ignore(None)),
            t => one(Action::Ignore(Some(regex(t, "ignore")?))),
        },
        "set" => Ok(Action::Set(setting(rest)?)),
        "prefix" => one(Action::Prefix(prefix(rest.first())?)),
        a => Err(format!(
            "unknown action '{a}' (keep, value, literal, max-slots, apart, header, no-merge, ignore, set, prefix)"
        )),
    }
}

/// A setting line's tokens: its settings (`header` has several).
fn setting(toks: &[Token]) -> Result<Vec<Setting>, String> {
    let Some(key) = toks.first() else { return Err("a setting without a name".into()) };
    let rest = &toks[1..];
    let one = |s: Setting| match rest.get(1) {
        Some(t) => Err(format!("'{}' after {} {}", t.text(), key.text(), rest[0].text())),
        None => Ok(vec![s]),
    };
    let range = |v: f32, what: &str| {
        if (0.0..=1.0).contains(&v) { Ok(v) } else { Err(format!("{what} is 0–1, not {v}")) }
    };
    match key.text() {
        "similarity" => {
            let v = number(rest.first(), "similarity")?;
            one(Setting::Similarity(range(v, "similarity")?))
        }
        "new-wildcards" => one(Setting::NewWildcards(number(rest.first(), "new-wildcards")?)),
        "new-slots" => one(Setting::NewSlots(number(rest.first(), "new-slots")?)),
        "side" => {
            let v: usize = number(rest.first(), "side")?;
            if v == 0 {
                return Err("side is 1 at least".into());
            }
            one(Setting::Side(v))
        }
        "state-values" => {
            let v: usize = number(rest.first(), "state-values")?;
            if !(2..=super::MAX_VALUES).contains(&v) {
                return Err(format!("state-values is 2–{}, not {v}", super::MAX_VALUES));
            }
            one(Setting::StateValues(v))
        }
        "merge" => match rest.first().map(Token::text) {
            Some("on") => one(Setting::Merge(true)),
            Some("off") => one(Setting::Merge(false)),
            _ => Err("merge on or merge off".into()),
        },
        "lone-word" => match rest.first().map(Token::text) {
            Some("on") => one(Setting::LoneWord(true)),
            Some("off") => one(Setting::LoneWord(false)),
            _ => Err("lone-word on or lone-word off".into()),
        },
        "header" => {
            if rest.is_empty() {
                return Err("header and then sample N, min N, share F or places N".into());
            }
            let mut out = Vec::new();
            for pair in rest.chunks(2) {
                let what = pair[0].text();
                let v = pair.get(1);
                out.push(match what {
                    "sample" => {
                        let n: u32 = number(v, "header sample")?;
                        if n == 0 {
                            return Err("header sample is 1 at least".into());
                        }
                        Setting::HeaderSample(n)
                    }
                    "min" => Setting::HeaderMin(number(v, "header min")?),
                    "share" => Setting::HeaderShare(range(number(v, "header share")?, "header share")?),
                    "places" => {
                        let n: usize = number(v, "header places")?;
                        if !(1..=8).contains(&n) {
                            return Err(format!("header places is 1–8, not {n}"));
                        }
                        Setting::HeaderPlaces(n)
                    }
                    w => return Err(format!("header {w}: sample, min, share or places")),
                });
            }
            Ok(out)
        }
        k => Err(format!(
            "unknown setting '{k}' (similarity, new-wildcards, new-slots, side, state-values, header, merge, lone-word; a rule: SCOPE: ACTION)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_and_comments() {
        let c = PatternConfig::parse(
            "# defaults changed\nsimilarity 0.7   # stage 1\nnew-slots 2\nheader sample 16 min 4 share 0.6 places 2\nmerge off\nlone-word off\n\n",
        )
        .unwrap();
        let s = &c.settings;
        assert_eq!((s.similarity, s.new_slots, s.header_sample, s.header_min), (0.7, 2, 16, 4));
        assert_eq!((s.header_share, s.header_places, s.merge, s.lone_word), (0.6, 2, false, false));
        assert!(c.rules.is_empty());
        assert!(Settings::default().lone_word);
        assert!(c.to_text().contains("lone-word off\n"), "{}", c.to_text());
    }

    #[test]
    fn errors_name_the_line() {
        let e = PatternConfig::parse("similarity 0.5\nsimilarty 0.5\n").unwrap_err();
        assert!(e.starts_with("line 2: unknown setting 'similarty'"), "{e}");
        let bad = [
            ("similarity 2", "0–1"),
            ("side 0", "1 at least"),
            ("state-values 40", "2–16"),
            ("merge maybe", "merge on"),
            ("lone-word yes", "lone-word on"),
            ("header sample", "a number"),
            ("header rows 3", "sample, min"),
            ("apid == NAV keep x", "AND or ':'"),
            ("apid NAV: keep x", "isn't a rule"),
            ("*: frobnicate", "unknown action"),
            ("*: keep", "a word"),
            ("*: apart one", "two words"),
            ("*: value x", "isn't a /regex/"),
            ("*: value /(/", "value /(/"),
            ("*: max-slots many", "isn't a number"),
            ("*: no-merge now", "after no-merge"),
            ("*: value /x", "without its closing"),
            ("*: prefix x", "isn't a /regex/"),
            ("*: prefix /(\\[x\\] )?/", "matches empty text"),
            ("*: prefix /a/ /b/", "after prefix"),
        ];
        for (line, want) in bad {
            let mut c = PatternConfig::default();
            let e = if line.contains("NAV:") && !line.contains("==") {
                c.add_rule(line).unwrap_err()
            } else {
                c.add_line(line).unwrap_err()
            };
            assert!(e.contains(want), "{line}: {e}");
        }
    }

    /// `prefix`: anchored at the start, the first matching rule's, an index
    /// into the prefixes in order; written back as it was.
    #[test]
    fn prefix_rules() {
        let c = PatternConfig::parse(
            "apid == NAV: prefix /\\[\\w+\\] /\n*: prefix /\\[[^\\]]*\\] \\[(?<uart>\\w+):(?<src>\\w+)\\] /\n",
        )
        .unwrap();
        let ps = c.prefixes();
        assert_eq!(ps.len(), 2);
        assert!(ps[1].is_match("[00:01.2] [uart0:APP] boot") && !ps[1].is_match("x [00:01.2] [uart0:APP] boot"));
        let names = vec!["apid".to_string()];
        assert_eq!(c.local(&names, &[Some(b"NAV")]).prefix, Some(0));
        assert_eq!(c.local(&names, &[Some(b"GPS")]).prefix, Some(1));
        assert_eq!(PatternConfig::default().local(&names, &[None]).prefix, None);
        assert!(c.to_text().contains("*: prefix /\\[[^\\]]*\\] \\[(?<uart>"), "{}", c.to_text());
    }

    #[test]
    fn rules_and_scopes() {
        let c = PatternConfig::parse(
            r#"apid == NAV: keep Elapsed   # a comment
ecu == ECU1 AND apid == GPS*: max-slots 2
*: value /[a-z][\w-]*(\.[a-z][\w-]*)+/   # hosts
apid == "PWR": apart voltage= undervoltage
ctid == TCP: no-merge
*: header INFO: WARN:
*: ignore /heart#beat\/x/
ctid == TCP: set similarity 0.7
"#,
        )
        .unwrap();
        assert_eq!(c.rules.len(), 8);
        assert_eq!(c.rules[0].text, "apid == NAV: keep Elapsed");
        assert_eq!(c.rules[1].scope.conds, vec![("ecu".into(), "ECU1".into()), ("apid".into(), "GPS*".into())]);
        match &c.rules[6].action {
            Action::Ignore(Some(re)) => assert_eq!(re.as_str(), "heart#beat/x"),
            a => panic!("{a:?}"),
        }
        let names: Vec<String> = ["ecu", "apid", "ctid"].map(String::from).to_vec();
        let at =
            |e: &str, a: &str, t: &str| c.local(&names, &[Some(e.as_bytes()), Some(a.as_bytes()), Some(t.as_bytes())]);
        let nav = at("ECU1", "NAV", "X");
        assert_eq!(nav.keep, vec!["Elapsed".into()]);
        assert_eq!((nav.max_slots, nav.values.len(), nav.header.len()), (None, 1, 2));
        assert!(nav.guards("Elapsed[us]:") && !nav.guards("Elapseds"));
        let gps = at("ECU1", "GPS2", "X");
        assert_eq!(gps.max_slots, Some(2));
        assert!(gps.keep.is_empty() && at("ECU2", "GPS2", "X").max_slots.is_none());
        let tcp = at("ECU1", "NET", "TCP");
        assert!(!tcp.s.merge);
        assert_eq!(tcp.s.similarity, 0.7);
        assert!(tcp.ignores("a heart#beat/x") && !tcp.ignores("heartbeat"));
        let pwr = at("ECU1", "PWR", "X");
        assert_eq!(pwr.apart.len(), 1);
        assert_eq!(pwr.guard, vec!["undervoltage".into(), "voltage".into()]);
        // A text file has none of the fields: only `*` rules.
        let txt = c.local(&[], &[]);
        assert!(txt.keep.is_empty() && txt.values.len() == 1 && txt.s.merge);
    }

    #[test]
    fn the_first_setting_applies() {
        let c = PatternConfig::parse("similarity 0.4\napid == A: set similarity 0.6\n*: set similarity 0.8 \n*: no-merge\napid == A: set merge on\n").unwrap();
        let names = vec!["apid".to_owned()];
        let a = c.local(&names, &[Some(b"A")]);
        assert_eq!((a.s.similarity, a.s.merge), (0.6, false));
        let b = c.local(&names, &[Some(b"B")]);
        assert_eq!((b.s.similarity, b.s.merge), (0.8, false));
    }

    #[test]
    fn text_reads_back_the_same() {
        let text = "similarity 0.7\nheader min 4 places 2\nmerge off\napid == NAV: keep Elapsed\n*: value /a\\/b/\n";
        let c = PatternConfig::parse(text).unwrap();
        let out = c.to_text();
        assert!(out.ends_with(
            "similarity 0.7\nheader min 4 places 2\nmerge off\napid == NAV: keep Elapsed\n*: value /a\\/b/\n"
        ));
        let again = PatternConfig::parse(&out).unwrap();
        assert_eq!(again.settings, c.settings);
        assert_eq!(again.to_text(), out);
        assert_eq!(PatternConfig::default().to_text().lines().count(), 1);
    }

    #[test]
    fn a_line_is_appended_to_the_file() {
        let dir = std::env::temp_dir().join(format!("nerka-patterns-append-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nerka").join("patterns");
        PatternConfig::append_to(&path, "*: ignore /heartbeat/").unwrap();
        std::fs::write(&path, std::fs::read_to_string(&path).unwrap() + "similarity 0.6").unwrap();
        PatternConfig::append_to(&path, "apid == NAV: keep Elapsed").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# nerka pattern configuration"), "{text}");
        assert!(text.ends_with("*: ignore /heartbeat/\nsimilarity 0.6\napid == NAV: keep Elapsed\n"), "{text}");
        let c = PatternConfig::parse(&text).unwrap();
        assert_eq!((c.rules.len(), c.settings.similarity), (2, 0.6));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn globs() {
        assert!(glob("*", "") && glob("GPS*", "GPS") && glob("GPS*", "GPS2") && !glob("GPS*", "XGPS"));
        assert!(glob("*N*V*", "NAV") && glob("a*c", "abc") && !glob("a*c", "ab") && glob("a*b*a", "aba"));
        assert!(!glob("a*b*a", "ab") && glob("NAV", "NAV") && !glob("NAV", "nav"));
    }

    #[test]
    fn set_from_the_command_line() {
        let mut c = PatternConfig::default();
        c.set("similarity=0.9").unwrap();
        c.set("header=sample 8 min 2").unwrap();
        c.set("new-slots 2").unwrap();
        assert_eq!((c.settings.similarity, c.settings.header_sample, c.settings.header_min), (0.9, 8, 2));
        assert_eq!(c.settings.new_slots, 2);
        assert!(c.set("*: keep x").is_err());
    }
}

//! Pattern detection (experimental): lines grouped by the message they
//! print, whatever values they carry, and the values a message's varying
//! parts take: a **state** (`gear={D|P|N}`, `OTA {downloading|done}`) is a
//! part with a few values that repeat.
//!
//! `connection established peer=10.0.3.7` and `connection established
//! peer=10.0.9.12` are one pattern, `connection established peer=<*>`. Each
//! pattern has its count, its first and last line, its slots (the varying
//! parts) with their values, and an NFD query that finds its lines, so a
//! pattern becomes a filter like any other.
//!
//! In two stages:
//!
//! 1. **Lines → templates** (Drain, He et al. 2017; parallel, per chunk of
//!    lines): a message is split at white space; the parts of a token
//!    holding a digit (numbers, hex, times, addresses) are masked. Messages
//!    of the same field values (DLT `apid`, …), token count and first
//!    literal token are compared token by
//!    token (a token that is only a masked value doesn't count, nor a header
//!    or footer of the context: `Sample`, `Group`); one at least
//!    [`Settings::similarity`] like a template, and new in one token
//!    at most, joins it, its differing tokens becoming slots, else it starts
//!    one. The chunks' templates are merged the same way.
//! 2. **Templates → patterns** (on templates, not lines, so the cost doesn't
//!    grow with the file): templates are split finer, into atoms by
//!    character class (a run of letters, digits and `_`; any other
//!    character; a masked value), and aligned pairwise by longest common
//!    subsequence (as Spell does with tokens), so a varying part can sit
//!    anywhere, inside a token (`gear=D`, `gear=P`), first (`RUNNING state
//!    entered`), or span different lengths (`throttling engaged at <*> C`,
//!    `throttling released`). Two templates join when they share enough
//!    words (`accept`) and no field (`apid`) tells them apart; the parts
//!    where they differ become a slot holding both sides' values. A merge
//!    makes one new slot at most: two at once would change together (two
//!    messages), and join only once a third shows them apart, in a later
//!    pass if need be.
//!
//! Finally, what all of a slot's values begin or end with moves out of the
//! slot (`{gear=D|gear=P}` → `gear={D|P}`).
//!
//! [`compare`] groups two sets of lines (two files, or two filters' lines)
//! together, so a pattern is the same on both sides, and counts each side;
//! [`differences`] ranks what changed between them: patterns and, inside a
//! pattern, its slots' values (`undervoltage` in B only, while
//! `{voltage=|undervoltage}` merged with the measurement holds steady).
//!
//! The message is a field's text (DLT: `payload`) or the line. Cheap text
//! fields (DLT `ecu`, `apid`, `ctid`) with one value on every line of a
//! pattern, where patterns differ in it, go into its query too.
//!
//! The settings (how alike, how many new slots, …) and rules scoped by
//! those fields (`apid == NAV: keep Elapsed`) are a [`PatternConfig`]
//! ([`PatternOptions::config`]), one global file the user can edit:
//! `config.rs`.

use crate::event::{FieldId, FieldKind, FieldValue};
use crate::input::LogSource;
use crate::{Error, LineNo, Progress, Result};
use rayon::prelude::*;
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

mod config;
use config::Local;
pub use config::{Action, PatternConfig, Rule, Scope, Setting, Settings};

// Markers inside texts (private-use characters, so text can't hold them by
// accident): a masked value, a whole slot, white space or none.
const MASK: char = '\u{E000}';
const MASK_STR: &str = "\u{E000}";
const ANY: char = '\u{E001}';
const EITHER: char = '\u{E002}';
/// The rest of a message past [`MAX_TOKENS`].
const REST: char = '\u{E003}';
const REST_STR: &str = "\u{E003}";
/// Lines grouped per task.
const CHUNK: usize = 64 * 1024;
/// Values a slot keeps.
pub const MAX_VALUES: usize = 16;
/// Tokens of a message looked at; the rest is one token, [`REST`].
const MAX_TOKENS: usize = 64;
/// Templates per chunk of lines (stage 1), and in all; past that, a line
/// fitting none goes to the catch-all of its token count ([`Pattern::unsorted`]):
/// lines that hardly repeat (binary read as text) cost neither time nor
/// memory per line.
const MAX_PER_CHUNK: usize = 8192;
const MAX_TEMPLATES: usize = 65536;
/// Templates per group (fields, token count, first literal token), so a
/// line isn't compared with more.
const MAX_GROUP: usize = 256;
/// Templates aligned (stage 2), most lines first; the rest stay as they are.
const MAX_MERGE: usize = 10_000;
/// Passes of stage 2 at most (the later ones over the patterns kept apart).
const MAX_PASSES: usize = 4;
/// Templates of more atoms aren't aligned (stage 2).
const MAX_ATOMS: usize = 256;
/// Patterns a template is aligned with, at most (stage 2).
const CANDIDATES: usize = 64;

#[derive(Debug, Clone, Default)]
pub struct PatternOptions {
    /// The field whose text is grouped; `None`: `payload` if the source has
    /// it, else the line (`raw`).
    pub field: Option<String>,
    /// Keep each pattern's lines ([`Pattern::lines`]): 8 bytes a line.
    pub lines: bool,
    /// Settings and rules ([`PatternConfig`]).
    pub config: PatternConfig,
}

/// Lines of one set: how many, the first and the last.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub count: u64,
    pub first: LineNo,
    pub last: LineNo,
}

impl Tally {
    fn add(&mut self, o: Tally) {
        if o.count == 0 {
        } else if self.count == 0 {
            *self = o;
        } else {
            self.count += o.count;
            self.first = self.first.min(o.first);
            self.last = self.last.max(o.last);
        }
    }
}

/// Lines per set: set A's, and set B's in a comparison (else none).
pub type Sides = [Tally; 2];

/// One line of set `side`.
fn one(side: usize, line: LineNo) -> Sides {
    let mut s = Sides::default();
    s[side] = Tally { count: 1, first: line, last: line };
    s
}

fn add_sides(a: &mut Sides, b: &Sides) {
    for (x, y) in a.iter_mut().zip(b) {
        x.add(*y);
    }
}

fn total(s: &Sides) -> u64 {
    s[0].count + s[1].count
}

/// The first line: set A's, if it has lines there.
fn first_of(s: &Sides) -> LineNo {
    if s[0].count > 0 { s[0].first } else { s[1].first }
}

/// Most lines first, then by first line.
fn by_count(a: &Sides, b: &Sides) -> std::cmp::Ordering {
    total(b).cmp(&total(a)).then(first_of(a).cmp(&first_of(b)))
}

/// The values a slot took.
#[derive(Debug, Clone, Default)]
pub struct Slot {
    /// Most lines first (in a pattern; while grouping, as found).
    pub values: Vec<SlotValue>,
    /// It took more than [`MAX_VALUES`] values; those past it aren't kept.
    pub overflow: bool,
    /// A value may be empty or span white space.
    wide: bool,
}

#[derive(Debug, Clone)]
pub struct SlotValue {
    /// With the markers.
    text: String,
    /// Its lines per set.
    pub sides: Sides,
}

impl SlotValue {
    /// Its lines (both sets).
    pub fn count(&self) -> u64 {
        total(&self.sides)
    }
    pub fn first(&self) -> LineNo {
        first_of(&self.sides)
    }
    pub fn last(&self) -> LineNo {
        if self.sides[0].count > 0 { self.sides[0].last } else { self.sides[1].last }
    }

    /// The value: masked parts as `<*>`, empty as `∅`.
    pub fn value(&self) -> String {
        if self.text.is_empty() {
            return "∅".into();
        }
        self.text.replace([MASK, ANY], "<*>").replace(EITHER, " ").replace(REST, "…")
    }
}

impl Slot {
    fn add(&mut self, text: &str, sides: &Sides) {
        if let Some(v) = self.values.iter_mut().find(|v| v.text == text) {
            add_sides(&mut v.sides, sides);
        } else if self.values.len() < MAX_VALUES {
            self.values.push(SlotValue { text: text.to_owned(), sides: *sides });
        } else {
            self.overflow = true;
        }
    }

    fn join(&mut self, other: Slot) {
        for v in other.values {
            self.add(&v.text, &v.sides);
        }
        self.overflow |= other.overflow;
        self.wide |= other.wide;
    }

    /// A state: all its values known, at least two, one of them on more than
    /// one line, and each with a letter (not only a masked number) or empty.
    pub fn is_state(&self) -> bool {
        let lines: u64 = self.values.iter().map(SlotValue::count).sum();
        !self.overflow
            && self.values.len() >= 2
            && lines > self.values.len() as u64
            && self.values.iter().all(|v| v.text.is_empty() || v.text.chars().any(char::is_alphabetic))
    }

    fn regex(&self, re: &mut String, loose: bool) {
        if self.is_state() {
            re.push_str("(?:");
            for (j, v) in self.values.iter().enumerate() {
                if j > 0 {
                    re.push('|');
                }
                text_regex(&v.text, re, loose);
            }
            re.push(')');
        } else if self.wide {
            re.push_str(".*?");
        } else {
            re.push_str(r"\S+");
        }
    }
}

/// What comes before a part: white space (`a b`), nothing (`gear` `=`), or
/// either (the pattern's messages differ).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sep {
    Space,
    Tight,
    Either,
}

#[derive(Debug, Clone)]
enum Atom {
    /// A word (letters, digits, `_`), another character, or [`MASK`].
    Lit(Box<str>),
    Slot(Slot),
}

#[derive(Debug, Clone)]
struct Part {
    sep: Sep,
    atom: Atom,
}

/// A piece of a pattern's template ([`Pattern::pieces`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Text, with its spaces (a masked value in it: `<*>`).
    Text(String),
    /// A slot of any value: `<*>`.
    Any,
    /// A state's values, most lines first (`∅`: none).
    State(Vec<String>),
}

/// One pattern: a message template and where it occurs.
#[derive(Debug, Clone)]
pub struct Pattern {
    parts: Vec<Part>,
    /// Its lines per set.
    pub sides: Sides,
    /// Not a template: lines of its token count that fit none, past the
    /// templates kept (8192 per chunk of lines).
    pub unsorted: bool,
    /// Fields with one value on all its lines (and not on all patterns'):
    /// `(name, value)`.
    pub fields: Vec<(String, String)>,
    /// Its lines per set, ascending, with [`PatternOptions::lines`] (else
    /// none).
    pub lines: [Vec<LineNo>; 2],
    /// The stage-1 templates it holds (ids given in [`group`]).
    ids: Vec<u32>,
    /// Its masked values may be any text (`value` rules): their regex is
    /// any token's.
    loose: bool,
}

impl Pattern {
    /// Its lines (both sets).
    pub fn count(&self) -> u64 {
        total(&self.sides)
    }
    pub fn first(&self) -> LineNo {
        first_of(&self.sides)
    }
    pub fn last(&self) -> LineNo {
        if self.sides[0].count > 0 { self.sides[0].last } else { self.sides[1].last }
    }

    /// The template: `<*>` where messages differ, a state's values as
    /// `{D|P|N}`.
    pub fn template(&self) -> String {
        let mut s = String::new();
        for p in self.pieces() {
            match p {
                Piece::Text(t) => s.push_str(&t),
                Piece::Any => s.push_str("<*>"),
                Piece::State(vs) => {
                    s.push('{');
                    s.push_str(&vs.join("|"));
                    s.push('}');
                }
            }
        }
        s
    }

    /// The template in pieces, to show each as it is: text (masked values
    /// in it as `<*>`), slots of any value, and states with their values.
    pub fn pieces(&self) -> Vec<Piece> {
        fn text(out: &mut Vec<Piece>, s: &str) {
            match out.last_mut() {
                Some(Piece::Text(t)) => t.push_str(s),
                _ => out.push(Piece::Text(s.to_owned())),
            }
        }
        let mut out = Vec::new();
        let punct = |p: &Part| matches!(&p.atom, Atom::Lit(l) if !is_word(l) && &**l != MASK_STR);
        for (i, p) in self.parts.iter().enumerate() {
            // Either: a space, but next to punctuation (`[{INFO|WARN}]`).
            let space = i > 0
                && match p.sep {
                    Sep::Space => true,
                    Sep::Tight => false,
                    Sep::Either => !punct(p) && !punct(&self.parts[i - 1]),
                };
            if space {
                text(&mut out, " ");
            }
            match &p.atom {
                Atom::Lit(l) => text(&mut out, &l.replace(MASK, "<*>").replace(REST, "…")),
                Atom::Slot(slot) if slot.is_state() => {
                    out.push(Piece::State(slot.values.iter().map(SlotValue::value).collect()))
                }
                Atom::Slot(_) => out.push(Piece::Any),
            }
        }
        out
    }

    /// Its slots, in order, with the values they took.
    pub fn slots(&self) -> impl Iterator<Item = &Slot> {
        self.parts.iter().filter_map(|p| match &p.atom {
            Atom::Slot(s) => Some(s),
            Atom::Lit(_) => None,
        })
    }

    /// A regex matching every message of the pattern, whole.
    pub fn regex(&self) -> String {
        self.regex_named(&[])
    }

    /// [`Self::regex`], each state slot captured by the group of its name in
    /// `states` (in order; past them, not captured).
    fn regex_named(&self, states: &[String]) -> String {
        parts_regex(&self.parts, states, self.loose)
    }

    /// Names for its states, as signals: the word before each (`gear=` →
    /// `gear`), else `state`; numbered where taken. Names are lower case
    /// letters, digits and `_`.
    pub fn state_names(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (i, p) in self.parts.iter().enumerate() {
            let Atom::Slot(s) = &p.atom else { continue };
            if !s.is_state() {
                continue;
            }
            let word = self.parts[..i].iter().rev().take(3).find_map(|q| match &q.atom {
                Atom::Lit(l) if is_word(l) && l.chars().any(char::is_alphabetic) => Some(l.to_lowercase()),
                _ => None,
            });
            let mut base: String = word
                .unwrap_or_else(|| "state".into())
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !base.starts_with(|c: char| c.is_ascii_alphabetic()) {
                base = format!("s_{base}");
            }
            let mut name = base.clone();
            let mut n = 2;
            while out.contains(&name) {
                name = format!("{base}_{n}");
                n += 1;
            }
            out.push(name);
        }
        out
    }
}

/// A pattern as a library query ([`Patterns::library_query`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryQuery {
    /// Its name: its first words (`gear`, `session_to`).
    pub name: String,
    /// NFD finding its lines, each state captured by a named group.
    pub nfd: String,
    /// The groups: its states, as enum signals.
    pub states: Vec<String>,
}

/// The regex of a template's `parts`: each state slot captured by the
/// group of its name in `states` (in order; past them, not captured);
/// `loose`: masked values any text ([`text_regex`]).
fn parts_regex(parts: &[Part], states: &[String], loose: bool) -> String {
    let mut re = String::from(r"^\s*");
    let mut k = 0;
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            re.push_str(sep_regex(p.sep));
        }
        match &p.atom {
            Atom::Lit(l) => text_regex(l, &mut re, loose),
            Atom::Slot(s) if s.is_state() && k < states.len() => {
                re.push_str(&format!("(?<{}>", states[k]));
                s.regex(&mut re, loose);
                re.push(')');
                k += 1;
            }
            Atom::Slot(s) => s.regex(&mut re, loose),
        }
    }
    re.push_str(r"\s*$");
    re
}

fn sep_regex(sep: Sep) -> &'static str {
    match sep {
        Sep::Space => r"\s+",
        Sep::Tight => "",
        Sep::Either => r"\s*",
    }
}

/// A regex for a text with markers; `loose`: a masked value is any text
/// (`value` rules), not only what [`mask`] masks.
fn text_regex(text: &str, re: &mut String, loose: bool) {
    let mut buf = [0u8; 4];
    for c in text.chars() {
        match c {
            // What `mask` replaces: digit-holding word runs and the `.`,
            // `:`, `-` between them.
            MASK if loose => re.push_str(r"\S+?"),
            MASK => re.push_str(r"[\w.:\-]+"),
            ANY => re.push_str(".*?"),
            REST => re.push_str(".*"),
            EITHER => re.push_str(r"\s*"),
            c if c.is_whitespace() => re.push_str(r"\s+"),
            c => re.push_str(&regex::escape(c.encode_utf8(&mut buf))),
        }
    }
}

/// The patterns of a source's lines, most lines first.
#[derive(Debug, Clone)]
pub struct Patterns {
    /// The field grouped; `None`: the line.
    pub field: Option<String>,
    pub patterns: Vec<Pattern>,
    /// Lines looked at, per set (those `ignore` rules leave out aside).
    pub lines: [u64; 2],
    pub stats: PatternStats,
}

/// How a run went: what tuning looks at.
#[derive(Debug, Clone, Default)]
pub struct PatternStats {
    /// Stage 1's templates.
    pub templates: usize,
    pub stage1: Duration,
    pub stage2: Duration,
    /// Stage 2: templates kept apart from a pattern alike enough, because
    /// the merge would make more new slots than `new-slots`, …
    pub apart: usize,
    /// … a side longer than `side` (or two phrases), …
    pub side: usize,
    /// … or a rule (`keep`, `apart`, `max-slots`).
    pub rules: usize,
    /// Lines left out by `ignore` rules, per set.
    pub ignored: [u64; 2],
}

impl Patterns {
    /// Lines looked at (both sets).
    pub fn total(&self) -> u64 {
        self.lines[0] + self.lines[1]
    }

    /// A configuration rule leaving `p`'s lines out ([`PatternConfig`]):
    /// scoped by the fields that tell it apart, its messages by its regex
    /// (`apid == "NAV": ignore /^\s*heartbeat\s*$/`).
    pub fn ignore_rule(&self, p: &Pattern) -> String {
        let quote = |v: &str| {
            let mut q = String::from('"');
            for c in v.chars() {
                if c == '"' || c == '\\' {
                    q.push('\\');
                }
                q.push(c);
            }
            q.push('"');
            q
        };
        let scope: Vec<String> = p.fields.iter().map(|(n, v)| format!("{n} == {}", quote(v))).collect();
        let scope = if scope.is_empty() { "*".to_owned() } else { scope.join(" AND ") };
        format!("{scope}: ignore /{}/", p.regex().replace('/', "\\/"))
    }

    /// `p` as a library query: its query, with each state captured by a
    /// group named for it, so the query makes enum signals of them
    /// (`gear=(?<gear>(?:D|P|N))`: `@gear`).
    pub fn library_query(&self, p: &Pattern) -> LibraryQuery {
        let states = p.state_names();
        let mut words = Vec::new();
        for piece in p.pieces() {
            if let Piece::Text(t) = piece {
                for w in
                    t.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| w.chars().any(|c| c.is_ascii_alphabetic()))
                {
                    if words.len() < 3 {
                        words.push(w.to_lowercase());
                    }
                }
            }
        }
        let mut name = words.join("_");
        if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
            name = format!("pattern_{name}").trim_end_matches('_').to_owned();
        }
        let nfd = self.query_with(p, &p.regex_named(&states));
        LibraryQuery { name, nfd, states }
    }

    /// NFD finding the lines of `p`, e.g. `apid == "NAV" AND payload ~ /^…$/`.
    pub fn query(&self, p: &Pattern) -> String {
        self.query_with(p, &p.regex())
    }

    fn query_with(&self, p: &Pattern, re: &str) -> String {
        let mut q = String::new();
        for (name, value) in &p.fields {
            q.push_str(name);
            q.push_str(" == \"");
            for c in value.chars() {
                if c == '"' || c == '\\' {
                    q.push('\\');
                }
                q.push(c);
            }
            q.push_str("\" AND ");
        }
        q.push_str(self.field.as_deref().unwrap_or("raw"));
        q.push_str(" ~ /");
        q.push_str(&re.replace('/', r"\x2f"));
        q.push('/');
        q
    }
}

/// Lines to group: a source's, or only `lines` (ascending) of it.
#[derive(Clone, Copy)]
pub struct Set<'a> {
    pub source: &'a dyn LogSource,
    pub lines: Option<&'a [LineNo]>,
}

/// Group the lines of `source` (or only `lines`, ascending) into patterns.
pub fn detect(
    source: &dyn LogSource,
    lines: Option<&[LineNo]>,
    options: &PatternOptions,
    progress: &Progress,
) -> Result<Patterns> {
    group(&[Set { source, lines }], options, progress)
}

/// Group sets `a` and `b` together, so a pattern is the same on both sides,
/// counting each side's lines ([`Pattern::sides`], [`SlotValue::sides`]);
/// [`differences`] ranks what changed.
pub fn compare(a: Set, b: Set, options: &PatternOptions, progress: &Progress) -> Result<Patterns> {
    group(&[a, b], options, progress)
}

/// What a set's lines are read as: the field grouped, and the context
/// fields' ids (by name, so two sources of different schemas line up).
struct Reading {
    field: Option<FieldId>,
    context: Vec<Option<FieldId>>,
}

fn group(sets: &[Set], options: &PatternOptions, progress: &Progress) -> Result<Patterns> {
    debug_assert!((1..=2).contains(&sets.len()));
    let config = &options.config;
    let field_of = |source: &dyn LogSource| -> Result<Option<FieldId>> {
        let schema = source.schema();
        Ok(match &options.field {
            Some(f) if f.eq_ignore_ascii_case("raw") => None,
            Some(f) => Some(
                schema
                    .iter()
                    .position(|d| d.name.eq_ignore_ascii_case(f))
                    .ok_or_else(|| Error::Format(format!("no field {f} in {}", source.name())))?,
            ),
            None => schema.iter().position(|d| d.name == "payload" && d.kind == FieldKind::Str),
        })
    };
    // Context: the cheap text fields of the first set, but the file: files
    // opened together are one set (their lifecycles compared across files),
    // and a configuration's rules outlive the file's name.
    let schema = sets[0].source.schema();
    let field = field_of(sets[0].source)?;
    let context: Vec<String> = (0..schema.len())
        .filter(|&i| {
            Some(i) != field
                && !schema[i].costly
                && matches!(schema[i].kind, FieldKind::Str | FieldKind::Name)
                && schema[i].name != crate::input::concat::FILE_FIELD
        })
        .map(|i| schema[i].name.to_string())
        .collect();
    let readings: Vec<Reading> = sets
        .iter()
        .map(|set| {
            let schema = set.source.schema();
            Ok(Reading {
                field: field_of(set.source)?,
                context: context.iter().map(|n| schema.iter().position(|d| d.name == n.as_str())).collect(),
            })
        })
        .collect::<Result<_>>()?;
    let mut lines = [0u64; 2];
    for (side, set) in sets.iter().enumerate() {
        lines[side] = set.lines.map_or(set.source.line_count(), |l| l.len() as u64);
    }
    progress.set_total(lines[0] + lines[1]);
    // The tasks: chunks of each set's lines, set A's first.
    let tasks: Vec<(usize, usize, usize)> = (0..sets.len())
        .flat_map(|side| {
            let n = lines[side] as usize;
            (0..n.div_ceil(CHUNK)).map(move |c| (side, c * CHUNK, ((c + 1) * CHUNK).min(n)))
        })
        .collect();
    let started = Instant::now();
    // Stage 1: per chunk its templates, the lines `ignore` rules left out,
    // and with `options.lines` each line's template (an index into them;
    // `u32::MAX`: left out).
    let parts: Vec<(Drain, Vec<u32>, u64, Rest)> = tasks
        .clone()
        .into_par_iter()
        .map(|(side, from, to)| {
            if progress.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let Set { source, lines } = sets[side];
            let reading = &readings[side];
            let mut locals = Locals::new(config, &context);
            let mut d = Drain::new(context.len(), MAX_PER_CHUNK);
            let mut of_line = Vec::new();
            if options.lines {
                of_line.resize(to - from, u32::MAX);
            }
            let mut vals = Vec::with_capacity(context.len());
            let mut ignored = 0u64;
            // Each context's first lines wait until its header and footer are
            // known (`Sample`).
            let mut samples: HashMap<u64, Sample> = HashMap::new();
            for i in from..to {
                let line = lines.map_or(i as LineNo, |l| l[i]);
                let text = text_of(source, reading, line);
                read_context(source, reading, line, &mut vals);
                let ctx = context_id(&vals);
                let local = locals.get(ctx, || vals.iter().map(|v| v.as_deref()).collect());
                if local.ignores(&text) {
                    ignored += 1;
                    continue;
                }
                let words = text.split_ascii_whitespace().take(MAX_TOKENS + 1);
                let mut toks: Vec<Cow<str>> = if local.plain_mask() {
                    words.map(mask).collect()
                } else {
                    words.map(|t| mask_with(t, local)).collect()
                };
                if toks.len() > MAX_TOKENS {
                    toks.truncate(MAX_TOKENS - 1);
                    toks.push(Cow::Borrowed(REST_STR));
                }
                if !d.boiler.contains_key(&ctx) {
                    let s = samples.entry(ctx).or_default();
                    s.count(&toks, local.s.header_places);
                    s.held.push((
                        i,
                        line,
                        toks.iter().map(|t| t.to_string()).collect(),
                        vals.iter().map(|v| v.as_deref().map(<[u8]>::to_vec)).collect(),
                    ));
                    if s.n >= local.s.header_sample {
                        let s = samples.remove(&ctx).expect("sampled");
                        flush(&mut d, ctx, &s, local, side, from, &mut of_line);
                    }
                    continue;
                }
                let t = d.add_line(&toks, side, line, &vals, ctx, local);
                if options.lines {
                    of_line[i - from] = t as u32;
                }
            }
            // Contexts with fewer lines here, in the order they began: pooled
            // with the other chunks' (`pool`).
            let mut rest: Vec<(u64, Sample)> = samples.into_iter().collect();
            rest.sort_by_key(|(_, s)| s.held[0].0);
            progress.add((to - from) as u64);
            Ok((d, of_line, ignored, rest))
        })
        .collect::<Result<_>>()?;
    let (mut parts, rests): (Vec<Chunk>, Vec<Rest>) = parts.into_iter().map(|(d, o, n, r)| ((d, o, n), r)).unzip();
    let pooled = pool(&mut parts, rests, &tasks, config, &context, context.len());
    // A template's id: its index in its chunk plus the templates of the
    // chunks before.
    let mut offsets = Vec::with_capacity(parts.len());
    let mut of_lines = Vec::with_capacity(parts.len());
    let mut all = Drain::new(context.len(), MAX_TEMPLATES);
    let mut next = 0u32;
    let mut ignored = [0u64; 2];
    for ((d, _, n), (side, _, _)) in parts.iter().zip(&tasks) {
        ignored[*side] += n;
        for (ctx, words) in &d.boiler {
            let all = all.boiler.entry(*ctx).or_default();
            for w in words {
                if !all.contains(w) {
                    all.push(w.clone());
                }
            }
        }
        for c in &d.clusters {
            all.prenote(c, d.groups.get(&c.key).map_or(1, |g| g.clusters.len()));
        }
    }
    if let Some((d, _)) = &pooled {
        for c in &d.clusters {
            all.prenote(c, d.groups.get(&c.key).map_or(1, |g| g.clusters.len()));
        }
    }
    let mut locals = Locals::new(config, &context);
    for (d, of_line, _) in parts {
        offsets.push(next);
        of_lines.push(of_line);
        let n = d.clusters.len() as u32;
        for (k, mut c) in d.clusters.into_iter().enumerate() {
            c.ids = vec![next + k as u32];
            let local = locals.get(c.ctx, || c.vals.iter().map(Val::bytes).collect());
            all.add_cluster(c, local);
        }
        next += n;
        if progress.is_cancelled() {
            return Err(Error::Cancelled);
        }
    }
    // The pooled lines' templates, after the chunks'.
    let mut pooled_lines = Vec::new();
    if let Some((d, lines)) = pooled {
        let n = d.clusters.len() as u32;
        for (k, mut c) in d.clusters.into_iter().enumerate() {
            c.ids = vec![next + k as u32];
            let local = locals.get(c.ctx, || c.vals.iter().map(Val::bytes).collect());
            all.add_cluster(c, local);
        }
        pooled_lines = lines.into_iter().map(|(task, i, t)| (task, i, next + t as u32)).collect();
        next += n;
    }
    // The words stage 1 saw different messages share, per context.
    let mut shared: HashMap<Vec<Val>, Boiler> = HashMap::new();
    for g in all.groups.values() {
        let (Some(toks), Some(&first)) = (g.boilerplate(), g.clusters.first()) else { continue };
        let words = shared.entry(all.clusters[first].vals.clone()).or_default();
        for t in toks.iter().flatten() {
            for (_, a) in atomize(t) {
                if is_word(&a) {
                    words.insert(a.into());
                }
            }
        }
    }
    let mut families: Vec<Family> = all.clusters.into_iter().map(Family::from).collect();
    let templates = families.len();
    let stage1 = started.elapsed();
    let started = Instant::now();
    // Each context's settings and rules; its `header` words count as
    // shared.
    let mut by_vals: HashMap<Vec<Val>, Local> = HashMap::new();
    for f in &families {
        if by_vals.contains_key(&f.vals) {
            continue;
        }
        let vals: Vec<Option<&[u8]>> = f.vals.iter().map(Val::bytes).collect();
        let local = config.local(&context, &vals);
        if !local.header.is_empty() {
            let words = shared.entry(f.vals.clone()).or_default();
            for (_, a) in local.header.iter().flat_map(|h| atomize(h)) {
                if is_word(&a) {
                    words.insert(a.into());
                }
            }
        }
        by_vals.insert(f.vals.clone(), local);
    }
    let (weights, ctxs) = weigh(&mut families, &shared);
    let locals: Vec<Local> = ctxs.iter().map(|v| by_vals.remove(v).unwrap_or_default()).collect();
    // Stage 2.
    let refused = Refused::default();
    if locals.iter().any(|l| l.s.merge) {
        families = merge_all(families, 1, &weights, &locals, &refused, progress)?;
        // A merge adds `new-slots` slots at most (`accept`), so a pattern
        // kept apart from one may join it once another has given it the
        // slot: again for those, while that joins any.
        for pass in 2..=MAX_PASSES {
            let n = families.len();
            if !families.iter().any(|f| f.apart) {
                break;
            }
            families = merge_all(families, pass, &weights, &locals, &refused, progress)?;
            if families.len() == n {
                break;
            }
        }
        // A pattern whose lines another's query takes is that one.
        families = absorb(families, &locals, sets, &readings, progress)?;
    }
    // A field goes into the queries where patterns differ in it.
    let telling: Vec<bool> = (0..context.len())
        .map(|f| {
            let mut seen: Option<&Val> = None;
            families.iter().any(|c| match (&c.vals[f], seen) {
                (Val::Mixed, _) => true,
                (v, None) => {
                    seen = Some(v);
                    false
                }
                (v, Some(s)) => v != s,
            })
        })
        .collect();
    let mut patterns: Vec<Pattern> = families
        .into_iter()
        .map(|f| {
            let local = &locals[f.ctx];
            let mut parts = f.parts;
            cap_values(&mut parts, local.s.state_values);
            Pattern {
                fields: context
                    .iter()
                    .zip(&f.vals)
                    .zip(&telling)
                    .filter_map(|((name, v), &t)| match v {
                        Val::One(Some(b)) if t => Some((name.clone(), String::from_utf8_lossy(b).into_owned())),
                        _ => None,
                    })
                    .collect(),
                parts: factor(parts),
                sides: f.sides,
                unsorted: f.unsorted,
                lines: Default::default(),
                ids: f.ids,
                loose: !local.values.is_empty(),
            }
        })
        .collect();
    patterns.sort_by(|a, b| by_count(&a.sides, &b.sides));
    if options.lines {
        // Template id → its pattern; each line to its template's.
        let mut pattern_of = vec![0u32; next as usize];
        for (k, p) in patterns.iter().enumerate() {
            for &id in &p.ids {
                pattern_of[id as usize] = k as u32;
            }
        }
        for (((side, from, _), offset), of_line) in tasks.iter().zip(&offsets).zip(&of_lines) {
            let set_lines = sets[*side].lines;
            for (i, &t) in of_line.iter().enumerate() {
                if t == u32::MAX {
                    continue;
                }
                let line = set_lines.map_or((from + i) as LineNo, |l| l[from + i]);
                patterns[pattern_of[(offset + t) as usize] as usize].lines[*side].push(line);
            }
        }
        if !pooled_lines.is_empty() {
            for &(task, i, id) in &pooled_lines {
                let (side, from, _) = tasks[task];
                let line = sets[side].lines.map_or((from + i) as LineNo, |l| l[from + i]);
                patterns[pattern_of[id as usize] as usize].lines[side].push(line);
            }
            for p in &mut patterns {
                p.lines.iter_mut().for_each(|l| l.sort_unstable());
            }
        }
    }
    let stats = PatternStats {
        templates,
        stage1,
        stage2: started.elapsed(),
        apart: refused.apart.load(Ordering::Relaxed),
        side: refused.side.load(Ordering::Relaxed),
        rules: refused.rules.load(Ordering::Relaxed),
        ignored,
    };
    let lines = [lines[0] - ignored[0], lines[1] - ignored[1]];
    Ok(Patterns { field: field.map(|id| schema[id].name.to_string()), patterns, lines, stats })
}

/// Each context's settings and rules ([`PatternConfig::local`]), resolved
/// once.
struct Locals<'c> {
    config: &'c PatternConfig,
    names: &'c [String],
    /// Without rules: the one for all.
    all: Option<Local>,
    by_ctx: HashMap<u64, Local>,
}

impl<'c> Locals<'c> {
    fn new(config: &'c PatternConfig, names: &'c [String]) -> Self {
        let all = config.rules.is_empty().then(|| config.local(names, &[]));
        Locals { config, names, all, by_ctx: HashMap::new() }
    }

    /// Context `ctx`'s, its fields' values `vals` (asked for once).
    fn get<'v>(&mut self, ctx: u64, vals: impl FnOnce() -> Vec<Option<&'v [u8]>>) -> &Local {
        let Locals { config, names, all, by_ctx } = self;
        match all {
            Some(l) => l,
            None => by_ctx.entry(ctx).or_insert_with(|| config.local(names, &vals())),
        }
    }
}

/// A context's held lines into `d`, now its header and footer are known
/// (`local`'s `header` words among them); each line's template into
/// `of_line` (if kept: [`PatternOptions::lines`]).
fn flush(d: &mut Drain, ctx: u64, s: &Sample, local: &Local, side: usize, from: usize, of_line: &mut [u32]) {
    let ts = flush_with(d, ctx, s, local, &[], &|_| side);
    for ((i, ..), t) in s.held.iter().zip(ts) {
        if let Some(o) = of_line.get_mut(i - from) {
            *o = t as u32;
        }
    }
}

/// [`flush`]'s work: the header and footer words (`known`: more, from
/// elsewhere) and the held lines into `d`, line `k` of set `side_of(k)`;
/// returns each held line's template.
fn flush_with(
    d: &mut Drain,
    ctx: u64,
    s: &Sample,
    local: &Local,
    known: &[Box<str>],
    side_of: &dyn Fn(usize) -> usize,
) -> Vec<usize> {
    let mut words = s.boilerplate(local);
    words.extend(local.header.iter().cloned());
    words.extend(known.iter().cloned());
    d.boiler.insert(ctx, words.into_iter().collect());
    let mut out = Vec::with_capacity(s.held.len());
    for (k, (_, line, toks, vals)) in s.held.iter().enumerate() {
        let toks: Vec<Cow<str>> = toks.iter().map(|t| Cow::Borrowed(t.as_str())).collect();
        let vals: Vec<Option<Cow<[u8]>>> = vals.iter().map(|v| v.as_deref().map(Cow::Borrowed)).collect();
        out.push(d.add_line(&toks, side_of(k), *line, &vals, ctx, local));
    }
    out
}

/// A chunk's stage 1: its templates, each line's template (`u32::MAX`:
/// left out; none without [`PatternOptions::lines`]), the lines ignored.
type Chunk = (Drain, Vec<u32>, u64);
/// A chunk's contexts it saw too few lines of ([`pool`]).
type Rest = Vec<(u64, Sample)>;
/// The pooled lines' drain, and each line's (task, index in its chunk,
/// template).
type Pooled = (Drain, Vec<(usize, usize, usize)>);

/// Lines held of a context, at most, pooled across chunks ([`pool`]); past
/// that, each chunk's rare contexts go as they would alone.
const POOL: usize = 256 * 1024;

/// The contexts each chunk saw too few lines of to tell their header and
/// footer (`rests`, per task), pooled: their counts summed, so a context
/// with a few lines in every chunk is sampled on them all, then their
/// lines grouped in a drain of their own (with what other chunks knew of
/// the context's header). Returns that drain and each line's (task, index
/// in its chunk, template); none if there were no such lines. Past
/// [`POOL`] lines, each chunk's go into its own drain instead (rare
/// contexts everywhere: holding them all would cost too much memory).
fn pool(
    parts: &mut [Chunk],
    rests: Vec<Rest>,
    tasks: &[(usize, usize, usize)],
    config: &PatternConfig,
    context: &[String],
    fields: usize,
) -> Option<Pooled> {
    let held: usize = rests.iter().flatten().map(|(_, s)| s.held.len()).sum();
    if held == 0 {
        return None;
    }
    let mut locals = Locals::new(config, context);
    if held > POOL {
        for ((d, of_line, _), (rest, &(side, from, _))) in parts.iter_mut().zip(rests.into_iter().zip(tasks)) {
            for (ctx, s) in rest {
                let vals = &s.held[0].3;
                let local = locals.get(ctx, || vals.iter().map(Option::as_deref).collect());
                flush(d, ctx, &s, local, side, from, of_line);
            }
        }
        return None;
    }
    // What the chunks knew of each context's header.
    let mut known: HashMap<u64, Vec<Box<str>>> = HashMap::new();
    for (d, _, _) in parts.iter() {
        for (ctx, words) in &d.boiler {
            let k = known.entry(*ctx).or_default();
            k.extend(words.iter().filter(|w| !k.contains(w)).cloned().collect::<Vec<_>>());
        }
    }
    // Per context, in the order first seen: its pooled sample, and each
    // held line's task.
    let mut order: Vec<u64> = Vec::new();
    let mut by_ctx: HashMap<u64, (Sample, Vec<usize>)> = HashMap::new();
    for (task, rest) in rests.into_iter().enumerate() {
        for (ctx, s) in rest {
            let (pooled, from) = by_ctx.entry(ctx).or_insert_with(|| {
                order.push(ctx);
                Default::default()
            });
            from.extend(std::iter::repeat_n(task, s.held.len()));
            pooled.absorb(s);
        }
    }
    let mut d = Drain::new(fields, MAX_TEMPLATES);
    let mut lines = Vec::with_capacity(held);
    for ctx in order {
        let (s, from) = by_ctx.remove(&ctx).expect("seen");
        let vals = &s.held[0].3;
        let local = locals.get(ctx, || vals.iter().map(Option::as_deref).collect());
        let empty = Vec::new();
        let ts = flush_with(&mut d, ctx, &s, local, known.get(&ctx).unwrap_or(&empty), &|k| tasks[from[k]].0);
        for (((i, ..), task), t) in s.held.iter().zip(&from).zip(ts) {
            lines.push((*task, i - tasks[*task].1, t));
        }
    }
    Some((d, lines))
}

/// Slots of more than `n` values (`state-values`): `<*>`, not a state;
/// those on the most lines kept.
fn cap_values(parts: &mut [Part], n: usize) {
    for p in parts {
        if let Atom::Slot(s) = &mut p.atom
            && s.values.len() > n
        {
            s.values.sort_by(|a, b| by_count(&a.sides, &b.sides));
            s.values.truncate(n);
            s.overflow = true;
        }
    }
}

/// The text grouped of `line`.
fn text_of<'s>(source: &'s dyn LogSource, reading: &Reading, line: LineNo) -> Cow<'s, str> {
    match reading.field {
        Some(id) => match source.field(line, id) {
            Some(FieldValue::Str(b)) => match b {
                Cow::Borrowed(b) => String::from_utf8_lossy(b),
                Cow::Owned(b) => Cow::Owned(String::from_utf8_lossy(&b).into_owned()),
            },
            _ => Cow::Borrowed(""),
        },
        None => source.line_text(line).unwrap_or_default(),
    }
}

/// The context fields' values of `line` into `vals`.
fn read_context<'s>(source: &'s dyn LogSource, reading: &Reading, line: LineNo, vals: &mut Vec<Option<Cow<'s, [u8]>>>) {
    vals.clear();
    vals.extend(reading.context.iter().map(|id| match id.and_then(|id| source.field(line, id)) {
        Some(FieldValue::Str(b)) => Some(b),
        _ => None,
    }));
}

/// A context's id: its fields' values hashed.
fn context_id(vals: &[Option<Cow<[u8]>>]) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    vals.hash(&mut h);
    h.finish()
}

/// A context's first lines in a chunk, held back from stage 1 until its
/// header and footer are known ([`Sample::boilerplate`]).
#[derive(Default)]
struct Sample {
    /// Lines counted.
    n: u32,
    /// Per token (values aside), the lines with it.
    words: HashMap<Box<str>, u32>,
    /// Per token and place (from the start: 0, 1, 2; from the end: -1, -2,
    /// -3), the message length it was first seen in there, and whether in
    /// another.
    places: HashMap<(Box<str>, i8), (usize, bool)>,
    /// The lines held.
    held: Vec<Held>,
}

/// A line held back ([`Sample`]): its index in the chunk, line, tokens and
/// fields' values.
type Held = (usize, LineNo, Vec<String>, Vec<Option<Vec<u8>>>);

impl Sample {
    /// Add `other`'s counts and lines (another chunk's of the context).
    fn absorb(&mut self, other: Sample) {
        self.n += other.n;
        for (w, c) in other.words {
            *self.words.entry(w).or_default() += c;
        }
        for (k, (len, other_len)) in other.places {
            match self.places.get_mut(&k) {
                Some((first, seen)) => *seen |= other_len || *first != len,
                None => {
                    self.places.insert(k, (len, other_len));
                }
            }
        }
        self.held.extend(other.held);
    }

    /// Count a message; `places`: from the start and the end, where a
    /// header or footer may be (`header places`).
    fn count(&mut self, toks: &[Cow<str>], places: usize) {
        self.n += 1;
        let len = toks.len();
        for (k, t) in toks.iter().enumerate() {
            let at = if k < places {
                k as i8
            } else if len - k <= places {
                -((len - k) as i8)
            } else {
                continue;
            };
            if is_value(t) {
                continue;
            }
            match self.places.get_mut(&(t.as_ref().into(), at)) {
                Some((first, other)) => *other |= *first != len,
                None => {
                    self.places.insert((t.as_ref().into(), at), (len, false));
                }
            }
        }
        let mut seen: Vec<&str> = toks.iter().map(|t| &**t).filter(|t| !is_value(t)).collect();
        seen.sort_unstable();
        seen.dedup();
        for t in seen {
            match self.words.get_mut(t) {
                Some(c) => *c += 1,
                None => {
                    self.words.insert(t.into(), 1);
                }
            }
        }
    }

    /// The tokens more than half its lines carry (values aside; `header
    /// share`) at one place from the start or the end (the first or last
    /// three: `header places`) in
    /// messages of different lengths: a header or footer the messages share
    /// (`LOG:` first, `Elapsed[us]:` third from last), which stage 1 then
    /// doesn't take for a sign of one message ([`similarity`]). Known before
    /// the context's first comparison, so its first lines can't join a
    /// template on it alone. The words of one message, its lines all as
    /// long, are none (`in state {Idle|Active}`); nor are any with fewer
    /// than 8 lines to go by (`header min`).
    fn boilerplate(&self, local: &Local) -> Boiler {
        if self.n < local.s.header_min.max(1) {
            return Boiler::default();
        }
        let placed: std::collections::HashSet<&str> =
            self.places.iter().filter(|(_, (_, other))| *other).map(|((t, _), _)| &**t).collect();
        let least = local.s.header_share * self.n as f32;
        self.words.iter().filter(|&(w, &c)| c as f32 > least && placed.contains(&**w)).map(|(w, _)| w.clone()).collect()
    }
}

/// `tok` with each run of word characters holding a digit (and the `.`,
/// `:`, `-` joining such runs) replaced by [`MASK`]: `peer=10.0.3.7` →
/// `peer=<*>`, `2026-10-17` → `<*>`, `abc-12` → `abc-<*>`, `node3` → `<*>`.
fn mask(tok: &str) -> Cow<'_, str> {
    if !tok.bytes().any(|b| b.is_ascii_digit()) {
        return Cow::Borrowed(tok);
    }
    fn push_run(out: &mut String, run: &str) {
        if !run.bytes().any(|b| b.is_ascii_digit()) {
            out.push_str(run);
            return;
        }
        let mut cs = out.chars().rev();
        if let (Some(j @ ('.' | ':' | '-')), Some(MASK)) = (cs.next(), cs.next()) {
            out.truncate(out.len() - j.len_utf8());
        } else {
            out.push(MASK);
        }
    }
    let mut out = String::with_capacity(tok.len());
    let mut start = None;
    for (i, c) in tok.char_indices() {
        if is_word_char(c) {
            start.get_or_insert(i);
        } else {
            if let Some(s) = start.take() {
                push_run(&mut out, &tok[s..i]);
            }
            out.push(c);
        }
    }
    if let Some(s) = start {
        push_run(&mut out, &tok[s..]);
    }
    Cow::Owned(out)
}

/// `tok` masked as [`mask`] does, and as `local`'s rules say: text a
/// `literal` regex finds is kept, text a `value` regex finds masked (where
/// they overlap, the one found first).
fn mask_with<'t>(tok: &'t str, local: &Local) -> Cow<'t, str> {
    if local.plain_mask() {
        return mask(tok);
    }
    // (start, end, literal)
    let mut spans: Vec<(usize, usize, bool)> = Vec::new();
    for (res, lit) in [(&local.literals, true), (&local.values, false)] {
        for re in res {
            spans.extend(re.find_iter(tok).filter(|m| !m.is_empty()).map(|m| (m.start(), m.end(), lit)));
        }
    }
    if spans.is_empty() {
        return mask(tok);
    }
    spans.sort_by_key(|&(s, e, lit)| (s, !lit, std::cmp::Reverse(e)));
    let mut out = String::with_capacity(tok.len());
    let mut at = 0;
    for (s, e, lit) in spans {
        if s < at {
            continue;
        }
        out.push_str(&mask(&tok[at..s]));
        if lit {
            out.push_str(&tok[s..e]);
        } else if !out.ends_with(MASK) {
            out.push(MASK);
        }
        at = e;
    }
    out.push_str(&mask(&tok[at..]));
    Cow::Owned(out)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_word(atom: &str) -> bool {
    atom.chars().next().is_some_and(is_word_char)
}

/// Atoms of a text, each with what comes before it.
type Atoms = Vec<(Sep, String)>;

/// A text with markers split into atoms by character class, each with what
/// comes before it (the first: [`Sep::Tight`] unless the text starts with
/// white space).
fn atomize(text: &str) -> Atoms {
    let mut out: Atoms = Vec::new();
    let mut sep = Sep::Tight;
    let mut in_word = false;
    for c in text.chars() {
        if is_word_char(c) {
            if in_word {
                out.last_mut().expect("in a word").1.push(c);
            } else {
                out.push((sep, c.to_string()));
                sep = Sep::Tight;
                in_word = true;
            }
            continue;
        }
        in_word = false;
        if c == EITHER {
            sep = Sep::Either;
        } else if c.is_whitespace() {
            sep = Sep::Space;
        } else {
            out.push((sep, c.to_string()));
            sep = Sep::Tight;
        }
    }
    out
}

/// Parts as a text with markers (a slot: [`ANY`]); the first part's
/// separator left out.
fn render(parts: &[Part]) -> String {
    let mut s = String::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            match p.sep {
                Sep::Space => s.push(' '),
                Sep::Tight => {}
                Sep::Either => s.push(EITHER),
            }
        }
        match &p.atom {
            Atom::Lit(l) => s.push_str(l),
            Atom::Slot(_) => s.push(ANY),
        }
    }
    s
}

/// A field's value on every line of a pattern so far.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Val {
    One(Option<Box<[u8]>>),
    Mixed,
}

impl Val {
    /// The value, if one and some.
    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Val::One(Some(b)) => Some(b),
            _ => None,
        }
    }

    fn join(&mut self, other: &Val) {
        if self != other {
            *self = Val::Mixed;
        }
    }
}

// ---- Stage 1: Drain over the lines ----

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    /// Literal text, with [`MASK`] for masked parts.
    Lit(Box<str>),
    /// Any token (the template's messages differ there).
    Any,
}

struct Cluster {
    key: u64,
    toks: Vec<Tok>,
    /// Per token; empty but for [`Tok::Any`].
    slots: Vec<Slot>,
    sides: Sides,
    vals: Vec<Val>,
    /// A catch-all ([`Pattern::unsorted`]): every token [`Tok::Any`].
    catchall: bool,
    /// The templates of the chunks merged into it (their ids: [`group`]).
    ids: Vec<u32>,
    /// Its context ([`context_id`]).
    ctx: u64,
}

/// `n` into `c`: its lines, values and fields; the tokens they differ in
/// become slots.
fn merge_into(c: &mut Cluster, mut n: Cluster) {
    for i in 0..c.toks.len() {
        if c.toks[i] == n.toks[i] && c.toks[i] != Tok::Any {
            continue;
        }
        if let Tok::Lit(l) = &c.toks[i] {
            let mut s = Slot::default();
            s.add(l, &c.sides);
            c.slots[i] = s;
            c.toks[i] = Tok::Any;
        }
        match &n.toks[i] {
            Tok::Lit(l) => c.slots[i].add(l, &n.sides),
            Tok::Any => c.slots[i].join(std::mem::take(&mut n.slots[i])),
        }
    }
    add_sides(&mut c.sides, &n.sides);
    for (v, w) in c.vals.iter_mut().zip(&n.vals) {
        v.join(w);
    }
    c.ids.append(&mut n.ids);
}

/// Templates of some lines: clusters, found by the fields' values, token
/// count and first literal token (`key`); at most `cap`, and
/// [`MAX_GROUP`] per group, then catch-alls by token count.
struct Drain {
    groups: HashMap<u64, Group>,
    clusters: Vec<Cluster>,
    fields: usize,
    cap: usize,
    /// Token count → its catch-all in `clusters`.
    catchall: HashMap<usize, usize>,
    /// Per context: its header and footer tokens ([`Sample`]; a few: a
    /// list is quicker than a set); a context not in it yet is still being
    /// sampled.
    boiler: HashMap<u64, Vec<Box<str>>>,
}

/// The templates of one group (`key`).
#[derive(Default)]
struct Group {
    clusters: Vec<usize>,
    /// Per token: the one every line of the group has there (`None`: they
    /// differ, or there is none yet). Once the group holds two templates,
    /// such a token is a header or footer the messages share (`LOG:`,
    /// `Elapsed[us]:`), no sign of the same message: [`similarity`] passes
    /// it over.
    shared: Vec<Option<Box<str>>>,
    /// Messages or templates noted.
    seen: usize,
    /// The most templates the group held apart in one chunk (noted before
    /// the chunks' are merged: [`Drain::prenote`]). The same template from
    /// two chunks is one message, not two.
    templates: usize,
}

impl Group {
    /// Note a message (or template: `None` where it has a wildcard).
    fn note<'a>(&mut self, toks: impl ExactSizeIterator<Item = Option<&'a str>>) {
        if self.seen == 0 {
            self.shared = toks.map(|t| t.map(Into::into)).collect();
        } else {
            for (s, t) in self.shared.iter_mut().zip(toks) {
                if s.as_deref() != t {
                    *s = None;
                }
            }
        }
        self.seen += 1;
    }

    /// The tokens to pass over: once it holds two templates.
    fn boilerplate(&self) -> Option<&[Option<Box<str>>]> {
        (self.clusters.len().max(self.templates) >= 2).then_some(&self.shared[..])
    }
}

/// The group of a message of tokens `toks` in context `ctx`
/// ([`context_id`]).
fn key<'a>(toks: impl ExactSizeIterator<Item = &'a str>, ctx: u64) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    ctx.hash(&mut h);
    toks.len().hash(&mut h);
    toks.into_iter().find(|&t| t != MASK_STR).hash(&mut h);
    h.finish()
}

/// How alike template `a` and the message or template `b` (`None`: any)
/// are: the share of equal tokens where neither is a value (1 if there are
/// none). A wildcard is a difference, as in Drain, also on both sides (two
/// templates join no more readily than their lines would): else a template
/// with a few would take any message of its length that shares the rest,
/// which may be only a header and footer (`LOG: <*> <*> <*> Elapsed[us]:
/// <*>; #<*>`). Stage 2 joins what this keeps apart where it should.
///
/// Two words differing at once make it 0, as [`accept`] keeps two new
/// slots apart: `Received disconnect: Bye Bye [preauth]` and `…:
/// disconnected by user` are two messages, not `{Bye|disconnected}
/// {Bye|by} {[preauth]|user}` (LogHub 2k: grouping accuracy 0.715 →
/// 0.738, `examples/loghub_bench.rs`).
///
/// `shared`: the group's header and footer ([`Group::shared`]), and
/// `boiler`: whether each token of `b` is the context's ([`Sample`]),
/// passed over where both have them.
///
/// `local`: the context's settings and rules: `new-wildcards` (not 1) words
/// may differ at once; a token holding a `keep` or `apart` word never
/// becomes or joins a wildcard (stage 2 joins what `apart` allows); a
/// template has `max-slots` wildcards at most. Where a rule refuses, 0.
fn similarity<'a>(
    a: &[Tok],
    b: impl Iterator<Item = Option<&'a str>>,
    shared: Option<&[Option<Box<str>>]>,
    boiler: &[bool],
    local: &Local,
) -> f32 {
    // `new`: words that would become wildcards (neither side is one yet).
    let (mut same, mut n, mut new) = (0u32, 0u32, 0u32);
    // With rules: the wildcards it would have.
    let strict = !local.guard.is_empty() || local.max_slots.is_some();
    let mut anys = 0usize;
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if strict {
            let differs = match (x, y) {
                (Tok::Any, _) => true,
                (Tok::Lit(x), Some(y)) => **x != *y,
                (Tok::Lit(_), None) => true,
            };
            if differs {
                anys += 1;
                let lit = match x {
                    Tok::Lit(x) => local.guards(x),
                    Tok::Any => false,
                };
                if lit || y.is_some_and(|y| local.guards(y)) {
                    return 0.0;
                }
            }
        }
        if let (Some(s), Some(y)) = (shared.and_then(|s| s.get(i)).and_then(Option::as_deref), y)
            && s == y
        {
            continue;
        }
        if let (Tok::Lit(x), Some(y)) = (x, y)
            && boiler.get(i) == Some(&true)
            && **x == *y
        {
            continue;
        }
        match (x, y) {
            (Tok::Lit(x), _) if is_value(x) => {}
            (_, Some(y)) if is_value(y) => {}
            (Tok::Lit(x), Some(y)) => {
                n += 1;
                if **x == *y {
                    same += 1;
                } else {
                    new += 1;
                }
            }
            _ => n += 1,
        }
    }
    if new > local.s.new_wildcards || local.max_slots.is_some_and(|m| anys > m) {
        return 0.0;
    }
    if n == 0 { 1.0 } else { same as f32 / n as f32 }
}

/// A token of masked values and ASCII punctuation only (`<*>`, `<*>;`,
/// `#<*>`). On bytes: it runs for each token of each comparison.
fn is_value(t: &str) -> bool {
    let (b, m) = (t.as_bytes(), MASK_STR.as_bytes());
    let (mut i, mut masked) = (0, false);
    while i < b.len() {
        if b[i..].starts_with(m) {
            masked = true;
            i += m.len();
        } else if b[i].is_ascii_punctuation() {
            i += 1;
        } else {
            return false;
        }
    }
    masked
}

impl Drain {
    fn new(fields: usize, cap: usize) -> Self {
        Self {
            groups: HashMap::new(),
            clusters: Vec::new(),
            fields,
            cap,
            catchall: HashMap::new(),
            boiler: HashMap::new(),
        }
    }

    /// No room for another template in group `key`.
    fn full(&self, key: u64) -> bool {
        self.clusters.len() - self.catchall.len() >= self.cap
            || self.groups.get(&key).is_some_and(|g| g.clusters.len() >= MAX_GROUP)
    }

    /// `n` into the catch-all of its token count.
    fn catch_all(&mut self, mut n: Cluster) {
        match self.catchall.get(&n.toks.len()) {
            Some(&ci) => merge_into(&mut self.clusters[ci], n),
            None => {
                for i in 0..n.toks.len() {
                    if let Tok::Lit(l) = &n.toks[i] {
                        let mut s = Slot::default();
                        s.add(l, &n.sides);
                        n.slots[i] = s;
                        n.toks[i] = Tok::Any;
                    }
                }
                n.catchall = true;
                self.catchall.insert(n.toks.len(), self.clusters.len());
                self.clusters.push(n);
            }
        }
    }

    /// The most alike template in group `key` with `len` tokens (`score`:
    /// how alike), if `sim` alike at least.
    fn best(
        &self,
        key: u64,
        len: usize,
        sim: f32,
        score: impl Fn(&[Tok], Option<&[Option<Box<str>>]>) -> f32,
    ) -> Option<usize> {
        let mut best = None;
        let g = self.groups.get(&key)?;
        let shared = g.boilerplate().filter(|s| s.len() == len);
        for &ci in &g.clusters {
            let c = &self.clusters[ci];
            if c.toks.len() != len {
                continue;
            }
            let s = score(&c.toks, shared);
            if s >= sim && best.is_none_or(|(_, bs)| s > bs) {
                best = Some((ci, s));
            }
        }
        best.map(|(ci, _)| ci)
    }

    /// Add a message of context `ctx` ([`context_id`] of `vals`, its
    /// settings and rules `local`); returns the template (its index) it went
    /// to.
    fn add_line(
        &mut self,
        toks: &[Cow<str>],
        side: usize,
        line: LineNo,
        vals: &[Option<Cow<[u8]>>],
        ctx: u64,
        local: &Local,
    ) -> usize {
        let here = one(side, line);
        debug_assert_eq!(vals.len(), self.fields);
        let k = key(toks.iter().map(|t| &**t), ctx);
        let bp: Vec<bool> = match self.boiler.get(&ctx) {
            Some(b) if !b.is_empty() => toks.iter().map(|t| b.iter().any(|w| **w == **t)).collect(),
            _ => Vec::new(),
        };
        let found = self.best(k, toks.len(), local.s.similarity, |c, s| {
            similarity(c, toks.iter().map(|t| Some(&**t)), s, &bp, local)
        });
        if let Some(g) = self.groups.get_mut(&k) {
            g.note(toks.iter().map(|t| Some(&**t)));
        }
        match found {
            Some(ci) => {
                let c = &mut self.clusters[ci];
                for (i, y) in toks.iter().enumerate() {
                    match &c.toks[i] {
                        Tok::Lit(l) if **l == **y => {}
                        Tok::Lit(l) => {
                            // The lines so far all had `l` there.
                            let mut s = Slot::default();
                            s.add(l, &c.sides);
                            s.add(y, &here);
                            c.slots[i] = s;
                            c.toks[i] = Tok::Any;
                        }
                        Tok::Any => c.slots[i].add(y, &here),
                    }
                }
                add_sides(&mut c.sides, &here);
                for (v, n) in c.vals.iter_mut().zip(vals) {
                    if !matches!(v, Val::One(o) if o.as_deref() == n.as_deref()) {
                        *v = Val::Mixed;
                    }
                }
                ci
            }
            None => {
                let n = Cluster {
                    key: k,
                    toks: toks.iter().map(|t| Tok::Lit(t.as_ref().into())).collect(),
                    slots: vec![Slot::default(); toks.len()],
                    sides: here,
                    vals: vals.iter().map(|v| Val::One(v.as_deref().map(Into::into))).collect(),
                    catchall: false,
                    ids: Vec::new(),
                    ctx,
                };
                if self.full(k) {
                    self.catch_all(n);
                    self.catchall[&toks.len()]
                } else {
                    let g = self.groups.entry(k).or_default();
                    if g.seen == 0 {
                        g.note(toks.iter().map(|t| Some(&**t)));
                    }
                    g.clusters.push(self.clusters.len());
                    self.clusters.push(n);
                    self.clusters.len() - 1
                }
            }
        }
    }

    /// Merge a template found among other lines (its context's settings
    /// and rules: `local`).
    fn add_cluster(&mut self, n: Cluster, local: &Local) {
        fn lit(t: &Tok) -> Option<&str> {
            match t {
                Tok::Lit(l) => Some(&**l),
                Tok::Any => None,
            }
        }
        if n.catchall {
            return self.catch_all(n);
        }
        let bp: Vec<bool> = match self.boiler.get(&n.ctx) {
            Some(b) if !b.is_empty() => {
                n.toks.iter().map(|t| lit(t).is_some_and(|t| b.iter().any(|w| **w == *t))).collect()
            }
            _ => Vec::new(),
        };
        match self
            .best(n.key, n.toks.len(), local.s.similarity, |c, s| similarity(c, n.toks.iter().map(lit), s, &bp, local))
        {
            Some(ci) => merge_into(&mut self.clusters[ci], n),
            None if self.full(n.key) => self.catch_all(n),
            None => {
                self.groups.entry(n.key).or_default().clusters.push(self.clusters.len());
                self.clusters.push(n);
            }
        }
    }

    /// Note a template before [`Drain::add_cluster`] merges them all, so
    /// each group knows its header and footer from the start; `apart`: the
    /// templates its chunk's group held.
    fn prenote(&mut self, n: &Cluster, apart: usize) {
        if n.catchall {
            return;
        }
        let g = self.groups.entry(n.key).or_default();
        g.note(n.toks.iter().map(|t| match t {
            Tok::Lit(l) => Some(&**l),
            Tok::Any => None,
        }));
        g.templates = g.templates.max(apart);
    }
}

// ---- Stage 2: templates merged by alignment ----

/// A template in atoms, and its lines.
struct Family {
    parts: Vec<Part>,
    /// The words of its wordiest template, by weight ([`weigh`]): merging
    /// keeps only the words in common, but how alike another is counts
    /// against these (else each merge would make the next easier).
    size: f32,
    sides: Sides,
    vals: Vec<Val>,
    unsorted: bool,
    /// Kept apart from a pattern by [`Verdict::Apart`] in the last pass.
    apart: bool,
    /// The pass of stage 2 that last merged another into it (0: none).
    grew: usize,
    /// The stage-1 templates it holds.
    ids: Vec<u32>,
    /// Its context (the fields' values): an index into [`weigh`]'s.
    ctx: usize,
}

/// Words most lines of a context carry.
type Boiler = std::collections::HashSet<Box<str>>;

/// A context's word weights ([`weigh`]): words not in it weigh 1.
type Weights = HashMap<Box<str>, f32>;

/// Per context (the fields' values, `apid`, `ctid`: all one in a text
/// file), how much each word tells its messages apart: 1 − the share of the
/// context's lines that carry it, for the words stage 1 saw different
/// messages share in one place (`shared`: [`Group::shared`]); others weigh
/// one. A header or footer on every line (`LOG`, `Elapsed`, `us`) weighs
/// about 0, so [`accept`] doesn't take it for a sign of one message. A word
/// every line has but no two messages were seen to share is the message
/// itself (`gear=D`, `gear=P`) and keeps its weight; so do all words of a
/// context with one template. Sets [`Family::ctx`] and [`Family::size`];
/// returns each context's weights and fields' values, by [`Family::ctx`].
fn weigh(families: &mut [Family], shared: &HashMap<Vec<Val>, Boiler>) -> (Vec<Weights>, Vec<Vec<Val>>) {
    let none = Boiler::default();
    let mut ctxs: HashMap<Vec<Val>, usize> = HashMap::new();
    // Per context: lines, templates, lines per word.
    let mut stats: Vec<(u64, usize, HashMap<Box<str>, u64>)> = Vec::new();
    for f in families.iter_mut() {
        let next = ctxs.len();
        f.ctx = *ctxs.entry(f.vals.clone()).or_insert(next);
        if f.ctx == stats.len() {
            stats.push(Default::default());
        }
        let (lines, n, words) = &mut stats[f.ctx];
        let count = total(&f.sides);
        *lines += count;
        *n += 1;
        let mut mine: Vec<&str> = f.words().collect();
        mine.sort_unstable();
        mine.dedup();
        for w in mine {
            *words.entry(w.into()).or_default() += count;
        }
    }
    let mut by_ctx: Vec<Option<&Vec<Val>>> = vec![None; ctxs.len()];
    for (vals, &k) in &ctxs {
        by_ctx[k] = Some(vals);
    }
    let weights: Vec<Weights> = stats
        .into_iter()
        .zip(by_ctx)
        .map(|((lines, n, words), vals)| {
            let seen = vals.and_then(|v| shared.get(v)).unwrap_or(&none);
            if n < 2 || seen.is_empty() || lines == 0 {
                return Weights::default();
            }
            words
                .into_iter()
                .filter(|(w, _)| seen.contains(w))
                .map(|(w, c)| (w, 1.0 - c as f32 / lines as f32))
                .collect()
        })
        .collect();
    for f in families.iter_mut() {
        let w = &weights[f.ctx];
        f.size = f.words().map(|x| w.get(x).copied().unwrap_or(1.0)).sum();
    }
    let mut vals: Vec<Vec<Val>> = vec![Vec::new(); ctxs.len()];
    for (v, k) in ctxs {
        vals[k] = v;
    }
    (weights, vals)
}

impl From<Cluster> for Family {
    fn from(c: Cluster) -> Self {
        let mut parts = Vec::new();
        for (t, slot) in c.toks.into_iter().zip(c.slots) {
            match t {
                Tok::Any => parts.push(Part { sep: Sep::Space, atom: Atom::Slot(slot) }),
                Tok::Lit(l) => {
                    for (k, (sep, a)) in atomize(&l).into_iter().enumerate() {
                        let sep = if k == 0 { Sep::Space } else { sep };
                        parts.push(Part { sep, atom: Atom::Lit(a.into()) });
                    }
                }
            }
        }
        let size = parts.iter().filter(|p| matches!(&p.atom, Atom::Lit(l) if is_word(l))).count() as f32;
        Family {
            parts,
            size,
            sides: c.sides,
            vals: c.vals,
            unsorted: c.catchall,
            apart: false,
            grew: 0,
            ids: c.ids,
            ctx: 0,
        }
    }
}

impl Family {
    fn words(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|p| match &p.atom {
            Atom::Lit(l) if is_word(l) => Some(&**l),
            _ => None,
        })
    }

    /// Its masked values match any text (`value` rules in its context).
    fn loose(&self, locals: &[Local]) -> bool {
        !locals[self.ctx].values.is_empty()
    }

    /// No field tells the two apart (both have one value, and not the same).
    fn compatible(&self, other: &Family) -> bool {
        self.vals.iter().zip(&other.vals).all(|(a, b)| match (a, b) {
            (Val::One(Some(x)), Val::One(Some(y))) => x == y,
            _ => true,
        })
    }
}

/// How much atoms `a` and `b` have in common when aligned: a word 2, another
/// character or masked value 1, two slots 1, else nothing.
fn weight(a: &Part, b: &Part) -> u32 {
    match (&a.atom, &b.atom) {
        (Atom::Lit(x), Atom::Lit(y)) if x == y => {
            if is_word(x) {
                2
            } else {
                1
            }
        }
        (Atom::Slot(_), Atom::Slot(_)) => 1,
        _ => 0,
    }
}

/// Aligned atoms: index in one template, in the other.
type Pairs = Vec<(usize, usize)>;

/// The pairs of aligned atoms of the heaviest common subsequence.
fn align(a: &[Part], b: &[Part]) -> Pairs {
    let (n, m) = (a.len(), b.len());
    let at = |i: usize, j: usize| i * (m + 1) + j;
    let mut dp = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let mut v = dp[at(i + 1, j)].max(dp[at(i, j + 1)]);
            let w = weight(&a[i], &b[j]);
            if w > 0 {
                v = v.max(w + dp[at(i + 1, j + 1)]);
            }
            dp[at(i, j)] = v;
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut pairs = Vec::new();
    while i < n && j < m {
        let w = weight(&a[i], &b[j]);
        if w > 0 && dp[at(i, j)] == w + dp[at(i + 1, j + 1)] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if dp[at(i + 1, j)] >= dp[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// Whether templates `a` and `b`, aligned as `pairs`, are one pattern, and
/// how alike they are (the share of words in common, of the wordier one;
/// words counted by their `weights` in the context, as [`Family::size`]:
/// [`weigh`]). They share half a word at least, at each
/// place they differ one side has one token at most (a state's value is a
/// word: `{D|P}`, `{engaged at <*> C|released}`, not two phrases) and the
/// other `side` (4; tokens: words, values and slots, so neither
/// nothing nor `<*> <*> <*>` makes room for a whole phrase), and:
/// - they differ in one place (one slot): the words in common are half the
///   words of the one with fewer at least, and a third of the other's;
/// - in several: the words in common are 60 % of either's at least.
///
/// A slot at most is new (a place where neither side is a slot yet;
/// `new-slots`): two at once vary together as far as anyone can tell,
/// `{warning|status} the data {was|is} read` holding only `warning … was`
/// and `status … is`, which are two messages. A third with one of them
/// (`warning … is`) shows the slots apart, and then both join.
///
/// `local`'s rules refuse a merge putting a `keep` word in a slot, two
/// words of an `apart` in one slot, or making more than `max-slots` slots.
fn accept(a: &Family, b: &Family, pairs: &[(usize, usize)], weights: &Weights, local: &Local) -> Verdict {
    assess(a, b, pairs, weights, local, None)
}

/// [`accept`], saying in `notes` what it weighed and why (for
/// [`explain`]; none in the merge itself).
fn assess(
    a: &Family,
    b: &Family,
    pairs: &[(usize, usize)],
    weights: &Weights,
    local: &Local,
    mut notes: Option<&mut Vec<String>>,
) -> Verdict {
    let mut say = |s: &dyn Fn() -> String| {
        if let Some(n) = notes.as_deref_mut() {
            n.push(s());
        }
    };
    let common: f32 = pairs
        .iter()
        .filter_map(|&(i, _)| match &a.parts[i].atom {
            Atom::Lit(l) if is_word(l) => Some(weights.get(l).copied().unwrap_or(1.0)),
            _ => None,
        })
        .sum();
    let (few, many) = (a.size.min(b.size), a.size.max(b.size));
    let tokens = |seg: &[Part]| {
        seg.iter()
            .filter(|p| {
                matches!(&p.atom, Atom::Slot(_)) || matches!(&p.atom, Atom::Lit(l) if is_word(l) || &**l == MASK_STR)
            })
            .count()
    };
    let slot = |seg: &[Part]| matches!(seg, [Part { atom: Atom::Slot(_), .. }]);
    let kept = |seg: &[Part]| {
        seg.iter().find_map(|p| match &p.atom {
            Atom::Lit(l) if local.keep.contains(l) => Some(l.to_string()),
            _ => None,
        })
    };
    let (mut gaps, mut new, mut widest) = (0, 0, 0);
    let (mut long, mut rule) = (false, false);
    // Slots it would have: those aligned with slots, and a slot per gap.
    let mut slots = pairs.iter().filter(|&&(i, _)| matches!(a.parts[i].atom, Atom::Slot(_))).count();
    let (mut i, mut j) = (0, 0);
    for &(pi, pj) in pairs.iter().chain(std::iter::once(&(a.parts.len(), b.parts.len()))) {
        if i < pi || j < pj {
            let (sa, sb) = (&a.parts[i..pi], &b.parts[j..pj]);
            gaps += 1;
            slots += 1;
            widest = widest.max(sa.len()).max(sb.len());
            let (ta, tb) = (tokens(sa), tokens(sb));
            let fresh = !slot(sa) && !slot(sb);
            say(&|| {
                format!(
                    "they differ: “{}” / “{}” ({ta} and {tb} tokens{})",
                    shown(sa),
                    shown(sb),
                    if fresh { ", a new slot" } else { ", into a slot" }
                )
            });
            if ta.min(tb) > 1 {
                long = true;
                say(&|| "both sides hold a phrase there: a state's value is one token".into());
            } else if ta.max(tb) > local.s.side {
                long = true;
                say(&|| format!("a side of {} tokens there, more than side {}", ta.max(tb), local.s.side));
            }
            if fresh {
                new += 1;
            }
            if !local.keep.is_empty()
                && let Some(w) = kept(sa).or_else(|| kept(sb))
            {
                rule = true;
                say(&|| format!("rule keep {w}: the word would be in a slot"));
            }
            for g in &local.apart {
                let (x, y) = (items_in(sa, g), items_in(sb, g));
                if clash(x, y) {
                    rule = true;
                    let name = |bits: u64| {
                        let k = bits.trailing_zeros() as usize;
                        g.get(k).map(|item| item.concat()).unwrap_or_default()
                    };
                    say(&|| {
                        format!(
                            "rule apart: {} and {} would be in one slot",
                            name(x),
                            name(if y & !x != 0 { y & !x } else { y })
                        )
                    });
                }
            }
        }
        (i, j) = (pi + 1, pj + 1);
    }
    if let Some(m) = local.max_slots.filter(|&m| slots > m) {
        rule = true;
        say(&|| format!("rule max-slots {m}: it would have {slots} slots"));
    }
    // One atom for another (or for nothing), all else the same: a state
    // (`gear=D`, `gear=P`), however common the words around it.
    let flip = gaps == 1 && widest <= 1;
    let ok = flip
        || common >= 0.5
            && match gaps {
                0 => true,
                1 => common * 2.0 >= few && common * 3.0 >= many,
                _ => common * 5.0 >= many * 3.0,
            };
    let alike = if many > 0.0 { common / many } else { 0.0 };
    say(&|| {
        let need = match gaps {
            _ if flip => "none: one atom for another, the rest the same".to_owned(),
            0 => "half a word".to_owned(),
            1 => format!("half of {few:.1} and a third of {many:.1}"),
            _ => format!("60 % of {many:.1}"),
        };
        format!("words in common: {common:.1} (of {few:.1} and {many:.1}); needed: {need}; {gaps} place(s) differ")
    });
    if !ok {
        say(&|| "apart: too few words in common".into());
        Verdict::No
    } else if long {
        say(&|| "apart: a side too long (setting side)".into());
        Verdict::Side
    } else if rule {
        say(&|| "apart: a rule refuses".into());
        Verdict::Rule
    } else if new > local.s.new_slots {
        say(&|| format!("apart: {new} new slots at once, more than new-slots {}", local.s.new_slots));
        Verdict::Apart
    } else {
        say(&|| format!("one pattern: alike {:.0} %", alike * 100.0));
        Verdict::Join(alike)
    }
}

/// A place's atoms as text: `<*>` for values and slots, `∅` for nothing.
fn shown(seg: &[Part]) -> String {
    let t = render(seg).replace([MASK, ANY], "<*>").replace(EITHER, " ").replace(REST, "…");
    if t.is_empty() { "∅".into() } else { t }
}

/// Why stage 2 keeps two patterns apart, or would join them, in words: the
/// places they differ in, the words in common, and the setting or rule
/// that decides (with `config`'s settings and rules for `a`'s fields).
/// For tuning: stage 2 alone, on the patterns as they came out, every word
/// weighing 1 (the run also weighs header words less, and stage 1 may
/// have kept their lines apart before).
pub fn explain(a: &Pattern, b: &Pattern, config: &PatternConfig) -> Vec<String> {
    for (n, v) in &a.fields {
        if let Some((_, w)) = b.fields.iter().find(|(m, _)| m == n)
            && v != w
        {
            return vec![format!("apart: their {n} differs ({v} / {w}); patterns never join across fields")];
        }
    }
    let family = |p: &Pattern| Family {
        parts: p.parts.clone(),
        size: p.parts.iter().filter(|x| matches!(&x.atom, Atom::Lit(l) if is_word(l))).count() as f32,
        sides: p.sides,
        vals: Vec::new(),
        unsorted: p.unsorted,
        apart: false,
        grew: 0,
        ids: Vec::new(),
        ctx: 0,
    };
    if a.unsorted || b.unsorted {
        return vec!["apart: lines that fit no pattern (not sorted) aren't aligned".into()];
    }
    let (fa, fb) = (family(a), family(b));
    let names: Vec<String> = a.fields.iter().map(|(n, _)| n.clone()).collect();
    let vals: Vec<Option<&[u8]>> = a.fields.iter().map(|(_, v)| Some(v.as_bytes())).collect();
    let local = config.local(&names, &vals);
    let mut notes = Vec::new();
    if !local.s.merge {
        notes.push("apart: merge is off here (merge off, or no-merge)".into());
        return notes;
    }
    let pairs = align(&fa.parts, &fb.parts);
    assess(&fa, &fb, &pairs, &Weights::default(), &local, Some(&mut notes));
    notes
}

/// The items of an `apart` group (each its atoms) a place two templates
/// differ in holds, as bits: in its atoms, or in a slot's values.
fn items_in(seg: &[Part], group: &[Vec<Box<str>>]) -> u64 {
    let mut found = 0u64;
    let mut look = |atoms: &[&str]| {
        for (k, item) in group.iter().enumerate().take(64) {
            if !item.is_empty() && atoms.windows(item.len()).any(|w| w.iter().zip(item).all(|(x, y)| *x == &**y)) {
                found |= 1 << k;
            }
        }
    };
    if let [Part { atom: Atom::Slot(s), .. }] = seg {
        for v in &s.values {
            let atoms = atomize(&v.text);
            look(&atoms.iter().map(|(_, a)| a.as_str()).collect::<Vec<_>>());
        }
    } else {
        let atoms: Vec<&str> = seg
            .iter()
            .filter_map(|p| match &p.atom {
                Atom::Lit(l) => Some(&**l),
                Atom::Slot(_) => None,
            })
            .collect();
        look(&atoms);
    }
    found
}

/// Two sides holding different items of an `apart` group.
fn clash(a: u64, b: u64) -> bool {
    a != 0 && b != 0 && (a | b).count_ones() >= 2
}

/// What [`accept`] says of two templates.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Verdict {
    /// One pattern; how alike they are.
    Join(f32),
    /// Alike enough, but more slots than `new-slots` would be new: apart
    /// until another pattern holds one of them as a slot.
    Apart,
    /// Alike enough, but a side is too long (`side`, or two phrases).
    Side,
    /// Alike enough, but a rule refuses (`keep`, `apart`, `max-slots`).
    Rule,
    No,
}

/// Templates kept apart in stage 2, by why ([`PatternStats`]).
#[derive(Default)]
struct Refused {
    apart: std::sync::atomic::AtomicUsize,
    side: std::sync::atomic::AtomicUsize,
    rules: std::sync::atomic::AtomicUsize,
}

/// `a` and `b` as one: the aligned atoms kept, each place they differ a slot
/// holding both sides' values.
fn merge(a: Family, b: &Family, pairs: &[(usize, usize)]) -> Family {
    /// A side's atoms at a place where they differ, into `slot`.
    fn add(slot: &mut Slot, seg: &[Part], f: &Family) {
        if let [Part { atom: Atom::Slot(s), .. }] = seg {
            slot.join(s.clone());
            return;
        }
        let text = render(seg);
        if text.is_empty() || text.contains([' ', EITHER, ANY]) {
            slot.wide = true;
        }
        slot.add(&text, &f.sides);
    }
    let mut parts = Vec::new();
    let (mut i, mut j) = (0, 0);
    for &(pi, pj) in pairs.iter().chain(std::iter::once(&(a.parts.len(), b.parts.len()))) {
        let (sa, sb) = (&a.parts[i..pi], &b.parts[j..pj]);
        let gap = !sa.is_empty() || !sb.is_empty();
        if gap {
            let mut slot = Slot::default();
            add(&mut slot, sa, &a);
            add(&mut slot, sb, b);
            let sep = match (sa.first(), sb.first()) {
                (Some(x), Some(y)) if x.sep == y.sep => x.sep,
                _ => Sep::Either,
            };
            parts.push(Part { sep, atom: Atom::Slot(slot) });
        }
        if pi < a.parts.len() {
            let (x, y) = (&a.parts[pi], &b.parts[pj]);
            // After a place where one side has nothing, either.
            let sep = if x.sep == y.sep && !(gap && (sa.is_empty() || sb.is_empty())) { x.sep } else { Sep::Either };
            let atom = match (&x.atom, &y.atom) {
                (Atom::Slot(s), Atom::Slot(t)) => {
                    let mut s = s.clone();
                    s.join(t.clone());
                    Atom::Slot(s)
                }
                (atom, _) => atom.clone(),
            };
            parts.push(Part { sep, atom });
        }
        (i, j) = (pi + 1, pj + 1);
    }
    let mut vals = a.vals;
    for (v, w) in vals.iter_mut().zip(&b.vals) {
        v.join(w);
    }
    let mut sides = a.sides;
    add_sides(&mut sides, &b.sides);
    let mut ids = a.ids;
    ids.extend_from_slice(&b.ids);
    Family {
        parts,
        size: a.size.max(b.size),
        sides,
        vals,
        unsorted: false,
        apart: a.apart,
        grew: a.grew,
        ids,
        ctx: a.ctx,
    }
}

/// Lines tried against another pattern's query in [`absorb`].
const SAMPLE_LINES: usize = 24;

/// Lines of `f` to try a query on: its first and last in each set, then
/// the first of each value of its slots (up to [`SAMPLE_LINES`]).
fn sample_lines(f: &Family) -> Vec<(usize, LineNo)> {
    let mut out: Vec<(usize, LineNo)> = Vec::new();
    let put = |side: usize, line: LineNo, out: &mut Vec<(usize, LineNo)>| {
        if out.len() < SAMPLE_LINES && !out.contains(&(side, line)) {
            out.push((side, line));
        }
    };
    for side in 0..2 {
        if f.sides[side].count > 0 {
            put(side, f.sides[side].first, &mut out);
            put(side, f.sides[side].last, &mut out);
        }
    }
    for p in &f.parts {
        if let Atom::Slot(s) = &p.atom {
            for v in &s.values {
                for side in 0..2 {
                    if v.sides[side].count > 0 {
                        put(side, v.sides[side].first, &mut out);
                    }
                }
            }
        }
    }
    out
}

/// Stage 2's patterns with each one whose lines another's query takes
/// merged into it: `a` takes `b` when `a`'s regex matches every line of
/// `b`'s tried ([`sample_lines`]), their fields don't tell them apart and
/// `local`'s rules allow it (`keep`, `apart`: [`assess`]'s). Without it a
/// split message (`proxy.cse.cuhk.edu.hk:<*> open through proxy …`, `<*>
/// open through proxy …`) is two patterns where the second's query, ▾
/// Filter or 🔍 Search, shows the first's lines too.
///
/// Of those taking `b`, the one of most words joins it (the closest), the
/// most lines among equals; the most specific go first, so a chain (`b` in
/// `a`, `a` in `c`) ends in the most general. Only patterns whose words
/// `b` holds all are tried; one of no words (a catch-all) takes none.
fn absorb(
    mut fams: Vec<Family>,
    locals: &[Local],
    sets: &[Set],
    readings: &[Reading],
    progress: &Progress,
) -> Result<Vec<Family>> {
    let words = |f: &Family| {
        let mut w: Vec<Box<str>> = f.words().map(Into::into).collect();
        w.sort_unstable();
        w
    };
    let sorted: Vec<Vec<Box<str>>> = fams.iter().map(words).collect();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for w in &sorted {
        let mut d: Vec<&str> = w.iter().map(|x| &**x).collect();
        d.dedup();
        for x in d {
            *seen.entry(x).or_default() += 1;
        }
    }
    let usable = |f: &Family| !f.unsorted && locals[f.ctx].s.merge && f.parts.len() <= MAX_ATOMS;
    // Each pattern by its rarest word: those whose rarest word `b` holds
    // are the only ones that may take it.
    let mut by_rarest: HashMap<&str, Vec<usize>> = HashMap::new();
    for (k, w) in sorted.iter().enumerate() {
        if usable(&fams[k])
            && let Some(r) = w.iter().map(|x| &**x).min_by_key(|x| (seen[x], *x))
        {
            by_rarest.entry(r).or_default().push(k);
        }
    }
    let subset = |a: &[Box<str>], b: &[Box<str>]| {
        let mut j = 0;
        a.iter().all(|x| {
            while j < b.len() && b[j] < *x {
                j += 1;
            }
            let hit = j < b.len() && b[j] == *x;
            j += 1;
            hit
        })
    };
    let mut regexes: HashMap<usize, Option<regex::Regex>> = HashMap::new();
    let mut order: Vec<usize> = (0..fams.len()).collect();
    order.sort_by(|&x, &y| sorted[y].len().cmp(&sorted[x].len()).then(by_count(&fams[x].sides, &fams[y].sides)));
    let mut gone = vec![false; fams.len()];
    for (n, &b) in order.iter().enumerate() {
        if n % 256 == 0 && progress.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if !usable(&fams[b]) {
            continue;
        }
        let mut candidates: Vec<usize> = Vec::new();
        let mut distinct: Vec<&str> = sorted[b].iter().map(|x| &**x).collect();
        distinct.dedup();
        for w in distinct {
            for &a in by_rarest.get(w).into_iter().flatten() {
                if a != b && !gone[a] && fams[a].compatible(&fams[b]) && subset(&sorted[a], &sorted[b]) {
                    candidates.push(a);
                }
            }
        }
        candidates
            .sort_by(|&x, &y| sorted[y].len().cmp(&sorted[x].len()).then(by_count(&fams[x].sides, &fams[y].sides)));
        let mut texts: Option<Vec<String>> = None;
        let found = candidates.into_iter().find_map(|a| {
            let re = regexes
                .entry(a)
                .or_insert_with(|| regex::Regex::new(&parts_regex(&fams[a].parts, &[], fams[a].loose(locals))).ok())
                .as_ref()?;
            let texts = texts.get_or_insert_with(|| {
                sample_lines(&fams[b])
                    .into_iter()
                    .map(|(side, line)| text_of(sets[side].source, &readings[side], line).into_owned())
                    .collect()
            });
            if !texts.iter().all(|t| re.is_match(t)) {
                return None;
            }
            let pairs = align(&fams[a].parts, &fams[b].parts);
            rules_allow(&fams[a], &fams[b], &pairs, &locals[fams[a].ctx]).then_some((a, pairs))
        });
        if let Some((a, pairs)) = found {
            let into = std::mem::replace(
                &mut fams[a],
                Family {
                    parts: vec![],
                    size: 0.0,
                    sides: Sides::default(),
                    vals: vec![],
                    unsorted: false,
                    apart: false,
                    grew: 0,
                    ids: vec![],
                    ctx: 0,
                },
            );
            fams[a] = merge(into, &fams[b], &pairs);
            // Its template may have grown slots: its regex again.
            regexes.remove(&a);
            gone[b] = true;
        }
    }
    let mut out: Vec<Family> = fams.into_iter().zip(gone).filter(|(_, g)| !g).map(|(f, _)| f).collect();
    out.sort_by(|a, b| by_count(&a.sides, &b.sides));
    Ok(out)
}

/// Whether `local`'s rules let `b` join `a` aligned as `pairs`: no `keep`
/// word into a slot, no two items of an `apart` group in one (as
/// [`assess`]).
fn rules_allow(a: &Family, b: &Family, pairs: &[(usize, usize)], local: &Local) -> bool {
    if local.keep.is_empty() && local.apart.is_empty() {
        return true;
    }
    let (mut i, mut j) = (0, 0);
    for &(pi, pj) in pairs.iter().chain(std::iter::once(&(a.parts.len(), b.parts.len()))) {
        if i < pi || j < pj {
            let (sa, sb) = (&a.parts[i..pi], &b.parts[j..pj]);
            let kept = |seg: &[Part]| seg.iter().any(|p| matches!(&p.atom, Atom::Lit(l) if local.keep.contains(l)));
            if kept(sa) || kept(sb) || local.apart.iter().any(|g| clash(items_in(sa, g), items_in(sb, g))) {
                return false;
            }
        }
        (i, j) = (pi + 1, pj + 1);
    }
    true
}

/// How many words `f` and a template of words `sorted` hold both (with
/// repeats): at least as many as an alignment of the two can match.
fn shared_words(f: &Family, sorted: &[&str]) -> usize {
    let mut theirs: Vec<&str> = f.words().collect();
    theirs.sort_unstable();
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < theirs.len() && j < sorted.len() {
        match theirs[i].cmp(sorted[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    n
}

/// Merge templates into patterns, most lines first: each is aligned with the
/// patterns sharing its rarer words and joins the most alike one that
/// [`accept`]s it. Catch-alls, and templates past the first [`MAX_MERGE`],
/// stay as they are.
///
/// Pass `pass` > 1 aligns only the patterns kept [`Verdict::Apart`] last
/// time, after all the others, and only with those that have grown a slot
/// since (merged in the pass before or in this one): the others they were
/// kept apart from as they are.
/// `weights` and `locals` (settings and rules) are per context
/// ([`Family::ctx`]): `no-merge` leaves a context's templates as they are;
/// `refused` counts those kept apart.
fn merge_all(
    mut templates: Vec<Family>,
    pass: usize,
    weights: &[Weights],
    locals: &[Local],
    refused: &Refused,
    progress: &Progress,
) -> Result<Vec<Family>> {
    let later = pass > 1;
    templates.sort_by(|a, b| by_count(&a.sides, &b.sides));
    if later {
        templates.sort_by_key(|t| t.apart);
    }
    let mut out: Vec<Family> = Vec::new();
    // Word → the patterns holding it (in a later pass, those grown).
    let mut index: HashMap<Box<str>, Vec<usize>> = HashMap::new();
    let indexed = |f: &Family| !later || f.grew + 1 >= pass;
    for (n, mut t) in templates.into_iter().enumerate() {
        if n % 1024 == 0 && progress.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let align_it = locals[t.ctx].s.merge && if later { t.apart } else { n < MAX_MERGE };
        t.apart = false;
        if t.unsorted || !align_it {
            if later && indexed(&t) {
                let at = out.len();
                for w in t.words() {
                    let list = index.entry(w.into()).or_default();
                    if list.last() != Some(&at) {
                        list.push(at);
                    }
                }
            }
            out.push(t);
            continue;
        }
        let apart = AtomicBool::new(false);
        let [long, rule] = [(); 2].map(|_| AtomicBool::new(false));
        let mut mine: Vec<&str> = t.words().collect();
        mine.sort_unstable();
        let mut words = mine.clone();
        words.dedup();
        words.sort_by_key(|w| index.get(*w).map_or(0, Vec::len));
        let mut candidates: Vec<usize> = Vec::new();
        for w in &words {
            for &c in index.get(*w).into_iter().flatten() {
                if !candidates.contains(&c) {
                    candidates.push(c);
                }
            }
            if candidates.len() >= CANDIDATES {
                break;
            }
        }
        // The most alike, the first of equals (in parallel: alignments of
        // long templates add up).
        let best = (t.parts.len() <= MAX_ATOMS)
            .then(|| {
                candidates
                    .par_iter()
                    .enumerate()
                    .filter_map(|(k, &c)| {
                        let f = &out[c];
                        let local = &locals[f.ctx];
                        if f.unsorted || !local.s.merge || f.parts.len() > MAX_ATOMS || !f.compatible(&t) {
                            return None;
                        }
                        // The words aligned are some of those both hold: too
                        // few of them, and `accept` can't pass.
                        let shared = shared_words(f, &mine);
                        if shared == 0 || shared as f32 * 3.0 < f.size.max(t.size) {
                            return None;
                        }
                        let pairs = align(&f.parts, &t.parts);
                        let flag = match accept(f, &t, &pairs, &weights[f.ctx], local) {
                            Verdict::Join(s) => return Some((k, c, s, pairs)),
                            Verdict::Apart => &apart,
                            Verdict::Side => &long,
                            Verdict::Rule => &rule,
                            Verdict::No => return None,
                        };
                        flag.store(true, Ordering::Relaxed);
                        None
                    })
                    .reduce_with(|a, b| if b.2 > a.2 || (b.2 == a.2 && b.0 < a.0) { b } else { a })
                    .map(|(_, c, s, pairs)| (c, s, pairs))
            })
            .flatten();
        let words: Vec<Box<str>> = words.into_iter().map(Into::into).collect();
        let at = match best {
            Some((c, _, pairs)) => {
                let f = std::mem::replace(
                    &mut out[c],
                    Family {
                        parts: vec![],
                        size: 0.0,
                        sides: Sides::default(),
                        vals: vec![],
                        unsorted: false,
                        apart: false,
                        grew: 0,
                        ids: vec![],
                        ctx: 0,
                    },
                );
                out[c] = merge(f, &t, &pairs);
                out[c].grew = pass;
                c
            }
            None => {
                t.apart = apart.into_inner();
                for (flag, count) in
                    [(t.apart, &refused.apart), (long.into_inner(), &refused.side), (rule.into_inner(), &refused.rules)]
                {
                    if flag {
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                }
                out.push(t);
                out.len() - 1
            }
        };
        if !indexed(&out[at]) {
            continue;
        }
        for w in words {
            let list = index.entry(w).or_default();
            if !list.contains(&at) {
                list.push(at);
            }
        }
    }
    Ok(out)
}

/// Each slot's values sorted, and what they all begin or end with moved
/// out of the slot: `{gear=D|gear=P}` → `gear={D|P}`.
fn factor(parts: Vec<Part>) -> Vec<Part> {
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        let Atom::Slot(mut slot) = p.atom else {
            out.push(p);
            continue;
        };
        slot.values.sort_by(|a, b| by_count(&a.sides, &b.sides));
        if slot.overflow || slot.values.len() < 2 || slot.values.iter().any(|v| v.text.is_empty()) {
            out.push(Part { sep: p.sep, atom: Atom::Slot(slot) });
            continue;
        }
        let atoms: Vec<Atoms> = slot.values.iter().map(|v| atomize(&v.text)).collect();
        let shortest = atoms.iter().map(Vec::len).min().unwrap_or(0);
        let same = |k: &dyn Fn(&Atoms) -> usize, with_sep: bool| {
            let (s0, t0) = &atoms[0][k(&atoms[0])];
            *t0 != ANY.to_string()
                && atoms.iter().all(|a| {
                    let (s, t) = &a[k(a)];
                    t == t0 && (!with_sep || s == s0)
                })
        };
        // Leave one atom at least in every value.
        let mut pre = 0;
        while pre + 1 < shortest && same(&|_| pre, pre > 0) {
            pre += 1;
        }
        let mut suf = 0;
        while pre + suf + 1 < shortest && same(&|a| a.len() - 1 - suf, true) {
            suf += 1;
        }
        if pre == 0 && suf == 0 {
            out.push(Part { sep: p.sep, atom: Atom::Slot(slot) });
            continue;
        }
        let first = &atoms[0];
        for (k, (s, t)) in first[..pre].iter().enumerate() {
            out.push(Part { sep: if k == 0 { p.sep } else { *s }, atom: Atom::Lit(t.as_str().into()) });
        }
        let slot_sep = if pre == 0 {
            p.sep
        } else if atoms.iter().all(|a| a[pre].0 == first[pre].0) {
            first[pre].0
        } else {
            Sep::Either
        };
        slot.wide = false;
        for (v, a) in slot.values.iter_mut().zip(&atoms) {
            let mut text = String::new();
            for (k, (s, t)) in a[pre..a.len() - suf].iter().enumerate() {
                if k > 0 {
                    match s {
                        Sep::Space => text.push(' '),
                        Sep::Tight => {}
                        Sep::Either => text.push(EITHER),
                    }
                }
                text.push_str(t);
            }
            slot.wide |= text.contains([' ', EITHER, ANY]);
            v.text = text;
        }
        out.push(Part { sep: slot_sep, atom: Atom::Slot(slot) });
        for (s, t) in &first[first.len() - suf..] {
            out.push(Part { sep: *s, atom: Atom::Lit(t.as_str().into()) });
        }
    }
    out
}

// ---- Differences between two sets ----

/// What changed between set A and set B: a pattern's lines, or a slot
/// value's share of its pattern's lines.
#[derive(Debug, Clone)]
pub struct Difference {
    /// Index into [`Patterns::patterns`].
    pub pattern: usize,
    /// A slot value: (slot, value) indexes into [`Pattern::slots`] and its
    /// [`Slot::values`]; `None`: the pattern itself.
    pub value: Option<(usize, usize)>,
    /// Lines in A and B.
    pub counts: [u64; 2],
    /// Out of how many lines in A and B: all lines (a pattern), the
    /// pattern's lines (a value).
    pub of: [u64; 2],
    /// How surely it changed: Dunning's log-likelihood ratio G² (one degree
    /// of freedom: 3.84 is p = 0.05, 10.8 is p = 0.001). Unlike a ratio it
    /// weighs the evidence: 0 → 1 line scores low, 0 → 500 high.
    pub score: f64,
}

impl Difference {
    pub fn change(&self) -> Change {
        match self.counts {
            [0, _] => Change::New,
            [_, 0] => Change::Gone,
            _ => Change::Changed,
        }
    }

    /// How many times more often in B than in A, by rate (smoothed, so a
    /// side with none stays finite).
    pub fn ratio(&self) -> f64 {
        let rate = |k: usize| (self.counts[k] as f64 + 0.5) / (self.of[k] as f64 + 1.0);
        rate(1) / rate(0)
    }
}

/// G² of a 2 × 2 table: `a` of `na` against `b` of `nb`.
fn g2(a: u64, na: u64, b: u64, nb: u64) -> f64 {
    fn xlx(x: f64) -> f64 {
        if x > 0.0 { x * x.ln() } else { 0.0 }
    }
    let (a, b, na, nb) = (a as f64, b as f64, na as f64, nb as f64);
    let (c, d) = (na - a, nb - b);
    let g = xlx(a) + xlx(b) + xlx(c) + xlx(d) - xlx(a + b) - xlx(c + d) - xlx(na) - xlx(nb) + xlx(na + nb);
    (2.0 * g).max(0.0)
}

/// What a [`Difference`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// Lines in B only.
    New,
    /// Lines in A only.
    Gone,
    /// Lines in both, more or fewer.
    Changed,
}

/// What changed between the sets of a [`compare`]: each pattern's lines
/// against all lines, and, where a pattern has lines on both sides, each
/// value of its slots (all values kept, `!overflow`) against the pattern's
/// lines. New ones first, then gone ones (most lines first, whatever their
/// score: one line in B only is weak evidence of a change in rate, but a
/// message that never showed in A is what one looks for), then changed ones
/// scoring `min_score` at least, surest first.
pub fn differences(ps: &Patterns, min_score: f64) -> Vec<Difference> {
    let mut out = Vec::new();
    let mut add = |d: Difference| {
        if d.change() != Change::Changed || d.score >= min_score {
            out.push(d);
        }
    };
    for (k, p) in ps.patterns.iter().enumerate() {
        let counts = [p.sides[0].count, p.sides[1].count];
        let score = g2(counts[0], ps.lines[0], counts[1], ps.lines[1]);
        add(Difference { pattern: k, value: None, counts, of: ps.lines, score });
        if counts[0] == 0 || counts[1] == 0 {
            continue;
        }
        for (s, slot) in p.slots().enumerate() {
            if slot.overflow || slot.values.len() < 2 {
                continue;
            }
            for (v, value) in slot.values.iter().enumerate() {
                let c = [value.sides[0].count, value.sides[1].count];
                let score = g2(c[0], counts[0], c[1], counts[1]);
                add(Difference { pattern: k, value: Some((s, v)), counts: c, of: counts, score });
            }
        }
    }
    out.sort_by(|a, b| {
        let lines = |d: &Difference| d.counts[0] + d.counts[1];
        a.change()
            .cmp(&b.change())
            .then_with(|| match a.change() {
                Change::Changed => b.score.total_cmp(&a.score),
                _ => lines(b).cmp(&lines(a)),
            })
            .then(a.pattern.cmp(&b.pattern))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::search::{Filter, search};
    use crate::input::mem::{MemSource, Row};
    use crate::{Query, TextFile};

    fn text(lines: &[&str]) -> TextFile {
        TextFile::from_bytes("t", lines.join("\n").into_bytes())
    }

    fn templates(ps: &Patterns) -> Vec<String> {
        ps.patterns.iter().map(Pattern::template).collect()
    }

    fn detect_all(source: &dyn LogSource) -> Patterns {
        let options = PatternOptions { lines: true, ..Default::default() };
        let ps = detect(source, None, &options, &Progress::new()).unwrap();
        check_queries(source, &ps);
        ps
    }

    /// Every pattern's query finds at least its own lines; kept lines
    /// ([`PatternOptions::lines`]) are each in one pattern, as many as it
    /// counts, and its query finds them.
    fn check_queries(source: &dyn LogSource, ps: &Patterns) {
        let mut covered = 0;
        let mut seen = std::collections::HashSet::new();
        let kept = ps.patterns.iter().any(|p| !p.lines[0].is_empty() || !p.lines[1].is_empty());
        for p in &ps.patterns {
            let q = Query::compile_in(&ps.query(p), source.schema(), &Default::default()).unwrap();
            let res = search(source, &Filter::Query(q), &Progress::new()).unwrap();
            assert!(res.lines.len() as u64 >= p.count(), "{}: {} < {}", ps.query(p), res.lines.len(), p.count());
            assert!(res.contains(p.first()) && res.contains(p.last()), "{}", ps.query(p));
            covered += p.count();
            if kept {
                for side in 0..2 {
                    let l = &p.lines[side];
                    assert_eq!(l.len() as u64, p.sides[side].count, "{}", p.template());
                    assert!(l.windows(2).all(|w| w[0] < w[1]), "{}", p.template());
                    if let (Some(a), Some(b)) = (l.first(), l.last()) {
                        assert_eq!((*a, *b), (p.sides[side].first, p.sides[side].last), "{}", p.template());
                    }
                    for &line in l {
                        assert!(res.contains(line), "{} misses line {line}", ps.query(p));
                        assert!(seen.insert((side, line)), "line {line} in two patterns");
                    }
                }
            }
        }
        assert_eq!(covered, ps.total());
        if kept {
            assert_eq!(seen.len() as u64, ps.total());
        }
    }

    #[test]
    fn masks_values() {
        let m = |s| mask(s).replace(MASK, "<*>");
        assert_eq!(m("peer=10.0.3.7"), "peer=<*>");
        assert_eq!(m("2026-10-17"), "<*>");
        assert_eq!(m("10:00:01.123"), "<*>");
        assert_eq!(m("abc-12"), "abc-<*>");
        assert_eq!(m("id=0x1f,"), "id=<*>,");
        assert_eq!(m("12ms"), "<*>");
        assert_eq!(m("-5"), "-<*>");
        assert_eq!(m("plain"), "plain");
        assert_eq!(m("żółw7"), "<*>");
        assert_eq!(m("żółw-7"), "żółw-<*>");
    }

    #[test]
    fn atoms_by_character_class() {
        let a = atomize("gear=D [ok] x_1 \u{E000}ms");
        let t: Vec<(Sep, &str)> = a.iter().map(|(s, t)| (*s, t.as_str())).collect();
        use Sep::*;
        assert_eq!(
            t,
            [
                (Tight, "gear"),
                (Tight, "="),
                (Tight, "D"),
                (Space, "["),
                (Tight, "ok"),
                (Tight, "]"),
                (Space, "x_1"),
                (Space, "\u{E000}"),
                (Tight, "ms")
            ]
        );
    }

    #[test]
    fn groups_messages_by_template() {
        let f = text(&[
            "2026-10-17 10:00:00.001 [INFO] net connected peer=10.0.0.1 in 12 ms",
            "2026-10-17 10:00:00.002 [WARN] disk almost full: 91%",
            "2026-10-17 10:00:00.003 [INFO] net connected peer=10.0.0.7 in 3 ms",
            "2026-10-17 10:00:00.004 [INFO] user alice logged in",
            "2026-10-17 10:00:00.005 [INFO] user bob logged in",
            "2026-10-17 10:00:00.006 [WARN] disk almost full: 97%",
            "2026-10-17 10:00:00.007 [INFO] user carol logged in",
        ]);
        let ps = detect_all(&f);
        let t: Vec<(u64, String)> = ps.patterns.iter().map(|p| (p.count(), p.template())).collect();
        assert_eq!(
            t,
            [
                (3, "<*> <*> [INFO] user <*> logged in".to_owned()),
                (2, "<*> <*> [INFO] net connected peer=<*> in <*> ms".to_owned()),
                (2, "<*> <*> [WARN] disk almost full: <*>%".to_owned()),
            ]
        );
        assert_eq!((ps.patterns[0].first(), ps.patterns[0].last()), (3, 6));
    }

    #[test]
    fn only_given_lines() {
        let f = text(&["a 1", "b x y", "a 2", "b x z"]);
        let ps = detect(&f, Some(&[1, 3]), &PatternOptions::default(), &Progress::new()).unwrap();
        assert_eq!(ps.lines, [2, 0]);
        assert_eq!(templates(&ps), ["b x <*>"]);
    }

    #[test]
    fn chunks_merge_like_one() {
        // More lines than a chunk: the parts' templates are merged.
        let mut lines = Vec::new();
        for i in 0..(CHUNK * 2 + 10) {
            lines.push(match i % 3 {
                0 => format!("send {i} bytes to node{}", i % 7),
                1 => format!("recv ok from {}", ["alpha", "beta", "gamma"][i / 3 % 3]),
                _ => format!("tick {i}"),
            });
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let f = text(&refs);
        let ps = detect_all(&f);
        assert_eq!(templates(&ps), ["send <*> bytes to <*>", "recv ok from {alpha|beta|gamma}", "tick <*>"]);
        assert_eq!(ps.patterns[0].first(), 0);
        assert_eq!(ps.patterns.iter().map(Pattern::count).sum::<u64>(), lines.len() as u64);
    }

    #[test]
    fn fields_that_tell_patterns_apart_go_into_queries() {
        let row = |text, apid| Row { text, apid: Some(apid), mcnt: None, level: None };
        let s = MemSource::new(vec![
            row("speed 10 km/h", "NAV"),
            row("speed 12 km/h", "NAV"),
            row("door open", "BODY"),
            row("door closed", "BODY"),
            // Alike, but another apid: another pattern.
            row("door open", "SEAT"),
        ]);
        let ps = detect_all(&s);
        assert_eq!(templates(&ps), ["speed <*> km/h", "door <*>", "door open"]);
        assert_eq!(ps.query(&ps.patterns[0]), r#"apid == "NAV" AND raw ~ /^\s*speed\s+[\w.:\-]+\s+km\x2fh\s*$/"#);
    }

    #[test]
    fn states_keep_their_values() {
        let f = text(&[
            "10:00:00 NAV gear P",
            "10:00:01 OTA downloading",
            "10:00:02 NAV gear D",
            "10:00:03 OTA installing",
            "10:00:04 NAV gear D",
            "10:00:05 OTA downloading",
            "10:00:06 NAV gear N",
            "10:00:07 OTA done",
            "10:00:08 NAV gear D",
            "10:00:09 user alice logged in",
            "10:00:10 user bob logged in",
            "10:00:11 state changed from IDLE to RUN",
            "10:00:12 state changed from RUN to IDLE",
            "10:00:13 state changed from IDLE to RUN",
        ]);
        let ps = detect_all(&f);
        assert_eq!(
            templates(&ps),
            [
                "<*> NAV gear {D|P|N}",
                "<*> OTA {downloading|installing|done}",
                "<*> user <*> logged in",
                // Two places that only ever change together: two messages.
                "<*> state changed from IDLE to RUN",
                "<*> state changed from RUN to IDLE",
            ]
        );
        // Each value: its lines, where it first and last shows.
        let gear: Vec<(String, u64, LineNo, LineNo)> = ps.patterns[0]
            .slots()
            .next()
            .unwrap()
            .values
            .iter()
            .map(|v| (v.value(), v.count(), v.first(), v.last()))
            .collect();
        assert_eq!(gear, [("D".into(), 3, 2, 8), ("P".into(), 1, 0, 0), ("N".into(), 1, 6, 6)]);
        let users = ps.patterns[2].slots().next().unwrap();
        assert!(!users.is_state() && users.values.len() == 2);
    }

    #[test]
    fn merges_what_drain_cannot() {
        let f = text(&[
            // Inside a token.
            "gear=D",
            "gear=P",
            "gear=D",
            "gear=N",
            // First.
            "RUNNING state entered",
            "IDLE state entered",
            "RUNNING state entered",
            // Of different lengths.
            "thermal throttling engaged at 91 C",
            "thermal throttling released",
            "thermal throttling engaged at 93 C",
            "thermal throttling released",
            // Not alike enough.
            "connection opened to server",
            "connection refused by peer firewall",
        ]);
        let ps = detect_all(&f);
        assert_eq!(
            templates(&ps),
            [
                "gear={D|P|N}",
                "thermal throttling {engaged at <*> C|released}",
                "{RUNNING|IDLE} state entered",
                "connection opened to server",
                "connection refused by peer firewall",
            ]
        );
        let mut off = PatternOptions::default();
        off.config.settings.merge = false;
        let ps = detect(&f, None, &off, &Progress::new()).unwrap();
        assert_eq!(ps.patterns.len(), 9);
        check_queries(&f, &ps);
    }

    #[test]
    fn slots_that_vary_together_are_two_messages() {
        let states = ["Idle", "Active", "Done", "Idle"];
        let lines = |combos: &[(&str, &str)]| -> Vec<String> {
            let mut v = Vec::new();
            for (k, s) in states.iter().enumerate() {
                for (who, verb) in combos {
                    v.push(format!("{who} the data {verb} read {k}s ago in state {s}"));
                }
            }
            v
        };
        let run = |lines: &[String]| {
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let f = text(&refs);
            let ps = detect_all(&f);
            check_queries(&f, &ps);
            templates(&ps)
        };
        // Only `Warning … was` and `status … is`: two messages.
        let mut t = run(&lines(&[("Warning", "was"), ("status", "is")]));
        t.sort();
        assert_eq!(
            t,
            [
                "Warning the data was read <*> ago in state {Idle|Active|Done}",
                "status the data is read <*> ago in state {Idle|Active|Done}",
            ]
        );
        // Every combination: the slots vary apart, one message.
        let all = [("Warning", "was"), ("status", "is"), ("Warning", "is"), ("status", "was")];
        assert_eq!(run(&lines(&all)), ["{Warning|status} the data {was|is} read <*> ago in state {Idle|Active|Done}"]);
        // A merge kept apart joins in a later pass: `status … has been`
        // (two new slots against `Warning … was`) comes before the lines
        // that show the slots apart.
        let mut v = Vec::new();
        for (who, verb, n) in
            [("Warning", "was", 4), ("status", "has been", 3), ("Warning", "has been", 2), ("status", "was", 1)]
        {
            for k in 0..n {
                v.push(format!("{who} the data {verb} read {k}s ago from the cache"));
            }
        }
        assert_eq!(run(&v), ["{Warning|status} the data {was|has been} read <*> ago from the cache"]);
    }

    #[test]
    fn phrases_are_no_values_and_merges_dont_drift() {
        let f = text(&[
            "frame received id 5 n=1",
            "frame drawn in us 7 n=2",
            "frame received id 6 n=3",
            "frame drawn in us 8 n=4",
            "voltage mV 9 n=5",
            "voltage mV 10 n=6",
            "[INFO ] x done",
            "[ERROR] x done",
            "[INFO ] x done",
        ]);
        let ps = detect_all(&f);
        assert_eq!(
            templates(&ps),
            [
                "[{INFO|ERROR}] x done",
                "frame received id <*> n=<*>",
                "frame drawn in us <*> n=<*>",
                "voltage mV <*> n=<*>"
            ]
        );
    }

    #[test]
    fn values_are_no_room_for_a_phrase() {
        // Three values against a phrase are no state: `LOG: {<*> <*>
        // <*>|Probe stats LOW - MID - TOP - ns: <*> <*> <*>} Elapsed[us]` was.
        let mut v = Vec::new();
        for k in 0..6 {
            v.push(format!("LOG: {k}1 {k}2 {k}3 Elapsed[us]: {k}; #{k}"));
        }
        for k in 0..2 {
            v.push(format!("LOG: Probe stats LOW - MID - TOP - ns: {k}1 {k}2 {k}3 Elapsed[us]: {k}; #{k}"));
        }
        // Nor is a slot that exists (`red`, `green`, `blue`) room for a
        // phrase longer than a state's value (the setting `side`).
        for w in ["red", "green", "blue"] {
            v.push(format!("job {w} finished in 5 ms"));
        }
        v.push("job of the big nightly batch finished in 5 ms".into());
        let refs: Vec<&str> = v.iter().map(String::as_str).collect();
        let f = text(&refs);
        let ps = detect_all(&f);
        check_queries(&f, &ps);
        let mut t = templates(&ps);
        t.sort();
        assert_eq!(
            t,
            [
                "LOG: <*> <*> <*> Elapsed[us]: <*>; #<*>",
                "LOG: Probe stats LOW - MID - TOP - ns: <*> <*> <*> Elapsed[us]: <*>; #<*>",
                "job <*> finished in <*> ms",
                "job of the big nightly batch finished in <*> ms",
            ]
        );
    }

    #[test]
    fn a_shared_header_and_footer_join_nothing() {
        // Short messages between a severity tag and a counter footer: the
        // tag and footer are no sign of one message, in either stage.
        let mut v = Vec::new();
        let msgs = [
            "radar: OK",
            "Clock sync started",
            "lidar: OK",
            "Bridge state: SLEEP",
            "imu: OFFLINE",
            "Vendor ID42",
            "Clock sync done",
            "gnss: OK",
            "BatchCount: 17",
            "Bridge state: NORMAL",
            "hmi: OFFLINE",
            "Config reload requested",
        ];
        for (i, m) in msgs.iter().cycle().take(48).enumerate() {
            v.push(format!("LOG: {m} Elapsed[us]: {}; #{}", 100 + i * 37, i % 10));
        }
        let refs: Vec<&str> = v.iter().map(String::as_str).collect();
        let f = text(&refs);
        let ps = detect_all(&f);
        let mut t = templates(&ps);
        t.sort();
        assert_eq!(
            t,
            [
                "LOG: BatchCount: <*> Elapsed[us]: <*>; #<*>",
                "LOG: Bridge state: {SLEEP|NORMAL} Elapsed[us]: <*>; #<*>",
                "LOG: Clock sync {started|done} Elapsed[us]: <*>; #<*>",
                "LOG: Config reload requested Elapsed[us]: <*>; #<*>",
                "LOG: Vendor <*> Elapsed[us]: <*>; #<*>",
                "LOG: {radar|lidar|imu|gnss|hmi}: {OK|OFFLINE} Elapsed[us]: <*>; #<*>",
            ]
        );
    }

    /// A message split by the shape of a value (host names here) is one
    /// pattern where one's query takes the others' lines: no line is
    /// matched by another pattern's query.
    #[test]
    fn a_pattern_whose_lines_another_takes_joins_it() {
        let hosts = [
            "a1.cdn.example.com",
            "api.example.net",
            "x.y.z.example.org",
            "static.example.com",
            "img7.news.example.cn",
            "mail.example.de",
            "s3.eu.store.example.io",
            "w.example.com",
            "push.mobile.example.com",
            "dl.example.org",
            "t.co",
            "beacon.ads.example.net",
        ];
        let mut v = Vec::new();
        for i in 0..60 {
            v.push(format!("proxy.lab.example.edu:{} open through proxy proxy.lab.example.edu:5070 HTTPS", 5000 + i));
            v.push(format!("{}:{} open through proxy proxy.lab.example.edu:5070 HTTPS", hosts[i % 12], 400 + i));
            if i % 3 == 0 {
                v.push(format!("{}:{} open through proxy socks.lab.example.edu:1080 SOCKS5", hosts[i % 7], 80 + i));
            }
        }
        let refs: Vec<&str> = v.iter().map(String::as_str).collect();
        let f = text(&refs);
        let ps = detect_all(&f);
        let mut owner = vec![usize::MAX; v.len()];
        for (k, p) in ps.patterns.iter().enumerate() {
            for &l in &p.lines[0] {
                owner[l as usize] = k;
            }
        }
        for (k, p) in ps.patterns.iter().enumerate() {
            let q = Query::compile_in(&ps.query(p), f.schema(), &Default::default()).unwrap();
            let res = search(&f, &Filter::Query(q), &Progress::new()).unwrap();
            let others: Vec<&str> =
                res.lines.iter().filter(|&&l| owner[l as usize] != k).map(|&l| refs[l as usize]).collect();
            assert!(others.is_empty(), "{} also takes {others:?}\n{:?}", p.template(), templates(&ps));
        }
    }

    /// Files opened together: the same message in each is one pattern,
    /// and the file is no part of its query or of a rule made from it.
    #[test]
    fn the_file_is_no_context() {
        let part = |name: &str, n: usize| -> Box<dyn LogSource> {
            let lines: Vec<String> = (0..n).map(|i| format!("connected to node{i} in {} ms", 10 + i)).collect();
            Box::new(TextFile::from_bytes(name, lines.join("\n").into_bytes()))
        };
        let both = crate::input::concat::Concat::new(vec![part("a.log", 20), part("b.log", 20)]).unwrap();
        let ps = detect_all(&both);
        assert_eq!(templates(&ps), ["connected to <*> in <*> ms"]);
        assert!(ps.patterns[0].fields.is_empty(), "{:?}", ps.patterns[0].fields);
        assert!(ps.ignore_rule(&ps.patterns[0]).starts_with("*: ignore /"), "{}", ps.ignore_rule(&ps.patterns[0]));
    }

    #[test]
    fn no_lines_no_patterns() {
        let f = text(&[]);
        let ps = detect(&f, None, &PatternOptions { lines: true, ..Default::default() }, &Progress::new()).unwrap();
        assert!(ps.patterns.is_empty());
        let ps = detect(&text(&["a 1", "b 2"]), Some(&[]), &PatternOptions::default(), &Progress::new()).unwrap();
        assert!(ps.patterns.is_empty() && ps.total() == 0);
    }

    #[test]
    fn a_slot_that_may_be_empty() {
        let f = text(&["job done", "job done fast", "job done", "job done fast"]);
        let ps = detect_all(&f);
        assert_eq!(templates(&ps), ["job done {∅|fast}"]);
    }

    #[test]
    fn many_values_are_no_state() {
        let names = "abcdefghijklmnopqrst";
        let lines: Vec<String> = (0..40).map(|i| format!("job {} started", &names[i % 20..i % 20 + 1])).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let f = text(&refs);
        let ps = detect_all(&f);
        assert_eq!(templates(&ps), ["job <*> started"]);
        let s = ps.patterns[0].slots().next().unwrap();
        assert!(s.overflow && s.values.len() == MAX_VALUES && !s.is_state());
    }

    #[test]
    fn regex_specials_are_literal() {
        let f = text(&["call f(1) [ok] $5 a.b*", "call f(2) [ok] $9 a.b*", "x (a)* y", "x [b]$ y", "x (a)* y"]);
        let ps = detect_all(&f);
        assert_eq!(templates(&ps), ["x {(a)*|[b]$} y", "call f(<*>) [ok] $<*> a.b*"]);
    }

    /// Set A and set B: the same voltage readings and doors; B also has
    /// undervoltage alarms and a panic.
    fn two_sets() -> (TextFile, TextFile) {
        let mut a = Vec::new();
        for i in 0..100 {
            a.push(format!("voltage={} mV", 12000 + i));
        }
        for _ in 0..10 {
            a.push("door open".to_owned());
            a.push("door closed".to_owned());
        }
        let mut b = a.clone();
        for i in 0..9 {
            b.insert(10 * i + 5, format!("undervoltage {} mV", 9000 + i));
        }
        for _ in 0..5 {
            b.push("panic in task".to_owned());
        }
        let file = |name: &str, lines: Vec<String>| TextFile::from_bytes(name, lines.join("\n").into_bytes());
        (file("a", a), file("b", b))
    }

    #[test]
    fn compare_ranks_new_patterns_and_values() {
        let (a, b) = two_sets();
        let (sa, sb) = (Set { source: &a, lines: None }, Set { source: &b, lines: None });
        let ps = compare(sa, sb, &PatternOptions::default(), &Progress::new()).unwrap();
        assert_eq!(ps.lines, [120, 134]);
        assert_eq!(templates(&ps), ["{voltage=|undervoltage} <*> mV", "door {open|closed}", "panic in task"]);
        // The alarm, merged into the readings' pattern, still stands out as a
        // value; the panic as a pattern; the doors don't.
        let d = differences(&ps, 3.84);
        let what: Vec<(String, [u64; 2])> = d
            .iter()
            .map(|d| {
                let p = &ps.patterns[d.pattern];
                let name = match d.value {
                    None => p.template(),
                    Some((s, v)) => p.slots().nth(s).unwrap().values[v].value(),
                };
                (name, d.counts)
            })
            .collect();
        assert_eq!(
            what,
            [
                ("undervoltage".to_owned(), [0, 9]),
                ("panic in task".to_owned(), [0, 5]),
                ("voltage=".to_owned(), [100, 100])
            ]
        );
        assert_eq!(d.iter().map(Difference::change).collect::<Vec<_>>(), [Change::New, Change::New, Change::Changed]);
        assert!(d[0].ratio() > 5.0 && d[1].ratio() > 5.0 && d[2].ratio() < 1.0);
        // Where each side's lines are: B's first alarm, A's first reading.
        let p = &ps.patterns[0];
        assert_eq!(p.slots().next().unwrap().values[1].sides[1].first, 5);
        assert_eq!((p.sides[0].first, p.sides[1].count), (0, 109));
    }

    #[test]
    fn compare_two_filters_of_one_file() {
        let f = text(&["x 1", "y on", "x 2", "y off", "x 3", "y on", "y on", "x 4"]);
        let (a, b): (Vec<LineNo>, Vec<LineNo>) = (0..8).partition(|&l| l < 4);
        let ps = compare(
            Set { source: &f, lines: Some(&a) },
            Set { source: &f, lines: Some(&b) },
            &PatternOptions { lines: true, ..Default::default() },
            &Progress::new(),
        )
        .unwrap();
        assert_eq!(templates(&ps), ["x <*>", "y {on|off}"]);
        // Each pattern's lines, per set.
        assert_eq!(ps.patterns[0].lines, [vec![0, 2], vec![4, 7]]);
        assert_eq!(ps.patterns[1].lines, [vec![1, 3], vec![5, 6]]);
        let y = ps.patterns[1].slots().next().unwrap();
        // `on`: line 1 in A, 5 and 6 in B; `off` only in A.
        assert_eq!(y.values[0].sides, [Tally { count: 1, first: 1, last: 1 }, Tally { count: 2, first: 5, last: 6 }]);
        assert_eq!(y.values[1].sides[1].count, 0);
        // Too few lines to be sure of a change; `off` is gone, though.
        let d = differences(&ps, 3.84);
        assert_eq!((d.len(), d[0].change(), d[0].counts), (1, Change::Gone, [1, 0]));
    }

    #[test]
    fn g2_weighs_the_evidence() {
        assert!(g2(0, 10_000, 1, 10_000) < 3.84);
        assert!(g2(0, 10_000, 500, 10_000) > 100.0);
        assert!(g2(50, 1000, 50, 1000) < 1e-9);
    }

    #[test]
    fn long_messages_end_in_the_rest() {
        let long = |w: &str| {
            (0..100).map(|i| format!("{w}{}", "abc"[i % 3..i % 3 + 1].repeat(i % 5 + 1))).collect::<Vec<_>>().join(" ")
        };
        let (a, b) = (long("x"), long("y"));
        let f = text(&[&a, &a, &b]);
        let ps = detect_all(&f);
        assert!(ps.patterns.iter().all(|p| p.template().ends_with(" …")), "{:?}", templates(&ps));
    }

    #[test]
    fn lines_that_hardly_repeat_go_to_a_catch_all() {
        // Words without digits, all different: each line a template of its
        // own, in one group (`k`, four tokens), until the group is full.
        let word = |mut n: usize| {
            let mut w = String::new();
            for _ in 0..4 {
                w.push((b'a' + (n % 26) as u8) as char);
                n /= 26;
            }
            w
        };
        let lines: Vec<String> =
            (0..300).map(|i| format!("k {} {} {}", word(i), word(i * 7 + 1), word(i * 13 + 2))).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let f = text(&refs);
        let ps = detect_all(&f);
        let unsorted: Vec<&Pattern> = ps.patterns.iter().filter(|p| p.unsorted).collect();
        assert_eq!(unsorted.len(), 1);
        assert_eq!((unsorted[0].count(), unsorted[0].template().as_str()), (300 - MAX_GROUP as u64, "<*> <*> <*> <*>"));
        assert_eq!(ps.patterns.len(), MAX_GROUP + 1);
    }

    #[test]
    fn cancel_stops() {
        let f = text(&["a"]);
        let p = Progress::new();
        p.cancel();
        assert!(matches!(detect(&f, None, &PatternOptions::default(), &p), Err(Error::Cancelled)));
    }

    // ---- The configuration's rules ----

    /// The lines of `states_keep_their_values`, as a file.
    fn states() -> TextFile {
        text(&[
            "10:00:00 NAV gear P",
            "10:00:01 OTA downloading",
            "10:00:02 NAV gear D",
            "10:00:03 OTA installing",
            "10:00:04 NAV gear D",
            "10:00:05 OTA downloading",
            "10:00:06 NAV gear N",
            "10:00:07 OTA done",
            "10:00:08 NAV gear D",
            "10:00:09 user alice logged in",
            "10:00:10 user bob logged in",
            "10:00:11 state changed from IDLE to RUN",
            "10:00:12 state changed from RUN to IDLE",
            "10:00:13 state changed from IDLE to RUN",
        ])
    }

    /// Patterns of `source` with the configuration `config` (a `patterns`
    /// file's text), each line kept and the queries checked.
    fn detect_with(source: &dyn LogSource, config: &str) -> Patterns {
        let config = PatternConfig::parse(config).unwrap();
        let options = PatternOptions { lines: true, config, ..Default::default() };
        let ps = detect(source, None, &options, &Progress::new()).unwrap();
        check_queries(source, &ps);
        ps
    }

    #[test]
    fn keep_puts_a_word_in_no_slot() {
        let f = states();
        let ps = detect_with(&f, "*: keep D\n");
        let t = templates(&ps);
        assert!(t.contains(&"<*> NAV gear D".to_owned()) && t.contains(&"<*> NAV gear <*>".to_owned()), "{t:?}");
        // Stage 2 too: `gear D` from stage 1 doesn't join `gear <*>` (P and
        // N, once each: no state).
        assert!(ps.stats.rules >= 1, "{:?}", ps.stats);
    }

    #[test]
    fn apart_keeps_values_out_of_one_slot() {
        let f = states();
        let t = templates(&detect_with(&f, "*: apart downloading done\n"));
        assert!(
            t.contains(&"<*> OTA {downloading|installing}".to_owned()) && t.contains(&"<*> OTA done".to_owned()),
            "{t:?}"
        );
        // Other slots as without the rule.
        assert!(t.contains(&"<*> NAV gear {D|P|N}".to_owned()), "{t:?}");
    }

    #[test]
    fn max_slots_refuses_merges() {
        let f = states();
        let ps = detect_with(&f, "*: max-slots 0\n");
        assert!(ps.patterns.iter().all(|p| p.slots().count() == 0), "{:?}", templates(&ps));
        assert_eq!(ps.patterns.len(), 10, "{:?}", templates(&ps));
        assert!(ps.stats.rules >= 1);
    }

    #[test]
    fn value_and_literal_rules_mask_text() {
        let f = states();
        let ps = detect_with(&f, "*: value /alice|bob/\n");
        let user = ps.patterns.iter().find(|p| p.template() == "<*> user <*> logged in").unwrap();
        // A masked value, not a slot; its query finds any text there.
        assert_eq!(user.slots().count(), 0);
        let f = text(&["ECU1 reset", "ECU2 reset", "ECU1 reset", "ECU1 reset", "ECU2 reset"]);
        assert_eq!(templates(&detect_with(&f, "")), ["<*> reset"]);
        assert_eq!(templates(&detect_with(&f, r"*: literal /ECU\d/")), ["{ECU1|ECU2} reset"]);
        let m = |tok, config: &str| {
            let c = PatternConfig::parse(config).unwrap();
            mask_with(tok, &c.local(&[], &[])).replace(MASK, "<*>")
        };
        assert_eq!(m("id=ECU12,v=17", r"*: literal /ECU\d+/"), "id=ECU12,v=<*>");
        assert_eq!(m("to=db.example.com:5432", r"*: value /[a-z]+(\.[a-z]+)+/"), "to=<*>:<*>");
        assert_eq!(m("peer=10.0.0.1", r"*: value /zzz/"), "peer=<*>");
    }

    #[test]
    fn a_patterns_ignore_rule_leaves_it_out() {
        let row = |text, apid| Row { text, apid: Some(apid), mcnt: None, level: None };
        let s = MemSource::new(vec![
            row("speed 10 km/h", "NAV"),
            row("speed 12 km/h", "NAV"),
            row("door \"a/b\" open", "BODY"),
            row("door \"a/b\" closed", "BODY"),
            row("speed 11 km/h", "SEAT"),
        ]);
        let ps = detect_all(&s);
        assert_eq!(templates(&ps), ["speed <*> km/h", "door \"a/b\" <*>", "speed <*> km/h"]);
        let rule = ps.ignore_rule(&ps.patterns[0]);
        assert_eq!(rule, r#"apid == "NAV": ignore /^\s*speed\s+[\w.:\-]+\s+km\/h\s*$/"#);
        let t = templates(&detect_with(&s, &rule));
        assert_eq!(t, ["door \"a/b\" <*>", "speed <*> km/h"]);
        let t = templates(&detect_with(&s, &ps.ignore_rule(&ps.patterns[1])));
        assert_eq!(t, ["speed <*> km/h", "speed <*> km/h"]);
    }

    #[test]
    fn a_pattern_as_a_library_query_captures_its_states() {
        let f = states();
        let ps = detect_with(&f, "");
        let gear = ps.patterns.iter().find(|p| p.template() == "<*> NAV gear {D|P|N}").unwrap();
        let q = ps.library_query(gear);
        assert_eq!((q.name.as_str(), q.states.as_slice()), ("nav_gear", &["gear".to_owned()][..]));
        assert_eq!(q.nfd, r"raw ~ /^\s*[\w.:\-]+\s+NAV\s+gear\s+(?<gear>(?:D|P|N))\s*$/");
        // Its signal: the gear on each line.
        let mut signals = crate::Signals::default();
        signals.set(crate::SignalDef::parse_spec(&format!("gear(gear):enum={}", q.nfd)).unwrap());
        let query = Query::compile_in("@gear == N", f.schema(), &signals).unwrap();
        let res = search(&f, &Filter::Query(query), &Progress::new()).unwrap();
        assert!(res.lines.contains(&6), "{:?}", res.lines);
        // No state: no group; the name from the first words.
        let user = ps.patterns.iter().find(|p| p.template() == "<*> user <*> logged in").unwrap();
        let q = ps.library_query(user);
        assert_eq!((q.name.as_str(), q.states.len()), ("user_logged_in", 0));
        assert_eq!(q.nfd, ps.query(user));
    }

    /// A context with a few lines in each chunk (too few to tell its
    /// header alone) is sampled on all its chunks' lines together.
    #[test]
    fn rare_contexts_are_sampled_across_chunks() {
        let msgs =
            ["LOG: pump on Elapsed[us]: 5; #1", "LOG: fan Elapsed[us]: 7; #2", "LOG: door open now Elapsed[us]: 9; #3"];
        let config = PatternConfig::default();
        let tasks: Vec<(usize, usize, usize)> = (0..4).map(|c| (0, c * 100, c * 100 + 100)).collect();
        let mut parts: Vec<Chunk> = (0..4).map(|_| (Drain::new(0, MAX_PER_CHUNK), Vec::new(), 0)).collect();
        let rests: Vec<Rest> = tasks
            .iter()
            .map(|&(_, from, _)| {
                let mut s = Sample::default();
                for (k, m) in msgs.iter().enumerate() {
                    let toks: Vec<Cow<str>> = m.split_ascii_whitespace().map(mask).collect();
                    s.count(&toks, 3);
                    s.held.push((from + k, (from + k) as LineNo, toks.iter().map(|t| t.to_string()).collect(), vec![]));
                }
                vec![(7, s)]
            })
            .collect();
        // Three lines a chunk: no header there.
        assert!(rests[0][0].1.boilerplate(&config.local(&[], &[])).is_empty());
        let (d, lines) = pool(&mut parts, rests, &tasks, &config, &[], 0).expect("pooled");
        let mut boiler = d.boiler[&7].clone();
        boiler.sort();
        assert_eq!(boiler, ["Elapsed[us]:".into(), "LOG:".into()] as [Box<str>; 2]);
        assert_eq!(lines.len(), 12);
        assert_eq!(lines[3], (1, 0, lines[0].2), "chunk 1's first line: the same message as chunk 0's");
        assert!(parts.iter().all(|(d, _, _)| d.clusters.is_empty()));
    }

    #[test]
    fn explain_says_why_apart() {
        let f = states();
        let ps = detect_with(&f, "");
        let find = |ps: &Patterns, t: &str| ps.patterns.iter().find(|p| p.template() == t).cloned().unwrap();
        let (a, b) = (find(&ps, "<*> state changed from IDLE to RUN"), find(&ps, "<*> state changed from RUN to IDLE"));
        let why = explain(&a, &b, &PatternConfig::default());
        assert!(why.last().unwrap().contains("2 new slots at once, more than new-slots 1"), "{why:?}");
        assert_eq!(why[0], "they differ: “IDLE to” / “∅” (2 and 0 tokens, a new slot)");
        let config = PatternConfig::parse("new-slots 2\n").unwrap();
        assert!(explain(&a, &b, &config).last().unwrap().starts_with("one pattern"));
        // A rule.
        let kept = detect_with(&f, "*: keep D\n");
        let (d, pn) = (find(&kept, "<*> NAV gear D"), find(&kept, "<*> NAV gear <*>"));
        let config = PatternConfig::parse("*: keep D\n").unwrap();
        let why = explain(&d, &pn, &config);
        assert!(why.iter().any(|w| w == "rule keep D: the word would be in a slot"), "{why:?}");
        assert_eq!(why.last().unwrap(), "apart: a rule refuses");
        // Fields.
        let row = |text, apid| Row { text, apid: Some(apid), mcnt: None, level: None };
        let s = MemSource::new(vec![row("door open", "BODY"), row("door open", "SEAT")]);
        let ps = detect_with(&s, "");
        let why = explain(&ps.patterns[0], &ps.patterns[1], &PatternConfig::default());
        assert_eq!(why, ["apart: their apid differs (BODY / SEAT); patterns never join across fields"]);
    }

    #[test]
    fn ignore_leaves_lines_out() {
        let f = states();
        let ps = detect_with(&f, "*: ignore /OTA/\n");
        assert!(templates(&ps).iter().all(|t| !t.contains("OTA")));
        assert_eq!((ps.lines[0], ps.stats.ignored[0]), (10, 4));
        let ps = detect_with(&f, "*: ignore\n");
        assert!(ps.patterns.is_empty() && ps.lines[0] == 0);
    }

    #[test]
    fn no_merge_and_settings() {
        let f = states();
        let off = templates(&detect_with(&f, "merge off\n"));
        assert_eq!(templates(&detect_with(&f, "*: no-merge\n")), off);
        // Two words may become wildcards at once: `state changed from …`
        // joins in stage 1.
        let t = templates(&detect_with(&f, "new-wildcards 2\nmerge off\n"));
        assert!(t.contains(&"<*> state changed from {IDLE|RUN} to {RUN|IDLE}".to_owned()), "{t:?}");
        // And in stage 2, two new slots at once.
        let t = templates(&detect_with(&f, "new-slots 2\n"));
        assert_eq!(t.iter().filter(|t| t.contains("state changed")).count(), 1, "{t:?}");
        // Gear's three values: no state with two at most.
        let ps = detect_with(&f, "state-values 2\n");
        let gear = ps.patterns.iter().find(|p| p.template().contains("gear")).unwrap();
        assert_eq!(gear.template(), "<*> NAV gear <*>");
        assert!(gear.slots().next().unwrap().overflow);
    }

    #[test]
    fn rules_apply_to_their_scope() {
        let row = |text, apid| Row { text, apid: Some(apid), mcnt: None, level: None };
        let mut rows = Vec::new();
        for apid in ["NAV", "BODY"] {
            for v in ["D", "P", "D", "N"] {
                rows.push(row(format!("gear {v}").leak(), apid));
            }
        }
        let s = MemSource::new(rows);
        let t = templates(&detect_with(&s, "apid == NAV: keep D\n"));
        assert_eq!(t, ["gear {D|P|N}", "gear D", "gear <*>"], "{t:?}");
        let t = templates(&detect_with(&s, "apid == B*: max-slots 0\n"));
        assert_eq!(t, ["gear {D|P|N}", "gear D", "gear P", "gear N"], "{t:?}");
    }

    #[test]
    fn header_words_are_passed_over() {
        let c = PatternConfig::parse("*: header LOG:\n").unwrap();
        let local = c.local(&[], &[]);
        let mut d = Drain::new(0, MAX_PER_CHUNK);
        let mut s = Sample::default();
        let toks: Vec<Cow<str>> = ["LOG:", "x"].map(Cow::Borrowed).to_vec();
        s.count(&toks, 3);
        s.held.push((0, 0, vec!["LOG:".into(), "x".into()], vec![]));
        flush(&mut d, 7, &s, &local, 0, 0, &mut []);
        assert_eq!(d.boiler[&7], vec![Box::from("LOG:")]);
    }
}

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
//!    token (a token that is only a masked value doesn't count); one at
//!    least [`PatternOptions::similarity`] like a template joins it, its
//!    differing tokens becoming slots, else it starts one. The chunks'
//!    templates are merged the same way.
//! 2. **Templates → patterns** (on templates, not lines, so the cost doesn't
//!    grow with the file): templates are split finer, into atoms by
//!    character class (a run of letters, digits and `_`; any other
//!    character; a masked value), and aligned pairwise by longest common
//!    subsequence (as Spell does with tokens), so a varying part can sit
//!    anywhere, inside a token (`gear=D`, `gear=P`), first (`RUNNING state
//!    entered`), or span different lengths (`throttling engaged at <*> C`,
//!    `throttling released`). Two templates join when they share enough
//!    words ([`accept`]) and no field (`apid`) tells them apart; the parts
//!    where they differ become a slot holding both sides' values.
//!
//! Finally, what all of a slot's values begin or end with moves out of the
//! slot (`{gear=D|gear=P}` → `gear={D|P}`).
//!
//! The message is a field's text (DLT: `payload`) or the line. Cheap text
//! fields (DLT `ecu`, `apid`, `ctid`) with one value on every line of a
//! pattern, where patterns differ in it, go into its query too.

use crate::event::{FieldId, FieldKind, FieldValue};
use crate::input::LogSource;
use crate::{Error, LineNo, Progress, Result};
use rayon::prelude::*;
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

// Markers inside texts (private-use characters, so text can't hold them by
// accident): a masked value, a whole slot, white space or none.
const MASK: char = '\u{E000}';
const MASK_STR: &str = "\u{E000}";
const ANY: char = '\u{E001}';
const EITHER: char = '\u{E002}';
/// Lines grouped per task.
const CHUNK: usize = 64 * 1024;
/// Values a slot keeps.
pub const MAX_VALUES: usize = 16;
/// Templates of more atoms aren't aligned (stage 2).
const MAX_ATOMS: usize = 256;
/// Patterns a template is aligned with, at most (stage 2).
const CANDIDATES: usize = 64;

#[derive(Debug, Clone)]
pub struct PatternOptions {
    /// The field whose text is grouped; `None`: `payload` if the source has
    /// it, else the line (`raw`).
    pub field: Option<String>,
    /// How alike (0–1: the share of a template's literal tokens) a message
    /// must be to join a template (stage 1).
    pub similarity: f32,
    /// Merge templates by alignment (stage 2).
    pub merge: bool,
}

impl Default for PatternOptions {
    fn default() -> Self {
        Self { field: None, similarity: 0.5, merge: true }
    }
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
    pub count: u64,
    pub first: LineNo,
    pub last: LineNo,
}

impl SlotValue {
    /// The value: masked parts as `<*>`, empty as `∅`.
    pub fn value(&self) -> String {
        if self.text.is_empty() {
            return "∅".into();
        }
        self.text.replace([MASK, ANY], "<*>").replace(EITHER, " ")
    }
}

impl Slot {
    fn add(&mut self, text: &str, count: u64, first: LineNo, last: LineNo) {
        if let Some(v) = self.values.iter_mut().find(|v| v.text == text) {
            v.count += count;
            v.first = v.first.min(first);
            v.last = v.last.max(last);
        } else if self.values.len() < MAX_VALUES {
            self.values.push(SlotValue { text: text.to_owned(), count, first, last });
        } else {
            self.overflow = true;
        }
    }

    fn join(&mut self, other: Slot) {
        for v in other.values {
            self.add(&v.text, v.count, v.first, v.last);
        }
        self.overflow |= other.overflow;
        self.wide |= other.wide;
    }

    /// A state: all its values known, at least two, one of them on more than
    /// one line, and each with a letter (not only a masked number) or empty.
    pub fn is_state(&self) -> bool {
        let lines: u64 = self.values.iter().map(|v| v.count).sum();
        !self.overflow
            && self.values.len() >= 2
            && lines > self.values.len() as u64
            && self.values.iter().all(|v| v.text.is_empty() || v.text.chars().any(char::is_alphabetic))
    }

    fn regex(&self, re: &mut String) {
        if self.is_state() {
            re.push_str("(?:");
            for (j, v) in self.values.iter().enumerate() {
                if j > 0 {
                    re.push('|');
                }
                text_regex(&v.text, re);
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

/// One pattern: a message template and where it occurs.
#[derive(Debug, Clone)]
pub struct Pattern {
    parts: Vec<Part>,
    /// Lines in the pattern.
    pub count: u64,
    pub first: LineNo,
    pub last: LineNo,
    /// Fields with one value on all its lines (and not on all patterns'):
    /// `(name, value)`.
    pub fields: Vec<(String, String)>,
}

impl Pattern {
    /// The template: `<*>` where messages differ, a state's values as
    /// `{D|P|N}`.
    pub fn template(&self) -> String {
        let mut s = String::new();
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
                s.push(' ');
            }
            match &p.atom {
                Atom::Lit(l) => s.push_str(&l.replace(MASK, "<*>")),
                Atom::Slot(slot) if slot.is_state() => {
                    s.push('{');
                    for (j, v) in slot.values.iter().enumerate() {
                        if j > 0 {
                            s.push('|');
                        }
                        s.push_str(&v.value());
                    }
                    s.push('}');
                }
                Atom::Slot(_) => s.push_str("<*>"),
            }
        }
        s
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
        let mut re = String::from(r"^\s*");
        for (i, p) in self.parts.iter().enumerate() {
            if i > 0 {
                re.push_str(sep_regex(p.sep));
            }
            match &p.atom {
                Atom::Lit(l) => text_regex(l, &mut re),
                Atom::Slot(s) => s.regex(&mut re),
            }
        }
        re.push_str(r"\s*$");
        re
    }
}

fn sep_regex(sep: Sep) -> &'static str {
    match sep {
        Sep::Space => r"\s+",
        Sep::Tight => "",
        Sep::Either => r"\s*",
    }
}

/// A regex for a text with markers.
fn text_regex(text: &str, re: &mut String) {
    let mut buf = [0u8; 4];
    for c in text.chars() {
        match c {
            // What `mask` replaces: digit-holding word runs and the `.`,
            // `:`, `-` between them.
            MASK => re.push_str(r"[\w.:\-]+"),
            ANY => re.push_str(".*?"),
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
    /// Lines looked at.
    pub lines: u64,
}

impl Patterns {
    /// NFD finding the lines of `p`, e.g. `apid == "NAV" AND payload ~ /^…$/`.
    pub fn query(&self, p: &Pattern) -> String {
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
        let re = p.regex().replace('/', r"\x2f");
        q.push_str(&re);
        q.push('/');
        q
    }
}

/// Group the lines of `source` (or only `lines`, ascending) into patterns.
pub fn detect(
    source: &dyn LogSource,
    lines: Option<&[LineNo]>,
    options: &PatternOptions,
    progress: &Progress,
) -> Result<Patterns> {
    let schema = source.schema();
    let field = match &options.field {
        Some(f) if f.eq_ignore_ascii_case("raw") => None,
        Some(f) => Some(
            schema
                .iter()
                .position(|d| d.name.eq_ignore_ascii_case(f))
                .ok_or_else(|| Error::Format(format!("no field {f} in {}", source.name())))?,
        ),
        None => schema.iter().position(|d| d.name == "payload" && d.kind == FieldKind::Str),
    };
    // Context: the cheap text fields.
    let context: Vec<FieldId> = (0..schema.len())
        .filter(|&i| {
            Some(i) != field && !schema[i].costly && matches!(schema[i].kind, FieldKind::Str | FieldKind::Name)
        })
        .collect();
    let total = lines.map_or(source.line_count(), |l| l.len() as u64);
    progress.set_total(total);
    let chunks = total.div_ceil(CHUNK as u64) as usize;
    let sim = options.similarity.clamp(0.0, 1.0);
    // Stage 1.
    let parts: Vec<Drain> = (0..chunks)
        .into_par_iter()
        .map(|c| {
            if progress.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let from = c * CHUNK;
            let to = ((c + 1) * CHUNK).min(total as usize);
            let mut d = Drain::new(sim, context.len());
            let mut vals = Vec::with_capacity(context.len());
            for i in from..to {
                let line = lines.map_or(i as LineNo, |l| l[i]);
                let text = match field {
                    Some(id) => match source.field(line, id) {
                        Some(FieldValue::Str(b)) => match b {
                            Cow::Borrowed(b) => String::from_utf8_lossy(b),
                            Cow::Owned(b) => Cow::Owned(String::from_utf8_lossy(&b).into_owned()),
                        },
                        _ => Cow::Borrowed(""),
                    },
                    None => source.line_text(line).unwrap_or_default(),
                };
                vals.clear();
                vals.extend(context.iter().map(|&id| match source.field(line, id) {
                    Some(FieldValue::Str(b)) => Some(b),
                    _ => None,
                }));
                let toks: Vec<Cow<str>> = text.split_ascii_whitespace().map(mask).collect();
                d.add_line(&toks, line, &vals);
            }
            progress.add((to - from) as u64);
            Ok(d)
        })
        .collect::<Result<_>>()?;
    let mut parts = parts.into_iter();
    let mut all = parts.next().unwrap_or_else(|| Drain::new(sim, context.len()));
    for d in parts {
        for c in d.clusters {
            all.add_cluster(c);
        }
    }
    let mut families: Vec<Family> = all.clusters.into_iter().map(Family::from).collect();
    // Stage 2.
    if options.merge {
        families = merge_all(families, progress)?;
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
        .map(|f| Pattern {
            fields: context
                .iter()
                .zip(&f.vals)
                .zip(&telling)
                .filter_map(|((&id, v), &t)| match v {
                    Val::One(Some(b)) if t => {
                        Some((schema[id].name.to_string(), String::from_utf8_lossy(b).into_owned()))
                    }
                    _ => None,
                })
                .collect(),
            parts: factor(f.parts),
            count: f.count,
            first: f.first,
            last: f.last,
        })
        .collect();
    patterns.sort_by(|a, b| b.count.cmp(&a.count).then(a.first.cmp(&b.first)));
    Ok(Patterns { field: field.map(|id| schema[id].name.to_string()), patterns, lines: total })
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
#[derive(Debug, Clone, PartialEq, Eq)]
enum Val {
    One(Option<Box<[u8]>>),
    Mixed,
}

impl Val {
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
    count: u64,
    first: LineNo,
    last: LineNo,
    vals: Vec<Val>,
}

/// Templates of some lines: clusters, found by the fields' values, token
/// count and first literal token (`key`).
struct Drain {
    sim: f32,
    groups: HashMap<u64, Vec<usize>>,
    clusters: Vec<Cluster>,
    fields: usize,
}

/// The group of a message of tokens `toks` on a line with field values
/// `vals`.
fn key<'a>(toks: impl ExactSizeIterator<Item = &'a str>, vals: &[Option<Cow<[u8]>>]) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    vals.hash(&mut h);
    toks.len().hash(&mut h);
    toks.into_iter().find(|&t| t != MASK_STR).hash(&mut h);
    h.finish()
}

/// How alike template `a` and the message or template `b` (`None`: any)
/// are: the share of equal tokens where neither is a wildcard nor only a
/// masked value (1 if there are none).
fn similarity<'a>(a: &[Tok], b: impl Iterator<Item = Option<&'a str>>) -> f32 {
    let (mut same, mut n) = (0u32, 0u32);
    for (x, y) in a.iter().zip(b) {
        if let (Tok::Lit(x), Some(y)) = (x, y)
            && &**x != MASK_STR
        {
            n += 1;
            same += u32::from(&**x == y);
        }
    }
    if n == 0 { 1.0 } else { same as f32 / n as f32 }
}

impl Drain {
    fn new(sim: f32, fields: usize) -> Self {
        Self { sim, groups: HashMap::new(), clusters: Vec::new(), fields }
    }

    /// The most alike template in group `key` with `len` tokens (`score`:
    /// how alike), if alike enough.
    fn best(&self, key: u64, len: usize, score: impl Fn(&[Tok]) -> f32) -> Option<usize> {
        let mut best = None;
        for &ci in self.groups.get(&key).into_iter().flatten() {
            let c = &self.clusters[ci];
            if c.toks.len() != len {
                continue;
            }
            let s = score(&c.toks);
            if s >= self.sim && best.is_none_or(|(_, bs)| s > bs) {
                best = Some((ci, s));
            }
        }
        best.map(|(ci, _)| ci)
    }

    fn add_line(&mut self, toks: &[Cow<str>], line: LineNo, vals: &[Option<Cow<[u8]>>]) {
        debug_assert_eq!(vals.len(), self.fields);
        let k = key(toks.iter().map(|t| &**t), vals);
        match self.best(k, toks.len(), |c| similarity(c, toks.iter().map(|t| Some(&**t)))) {
            Some(ci) => {
                let c = &mut self.clusters[ci];
                for (i, y) in toks.iter().enumerate() {
                    match &c.toks[i] {
                        Tok::Lit(l) if **l == **y => {}
                        Tok::Lit(l) => {
                            // The lines so far all had `l` there.
                            let mut s = Slot::default();
                            s.add(l, c.count, c.first, c.last);
                            s.add(y, 1, line, line);
                            c.slots[i] = s;
                            c.toks[i] = Tok::Any;
                        }
                        Tok::Any => c.slots[i].add(y, 1, line, line),
                    }
                }
                c.count += 1;
                c.last = line;
                for (v, n) in c.vals.iter_mut().zip(vals) {
                    if !matches!(v, Val::One(o) if o.as_deref() == n.as_deref()) {
                        *v = Val::Mixed;
                    }
                }
            }
            None => {
                self.groups.entry(k).or_default().push(self.clusters.len());
                self.clusters.push(Cluster {
                    key: k,
                    toks: toks.iter().map(|t| Tok::Lit(t.as_ref().into())).collect(),
                    slots: vec![Slot::default(); toks.len()],
                    count: 1,
                    first: line,
                    last: line,
                    vals: vals.iter().map(|v| Val::One(v.as_deref().map(Into::into))).collect(),
                });
            }
        }
    }

    /// Merge a template found among other lines.
    fn add_cluster(&mut self, mut n: Cluster) {
        fn lit(t: &Tok) -> Option<&str> {
            match t {
                Tok::Lit(l) => Some(&**l),
                Tok::Any => None,
            }
        }
        match self.best(n.key, n.toks.len(), |c| similarity(c, n.toks.iter().map(lit))) {
            Some(ci) => {
                let c = &mut self.clusters[ci];
                for i in 0..c.toks.len() {
                    if c.toks[i] == n.toks[i] && c.toks[i] != Tok::Any {
                        continue;
                    }
                    if let Tok::Lit(l) = &c.toks[i] {
                        let mut s = Slot::default();
                        s.add(l, c.count, c.first, c.last);
                        c.slots[i] = s;
                        c.toks[i] = Tok::Any;
                    }
                    match &n.toks[i] {
                        Tok::Lit(l) => c.slots[i].add(l, n.count, n.first, n.last),
                        Tok::Any => c.slots[i].join(std::mem::take(&mut n.slots[i])),
                    }
                }
                c.count += n.count;
                c.first = c.first.min(n.first);
                c.last = c.last.max(n.last);
                for (v, w) in c.vals.iter_mut().zip(&n.vals) {
                    v.join(w);
                }
            }
            None => {
                self.groups.entry(n.key).or_default().push(self.clusters.len());
                self.clusters.push(n);
            }
        }
    }
}

// ---- Stage 2: templates merged by alignment ----

/// A template in atoms, and its lines.
struct Family {
    parts: Vec<Part>,
    /// The words of its wordiest template: merging keeps only the words in
    /// common, but how alike another is counts against these (else each
    /// merge would make the next easier).
    size: usize,
    count: u64,
    first: LineNo,
    last: LineNo,
    vals: Vec<Val>,
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
        let size = parts.iter().filter(|p| matches!(&p.atom, Atom::Lit(l) if is_word(l))).count();
        Family { parts, size, count: c.count, first: c.first, last: c.last, vals: c.vals }
    }
}

impl Family {
    fn words(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|p| match &p.atom {
            Atom::Lit(l) if is_word(l) => Some(&**l),
            _ => None,
        })
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
/// words counted as [`Family::size`]). They share a word at least, at each
/// place they differ one side has one word at most (a state's value is a
/// word: `{D|P}`, `{engaged at <*> C|released}`, not two phrases), and:
/// - they differ in one place (one slot): the words in common are half the
///   words of the one with fewer at least, and a third of the other's;
/// - in several: the words in common are 60 % of either's at least.
fn accept(a: &Family, b: &Family, pairs: &[(usize, usize)]) -> Option<f32> {
    let common = pairs.iter().filter(|&&(i, _)| matches!(&a.parts[i].atom, Atom::Lit(l) if is_word(l))).count();
    let (few, many) = (a.size.min(b.size), a.size.max(b.size));
    let words = |seg: &[Part]| seg.iter().filter(|p| matches!(&p.atom, Atom::Lit(l) if is_word(l))).count();
    let mut gaps = 0;
    let (mut i, mut j) = (0, 0);
    for &(pi, pj) in pairs.iter().chain(std::iter::once(&(a.parts.len(), b.parts.len()))) {
        if i < pi || j < pj {
            gaps += 1;
            if words(&a.parts[i..pi]).min(words(&b.parts[j..pj])) > 1 {
                return None;
            }
        }
        (i, j) = (pi + 1, pj + 1);
    }
    let ok = common > 0
        && match gaps {
            0 => true,
            1 => common * 2 >= few && common * 3 >= many,
            _ => common * 5 >= many * 3,
        };
    ok.then(|| common as f32 / many.max(1) as f32)
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
        slot.add(&text, f.count, f.first, f.last);
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
    Family {
        parts,
        size: a.size.max(b.size),
        count: a.count + b.count,
        first: a.first.min(b.first),
        last: a.last.max(b.last),
        vals,
    }
}

/// Merge templates into patterns, most lines first: each is aligned with the
/// patterns sharing its rarer words and joins the most alike one that
/// [`accept`]s it.
fn merge_all(mut templates: Vec<Family>, progress: &Progress) -> Result<Vec<Family>> {
    templates.sort_by(|a, b| b.count.cmp(&a.count).then(a.first.cmp(&b.first)));
    let mut out: Vec<Family> = Vec::new();
    // Word → the patterns holding it.
    let mut index: HashMap<Box<str>, Vec<usize>> = HashMap::new();
    for (n, t) in templates.into_iter().enumerate() {
        if n % 1024 == 0 && progress.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut words: Vec<&str> = t.words().collect();
        words.sort_unstable();
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
        let mut best: Option<(usize, f32, Pairs)> = None;
        if t.parts.len() <= MAX_ATOMS {
            for c in candidates {
                let f = &out[c];
                if f.parts.len() > MAX_ATOMS || !f.compatible(&t) {
                    continue;
                }
                let pairs = align(&f.parts, &t.parts);
                if let Some(s) = accept(f, &t, &pairs)
                    && best.as_ref().is_none_or(|b| s > b.1)
                {
                    best = Some((c, s, pairs));
                }
            }
        }
        let words: Vec<Box<str>> = words.into_iter().map(Into::into).collect();
        let at = match best {
            Some((c, _, pairs)) => {
                let f = std::mem::replace(
                    &mut out[c],
                    Family { parts: vec![], size: 0, count: 0, first: 0, last: 0, vals: vec![] },
                );
                out[c] = merge(f, &t, &pairs);
                c
            }
            None => {
                out.push(t);
                out.len() - 1
            }
        };
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
        slot.values.sort_by(|a, b| b.count.cmp(&a.count).then(a.first.cmp(&b.first)));
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
        let ps = detect(source, None, &PatternOptions::default(), &Progress::new()).unwrap();
        check_queries(source, &ps);
        ps
    }

    /// Every pattern's query finds at least its own lines.
    fn check_queries(source: &dyn LogSource, ps: &Patterns) {
        let mut covered = 0;
        for p in &ps.patterns {
            let q = Query::compile_in(&ps.query(p), source.schema(), &Default::default()).unwrap();
            let res = search(source, &Filter::Query(q), &Progress::new()).unwrap();
            assert!(res.lines.len() as u64 >= p.count, "{}: {} < {}", ps.query(p), res.lines.len(), p.count);
            assert!(res.contains(p.first) && res.contains(p.last), "{}", ps.query(p));
            covered += p.count;
        }
        assert_eq!(covered, ps.lines);
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
        let t: Vec<(u64, String)> = ps.patterns.iter().map(|p| (p.count, p.template())).collect();
        assert_eq!(
            t,
            [
                (3, "<*> <*> [INFO] user <*> logged in".to_owned()),
                (2, "<*> <*> [INFO] net connected peer=<*> in <*> ms".to_owned()),
                (2, "<*> <*> [WARN] disk almost full: <*>%".to_owned()),
            ]
        );
        assert_eq!((ps.patterns[0].first, ps.patterns[0].last), (3, 6));
    }

    #[test]
    fn only_given_lines() {
        let f = text(&["a 1", "b x y", "a 2", "b x z"]);
        let ps = detect(&f, Some(&[1, 3]), &PatternOptions::default(), &Progress::new()).unwrap();
        assert_eq!(ps.lines, 2);
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
        assert_eq!(ps.patterns[0].first, 0);
        assert_eq!(ps.patterns.iter().map(|p| p.count).sum::<u64>(), lines.len() as u64);
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
                "<*> state changed from {IDLE|RUN} to {RUN|IDLE}",
                "<*> user <*> logged in",
            ]
        );
        // Each value: its lines, where it first and last shows.
        let gear: Vec<(String, u64, LineNo, LineNo)> = ps.patterns[0]
            .slots()
            .next()
            .unwrap()
            .values
            .iter()
            .map(|v| (v.value(), v.count, v.first, v.last))
            .collect();
        assert_eq!(gear, [("D".into(), 3, 2, 8), ("P".into(), 1, 0, 0), ("N".into(), 1, 6, 6)]);
        let users = ps.patterns[3].slots().next().unwrap();
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
        let off = PatternOptions { merge: false, ..Default::default() };
        let ps = detect(&f, None, &off, &Progress::new()).unwrap();
        assert_eq!(ps.patterns.len(), 9);
        check_queries(&f, &ps);
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

    #[test]
    fn cancel_stops() {
        let f = text(&["a"]);
        let p = Progress::new();
        p.cancel();
        assert!(matches!(detect(&f, None, &PatternOptions::default(), &p), Err(Error::Cancelled)));
    }
}

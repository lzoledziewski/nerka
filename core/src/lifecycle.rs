//! Lifecycles: the boots (runs, sessions) of one entity a log is about,
//! when each starts and how it ends (`docs/dev/lifecycles.md`).
//!
//! A [`LifecycleDef`] is about one entity (owner, 2026-11-07: one ECU, one
//! application, each with rules of its own): **its lines** (a query, e.g.
//! `ecu == ECU1`; every line without one), what starts a lifecycle among
//! them (a line matching a query, its uptime going back, a gap in its
//! lines) and what says how one ended (queries). It is built from generic
//! pieces only. [`LifecycleDef::lifecycles`] finds them in one pass over
//! its lines ([`Lifecycles`]), with findings where the definition and the
//! uptime disagree.
//!
//! A lifecycle lasts from its first line to the next one's first line: a
//! line of any entity in between is "in" it (for signals that reset with
//! it, `per(NAME)`).
//!
//! Spec text (CLI `--lifecycle`, the saved signals file after `lifecycle `):
//!
//! ```text
//! spec   := NAME ":" clause ( ";" clause )*
//! clause := "lines" RULE                       -- its lines (default: all)
//!         | "start" RULE                       -- one of them starts one
//!         | "reset" VALUE [ "tolerance" DUR ]  -- the value goes back
//!         | "gap" DUR                          -- its lines stop for longer
//!         | "planned" RULE | "crash" RULE       -- how one ended
//!         | "late" DUR                         -- "late start" above this uptime
//! RULE   := NFD
//!         | "{" PACK.QUERY ( NAME "=" VALUE )* "}"   -- a library query, its parameters
//! VALUE  := "uptime"                           -- the uptime its lines carry
//!         | "@" SIGNAL                         -- a numeric signal of the tab
//! DUR    := NUMBER [ "ms" | "s" | "m" | "h" ]   -- default seconds
//! ```
//!
//! e.g. `ecu1: lines ecu == ECU1; reset uptime; planned {sys.shutdown}`.
//! A library query is kept by its name (owner, 2026-11-07: reuse queries),
//! so a fix in its pack reaches every lifecycle that uses it;
//! [`LifecycleDef::resolved`] expands it with the library.
//! At least one of `start`, `reset` and `gap` is needed. `uptime` is what
//! the source records of the line's writer ([`LogSource::uptime`], DLT's
//! `tmsp`): with `lines` naming one writer it is that writer's.

use crate::dsl::{DslError, Query};
use crate::engine::search::{Filter, scan};
use crate::pack::{Library, ParamValue, Params, Pick, Selection};
use crate::signal::{SignalDef, SignalKind, Signals, ValueRef, is_name, source_key};
use crate::{LineNo, LogSource, Progress};
use rayon::prelude::*;
use std::sync::{Arc, Mutex};

/// Tolerance of `reset` (and of the missed-reset check) when not given:
/// lines of different writers interleave, so a small step back isn't one.
pub const DEFAULT_TOLERANCE: f64 = 5.0;
/// A lifecycle whose first line has an uptime above this started late.
pub const DEFAULT_LATE: f64 = 10.0;

/// Lines between checks for cancellation.
const CHUNK: usize = 1 << 16;

/// What going back starts a lifecycle.
#[derive(Debug, Clone, PartialEq)]
pub enum ResetValue {
    /// The uptime the lines carry ([`LogSource::uptime`]).
    Uptime,
    /// A numeric signal of the tab (lower case, without `@`).
    Signal(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reset {
    pub value: ResetValue,
    pub tolerance: f64,
}

/// Per source: the lifecycles found, or why they couldn't be (kept, so a
/// UI can say so instead of waiting).
type Cache = Arc<Mutex<Vec<((usize, LineNo), Result<Arc<Lifecycles>, String>)>>>;

/// A query clause: NFD, or a library query by name with its parameters
/// (expanded by [`LifecycleDef::resolved`]).
#[derive(Debug, Clone)]
pub enum Rule {
    Nfd(Query),
    Library {
        /// `pack.query`.
        id: String,
        params: Vec<(String, String)>,
        /// Its expansion, once resolved.
        query: Option<Query>,
    },
}

impl Rule {
    /// Read a rule; error positions are byte offsets in `text`.
    pub fn parse(text: &str) -> Result<Rule, DslError> {
        let t = text.trim();
        let lead = text.len() - text.trim_start().len();
        let Some(inner) = t.strip_prefix('{') else {
            return Query::compile(t).map(Rule::Nfd).map_err(|e| DslError { pos: e.pos + lead, ..e });
        };
        let Some(inner) = inner.strip_suffix('}') else {
            return Err(DslError { msg: "expected '}' after the library query".into(), pos: lead + t.len() });
        };
        let body = inner.trim();
        let body_at = lead + 1 + (inner.len() - inner.trim_start().len());
        // Its name may hold spaces (owner, 2026-11-07: `{Aurix_logs.Fusa
        // Fault}`), kept as written (two spaces too): it runs up to the
        // first `NAME=VALUE` word. A name holding such a word, or `{` `}`,
        // is quoted: `{"pack.a=b" x=1}`.
        let (id, rest, rest_at) = if let Some(q) = body.strip_prefix('"') {
            let Some(end) = q.find('"') else {
                return Err(DslError { msg: "expected '\"' after the query's name".into(), pos: body_at });
            };
            (q[..end].to_owned(), &q[end + 1..], body_at + end + 2)
        } else {
            let mut cut = body.len();
            for (at, w) in words_at(body) {
                if is_param(w) {
                    cut = at;
                    break;
                }
            }
            (body[..cut].trim_end().to_owned(), &body[cut..], body_at + cut)
        };
        if !id.contains('.') {
            return Err(DslError {
                msg: format!("expected a library query (pack.query) but found '{id}'"),
                pos: lead + 1,
            });
        }
        let mut params = Vec::new();
        for (at, w) in words_at(rest) {
            let Some((k, v)) = w.split_once('=').filter(|_| is_param(w)) else {
                return Err(DslError { msg: format!("expected NAME=VALUE but found '{w}'"), pos: rest_at + at });
            };
            params.push((k.to_owned(), v.to_owned()));
        }
        Ok(Rule::Library { id, params, query: None })
    }

    /// A library query by name.
    pub fn library(id: &str, params: Vec<(String, String)>) -> Rule {
        Rule::Library { id: id.to_owned(), params, query: None }
    }

    /// As written: NFD, or `{pack.query name=value}`.
    pub fn text(&self) -> String {
        match self {
            Rule::Nfd(q) => q.source().to_owned(),
            Rule::Library { id, params, .. } => {
                // Quoted where it wouldn't read back as written.
                let plain = !id.is_empty()
                    && id.trim() == id
                    && !id.starts_with('"')
                    && !id.contains(['{', '}'])
                    && !words_at(id).any(|(_, w)| is_param(w));
                let mut s = if plain { format!("{{{id}") } else { format!("{{\"{id}\"") };
                for (k, v) in params {
                    s += &format!(" {k}={v}");
                }
                s + "}"
            }
        }
    }

    /// The query it stands for: `None` for a library query not resolved.
    pub fn query(&self) -> Option<&Query> {
        match self {
            Rule::Nfd(q) => Some(q),
            Rule::Library { query, .. } => query.as_ref(),
        }
    }

    /// The library query's name, if it is one.
    pub fn library_id(&self) -> Option<&str> {
        match self {
            Rule::Library { id, .. } => Some(id),
            Rule::Nfd(_) => None,
        }
    }

    /// Expanded with `lib`.
    pub(crate) fn resolved(&self, lib: &Library) -> Result<Rule, String> {
        let Rule::Library { id, params, .. } = self else { return Ok(self.clone()) };
        let sel = Selection { include: vec![Pick::Query(id.clone())], ..Selection::default() };
        let values: Params =
            params.iter().map(|(k, v)| (k.clone(), ParamValue { value: v.clone(), raw: false })).collect();
        let e = crate::pack::expand(&sel, lib, &values).map_err(|e| e.to_string())?;
        let q = Query::compile(&e.nfd).map_err(|e| format!("{id}: {}", e.msg))?;
        Ok(Rule::Library { id: id.clone(), params: params.clone(), query: Some(q) })
    }
}

/// A spec's clauses: its text split at each `;` that isn't inside a library
/// query `{…}`, a string `"…"` or a regex `/…/` (a `;` in a regex or in a
/// query's name cut the clause; checks use it too).
pub(crate) fn clauses(text: &str) -> Vec<&str> {
    let (mut out, mut start, mut braces) = (Vec::new(), 0, 0usize);
    let (mut quote, mut escaped, mut prev) = (None::<char>, false, ' ');
    for (i, c) in text.char_indices() {
        let before = std::mem::replace(&mut prev, c);
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' => quote = Some('"'),
            // A regex starts where a value does: a `/` inside `{…}` is a
            // parameter's value (`path=/a/b`), one after a word a label's
            // level (`labels system/vision`).
            '/' if braces == 0 && (before.is_whitespace() || "(~!=&|<>".contains(before)) => quote = Some('/'),
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            ';' if braces == 0 => {
                out.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&text[start..]);
    out
}

/// The words of `text` with their byte offsets.
fn words_at(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split_whitespace().map(move |w| (w.as_ptr() as usize - text.as_ptr() as usize, w))
}

/// A library query's parameter: `NAME=VALUE`, NAME a pack parameter name.
fn is_param(word: &str) -> bool {
    word.split_once('=').is_some_and(|(k, _)| crate::pack::is_name(k))
}

#[derive(Debug, Clone)]
pub struct LifecycleDef {
    name: String,
    lines: Option<Rule>,
    start: Option<Rule>,
    reset: Option<Reset>,
    gap: Option<f64>,
    planned: Option<Rule>,
    crash: Option<Rule>,
    late: f64,
    /// The `reset` signal and a `time` signal of the tab, set by [`Self::linked`].
    reset_sig: Option<Arc<SignalDef>>,
    time_sig: Option<Arc<SignalDef>>,
    cache: Cache,
}

/// What started a lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Its first line in the log.
    First,
    /// A line matching `start`.
    Query,
    /// The `reset` value went back.
    Reset,
    /// Its lines stopped for longer than `gap`.
    Gap,
}

impl Start {
    pub fn as_str(self) -> &'static str {
        match self {
            Start::First => "first line",
            Start::Query => "start line",
            Start::Reset => "reset",
            Start::Gap => "gap",
        }
    }
}

/// How a lifecycle ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A `planned` marker matched in it (the last marker wins).
    Planned,
    /// A `crash` marker matched in it.
    Crash,
    /// No marker, and a new one started.
    Unclean,
    /// Ended by a `gap`.
    Lost,
    /// The log ends in it.
    Open,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Planned => "planned",
            Outcome::Crash => "crash",
            Outcome::Unclean => "unclean",
            Outcome::Lost => "lost",
            Outcome::Open => "open",
        }
    }

    /// Ended badly: a crash, unclean or lost.
    pub fn is_bad(self) -> bool {
        matches!(self, Outcome::Crash | Outcome::Unclean | Outcome::Lost)
    }
}

/// One lifecycle.
#[derive(Debug, Clone, PartialEq)]
pub struct Life {
    /// From 1.
    pub number: u32,
    /// Its first and last line (of its lines).
    pub first: LineNo,
    pub last: LineNo,
    pub started_by: Start,
    pub outcome: Outcome,
    /// The marker's line, for `planned` and `crash`.
    pub marker: Option<LineNo>,
    /// Times of its first and last line, if known.
    pub first_time: Option<f64>,
    pub last_time: Option<f64>,
    /// The uptime at its first line, if known.
    pub first_uptime: Option<f64>,
    /// Boot time (the least `time − uptime`) and how far the lines sit
    /// above it on average, where both are known.
    pub boot: Option<f64>,
    pub spread: Option<f64>,
    /// Its lines.
    pub lines: u64,
}

impl Life {
    pub fn duration(&self) -> Option<f64> {
        Some(self.last_time? - self.first_time?)
    }
}

/// A place where the definition and the uptime disagree, or a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// The uptime went back but nothing started a lifecycle.
    MissedReset,
    /// A lifecycle started but the uptime kept counting.
    FalseReset,
    /// Its first line came long after its boot: early lines are missing.
    LateStart,
}

impl FindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FindingKind::MissedReset => "missed reset",
            FindingKind::FalseReset => "false reset",
            FindingKind::LateStart => "late start",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub kind: FindingKind,
    /// The line that shows it.
    pub row: LineNo,
    /// Index into [`Lifecycles::lives`].
    pub life: usize,
    pub detail: String,
    /// How often it happened in its lifecycle (`row` is the first).
    pub count: u64,
}

/// The lifecycles found in a source.
#[derive(Debug, Default)]
pub struct Lifecycles {
    /// In the order they start.
    pub lives: Vec<Life>,
    pub findings: Vec<Finding>,
}

impl Lifecycles {
    /// The lifecycle `row` is in: the last one started at or before it
    /// (from its first line to the next one's).
    pub fn at(&self, row: LineNo) -> Option<(usize, &Life)> {
        let i = self.lives.partition_point(|l| l.first <= row).checked_sub(1)?;
        Some((i, &self.lives[i]))
    }

    /// Findings of lifecycle `i`.
    pub fn findings_of(&self, i: usize) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(move |f| f.life == i)
    }

    /// "5 lifecycles, 1 unclean, 1 finding".
    pub fn summary(&self) -> String {
        let n = self.lives.len();
        let mut s = format!("{n} lifecycle{}", if n == 1 { "" } else { "s" });
        for o in [Outcome::Crash, Outcome::Unclean, Outcome::Lost] {
            let c = self.lives.iter().filter(|l| l.outcome == o).count();
            if c > 0 {
                s += &format!(", {c} {}", o.as_str());
            }
        }
        let f = self.findings.len();
        if f > 0 {
            s += &format!(", {f} finding{}", if f == 1 { "" } else { "s" });
        }
        s
    }
}

/// A duration: a number with `ms`, `s`, `m` or `h` (default seconds).
pub fn parse_duration(s: &str) -> Option<f64> {
    let s = s.trim();
    let (num, unit) = match s.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => s.split_at(i),
        None => (s, "s"),
    };
    let n: f64 = num.trim().parse().ok().filter(|n: &f64| n.is_finite() && *n >= 0.0)?;
    Some(
        n * match unit {
            "ms" => 0.001,
            "s" => 1.0,
            "m" | "min" => 60.0,
            "h" => 3600.0,
            _ => return None,
        },
    )
}

/// A duration as written back: `30s`, `500ms`.
pub fn format_duration(d: f64) -> String {
    if d != 0.0 && d < 1.0 { format!("{}ms", (d * 1000.0 * 1e6).round() / 1e6) } else { format!("{d}s") }
}

impl LifecycleDef {
    /// A definition named `name` with no rules yet (they are set with the
    /// `with_*` methods, as a form does); [`Self::check`] says if it is
    /// complete.
    pub fn new(name: &str) -> Result<LifecycleDef, String> {
        let name = name.trim();
        if !is_name(name) {
            return Err(format!("'{name}' isn't a name (letters, digits and _, not first a digit)"));
        }
        Ok(LifecycleDef {
            name: name.to_ascii_lowercase(),
            lines: None,
            start: None,
            reset: None,
            gap: None,
            planned: None,
            crash: None,
            late: DEFAULT_LATE,
            reset_sig: None,
            time_sig: None,
            cache: Cache::default(),
        })
    }

    /// Parse a spec line (module docs). Error positions are byte offsets
    /// in `spec`.
    pub fn parse_spec(spec: &str) -> Result<LifecycleDef, DslError> {
        let err = |msg: String, pos: usize| DslError { msg, pos };
        let Some(colon) = spec.find(':') else {
            return Err(err("expected NAME: followed by clauses (lines, start, reset, gap, planned, crash)".into(), 0));
        };
        let lead = spec.len() - spec.trim_start().len();
        let mut def = LifecycleDef::new(&spec[..colon]).map_err(|m| err(m, lead))?;
        let mut at = colon + 1;
        for clause in clauses(&spec[colon + 1..]) {
            let start = at + clause.len() - clause.trim_start().len();
            at += clause.len() + 1;
            let c = clause.trim();
            if c.is_empty() {
                continue;
            }
            let (word, rest) = c.split_once(char::is_whitespace).unwrap_or((c, ""));
            let rest_at = start + word.len() + (rest.len() - rest.trim_start().len()) + usize::from(!rest.is_empty());
            let rest = rest.trim();
            let query = |what: &str| -> Result<Rule, DslError> {
                if rest.is_empty() {
                    return Err(err(format!("'{what}' needs a query"), start + word.len()));
                }
                Rule::parse(rest).map_err(|e| err(e.msg, rest_at + e.pos))
            };
            let duration = |what: &str| -> Result<f64, DslError> {
                parse_duration(rest).ok_or_else(|| {
                    err(format!("'{what}' needs a duration (30s, 500ms, 2m, 1h) but found '{rest}'"), rest_at)
                })
            };
            match word.to_ascii_lowercase().as_str() {
                "lines" => def.lines = Some(query("lines")?),
                "start" => def.start = Some(query("start")?),
                "planned" => def.planned = Some(query("planned")?),
                "crash" => def.crash = Some(query("crash")?),
                "gap" => def.gap = Some(duration("gap")?),
                "late" => def.late = duration("late")?,
                "reset" => {
                    let mut words = rest.split_whitespace();
                    let v = words.next().unwrap_or_default();
                    let value = if v.eq_ignore_ascii_case("uptime") {
                        ResetValue::Uptime
                    } else if let Some(name) = v.strip_prefix('@').filter(|n| is_name(n)) {
                        ResetValue::Signal(name.to_ascii_lowercase())
                    } else {
                        return Err(err(format!("'reset' needs uptime or a signal (@name) but found '{v}'"), rest_at));
                    };
                    let mut tolerance = DEFAULT_TOLERANCE;
                    match (words.next(), words.next(), words.next()) {
                        (None, ..) => {}
                        (Some(t), Some(d), None) if t.eq_ignore_ascii_case("tolerance") => {
                            let d_at = rest_at + rest.rfind(d).unwrap_or(0);
                            tolerance = parse_duration(d)
                                .ok_or_else(|| err(format!("expected a duration (1s, 500ms) but found '{d}'"), d_at))?;
                        }
                        _ => {
                            return Err(err("expected 'reset uptime' or 'reset uptime tolerance DUR'".into(), rest_at));
                        }
                    }
                    def.reset = Some(Reset { value, tolerance });
                }
                w => {
                    return Err(err(
                        format!("unknown clause '{w}' (lines, start, reset, gap, planned, crash, late)"),
                        start,
                    ));
                }
            }
        }
        def.check().map_err(|m| err(m, spec.len()))?;
        Ok(def)
    }

    /// Whether it can find lifecycles: it needs a start, reset or gap.
    pub fn check(&self) -> Result<(), String> {
        if self.start.is_none() && self.reset.is_none() && self.gap.is_none() {
            return Err("a lifecycle needs a start, reset or gap clause".into());
        }
        Ok(())
    }

    /// The spec line [`Self::parse_spec`] reads back.
    pub fn spec(&self) -> String {
        let mut clauses = Vec::new();
        if let Some(q) = &self.lines {
            clauses.push(format!("lines {}", q.text()));
        }
        if let Some(q) = &self.start {
            clauses.push(format!("start {}", q.text()));
        }
        if let Some(r) = &self.reset {
            let v = match &r.value {
                ResetValue::Uptime => "uptime".to_owned(),
                ResetValue::Signal(s) => format!("@{s}"),
            };
            clauses.push(if r.tolerance == DEFAULT_TOLERANCE {
                format!("reset {v}")
            } else {
                format!("reset {v} tolerance {}", format_duration(r.tolerance))
            });
        }
        if let Some(g) = self.gap {
            clauses.push(format!("gap {}", format_duration(g)));
        }
        if let Some(q) = &self.planned {
            clauses.push(format!("planned {}", q.text()));
        }
        if let Some(q) = &self.crash {
            clauses.push(format!("crash {}", q.text()));
        }
        if self.late != DEFAULT_LATE {
            clauses.push(format!("late {}", format_duration(self.late)));
        }
        format!("{}: {}", self.name, clauses.join("; "))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its lines' query (none: every line).
    pub fn lines(&self) -> Option<&Rule> {
        self.lines.as_ref()
    }

    pub fn start(&self) -> Option<&Rule> {
        self.start.as_ref()
    }

    pub fn reset(&self) -> Option<&Reset> {
        self.reset.as_ref()
    }

    pub fn gap(&self) -> Option<f64> {
        self.gap
    }

    pub fn planned(&self) -> Option<&Rule> {
        self.planned.as_ref()
    }

    pub fn crash(&self) -> Option<&Rule> {
        self.crash.as_ref()
    }

    pub fn late(&self) -> f64 {
        self.late
    }

    /// A query clause from text (empty: none); an error says where.
    fn query_of(text: &str) -> Result<Option<Rule>, DslError> {
        if text.trim().is_empty() { Ok(None) } else { Rule::parse(text).map(Some) }
    }

    /// Its library queries expanded with `lib` (a fresh cache when an
    /// expansion changed). An error names the query.
    pub fn resolved(&self, lib: &Library) -> Result<LifecycleDef, String> {
        let r = |q: &Option<Rule>| q.as_ref().map(|q| q.resolved(lib)).transpose();
        let mut out = LifecycleDef {
            lines: r(&self.lines)?,
            start: r(&self.start)?,
            planned: r(&self.planned)?,
            crash: r(&self.crash)?,
            ..self.clone()
        };
        if out.sources() != self.sources() {
            out.cache = Cache::default();
        }
        Ok(out)
    }

    /// The NFD of its queries (`lines`, `start`, `planned`, `crash`) as
    /// they run: a library query's once resolved, else none.
    pub(crate) fn sources(&self) -> [Option<String>; 4] {
        [&self.lines, &self.start, &self.planned, &self.crash]
            .map(|q| q.as_ref().and_then(|q| q.query()).map(|q| q.source().to_owned()))
    }

    /// The library queries it uses, not resolved yet.
    pub fn unresolved(&self) -> Vec<&str> {
        [&self.lines, &self.start, &self.planned, &self.crash]
            .into_iter()
            .flatten()
            .filter(|q| q.query().is_none())
            .filter_map(Rule::library_id)
            .collect()
    }

    pub fn with_lines(mut self, nfd: &str) -> Result<LifecycleDef, DslError> {
        self.lines = Self::query_of(nfd)?;
        Ok(self)
    }

    pub fn with_start(mut self, nfd: &str) -> Result<LifecycleDef, DslError> {
        self.start = Self::query_of(nfd)?;
        Ok(self)
    }

    pub fn with_planned(mut self, nfd: &str) -> Result<LifecycleDef, DslError> {
        self.planned = Self::query_of(nfd)?;
        Ok(self)
    }

    pub fn with_crash(mut self, nfd: &str) -> Result<LifecycleDef, DslError> {
        self.crash = Self::query_of(nfd)?;
        Ok(self)
    }

    pub fn with_reset(mut self, reset: Option<Reset>) -> LifecycleDef {
        self.reset = reset;
        self
    }

    pub fn with_gap(mut self, gap: Option<f64>) -> LifecycleDef {
        self.gap = gap;
        self
    }

    pub fn with_late(mut self, late: f64) -> LifecycleDef {
        self.late = late;
        self
    }

    /// The definition with the tab's signals it uses: a `reset` signal and
    /// a `time` signal for sources without their own time. A fresh cache
    /// when they changed.
    pub fn linked(&self, signals: &Signals) -> LifecycleDef {
        let defined = |d: &&Arc<SignalDef>| !d.is_builtin();
        let reset_sig = match self.reset.as_ref().map(|r| &r.value) {
            Some(ResetValue::Signal(s)) => signals.get(s).filter(defined).cloned(),
            _ => None,
        };
        let time_sig = signals.iter().filter(defined).find(|d| d.kind() == SignalKind::Time).cloned();
        let mut out = LifecycleDef { reset_sig, time_sig, ..self.clone() };
        if !out.same_links(self) {
            out.cache = Cache::default();
        }
        out
    }

    /// Whether it uses the same signals as `other` (by their definitions).
    pub fn same_links(&self, other: &LifecycleDef) -> bool {
        let spec = |d: &Option<Arc<SignalDef>>| d.as_ref().map(|d| d.spec());
        spec(&self.reset_sig) == spec(&other.reset_sig) && spec(&self.time_sig) == spec(&other.time_sig)
    }

    /// The lifecycles in `source`: one pass on first use, then cached for
    /// this definition and its copies.
    pub fn lifecycles(&self, source: &dyn LogSource, progress: &Progress) -> crate::Result<Arc<Lifecycles>> {
        let key = source_key(source);
        if let Some(r) = self.done(source) {
            return r.map_err(crate::Error::Format);
        }
        let r = match self.detect(source, progress) {
            Ok(l) => Ok(Arc::new(l)),
            // Not an answer: asked again later.
            Err(crate::Error::Cancelled) => return Err(crate::Error::Cancelled),
            Err(e) => Err(e.to_string()),
        };
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if !cache.iter().any(|(k, _)| *k == key) {
            if cache.len() >= 16 {
                drop(cache.remove(0));
            }
            cache.push((key, r.clone()));
        }
        r.map_err(crate::Error::Format)
    }

    /// The lifecycles, or why there are none, if worked out already.
    pub fn done(&self, source: &dyn LogSource) -> Option<Result<Arc<Lifecycles>, String>> {
        let key = source_key(source);
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.iter().find(|(k, _)| *k == key).map(|(_, r)| r.clone())
    }

    /// The lifecycles if found already.
    pub fn cached(&self, source: &dyn LogSource) -> Option<Arc<Lifecycles>> {
        self.done(source)?.ok()
    }

    fn hits(&self, q: &Option<Rule>, source: &dyn LogSource, progress: &Progress) -> crate::Result<Vec<LineNo>> {
        match q.as_ref().and_then(Rule::query) {
            Some(q) => {
                let q = q.clone().bind(source.schema())?;
                scan(source, &Filter::Query(q), progress)
            }
            None => Ok(Vec::new()),
        }
    }

    fn detect(&self, source: &dyn LogSource, progress: &Progress) -> crate::Result<Lifecycles> {
        if let Some(id) = self.unresolved().first() {
            return Err(crate::Error::Format(format!(
                "lifecycle '{}': library query {id} isn't loaded (load its pack)",
                self.name
            )));
        }
        // Its lines: the query's hits (the engine's fast path), else all.
        let lines: Option<Vec<LineNo>> = match &self.lines {
            Some(_) => Some(self.hits(&self.lines, source, progress)?),
            None => None,
        };
        let n = lines.as_ref().map_or(source.line_count() as usize, Vec::len);
        let row_at = |i: usize| lines.as_ref().map_or(i as LineNo, |l| l[i]);
        let starts = self.hits(&self.start, source, progress)?;
        let planned = self.hits(&self.planned, source, progress)?;
        let crash = self.hits(&self.crash, source, progress)?;
        // The value that goes back: a defined signal's events, or the
        // uptime the lines carry.
        let (up_series, src_up) = match self.reset.as_ref().map(|r| &r.value) {
            Some(ResetValue::Signal(s)) => match &self.reset_sig {
                Some(d) => (Some(d.series(source, progress)?), false),
                None => {
                    return Err(crate::Error::Format(format!(
                        "lifecycle '{}': no signal @{s} (define it, or reset on uptime)",
                        self.name
                    )));
                }
            },
            // Uptime for the findings even without `reset`.
            _ => (None, true),
        };
        let reset_on = self.reset.is_some();
        // Some lines may carry none (a DLT message without tmsp): any of the
        // first thousand will do.
        let has_uptime = (0..n.min(1000)).any(|i| source.uptime(row_at(i)).is_some());
        if matches!(self.reset.as_ref().map(|r| &r.value), Some(ResetValue::Uptime)) && n > 0 && !has_uptime {
            return Err(crate::Error::Format(format!(
                "lifecycle '{}': its lines carry no uptime (DLT: the tmsp header field); reset on a signal instead",
                self.name
            )));
        }
        let time_series = match (&self.time_sig, source.line_count() > 0 && source.time(0).is_some()) {
            (Some(d), false) => Some(d.series(source, progress)?),
            _ => None,
        };
        let tolerance = self.reset.as_ref().map_or(DEFAULT_TOLERANCE, |r| r.tolerance);
        let mut up_events = up_series.as_ref().map(|s| s.iter().peekable());
        let mut time_state: Option<f64> = None;
        let mut time_events = time_series.as_ref().map(|s| s.iter().peekable());
        let (mut si, mut pi, mut ci) = (0, 0, 0);
        let own_time = time_series.is_none();

        let mut out = Lifecycles::default();
        // The open lifecycle: its last uptime and time, boot sums, marker.
        let (mut last_up, mut last_time): (Option<f64>, Option<f64>) = (None, None);
        let (mut bsum, mut bn) = (0.0, 0u64);
        let mut marker: Option<(Outcome, LineNo)> = None;
        let close = |out: &mut Lifecycles, marker: Option<(Outcome, LineNo)>, bsum: f64, bn: u64, ended: Outcome| {
            if let Some(l) = out.lives.last_mut() {
                (l.outcome, l.marker) = match marker {
                    Some((m, row)) => (m, Some(row)),
                    None => (ended, None),
                };
                if let Some(b) = l.boot {
                    l.spread = Some(bsum / bn as f64 - b);
                }
            }
        };
        let mut chunk_start = 0;
        while chunk_start < n {
            if progress.is_cancelled() {
                return Err(crate::Error::Cancelled);
            }
            let chunk_end = (chunk_start + CHUNK).min(n);
            // What the source says of each line, read in parallel; the walk
            // below is in order.
            let pre: Vec<(LineNo, Option<f64>, Option<f64>)> = (chunk_start..chunk_end)
                .into_par_iter()
                .map(|i| {
                    let r = row_at(i);
                    (r, if own_time { source.time(r) } else { None }, if src_up { source.uptime(r) } else { None })
                })
                .collect();
            for &(row, row_time, row_up) in &pre {
                let mut up = row_up;
                if let Some(ev) = &mut up_events {
                    up = None;
                    while let Some(&(r, v)) = ev.peek() {
                        if r > row {
                            break;
                        }
                        if r == row
                            && let ValueRef::Num(n) = v
                        {
                            up = Some(n);
                        }
                        ev.next();
                    }
                }
                if let Some(ev) = &mut time_events {
                    while let Some(&(r, v)) = ev.peek() {
                        if r > row {
                            break;
                        }
                        if let ValueRef::Num(n) = v {
                            time_state = Some(n);
                        }
                        ev.next();
                    }
                }
                let time = if own_time { row_time } else { time_state };
                let is_start = advance(&starts, &mut si, row);
                let is_planned = advance(&planned, &mut pi, row);
                let is_crash = advance(&crash, &mut ci, row);
                let back = matches!((up, last_up), (Some(u), Some(p)) if u < p - tolerance);
                let by = if out.lives.is_empty() {
                    Some(Start::First)
                } else if is_start {
                    Some(Start::Query)
                } else if reset_on && back {
                    Some(Start::Reset)
                } else if matches!((self.gap, time, last_time), (Some(g), Some(t), Some(p)) if t - p > g) {
                    Some(Start::Gap)
                } else {
                    None
                };
                match by {
                    Some(by) => {
                        let number = out.lives.last().map_or(1, |l| l.number + 1);
                        let kept = matches!((up, last_up), (Some(u), Some(p)) if u >= p - tolerance);
                        close(
                            &mut out,
                            marker,
                            bsum,
                            bn,
                            if by == Start::Gap { Outcome::Lost } else { Outcome::Unclean },
                        );
                        out.lives.push(Life {
                            number,
                            first: row,
                            last: row,
                            started_by: by,
                            outcome: Outcome::Open,
                            marker: None,
                            first_time: time,
                            last_time: time,
                            first_uptime: up,
                            boot: None,
                            spread: None,
                            lines: 0,
                        });
                        if matches!(by, Start::Query | Start::Gap) && kept {
                            let (u, p) = (up.unwrap_or_default(), last_up.unwrap_or_default());
                            out.findings.push(Finding {
                                kind: FindingKind::FalseReset,
                                row,
                                life: out.lives.len() - 1,
                                detail: format!(
                                    "a lifecycle started here ({}) but the uptime kept counting ({p:.1} s → {u:.1} s)",
                                    by.as_str()
                                ),
                                count: 1,
                            });
                        }
                        (last_up, last_time, bsum, bn, marker) = (None, None, 0.0, 0, None);
                    }
                    None if back => {
                        let (u, p) = (up.unwrap_or_default(), last_up.unwrap_or_default());
                        // Once per lifecycle, counted (stress test, 2026-11-07: a
                        // lifecycle over several writers' uptimes had 2 M).
                        let life = out.lives.len() - 1;
                        let seen = out
                            .findings
                            .iter_mut()
                            .rev()
                            .take_while(|f| f.life == life)
                            .find(|f| f.kind == FindingKind::MissedReset);
                        match seen {
                            Some(f) => f.count += 1,
                            None => out.findings.push(Finding {
                                kind: FindingKind::MissedReset,
                                row,
                                life,
                                detail: format!("uptime went back {p:.1} s → {u:.1} s, but no lifecycle started"),
                                count: 1,
                            }),
                        }
                    }
                    None => {}
                }
                let l = out.lives.last_mut().expect("one is open");
                l.last = row;
                l.lines += 1;
                if time.is_some() {
                    l.last_time = time;
                    l.first_time = l.first_time.or(time);
                }
                if l.lines == 1 && up.is_some() {
                    l.first_uptime = up;
                }
                if let (Some(t), Some(u)) = (time, up) {
                    let b = t - u;
                    l.boot = Some(l.boot.map_or(b, |x| x.min(b)));
                    bsum += b;
                    bn += 1;
                }
                if up.is_some() {
                    last_up = up;
                }
                if time.is_some() {
                    last_time = time;
                }
                if is_planned {
                    marker = Some((Outcome::Planned, row));
                }
                if is_crash {
                    marker = Some((Outcome::Crash, row));
                }
            }
            chunk_start = chunk_end;
        }
        close(&mut out, marker, bsum, bn, Outcome::Open);
        for (i, l) in out.lives.iter().enumerate() {
            // A start that wasn't a boot isn't late either.
            let false_start = out.findings.iter().any(|f| f.life == i && f.kind == FindingKind::FalseReset);
            if let Some(u) = l.first_uptime.filter(|&u| u > self.late && !false_start) {
                let note = if l.number == 1 { " (or the log started during it)" } else { "" };
                out.findings.push(Finding {
                    kind: FindingKind::LateStart,
                    row: l.first,
                    life: i,
                    detail: format!("first line at uptime {u:.1} s: its early lines are missing{note}"),
                    count: 1,
                });
            }
        }
        for f in out.findings.iter_mut().filter(|f| f.count > 1) {
            f.detail += &format!(" (the first of {} times in this lifecycle", f.count);
            // Many: its lines are likely of several writers, each with its uptime.
            f.detail += if f.count >= 100 {
                "; its lines may mix several writers' uptimes: give it the lines of one)"
            } else {
                ")"
            };
        }
        out.findings.sort_by_key(|f| f.row);
        Ok(out)
    }
}

/// Whether `row` is the next of the ascending `hits` (moving past it).
fn advance(hits: &[LineNo], i: &mut usize, row: LineNo) -> bool {
    while *i < hits.len() && hits[*i] < row {
        *i += 1;
    }
    if hits.get(*i) == Some(&row) {
        *i += 1;
        return true;
    }
    false
}

#[cfg(test)]
mod tests;

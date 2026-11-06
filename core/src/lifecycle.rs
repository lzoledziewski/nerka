//! Lifecycles: the boots (runs, sessions) of the entities a log is about,
//! when each starts and how it ends (`doc/lifecycles.md`).
//!
//! A [`LifecycleDef`] is built from generic pieces only: a key field (one
//! stream of lifecycles per value, e.g. `ecu`), start triggers (a query, a
//! numeric signal going back such as `@uptime`, a gap in time) and outcome
//! markers (queries). [`LifecycleDef::lifecycles`] finds them in a source
//! in one pass ([`Lifecycles`]), with findings where the definition and the
//! uptime disagree.
//!
//! Spec text (CLI `--lifecycle`, the saved signals file after `lifecycle `):
//!
//! ```text
//! spec   := NAME [ "(" KEY ")" ] ":" clause ( ";" clause )*
//! clause := "start" NFD                        -- a matching line starts one
//!         | "reset" "@" SIGNAL [ "tolerance" DUR ]  -- the signal goes back
//!         | "gap" DUR                          -- no line of the key for longer
//!         | "planned" NFD | "crash" NFD         -- outcome markers
//!         | "late" DUR                         -- "late start" above this uptime
//! DUR    := NUMBER [ "ms" | "s" | "m" | "h" ]   -- default seconds
//! ```
//!
//! e.g. `boot(ecu): reset @uptime; planned payload ~ /shutdown/`. At least
//! one of `start`, `reset` and `gap` is needed. `@uptime` is a signal of
//! that name the tab defines, else the source's own ([`LogSource::uptime`]).

use crate::dsl::{DslError, Query};
use crate::engine::search::{Filter, scan};
use crate::event::FieldValue;
use crate::signal::{SignalDef, SignalKind, Signals, ValueRef, is_name, source_key};
use crate::{LineNo, LogSource, Progress};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Tolerance of `reset` (and of the missed-reset check) when not given:
/// lines of different writers interleave, so a small step back isn't one.
pub const DEFAULT_TOLERANCE: f64 = 5.0;
/// A lifecycle whose first line has an uptime above this started late.
pub const DEFAULT_LATE: f64 = 10.0;

/// Rows between checks for cancellation.
const CHUNK: LineNo = 1 << 16;

/// A numeric signal going back starts a lifecycle.
#[derive(Debug, Clone)]
pub struct Reset {
    /// Lower case, without `@`.
    pub signal: String,
    pub tolerance: f64,
}

type Cache = Arc<Mutex<Vec<((usize, LineNo), Arc<Lifecycles>)>>>;

#[derive(Debug, Clone)]
pub struct LifecycleDef {
    name: String,
    key: Option<String>,
    start: Option<Query>,
    reset: Option<Reset>,
    gap: Option<f64>,
    planned: Option<Query>,
    crash: Option<Query>,
    late: f64,
    /// The `reset` signal and a `time` signal of the tab, set by [`Self::linked`].
    reset_sig: Option<Arc<SignalDef>>,
    time_sig: Option<Arc<SignalDef>>,
    cache: Cache,
}

/// What started a lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// The key's first line.
    First,
    /// A line matching `start`.
    Query,
    /// The `reset` signal went back.
    Reset,
    /// No line of the key for longer than `gap`.
    Gap,
}

impl Start {
    pub fn as_str(self) -> &'static str {
        match self {
            Start::First => "first line",
            Start::Query => "start",
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

/// One lifecycle of one key.
#[derive(Debug, Clone, PartialEq)]
pub struct Life {
    /// The key's value (`None` for a definition without a key).
    pub key: Option<String>,
    /// Per key, from 1.
    pub number: u32,
    pub first: LineNo,
    /// Its last line (of its key).
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
    /// Its lines (of its key).
    pub lines: u64,
}

impl Life {
    pub fn duration(&self) -> Option<f64> {
        Some(self.last_time? - self.first_time?)
    }

    /// `A#2`, or `#2` without a key.
    pub fn label(&self) -> String {
        format!("{}#{}", self.key.as_deref().unwrap_or(""), self.number)
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
}

/// The lifecycles found in a source.
#[derive(Debug, Default)]
pub struct Lifecycles {
    /// In the order they start.
    pub lives: Vec<Life>,
    pub findings: Vec<Finding>,
    /// Per key hash: indices into `lives`, ascending.
    by_key: HashMap<u64, Vec<usize>>,
    key_field: Option<String>,
}

/// The hash a key's value is known by (FNV-1a, 64 bits).
pub fn key_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// The hash of the key of a definition without one.
const NO_KEY: u64 = 0;

/// The text of a field's value, for a key.
fn key_text(v: FieldValue<'_>) -> Vec<u8> {
    match v {
        FieldValue::Str(s) => s.into_owned(),
        FieldValue::U64(n) => n.to_string().into_bytes(),
        FieldValue::F64(n) => n.to_string().into_bytes(),
    }
}

impl Lifecycles {
    /// The key hash of `row`: `None` if the definition has a key and the
    /// row has no value for it (such rows belong to no lifecycle).
    pub fn key_of(&self, source: &dyn LogSource, row: LineNo) -> Option<u64> {
        let Some(name) = &self.key_field else { return Some(NO_KEY) };
        let id = source.schema().iter().position(|d| d.name == name.as_str())?;
        source.field(row, id).map(|v| key_hash(&key_text(v)))
    }

    /// The lifecycle `row` is in, given its key hash.
    pub fn life_at(&self, key: u64, row: LineNo) -> Option<(usize, &Life)> {
        let idx = self.by_key.get(&key)?;
        let i = idx.partition_point(|&i| self.lives[i].first <= row).checked_sub(1)?;
        Some((idx[i], &self.lives[idx[i]]))
    }

    /// The lifecycle of `row` in `source`.
    pub fn at(&self, source: &dyn LogSource, row: LineNo) -> Option<(usize, &Life)> {
        self.life_at(self.key_of(source, row)?, row)
    }

    /// Findings of lifecycle `i`.
    pub fn findings_of(&self, i: usize) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(move |f| f.life == i)
    }

    /// Distinct keys, in the order they first appear.
    pub fn keys(&self) -> Vec<Option<&str>> {
        let mut out: Vec<Option<&str>> = Vec::new();
        for l in &self.lives {
            if !out.contains(&l.key.as_deref()) {
                out.push(l.key.as_deref());
            }
        }
        out
    }

    /// "5 lifecycles (2 keys), 1 unclean, 1 finding".
    pub fn summary(&self) -> String {
        let n = self.lives.len();
        let mut s = format!("{n} lifecycle{}", if n == 1 { "" } else { "s" });
        let keys = self.keys().len();
        if self.key_field.is_some() && keys > 1 {
            s += &format!(" ({keys} keys)");
        }
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
fn format_duration(d: f64) -> String {
    if d != 0.0 && d < 1.0 { format!("{}ms", (d * 1000.0 * 1e6).round() / 1e6) } else { format!("{d}s") }
}

impl LifecycleDef {
    /// Parse a spec line (module docs). Error positions are byte offsets
    /// in `spec`.
    pub fn parse_spec(spec: &str) -> Result<LifecycleDef, DslError> {
        let err = |msg: String, pos: usize| DslError { msg, pos };
        let Some(colon) = spec.find(':') else {
            return Err(err("expected NAME[(KEY)]: followed by clauses (start, reset, gap, planned, crash)".into(), 0));
        };
        let head = &spec[..colon];
        let (name, key) = match head.find('(') {
            Some(open) => {
                let close = head.rfind(')').filter(|&c| c > open && head[c + 1..].trim().is_empty());
                let Some(close) = close else { return Err(err("expected ')' after the key".into(), colon)) };
                (&head[..open], Some((head[open + 1..close].trim(), open + 1)))
            }
            None => (head, None),
        };
        let lead = name.len() - name.trim_start().len();
        let name = name.trim();
        if !is_name(name) {
            return Err(err(format!("'{name}' isn't a name (letters, digits and _, not first a digit)"), lead));
        }
        let mut def = LifecycleDef {
            name: name.to_ascii_lowercase(),
            key: None,
            start: None,
            reset: None,
            gap: None,
            planned: None,
            crash: None,
            late: DEFAULT_LATE,
            reset_sig: None,
            time_sig: None,
            cache: Cache::default(),
        };
        if let Some((k, at)) = key {
            if k.is_empty() {
                return Err(err("the key is empty: write a field's name, e.g. (ecu)".into(), at));
            }
            def.key = Some(k.to_ascii_lowercase());
        }
        let mut at = colon + 1;
        for clause in spec[colon + 1..].split(';') {
            let start = at + clause.len() - clause.trim_start().len();
            at += clause.len() + 1;
            let c = clause.trim();
            if c.is_empty() {
                continue;
            }
            let (word, rest) = c.split_once(char::is_whitespace).unwrap_or((c, ""));
            let rest_at = start + word.len() + (rest.len() - rest.trim_start().len()) + usize::from(!rest.is_empty());
            let rest = rest.trim();
            let query = |what: &str| -> Result<Query, DslError> {
                if rest.is_empty() {
                    return Err(err(format!("'{what}' needs a query"), start + word.len()));
                }
                Query::compile(rest).map_err(|e| err(e.msg, rest_at + e.pos))
            };
            let duration = |what: &str| -> Result<f64, DslError> {
                parse_duration(rest).ok_or_else(|| {
                    err(format!("'{what}' needs a duration (30s, 500ms, 2m, 1h) but found '{rest}'"), rest_at)
                })
            };
            match word.to_ascii_lowercase().as_str() {
                "start" => def.start = Some(query("start")?),
                "planned" => def.planned = Some(query("planned")?),
                "crash" => def.crash = Some(query("crash")?),
                "gap" => def.gap = Some(duration("gap")?),
                "late" => def.late = duration("late")?,
                "reset" => {
                    let mut words = rest.split_whitespace();
                    let sig = words.next().unwrap_or_default();
                    let Some(name) = sig.strip_prefix('@').filter(|n| is_name(n)) else {
                        return Err(err(format!("'reset' needs a signal, e.g. @uptime, but found '{sig}'"), rest_at));
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
                            return Err(err(
                                "expected 'reset @SIGNAL' or 'reset @SIGNAL tolerance DUR'".into(),
                                rest_at,
                            ));
                        }
                    }
                    def.reset = Some(Reset { signal: name.to_ascii_lowercase(), tolerance });
                }
                w => {
                    return Err(err(format!("unknown clause '{w}' (start, reset, gap, planned, crash, late)"), start));
                }
            }
        }
        if def.start.is_none() && def.reset.is_none() && def.gap.is_none() {
            return Err(err("a lifecycle needs a start, reset or gap clause".into(), spec.len()));
        }
        Ok(def)
    }

    /// The spec line [`Self::parse_spec`] reads back.
    pub fn spec(&self) -> String {
        let mut clauses = Vec::new();
        if let Some(q) = &self.start {
            clauses.push(format!("start {}", q.source()));
        }
        if let Some(r) = &self.reset {
            clauses.push(if r.tolerance == DEFAULT_TOLERANCE {
                format!("reset @{}", r.signal)
            } else {
                format!("reset @{} tolerance {}", r.signal, format_duration(r.tolerance))
            });
        }
        if let Some(g) = self.gap {
            clauses.push(format!("gap {}", format_duration(g)));
        }
        if let Some(q) = &self.planned {
            clauses.push(format!("planned {}", q.source()));
        }
        if let Some(q) = &self.crash {
            clauses.push(format!("crash {}", q.source()));
        }
        if self.late != DEFAULT_LATE {
            clauses.push(format!("late {}", format_duration(self.late)));
        }
        let key = self.key.as_ref().map(|k| format!("({k})")).unwrap_or_default();
        format!("{}{key}: {}", self.name, clauses.join("; "))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    pub fn reset(&self) -> Option<&Reset> {
        self.reset.as_ref()
    }

    /// The definition with the tab's signals it uses: the `reset` signal
    /// (a defined one of that name, else the source's `@uptime`) and a
    /// `time` signal for sources without their own time. A fresh cache
    /// when they changed.
    pub fn linked(&self, signals: &Signals) -> LifecycleDef {
        // The source's own `@uptime` is read from the source directly.
        let defined = |d: &&Arc<SignalDef>| !d.is_from_source() && d.as_lifecycle().is_none();
        let reset_sig = self.reset.as_ref().and_then(|r| signals.get(&r.signal)).filter(defined).cloned();
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
        if let Some(l) = self.cached(source) {
            return Ok(l);
        }
        let l = Arc::new(self.detect(source, progress)?);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if !cache.iter().any(|(k, _)| *k == key) {
            if cache.len() >= 16 {
                cache.remove(0);
            }
            cache.push((key, l.clone()));
        }
        Ok(l)
    }

    /// The lifecycles if found already.
    pub fn cached(&self, source: &dyn LogSource) -> Option<Arc<Lifecycles>> {
        let key = source_key(source);
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.iter().find(|(k, _)| *k == key).map(|(_, l)| l.clone())
    }

    fn hits(&self, q: &Option<Query>, source: &dyn LogSource, progress: &Progress) -> crate::Result<Vec<LineNo>> {
        match q {
            Some(q) => {
                let q = q.clone().bind(source.schema())?;
                scan(source, &Filter::Query(q), progress)
            }
            None => Ok(Vec::new()),
        }
    }

    fn detect(&self, source: &dyn LogSource, progress: &Progress) -> crate::Result<Lifecycles> {
        let key_id = match &self.key {
            Some(k) => match source.schema().iter().position(|d| d.name == k.as_str()) {
                Some(id) => Some(id),
                None => {
                    let have: Vec<&str> = source.schema().iter().map(|d| d.name.as_ref()).collect();
                    let have = if have.is_empty() { "none".to_owned() } else { have.join(", ") };
                    return Err(crate::Error::Format(format!(
                        "lifecycle '{}': unknown key field '{k}' (this source has: {have})",
                        self.name
                    )));
                }
            },
            None => None,
        };
        let starts = self.hits(&self.start, source, progress)?;
        let planned = self.hits(&self.planned, source, progress)?;
        let crash = self.hits(&self.crash, source, progress)?;
        // The uptime: a defined signal's events, or the source's own.
        let up_series = match &self.reset_sig {
            Some(d) => Some(d.series(source, progress)?),
            None => None,
        };
        let src_up = up_series.is_none() && source.line_count() > 0 && source.uptime(0).is_some();
        let time_series = match (&self.time_sig, source.line_count() > 0 && source.time(0).is_some()) {
            (Some(d), false) => Some(d.series(source, progress)?),
            _ => None,
        };
        let reset_on = self.reset.is_some() && (up_series.is_some() || src_up);
        if self.reset.is_some() && !reset_on && self.start.is_none() && self.gap.is_none() {
            let r = self.reset.as_ref().map(|r| r.signal.as_str()).unwrap_or_default();
            return Err(crate::Error::Format(format!(
                "lifecycle '{}': no @{r} here (define a signal '{r}', or open a file that knows its uptime)",
                self.name
            )));
        }
        let tolerance = self.reset.as_ref().map_or(DEFAULT_TOLERANCE, |r| r.tolerance);
        let mut up_events = up_series.as_ref().map(|s| s.iter().peekable());
        let mut time_state: Option<f64> = None;
        let mut time_events = time_series.as_ref().map(|s| s.iter().peekable());
        let (mut si, mut pi, mut ci) = (0, 0, 0);

        struct Open {
            life: usize,
            last_up: Option<f64>,
            last_time: Option<f64>,
            bsum: f64,
            bn: u64,
            marker: Option<(Outcome, LineNo)>,
        }
        let mut out = Lifecycles { key_field: self.key.clone(), ..Lifecycles::default() };
        let mut open: HashMap<u64, Open> = HashMap::new();
        let close = |out: &mut Lifecycles, o: &Open, ended: Outcome| {
            let l = &mut out.lives[o.life];
            (l.outcome, l.marker) = match o.marker {
                Some((m, row)) => (m, Some(row)),
                None => (ended, None),
            };
            if let Some(b) = l.boot {
                l.spread = Some(o.bsum / o.bn as f64 - b);
            }
        };
        let n = source.line_count();
        let own_time = time_series.is_none();
        let mut chunk_start = 0;
        while chunk_start < n {
            if progress.is_cancelled() {
                return Err(crate::Error::Cancelled);
            }
            let chunk_end = (chunk_start + CHUNK).min(n);
            // What the source says of each line, read in parallel: its key,
            // time and uptime. The walk below is in order.
            let pre: Vec<(Option<u64>, Option<f64>, Option<f64>)> = (chunk_start..chunk_end)
                .into_par_iter()
                .map(|r| {
                    let k = match key_id {
                        Some(id) => source.field(r, id).map(|v| key_hash(&key_text(v))),
                        None => Some(NO_KEY),
                    };
                    (k, if own_time { source.time(r) } else { None }, if src_up { source.uptime(r) } else { None })
                })
                .collect();
            for (row, &(row_key, row_time, row_up)) in (chunk_start..chunk_end).zip(&pre) {
                // Signal events at or before this row.
                let mut up = None;
                if let Some(ev) = &mut up_events {
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
                } else {
                    up = row_up;
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
                let Some(k) = row_key else { continue };
                let begin = |out: &mut Lifecycles, by: Start, number: u32| {
                    let key = key_id
                        .and_then(|id| source.field(row, id))
                        .map(|v| String::from_utf8_lossy(&key_text(v)).into_owned());
                    out.lives.push(Life {
                        key,
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
                    out.by_key.entry(k).or_default().push(out.lives.len() - 1);
                    out.lives.len() - 1
                };
                let new_open = |life| Open { life, last_up: None, last_time: None, bsum: 0.0, bn: 0, marker: None };
                match open.remove(&k) {
                    None => {
                        let i = begin(&mut out, Start::First, 1);
                        open.insert(k, new_open(i));
                    }
                    Some(o) => {
                        let back = matches!((up, o.last_up), (Some(u), Some(p)) if u < p - tolerance);
                        let by = if is_start {
                            Some(Start::Query)
                        } else if reset_on && back {
                            Some(Start::Reset)
                        } else if matches!((self.gap, time, o.last_time), (Some(g), Some(t), Some(p)) if t - p > g) {
                            Some(Start::Gap)
                        } else {
                            None
                        };
                        match by {
                            Some(by) => {
                                let number = out.lives[o.life].number + 1;
                                let kept = matches!((up, o.last_up), (Some(u), Some(p)) if u >= p - tolerance);
                                close(&mut out, &o, if by == Start::Gap { Outcome::Lost } else { Outcome::Unclean });
                                let i = begin(&mut out, by, number);
                                if by != Start::Reset && kept {
                                    let (u, p) = (up.unwrap_or_default(), o.last_up.unwrap_or_default());
                                    out.findings.push(Finding {
                                    kind: FindingKind::FalseReset,
                                    row,
                                    life: i,
                                    detail: format!(
                                        "a lifecycle started here ({}) but the uptime kept counting ({p:.1} s → {u:.1} s)",
                                        by.as_str()
                                    ),
                                });
                                }
                                open.insert(k, new_open(i));
                            }
                            None => {
                                if back {
                                    let (u, p) = (up.unwrap_or_default(), o.last_up.unwrap_or_default());
                                    out.findings.push(Finding {
                                        kind: FindingKind::MissedReset,
                                        row,
                                        life: o.life,
                                        detail: format!(
                                            "uptime went back {p:.1} s → {u:.1} s, but no lifecycle started"
                                        ),
                                    });
                                }
                                open.insert(k, o);
                            }
                        }
                    }
                }
                let o = open.get_mut(&k).expect("opened above");
                let l = &mut out.lives[o.life];
                l.last = row;
                l.lines += 1;
                if time.is_some() {
                    l.last_time = time;
                    l.first_time = l.first_time.or(time);
                }
                if l.first_uptime.is_none() && up.is_some() && l.lines == 1 {
                    l.first_uptime = up;
                }
                if let (Some(t), Some(u)) = (time, up) {
                    let b = t - u;
                    l.boot = Some(l.boot.map_or(b, |x| x.min(b)));
                    o.bsum += b;
                    o.bn += 1;
                }
                if up.is_some() {
                    o.last_up = up;
                }
                if time.is_some() {
                    o.last_time = time;
                }
                if is_planned {
                    o.marker = Some((Outcome::Planned, row));
                }
                if is_crash {
                    o.marker = Some((Outcome::Crash, row));
                }
            }
            chunk_start = chunk_end;
        }
        for o in open.values() {
            close(&mut out, o, Outcome::Open);
        }
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
                });
            }
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

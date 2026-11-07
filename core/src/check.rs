//! Checks: tests that run on a log and say whether a known issue shows up,
//! or what a fact is, overall or per lifecycle, with the lines that show
//! it (owner, 2026-11-07: issues and facts, scopes, primitives; built in
//! small steps with the owner's feedback).
//!
//! A [`CheckDef`] has **its lines** (a query: NFD or a library query), a
//! scope (overall, or one result per lifecycle) and **tests** on its lines.
//! [`CheckDef::run`] gives a [`CheckResult`]: per scope a verdict and the
//! evidence (lines). [`Checks::run_all`] runs a tab's checks; each check
//! is also a signal (`@name`, [`signal_name`]: an event per occurrence),
//! which other checks, filters and the plot use (owner, 2026-11-07: checks
//! cascade).
//!
//! Spec text (CLI `--check`, the saved checks file):
//!
//! ```text
//! spec   := NAME ":" clause ( ";" clause )*
//! clause := "lines" RULE                     -- the lines it looks at
//!         | "per" LIFECYCLE                  -- a result per lifecycle
//!         | "issue" | "fact"                 -- what it is (default: from its test)
//!         | "any" | "all"                    -- several tests: an issue when one / all
//!                                            -- find one (default: all)
//!         | "labels" LABEL ( "," LABEL )*     -- groups, switched on and off together
//!         | test                             -- one or more (a fact has one)
//! test   := "exists"                         -- issue: each of its lines; fact: how many
//!         | "count" [ OP NUMBER ]            -- fact: how many; with OP: an issue when it holds
//!         | "rate" NUMBER "per" DUR          -- issue: more than NUMBER in a DUR window;
//!                                            -- fact: the most in one window
//!         | "gap" DUR                        -- issue: its lines stop for longer; fact: the longest stop
//!         | "then" RULE "within" DUR         -- a step: RULE's next line within DUR (after its
//!         | "missing" RULE "within" DUR      --   line, or the step before); steps make one
//!                                            --   sequence, `then` steps first. Issue: each line
//!                                            --   whose `then` steps happen and a `missing` step
//!                                            --   doesn't; fact: how often
//!         | "value" PICK [ OP NUMBER ]       -- fact: what its first named group (?<name>…)
//!                                            -- captures; with OP: an issue when it holds
//! PICK   := "first" | "last" | "min" | "max"
//! OP     := ">" | ">=" | "<" | "<=" | "==" | "!="
//! RULE   := NFD | "{" PACK.QUERY ( NAME "=" VALUE )* "}"
//! DUR    := NUMBER [ "ms" | "s" | "m" | "h" ]
//! ```
//!
//! Every test measures something; the kind says how it shows (owner,
//! 2026-11-07: issue or fact is a choice): a fact shows the measure, an
//! issue whether (and how often) the problem shows up.
//!
//! e.g. `stall: lines {net.down}; missing {ota.progress} within 30s; per ecu1`
//! or `temp: lines /temp=(?<t>\d+)/; value max; per ecu1`.
//!
//! A lifecycle's scope is its time span: from its first line to the next
//! one's ([`crate::lifecycle`]). Tests on time (`rate`, `gap`, `within`)
//! need the lines' times: the source's, or a `time` signal of the tab.
//! A check's queries may use the tab's `@signals` (the ones switched on).

use crate::dsl::{DslError, Query};
use crate::engine::search::{Filter, scan};
use crate::lifecycle::{Lifecycles, Rule, parse_duration};
use crate::pack::Library;
use crate::signal::{Clock, SignalDef, SignalKind, SignalValue, Signals, ValueRef, is_name};
use crate::{LineNo, LogSource, Progress};
use std::sync::Arc;

/// Evidence kept per scope at most (the count is exact).
pub const MAX_EVIDENCE: usize = 1000;

/// An issue or a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Issue,
    Fact,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Issue => "issue",
            Kind::Fact => "fact",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Ne,
}

impl Op {
    fn parse(s: &str) -> Option<Op> {
        Some(match s {
            ">" => Op::Gt,
            ">=" => Op::Ge,
            "<" => Op::Lt,
            "<=" => Op::Le,
            "==" => Op::Eq,
            "!=" => Op::Ne,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Eq => "==",
            Op::Ne => "!=",
        }
    }

    fn test(self, a: f64, b: f64) -> bool {
        match self {
            Op::Gt => a > b,
            Op::Ge => a >= b,
            Op::Lt => a < b,
            Op::Le => a <= b,
            Op::Eq => a == b,
            Op::Ne => a != b,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    First,
    Last,
    Min,
    Max,
}

impl Pick {
    fn as_str(self) -> &'static str {
        match self {
            Pick::First => "first",
            Pick::Last => "last",
            Pick::Min => "min",
            Pick::Max => "max",
        }
    }
}

/// One step of a sequence: the next line of `rule` after the previous
/// step's line, within `within`. A `then` step must happen for the
/// sequence to go on; a `missing` step is an expectation: the issue is
/// that it doesn't happen.
#[derive(Debug, Clone)]
pub struct Step {
    pub rule: Rule,
    pub within: f64,
    pub missing: bool,
}

impl Step {
    fn text(&self) -> String {
        let word = if self.missing { "missing" } else { "then" };
        format!("{word} {} within {}", self.rule.text(), crate::lifecycle::format_duration(self.within))
    }
}

/// What a check tests on its lines.
#[derive(Debug, Clone)]
pub enum Test {
    Exists,
    Count,
    Rate {
        max: u64,
        window: f64,
    },
    Gap(f64),
    /// `then` steps, then `missing` steps (owner, 2026-11-07: "a sequence
    /// of steps"); one `then` or `missing` is a sequence of one.
    Steps(Vec<Step>),
    Value(Pick),
}

impl Test {
    /// Its kind when the check doesn't say: a count or a value without a
    /// condition is a fact.
    fn default_kind(&self, cond: bool) -> Kind {
        match self {
            Test::Count | Test::Value(_) if !cond => Kind::Fact,
            _ => Kind::Issue,
        }
    }

    fn text(&self) -> String {
        let d = crate::lifecycle::format_duration;
        match self {
            Test::Exists => "exists".into(),
            Test::Count => "count".into(),
            Test::Rate { max, window } => format!("rate {max} per {}", d(*window)),
            Test::Gap(g) => format!("gap {}", d(*g)),
            Test::Steps(steps) => steps.iter().map(Step::text).collect::<Vec<_>>().join("; "),
            Test::Value(p) => format!("value {}", p.as_str()),
        }
    }

    fn rules(&self) -> Vec<&Rule> {
        match self {
            Test::Steps(steps) => steps.iter().map(|s| &s.rule).collect(),
            _ => Vec::new(),
        }
    }

    fn needs_time(&self) -> bool {
        matches!(self, Test::Rate { .. } | Test::Gap(_) | Test::Steps(_))
    }
}

/// One test of a check and its condition (a count's or a value's: `count
/// >= 3`, `value max > 90`).
#[derive(Debug, Clone)]
pub struct Part {
    pub test: Test,
    pub cond: Option<(Op, f64)>,
}

impl Part {
    fn text(&self) -> String {
        let mut t = self.test.text();
        if let Some((op, n)) = self.cond {
            t += &format!(" {} {n}", op.as_str());
        }
        t
    }
}

#[derive(Debug, Clone)]
pub struct CheckDef {
    name: String,
    lines: Option<Rule>,
    per: Option<String>,
    /// Its tests (owner, 2026-11-07: "several tests in one check"): an
    /// issue when all of them find one, or with `any` when one does.
    parts: Vec<Part>,
    any: bool,
    kind: Kind,
    labels: Vec<String>,
    /// Switched off: kept, not run.
    off: bool,
}

/// What a check covers: the whole log, or one lifecycle's span.
#[derive(Debug, Clone, PartialEq)]
pub struct Scope {
    /// `overall`, or `ecu1 #2`.
    pub label: String,
    /// The lifecycle's number, for a lifecycle's scope.
    pub life: Option<u32>,
    /// Its rows, `to` excluded.
    pub from: LineNo,
    pub to: LineNo,
}

/// A scope's verdict.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// An issue that doesn't show up.
    Ok,
    /// An issue that shows up this often.
    Issue(u64),
    /// A fact's value; `None`: nothing to tell (no lines).
    Fact(Option<String>),
}

/// A place that shows an issue (or a fact's value): its lines and a note.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    pub rows: Vec<LineNo>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopeResult {
    pub scope: Scope,
    pub verdict: Verdict,
    /// At most [`MAX_EVIDENCE`].
    pub evidence: Vec<Evidence>,
    /// Where each occurrence shows (its last line: where it is known),
    /// all of them, ascending: the check's signal.
    pub marks: Vec<LineNo>,
    /// A fact's measure as a signal value (rate, gap, value), at `marks`.
    pub measure: Option<SignalValue>,
}

/// A check's results: overall first, then one per lifecycle (`per`).
#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub name: String,
    pub kind: Kind,
    pub scopes: Vec<ScopeResult>,
    /// The lifecycle it runs per.
    pub per: Option<String>,
}

impl CheckResult {
    /// The overall result.
    pub fn overall(&self) -> &ScopeResult {
        &self.scopes[0]
    }

    /// The check as a signal of `source` (that it ran on), named
    /// [`signal_name`]: an event where each occurrence shows. Its value
    /// counts them (1, 2, 3, …; 0 before the first), per lifecycle for a
    /// check `per` one; a fact on a rate, a gap or a value carries the
    /// measure instead, where it was found.
    pub fn signal(&self, source: &dyn LogSource) -> SignalDef {
        // Per lifecycle: the lifecycles' scopes; else the whole log.
        let scopes = if self.per.is_some() { &self.scopes[1..] } else { &self.scopes[..1] };
        let measured = scopes.iter().any(|s| s.measure.is_some());
        let mut events = Vec::new();
        for s in scopes {
            for (i, &r) in s.marks.iter().enumerate() {
                let v = match &s.measure {
                    Some(m) => m.clone(),
                    None => SignalValue::Num((i + 1) as f64),
                };
                events.push((r, v));
            }
        }
        events.sort_by_key(|e| e.0);
        let kind =
            if events.iter().all(|e| matches!(e.1, SignalValue::Num(_))) { SignalKind::Num } else { SignalKind::Enum };
        let default = (!measured).then_some(SignalValue::Num(0.0));
        SignalDef::from_check(&signal_name(&self.name), kind, source, events, self.per.as_deref(), default)
    }
}

/// The name of a check's signal: its name in lower case, every run of
/// other characters than letters and digits one `_` (`Timeout unanswered`
/// is `@timeout_unanswered`).
pub fn signal_name(check: &str) -> String {
    let mut out = String::new();
    for c in check.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_owned();
    if out.is_empty() {
        "check".into()
    } else if out.starts_with(|c: char| c.is_ascii_digit()) {
        format!("check_{out}")
    } else {
        out
    }
}

impl CheckDef {
    /// Parse a spec line (module docs); error positions are byte offsets.
    pub fn parse_spec(spec: &str) -> Result<CheckDef, DslError> {
        let err = |msg: String, pos: usize| DslError { msg, pos };
        if let Some(rest) = spec.strip_prefix("off ") {
            return CheckDef::parse_spec(rest).map(|d| d.with_on(false)).map_err(|e| err(e.msg, e.pos + 4));
        }
        let Some(colon) = spec.find(':') else {
            return Err(err("expected NAME: followed by clauses (lines, per, and a test)".into(), 0));
        };
        let lead = spec.len() - spec.trim_start().len();
        let name = spec[..colon].trim();
        // Any text names a check (owner, 2026-11-07: "Fusa Faults from
        // aurix"); `:` and `;` end it and its clauses.
        if name.is_empty() || name.contains(';') {
            return Err(err("a check needs a name before ':'".into(), lead));
        }
        let (mut lines, mut per, mut parts, mut any): (_, _, Vec<Part>, _) = (None, None, Vec::new(), false);
        let (mut kind, mut labels): (Option<Kind>, Vec<String>) = (None, Vec::new());
        // `OP NUMBER` after a count or a value; `text` starts at `pos`.
        let condition = |text: &str, pos: usize| -> Result<(Op, f64), DslError> {
            let (op, n) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
            let op =
                Op::parse(op).ok_or_else(|| err(format!("expected >, >=, <, <=, == or != but found '{op}'"), pos))?;
            let n_at = pos + text.len() - n.trim_start().len();
            let n: f64 =
                n.trim().parse().map_err(|_| err(format!("expected a number but found '{}'", n.trim()), n_at))?;
            Ok((op, n))
        };
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
            let rule = |text: &str, pos: usize| Rule::parse(text).map_err(|e| err(e.msg, pos + e.pos));
            let dur = |text: &str, pos: usize| {
                parse_duration(text)
                    .filter(|d| *d > 0.0)
                    .ok_or_else(|| err(format!("expected a duration (30s, 500ms, 2m) but found '{text}'"), pos))
            };
            // `RULE within DUR`: the duration is the end.
            let within = |what: &str| -> Result<(Rule, f64), DslError> {
                let Some(i) = rest.to_ascii_lowercase().rfind(" within ") else {
                    return Err(err(format!("expected '{what} QUERY within DURATION'"), rest_at));
                };
                let d = rest[i + " within ".len()..].trim();
                Ok((rule(&rest[..i], rest_at)?, dur(d, rest_at + rest.len() - d.len())?))
            };
            let mut cond = None;
            let t = match word.to_ascii_lowercase().as_str() {
                "lines" => {
                    if rest.is_empty() {
                        return Err(err("'lines' needs a query".into(), start + word.len()));
                    }
                    lines = Some(rule(rest, rest_at)?);
                    continue;
                }
                "per" => {
                    if !is_name(rest) {
                        return Err(err(format!("'per' needs a lifecycle's name but found '{rest}'"), rest_at));
                    }
                    per = Some(rest.to_ascii_lowercase());
                    continue;
                }
                "issue" | "fact" if rest.is_empty() => {
                    kind = Some(if word.eq_ignore_ascii_case("issue") { Kind::Issue } else { Kind::Fact });
                    continue;
                }
                "any" | "all" if rest.is_empty() => {
                    any = word.eq_ignore_ascii_case("any");
                    continue;
                }
                "labels" => {
                    labels = rest.split(',').map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect();
                    if labels.is_empty() {
                        return Err(err("'labels' needs a label".into(), start + word.len()));
                    }
                    continue;
                }
                "exists" if rest.is_empty() => Test::Exists,
                "count" => {
                    if !rest.is_empty() {
                        cond = Some(condition(rest, rest_at)?);
                    }
                    Test::Count
                }
                "rate" => {
                    let mut w = rest.splitn(3, char::is_whitespace);
                    let (n, p, d) = (w.next().unwrap_or(""), w.next().unwrap_or(""), w.next().unwrap_or("").trim());
                    let max = n.parse().map_err(|_| err(format!("expected a count but found '{n}'"), rest_at))?;
                    if !p.eq_ignore_ascii_case("per") {
                        return Err(err("expected 'rate COUNT per DURATION'".into(), rest_at));
                    }
                    Test::Rate { max, window: dur(d, rest_at + rest.len() - d.len())? }
                }
                "gap" => Test::Gap(dur(rest, rest_at)?),
                // Steps join the check's sequence: `then` steps, then
                // `missing` ones.
                w @ ("then" | "missing") => {
                    let missing = w == "missing";
                    let (rule, within) = within(w)?;
                    let step = Step { rule, within, missing };
                    match parts.iter_mut().find_map(|p| match &mut p.test {
                        Test::Steps(s) => Some(s),
                        _ => None,
                    }) {
                        Some(steps) => {
                            if !missing && steps.last().is_some_and(|s| s.missing) {
                                return Err(err(
                                    "a 'then' step can't follow a 'missing' one (what must happen comes first)".into(),
                                    start,
                                ));
                            }
                            steps.push(step);
                        }
                        None => parts.push(Part { test: Test::Steps(vec![step]), cond: None }),
                    }
                    continue;
                }
                "value" => {
                    let (pick, more) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                    let p = match pick.to_ascii_lowercase().as_str() {
                        "first" => Pick::First,
                        "last" => Pick::Last,
                        "min" => Pick::Min,
                        "max" => Pick::Max,
                        _ => return Err(err(format!("expected first, last, min or max but found '{pick}'"), rest_at)),
                    };
                    if !more.trim().is_empty() {
                        cond = Some(condition(more.trim(), rest_at + rest.len() - more.trim_start().len())?);
                    }
                    Test::Value(p)
                }
                w => {
                    return Err(err(
                        format!(
                            "unknown clause '{w}' (lines, per, issue, fact, labels, any, all; exists, count, rate, \
                             gap, then, missing, value)"
                        ),
                        start,
                    ));
                }
            };
            parts.push(Part { test: t, cond });
        }
        let Some(lines) = lines else {
            return Err(err("a check needs its lines: lines QUERY".into(), spec.len()));
        };
        if parts.is_empty() {
            parts.push(Part { test: Test::Exists, cond: None });
        }
        let kind = kind.unwrap_or(default_kind(&parts));
        if kind == Kind::Fact && parts.len() > 1 {
            return Err(err("a fact has one test (several tests make an issue)".into(), spec.len()));
        }
        if kind == Kind::Issue && parts.iter().any(|p| matches!(p.test, Test::Value(_)) && p.cond.is_none()) {
            return Err(err("an issue on a value needs a condition, e.g. value max > 90".into(), spec.len()));
        }
        Ok(CheckDef { name: name.to_owned(), lines: Some(lines), per, parts, any, kind, labels, off: false })
    }

    /// The spec line [`Self::parse_spec`] reads back.
    pub fn spec(&self) -> String {
        let mut c = Vec::new();
        if let Some(l) = &self.lines {
            c.push(format!("lines {}", l.text()));
        }
        c.extend(self.parts.iter().map(Part::text));
        if self.any {
            c.push("any".into());
        }
        if self.kind != default_kind(&self.parts) {
            c.push(self.kind.as_str().to_owned());
        }
        if let Some(p) = &self.per {
            c.push(format!("per {p}"));
        }
        if !self.labels.is_empty() {
            c.push(format!("labels {}", self.labels.join(", ")));
        }
        format!("{}{}: {}", if self.off { "off " } else { "" }, self.name, c.join("; "))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    pub fn per(&self) -> Option<&str> {
        self.per.as_deref()
    }

    pub fn is_on(&self) -> bool {
        !self.off
    }

    pub fn with_on(mut self, on: bool) -> CheckDef {
        self.off = !on;
        self
    }

    /// Its lines' query, as written.
    pub fn lines_text(&self) -> String {
        self.lines.as_ref().map(Rule::text).unwrap_or_default()
    }

    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// Several tests: an issue when any of them finds one (else all).
    pub fn is_any(&self) -> bool {
        self.any
    }

    /// Its library queries expanded with `lib`.
    pub fn resolved(&self, lib: &Library) -> Result<CheckDef, String> {
        let mut out = self.clone();
        if let Some(l) = &self.lines {
            out.lines = Some(l.resolved(lib)?);
        }
        for p in &mut out.parts {
            if let Test::Steps(steps) = &mut p.test {
                for s in steps {
                    s.rule = s.rule.resolved(lib)?;
                }
            }
        }
        Ok(out)
    }

    /// Every query it has: its lines', then its steps'.
    fn rules(&self) -> Vec<&Rule> {
        self.lines.iter().chain(self.parts.iter().flat_map(|p| p.test.rules())).collect()
    }

    /// The library queries it uses, not resolved yet.
    pub fn unresolved(&self) -> Vec<&str> {
        self.rules().into_iter().filter(|r| r.query().is_none()).filter_map(Rule::library_id).collect()
    }

    /// The signals its queries use (`@name`), each once.
    pub fn signals_used(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in self.rules().into_iter().filter_map(Rule::query).flat_map(Query::signal_names) {
            if !out.contains(&n) {
                out.push(n);
            }
        }
        out
    }

    /// The lines of `rule`; its `@signal` conditions read the tab's
    /// `signals` (the ones switched on).
    fn hits(
        &self,
        rule: Option<&Rule>,
        source: &dyn LogSource,
        signals: &Signals,
        progress: &Progress,
    ) -> crate::Result<Vec<LineNo>> {
        let Some(rule) = rule else { return Ok(Vec::new()) };
        let Some(q) = rule.query() else {
            return Err(self.fail(format!("library query {} isn't loaded (load its pack)", rule.text())));
        };
        let q = q.clone().with_signals(signals).and_then(|q| q.bind(source.schema())).map_err(|e| self.fail(e.msg))?;
        let filter = Filter::Query(q).prepare(source, progress)?;
        scan(source, &filter, progress)
    }

    fn fail(&self, msg: String) -> crate::Error {
        crate::Error::Format(format!("check '{}': {msg}", self.name))
    }

    /// Run it on `source` with the tab's `signals` (its lifecycle, a time
    /// signal, other checks' signals): overall, and per lifecycle if it has
    /// `per`.
    pub fn run(&self, source: &dyn LogSource, signals: &Signals, progress: &Progress) -> crate::Result<CheckResult> {
        let rows = self.hits(self.lines.as_ref(), source, signals, progress)?;
        // Each step's lines, per test.
        let mut others: Vec<Vec<Vec<LineNo>>> = Vec::with_capacity(self.parts.len());
        for p in &self.parts {
            let mut v = Vec::new();
            for r in p.test.rules() {
                v.push(self.hits(Some(r), source, signals, progress)?);
            }
            others.push(v);
        }
        // Times, where a test needs them.
        let clock = Clock::of(source, signals);
        let clock_series = match &clock {
            Clock::Signal(d) => Some(d.series(source, progress)?),
            _ => None,
        };
        if self.parts.iter().any(|p| p.test.needs_time()) && !clock.is_time() {
            return Err(self.fail("its test needs times, and these lines have none (define a time signal)".into()));
        }
        let time = |r: LineNo| clock.at(source, clock_series.as_ref(), r);
        // Values, for `value`: what its first named group captures.
        let mut values = Vec::with_capacity(self.parts.len());
        for p in &self.parts {
            values.push(match &p.test {
                Test::Value(pick) => {
                    let q = self.lines.as_ref().and_then(Rule::query).expect("hits checked it");
                    let kind = if matches!(pick, Pick::Min | Pick::Max) { SignalKind::Num } else { SignalKind::Enum };
                    let def = capture(q, kind).map_err(|m| self.fail(m))?;
                    Some(def.series(source, progress)?)
                }
                _ => None,
            });
        }
        // Scopes: overall, then each lifecycle's span.
        let n = source.line_count();
        let mut scopes = vec![Scope { label: "overall".into(), life: None, from: 0, to: n }];
        if let Some(p) = &self.per {
            let Some(def) = signals.lifecycles().find(|l| l.name() == p) else {
                return Err(self.fail(format!("no lifecycle '{p}' here")));
            };
            let lives: Arc<Lifecycles> = def.lifecycles(source, progress)?;
            for (i, l) in lives.lives.iter().enumerate() {
                let to = lives.lives.get(i + 1).map_or(n, |x| x.first);
                scopes.push(Scope { label: format!("{p} #{}", l.number), life: Some(l.number), from: l.first, to });
            }
        }
        let mut out = Vec::with_capacity(scopes.len());
        for scope in scopes {
            if progress.is_cancelled() {
                return Err(crate::Error::Cancelled);
            }
            let within = |v: &[LineNo]| {
                let a = v.partition_point(|&r| r < scope.from);
                let b = v.partition_point(|&r| r < scope.to);
                (a, b)
            };
            let (a, b) = within(&rows);
            let mine = &rows[a..b];
            // The scope's last time: a missing step is only judged when the
            // scope lasts long enough.
            let end = (scope.from..scope.to).rev().take(1000).find_map(time);
            let mut judged = Vec::with_capacity(self.parts.len());
            for (i, p) in self.parts.iter().enumerate() {
                let theirs: Vec<&[LineNo]> = others[i]
                    .iter()
                    .map(|v| {
                        let (a, b) = within(v);
                        &v[a..b]
                    })
                    .collect();
                let cx = Cx { mine, theirs: &theirs, time: &time, end, values: values[i].as_ref(), scope: &scope };
                judged.push(self.judge(p, &cx));
            }
            let j = self.combine(judged);
            out.push(ScopeResult {
                scope,
                verdict: j.verdict,
                evidence: j.evidence,
                marks: j.marks,
                measure: j.measure,
            });
        }
        Ok(CheckResult { name: self.name.clone(), kind: self.kind(), scopes: out, per: self.per.clone() })
    }

    /// Several tests' verdicts on one scope as the check's: an issue when
    /// all (`any`: one) find one, as often as they do together.
    fn combine(&self, mut judged: Vec<Judged>) -> Judged {
        if judged.len() == 1 {
            return judged.pop().expect("one");
        }
        let holds: Vec<bool> = judged.iter().map(|j| matches!(j.verdict, Verdict::Issue(_))).collect();
        let found = if self.any { holds.iter().any(|&h| h) } else { holds.iter().all(|&h| h) };
        if !found {
            return Judged::ok();
        }
        let (mut count, mut evidence, mut marks) = (0u64, Vec::new(), Vec::new());
        for ((p, j), _) in self.parts.iter().zip(judged).zip(&holds).filter(|(_, h)| **h) {
            if let Verdict::Issue(n) = j.verdict {
                count += n;
            }
            let what = p.text();
            for e in j.evidence {
                if evidence.len() < MAX_EVIDENCE {
                    let note = if e.note.is_empty() { what.clone() } else { format!("{what}: {}", e.note) };
                    evidence.push(Evidence { rows: e.rows, note });
                }
            }
            marks.extend(j.marks);
        }
        marks.sort_unstable();
        marks.dedup();
        Judged { verdict: Verdict::Issue(count), evidence, marks, measure: None }
    }

    /// One test's verdict on one scope.
    fn judge(&self, part: &Part, cx: &Cx<'_>) -> Judged {
        let Cx { mine, theirs, time, end, values, scope } = *cx;
        let d = shown_duration;
        let mut ev: Vec<Evidence> = Vec::new();
        let mut marks: Vec<LineNo> = Vec::new();
        let mut count = 0u64;
        // An occurrence: counted, its evidence kept up to the cap, its
        // last line marked.
        let mut add = |ev: &mut Vec<Evidence>, marks: &mut Vec<LineNo>, rows: Vec<LineNo>, note: String| {
            count += 1;
            marks.push(*rows.last().expect("rows"));
            if ev.len() < MAX_EVIDENCE {
                ev.push(Evidence { rows, note });
            }
        };
        let issue = |count: u64, evidence: Vec<Evidence>, mut marks: Vec<LineNo>| {
            marks.sort_unstable();
            let verdict = if count == 0 { Verdict::Ok } else { Verdict::Issue(count) };
            Judged { verdict, evidence, marks, measure: None }
        };
        let fact = |v: String, evidence: Vec<Evidence>, marks: Vec<LineNo>, measure: Option<SignalValue>| Judged {
            verdict: Verdict::Fact(Some(v)),
            evidence,
            marks,
            measure,
        };
        let as_fact = self.kind == Kind::Fact;
        let first = || mine.first().map(|&r| Evidence { rows: vec![r], note: "the first".into() });
        match &part.test {
            Test::Exists | Test::Count if as_fact => {
                fact(mine.len().to_string(), first().into_iter().collect(), mine.to_vec(), None)
            }
            Test::Exists => {
                for &r in mine {
                    add(&mut ev, &mut marks, vec![r], String::new());
                }
                issue(count, ev, marks)
            }
            Test::Count => {
                // An issue where the condition holds (none: any line): as
                // often as there are lines, each one evidence (bug, owner,
                // 2026-11-07: it said 1x, its lines chained in one).
                let n = mine.len() as f64;
                let holds = match part.cond {
                    Some((op, k)) => op.test(n, k),
                    None => n > 0.0,
                };
                if !holds || mine.is_empty() {
                    return Judged::ok();
                }
                let why = part.cond.map(|(op, k)| format!("{n} lines {} {k}", op.as_str())).unwrap_or_default();
                for &r in mine {
                    add(&mut ev, &mut marks, vec![r], why.clone());
                }
                issue(count, ev, marks)
            }
            Test::Rate { max, window } => {
                // More than `max` in a window: one burst per stretch; as a
                // fact, the most in one window.
                let ts: Vec<(LineNo, f64)> = mine.iter().filter_map(|&r| Some((r, time(r)?))).collect();
                let (mut lo, mut burst_end, mut most) = (0usize, f64::NEG_INFINITY, (0usize, 0usize, 0usize));
                for hi in 0..ts.len() {
                    while ts[hi].1 - ts[lo].1 > *window {
                        lo += 1;
                    }
                    let n = hi - lo + 1;
                    if n > most.0 {
                        most = (n, lo, hi);
                    }
                    if n as u64 > *max && ts[lo].1 > burst_end {
                        burst_end = ts[hi].1 + window;
                        add(&mut ev, &mut marks, vec![ts[lo].0, ts[hi].0], format!("{n} lines within {}", d(*window)));
                    }
                }
                if !as_fact {
                    return issue(count, ev, marks);
                }
                if most.0 == 0 {
                    return fact("0".into(), Vec::new(), Vec::new(), None);
                }
                let (a, b) = (ts[most.1].0, ts[most.2].0);
                let note = format!("{} within {}", most.0, d(*window));
                let m = Some(SignalValue::Num(most.0 as f64));
                fact(most.0.to_string(), vec![Evidence { rows: vec![a, b], note }], vec![b], m)
            }
            Test::Gap(g) => {
                let ts: Vec<(LineNo, f64)> = mine.iter().filter_map(|&r| Some((r, time(r)?))).collect();
                let mut longest: Option<(f64, LineNo, LineNo)> = None;
                for w in ts.windows(2) {
                    let dt = w[1].1 - w[0].1;
                    if longest.is_none_or(|l| dt > l.0) {
                        longest = Some((dt, w[0].0, w[1].0));
                    }
                    if dt > *g {
                        add(&mut ev, &mut marks, vec![w[0].0, w[1].0], format!("none for {}", d(dt)));
                    }
                }
                match (as_fact, longest) {
                    (true, Some((dt, a, b))) => {
                        let e = Evidence { rows: vec![a, b], note: format!("none for {}", d(dt)) };
                        fact(d(dt), vec![e], vec![b], Some(SignalValue::Num(dt)))
                    }
                    (true, None) => Judged { verdict: Verdict::Fact(None), ..Judged::ok() },
                    (false, _) => issue(count, ev, marks),
                }
            }
            Test::Steps(steps) => {
                let one = steps.len() == 1;
                let expects = steps.iter().any(|s| s.missing);
                for &r in mine {
                    let Some(t0) = time(r) else { continue };
                    let (mut cur, mut tcur) = (r, t0);
                    let (mut rows, mut notes) = (vec![r], Vec::new());
                    // `Some(true)`: an occurrence; `Some(false)`: none.
                    let mut found = None;
                    for (k, s) in steps.iter().enumerate() {
                        let theirs = theirs[k];
                        let j = theirs.partition_point(|&x| x <= cur);
                        let next = theirs[j..].iter().find_map(|&x| Some((x, time(x)?)));
                        let next = next.filter(|(_, tx)| tx - tcur <= s.within);
                        match (s.missing, next) {
                            (false, Some((x, tx))) => {
                                notes.push(if one {
                                    format!("followed after {}", d(tx - tcur))
                                } else {
                                    format!("step {} after {}", k + 2, d(tx - tcur))
                                });
                                rows.push(x);
                                (cur, tcur) = (x, tx);
                            }
                            (false, None) => {
                                found = Some(false);
                                break;
                            }
                            // An expectation met: on to the next step.
                            (true, Some((x, tx))) => {
                                rows.push(x);
                                (cur, tcur) = (x, tx);
                            }
                            (true, None) => {
                                let judged = end.is_some_and(|e| e - tcur >= s.within);
                                if judged {
                                    notes.push(if one {
                                        format!("nothing followed within {}", d(s.within))
                                    } else {
                                        format!("no step {} within {}", k + 2, d(s.within))
                                    });
                                }
                                found = Some(judged);
                                break;
                            }
                        }
                    }
                    // Every step went as it says: only `then` steps make an
                    // occurrence; met expectations don't.
                    if found.unwrap_or(!expects) {
                        add(&mut ev, &mut marks, rows, notes.join(", "));
                    }
                }
                if as_fact { fact(count.to_string(), ev, marks, None) } else { issue(count, ev, marks) }
            }
            Test::Value(p) => {
                let Some(series) = values else { return Judged { verdict: Verdict::Fact(None), ..Judged::ok() } };
                let evs: Vec<(LineNo, ValueRef<'_>)> =
                    series.iter().filter(|(r, _)| *r >= scope.from && *r < scope.to).collect();
                let pick = match p {
                    Pick::First => evs.first().copied(),
                    Pick::Last => evs.last().copied(),
                    Pick::Min | Pick::Max => evs
                        .iter()
                        .filter_map(|&(r, v)| match v {
                            ValueRef::Num(n) => Some((r, n)),
                            ValueRef::Enum(_) => None,
                        })
                        .reduce(|a, b| if (*p == Pick::Max) == (b.1 > a.1) { b } else { a })
                        .map(|(r, n)| (r, ValueRef::Num(n))),
                };
                let Some((r, v)) = pick else {
                    return if as_fact {
                        Judged { verdict: Verdict::Fact(None), ..Judged::ok() }
                    } else {
                        Judged::ok()
                    };
                };
                let text = match v {
                    ValueRef::Num(n) => n.to_string(),
                    ValueRef::Enum(t) => String::from_utf8_lossy(t).into_owned(),
                };
                let ev = vec![Evidence { rows: vec![r], note: format!("the {}: {text}", p.as_str()) }];
                if as_fact {
                    return fact(text, ev, vec![r], Some(v.to_value()));
                }
                // A value that isn't a number holds no condition.
                match (text.trim().parse::<f64>(), part.cond) {
                    (Ok(v), Some((op, n))) if op.test(v, n) => {
                        Judged { verdict: Verdict::Issue(1), evidence: ev, marks: vec![r], measure: None }
                    }
                    _ => Judged::ok(),
                }
            }
        }
    }
}

/// What a test looks at in one scope.
#[derive(Clone, Copy)]
struct Cx<'a> {
    /// Its lines in the scope.
    mine: &'a [LineNo],
    /// Each step's lines in the scope.
    theirs: &'a [&'a [LineNo]],
    time: &'a dyn Fn(LineNo) -> Option<f64>,
    /// The scope's last time.
    end: Option<f64>,
    values: Option<&'a crate::SignalSeries>,
    scope: &'a Scope,
}

/// One test's verdict on one scope.
struct Judged {
    verdict: Verdict,
    evidence: Vec<Evidence>,
    marks: Vec<LineNo>,
    measure: Option<SignalValue>,
}

impl Judged {
    fn ok() -> Judged {
        Judged { verdict: Verdict::Ok, evidence: Vec::new(), marks: Vec::new(), measure: None }
    }
}

/// A check's kind when it doesn't say: its test's (several tests: an issue).
fn default_kind(parts: &[Part]) -> Kind {
    match parts {
        [p] => p.test.default_kind(p.cond.is_some()),
        _ => Kind::Issue,
    }
}

/// A duration for people: `315 ms`, `4.2 s`, `3 min 12 s`, `1 h 5 min`.
pub fn shown_duration(d: f64) -> String {
    if d < 1.0 {
        format!("{:.0} ms", d * 1000.0)
    } else if d < 60.0 {
        // To the ms: a stop of 1.04 s shown as "1.0 s" looked shorter than
        // the 1 s it exceeded.
        format!("{:.3} s", d)
    } else if d < 3600.0 {
        format!("{} min {} s", (d / 60.0) as u64, (d % 60.0) as u64)
    } else {
        format!("{} h {} min", (d / 3600.0) as u64, ((d % 3600.0) / 60.0) as u64)
    }
}

/// A value read by `q`'s first named group, as a signal (not one of the tab).
fn capture(q: &Query, kind: SignalKind) -> Result<SignalDef, String> {
    let group = crate::signal::first_group(q)
        .ok_or("'value' needs a named group in its lines' query, e.g. /temp=(?<t>\\d+)/")?;
    SignalDef::new("value", kind, q.source(), Some(&group)).map_err(|e| e.to_string())
}

/// What [`Checks::run_all`] found: each check's result (by name), and the
/// tab's signals plus the checks' own.
#[derive(Debug, Clone, Default)]
pub struct CheckRun {
    pub results: Vec<(String, Result<CheckResult, String>)>,
    pub signals: Signals,
}

/// The checks of a tab, by unique name; saved as spec lines.
#[derive(Debug, Clone, Default)]
pub struct Checks(Vec<CheckDef>);

impl Checks {
    pub fn iter(&self) -> impl Iterator<Item = &CheckDef> {
        self.0.iter()
    }

    pub fn get(&self, name: &str) -> Option<&CheckDef> {
        self.0.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Add `c`, replacing one of the same name.
    pub fn set(&mut self, c: CheckDef) {
        match self.0.iter_mut().find(|d| d.name.eq_ignore_ascii_case(&c.name)) {
            Some(d) => *d = c,
            None => self.0.push(c),
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.0.retain(|c| !c.name.eq_ignore_ascii_case(name));
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every label, sorted, each once.
    pub fn labels(&self) -> Vec<String> {
        let mut out: Vec<String> = self.0.iter().flat_map(|c| c.labels.iter().cloned()).collect();
        out.sort();
        out.dedup();
        out
    }

    /// Switch every check with `label` on or off.
    pub fn set_label_on(&mut self, label: &str, on: bool) {
        for c in self.0.iter_mut().filter(|c| c.labels.iter().any(|l| l == label)) {
            c.off = !on;
        }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Run every check that is on, on `source` with the tab's `signals`
    /// and library queries from `lib`: a check that uses another's signal
    /// runs after it (owner, 2026-11-07: checks cascade). The results, in
    /// the list's order, and the signals with each check's added (where no
    /// signal of the tab has its name).
    pub fn run_all(
        &self,
        source: &dyn LogSource,
        signals: &Signals,
        lib: &Library,
        progress: &Progress,
    ) -> crate::Result<CheckRun> {
        let on: Vec<&CheckDef> = self.0.iter().filter(|c| c.is_on()).collect();
        let defs: Vec<Result<CheckDef, String>> =
            on.iter().map(|c| c.resolved(lib).map_err(|e| format!("check '{}': {e}", c.name))).collect();
        // The checks whose signals each uses.
        let own: Vec<String> = on.iter().map(|c| signal_name(&c.name)).collect();
        let theirs = |n: &str| signals.get(n).is_none().then(|| own.iter().position(|o| o == n)).flatten();
        let needs: Vec<Vec<usize>> = defs
            .iter()
            .map(|d| {
                d.as_ref().map(|d| d.signals_used().iter().filter_map(|n| theirs(n)).collect()).unwrap_or_default()
            })
            .collect();
        let mut results: Vec<Option<Result<CheckResult, String>>> = vec![None; on.len()];
        let mut sig = signals.clone();
        loop {
            let ready = (0..on.len()).find(|&i| results[i].is_none() && needs[i].iter().all(|&j| results[j].is_some()));
            let Some(i) = ready else { break };
            let failed = needs[i].iter().find(|&&j| results[j].as_ref().is_some_and(|r| r.is_err()));
            let r = match (&defs[i], failed) {
                (Err(e), _) => Err(e.clone()),
                (Ok(_), Some(&j)) => {
                    Err(format!("check '{}': uses @{} of check '{}', which failed", on[i].name, own[j], on[j].name))
                }
                (Ok(d), None) => match d.run(source, &sig, progress) {
                    Err(crate::Error::Cancelled) => return Err(crate::Error::Cancelled),
                    r => r.map_err(|e| e.to_string()),
                },
            };
            if let Ok(res) = &r
                && signals.get(&own[i]).is_none()
            {
                sig.set(res.signal(source));
            }
            results[i] = Some(r);
        }
        // What is left uses itself, or checks that use it.
        let results = results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                let r = r.unwrap_or_else(|| {
                    let names: Vec<String> = needs[i].iter().map(|&j| format!("@{}", own[j])).collect();
                    Err(format!(
                        "check '{}': uses {}, which come back to it (checks can't use each other in a circle)",
                        on[i].name,
                        names.join(", ")
                    ))
                });
                (on[i].name.clone(), r)
            })
            .collect();
        Ok(CheckRun { results, signals: sig })
    }

    pub fn to_text(&self) -> String {
        let mut out = String::from("# nerka checks: [off] NAME: lines QUERY; TEST[; per LIFECYCLE], one per line\n");
        for c in &self.0 {
            out += &c.spec();
            out.push('\n');
        }
        out
    }

    /// Read a saved file: the checks, and an error per line that isn't one.
    pub fn parse_text(text: &str) -> (Checks, Vec<String>) {
        let (mut s, mut errors) = (Checks::default(), Vec::new());
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match CheckDef::parse_spec(line) {
                Ok(c) => s.set(c),
                Err(e) => errors.push(format!("line {}: {} (at column {})", i + 1, e.msg, e.pos + 1)),
            }
        }
        (s, errors)
    }
}

#[cfg(test)]
mod tests;

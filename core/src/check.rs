//! Checks: tests that run on a log and say whether a known issue shows up,
//! or what a fact is, overall or per lifecycle, with the lines that show
//! it (owner, 2026-11-07: issues and facts, scopes, primitives; built in
//! small steps with the owner's feedback).
//!
//! A [`CheckDef`] has **its lines** (a query: NFD or a library query), a
//! scope (overall, or one result per lifecycle) and one **test** on its
//! lines. [`CheckDef::run`] gives a [`CheckResult`]: per scope a verdict
//! and the evidence (lines).
//!
//! Spec text (CLI `--check`, the saved checks file):
//!
//! ```text
//! spec   := NAME ":" clause ( ";" clause )*
//! clause := "lines" RULE                     -- the lines it looks at
//!         | "per" LIFECYCLE                  -- a result per lifecycle
//!         | "issue" | "fact"                 -- what it is (default: from its test)
//!         | "labels" LABEL ( "," LABEL )*     -- groups, switched on and off together
//!         | test
//! test   := "exists"                         -- issue: each of its lines; fact: how many
//!         | "count" [ OP NUMBER ]            -- fact: how many; with OP: an issue when it holds
//!         | "rate" NUMBER "per" DUR          -- issue: more than NUMBER in a DUR window;
//!                                            -- fact: the most in one window
//!         | "gap" DUR                        -- issue: its lines stop for longer; fact: the longest stop
//!         | "then" RULE "within" DUR         -- issue: one of its lines, then RULE within DUR;
//!         | "missing" RULE "within" DUR      --   or no RULE within DUR; fact: how often
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

use crate::dsl::{DslError, Query};
use crate::engine::search::{Filter, scan};
use crate::lifecycle::{Lifecycles, Rule, parse_duration};
use crate::pack::Library;
use crate::signal::{Clock, SignalDef, SignalKind, Signals, ValueRef, is_name};
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

/// What a check tests on its lines.
#[derive(Debug, Clone)]
pub enum Test {
    Exists,
    Count,
    Rate { max: u64, window: f64 },
    Gap(f64),
    Then { rule: Rule, within: f64 },
    Missing { rule: Rule, within: f64 },
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
            Test::Then { rule, within } => format!("then {} within {}", rule.text(), d(*within)),
            Test::Missing { rule, within } => format!("missing {} within {}", rule.text(), d(*within)),
            Test::Value(p) => format!("value {}", p.as_str()),
        }
    }

    fn rule(&self) -> Option<&Rule> {
        match self {
            Test::Then { rule, .. } | Test::Missing { rule, .. } => Some(rule),
            _ => None,
        }
    }

    fn needs_time(&self) -> bool {
        matches!(self, Test::Rate { .. } | Test::Gap(_) | Test::Then { .. } | Test::Missing { .. })
    }
}

#[derive(Debug, Clone)]
pub struct CheckDef {
    name: String,
    lines: Option<Rule>,
    per: Option<String>,
    test: Test,
    /// A condition on a count or a value (`count >= 3`, `value max > 90`).
    cond: Option<(Op, f64)>,
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
}

/// A check's results: overall first, then one per lifecycle (`per`).
#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub name: String,
    pub kind: Kind,
    pub scopes: Vec<ScopeResult>,
}

impl CheckResult {
    /// The overall result.
    pub fn overall(&self) -> &ScopeResult {
        &self.scopes[0]
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
        let (mut lines, mut per, mut test) = (None, None, None);
        let (mut cond, mut kind, mut labels): (Option<(Op, f64)>, Option<Kind>, Vec<String>) = (None, None, Vec::new());
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
                "then" => {
                    let (rule, within) = within("then")?;
                    Test::Then { rule, within }
                }
                "missing" => {
                    let (rule, within) = within("missing")?;
                    Test::Missing { rule, within }
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
                            "unknown clause '{w}' (lines, per, issue, fact, labels; exists, count, rate, gap, then, \
                             missing, value)"
                        ),
                        start,
                    ));
                }
            };
            if test.is_some() {
                return Err(err("a check has one test".into(), start));
            }
            test = Some(t);
        }
        let Some(lines) = lines else {
            return Err(err("a check needs its lines: lines QUERY".into(), spec.len()));
        };
        let test = test.unwrap_or(Test::Exists);
        let kind = kind.unwrap_or(test.default_kind(cond.is_some()));
        if kind == Kind::Issue && matches!(test, Test::Value(_)) && cond.is_none() {
            return Err(err("an issue on a value needs a condition, e.g. value max > 90".into(), spec.len()));
        }
        Ok(CheckDef { name: name.to_owned(), lines: Some(lines), per, test, cond, kind, labels, off: false })
    }

    /// The spec line [`Self::parse_spec`] reads back.
    pub fn spec(&self) -> String {
        let mut c = Vec::new();
        if let Some(l) = &self.lines {
            c.push(format!("lines {}", l.text()));
        }
        let mut test = self.test.text();
        if let Some((op, n)) = self.cond {
            test += &format!(" {} {n}", op.as_str());
        }
        c.push(test);
        if self.kind != self.test.default_kind(self.cond.is_some()) {
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

    /// Its library queries expanded with `lib`.
    pub fn resolved(&self, lib: &Library) -> Result<CheckDef, String> {
        let mut out = self.clone();
        if let Some(l) = &self.lines {
            out.lines = Some(l.resolved(lib)?);
        }
        out.test = match &self.test {
            Test::Then { rule, within } => Test::Then { rule: rule.resolved(lib)?, within: *within },
            Test::Missing { rule, within } => Test::Missing { rule: rule.resolved(lib)?, within: *within },
            t => t.clone(),
        };
        Ok(out)
    }

    /// The library queries it uses, not resolved yet.
    pub fn unresolved(&self) -> Vec<&str> {
        [self.lines.as_ref(), self.test.rule()]
            .into_iter()
            .flatten()
            .filter(|r| r.query().is_none())
            .filter_map(Rule::library_id)
            .collect()
    }

    fn hits(&self, rule: Option<&Rule>, source: &dyn LogSource, progress: &Progress) -> crate::Result<Vec<LineNo>> {
        let Some(rule) = rule else { return Ok(Vec::new()) };
        let Some(q) = rule.query() else {
            return Err(self.fail(format!("library query {} isn't loaded (load its pack)", rule.text())));
        };
        let q = q.clone().bind(source.schema()).map_err(|e| self.fail(e.msg))?;
        scan(source, &Filter::Query(q), progress)
    }

    fn fail(&self, msg: String) -> crate::Error {
        crate::Error::Format(format!("check '{}': {msg}", self.name))
    }

    /// Run it on `source` with the tab's `signals` (its lifecycle, a time
    /// signal): overall, and per lifecycle if it has `per`.
    pub fn run(&self, source: &dyn LogSource, signals: &Signals, progress: &Progress) -> crate::Result<CheckResult> {
        let rows = self.hits(self.lines.as_ref(), source, progress)?;
        let other = self.hits(self.test.rule(), source, progress)?;
        // Times, where the test needs them.
        let clock = Clock::of(source, signals);
        let clock_series = match &clock {
            Clock::Signal(d) => Some(d.series(source, progress)?),
            _ => None,
        };
        if self.test.needs_time() && !clock.is_time() {
            return Err(self.fail("its test needs times, and these lines have none (define a time signal)".into()));
        }
        let time = |r: LineNo| clock.at(source, clock_series.as_ref(), r);
        // Values, for `value`: what its first named group captures.
        let values = match &self.test {
            Test::Value(p) => {
                let q = self.lines.as_ref().and_then(Rule::query).expect("hits checked it");
                let def =
                    capture(q, if matches!(p, Pick::Min | Pick::Max) { SignalKind::Num } else { SignalKind::Enum })
                        .map_err(|m| self.fail(m))?;
                Some(def.series(source, progress)?)
            }
            _ => None,
        };
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
            let (oa, ob) = within(&other);
            let theirs = &other[oa..ob];
            let (verdict, evidence) = self.judge(mine, theirs, &time, values.as_ref(), &scope);
            out.push(ScopeResult { scope, verdict, evidence });
        }
        Ok(CheckResult { name: self.name.clone(), kind: self.kind(), scopes: out })
    }

    /// The verdict on one scope: its lines `mine`, the test's other lines
    /// `theirs` (both in the scope).
    fn judge(
        &self,
        mine: &[LineNo],
        theirs: &[LineNo],
        time: &dyn Fn(LineNo) -> Option<f64>,
        values: Option<&crate::SignalSeries>,
        scope: &Scope,
    ) -> (Verdict, Vec<Evidence>) {
        let d = shown_duration;
        let mut ev: Vec<Evidence> = Vec::new();
        let mut count = 0u64;
        let mut add = |ev: &mut Vec<Evidence>, rows: Vec<LineNo>, note: String| {
            count += 1;
            if ev.len() < MAX_EVIDENCE {
                ev.push(Evidence { rows, note });
            }
        };
        let issue = |count: u64, ev: Vec<Evidence>| (if count == 0 { Verdict::Ok } else { Verdict::Issue(count) }, ev);
        let fact = |v: String, ev: Vec<Evidence>| (Verdict::Fact(Some(v)), ev);
        let as_fact = self.kind == Kind::Fact;
        // A count or a value: a fact, or an issue when the condition holds
        // (a count without one: when there is any).
        match &self.test {
            Test::Exists if as_fact => {
                let first = mine.first().map(|&r| Evidence { rows: vec![r], note: "the first".into() });
                fact(mine.len().to_string(), first.into_iter().collect())
            }
            Test::Exists => {
                for &r in mine {
                    add(&mut ev, vec![r], String::new());
                }
                issue(count, ev)
            }
            Test::Count => {
                let first = mine.first().map(|&r| Evidence { rows: vec![r], note: "the first".into() });
                if as_fact {
                    return fact(mine.len().to_string(), first.into_iter().collect());
                }
                // An issue where the condition holds (none: any line): as
                // often as there are lines, each one evidence (bug, owner,
                // 2026-11-07: it said 1x, its lines chained in one).
                let n = mine.len() as f64;
                let holds = match self.cond {
                    Some((op, k)) => op.test(n, k),
                    None => n > 0.0,
                };
                if !holds || mine.is_empty() {
                    return (Verdict::Ok, Vec::new());
                }
                let why = self.cond.map(|(op, k)| format!("{n} lines {} {k}", op.as_str())).unwrap_or_default();
                for &r in mine {
                    add(&mut ev, vec![r], why.clone());
                }
                issue(count, ev)
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
                        add(&mut ev, vec![ts[lo].0, ts[hi].0], format!("{n} lines within {}", d(*window)));
                    }
                }
                if as_fact {
                    let ev = if most.0 == 0 {
                        Vec::new()
                    } else {
                        vec![Evidence {
                            rows: vec![ts[most.1].0, ts[most.2].0],
                            note: format!("{} within {}", most.0, d(*window)),
                        }]
                    };
                    fact(most.0.to_string(), ev)
                } else {
                    issue(count, ev)
                }
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
                        add(&mut ev, vec![w[0].0, w[1].0], format!("none for {}", d(dt)));
                    }
                }
                match (as_fact, longest) {
                    (true, Some((dt, a, b))) => {
                        fact(d(dt), vec![Evidence { rows: vec![a, b], note: format!("none for {}", d(dt)) }])
                    }
                    (true, None) => (Verdict::Fact(None), Vec::new()),
                    (false, _) => issue(count, ev),
                }
            }
            Test::Then { within, .. } | Test::Missing { within, .. } => {
                let then = matches!(self.test, Test::Then { .. });
                // The scope's last time: a missing follow-up is only judged
                // when the scope lasts long enough.
                let end = (scope.from..scope.to).rev().take(1000).find_map(time);
                for &r in mine {
                    let Some(t) = time(r) else { continue };
                    let j = theirs.partition_point(|&x| x <= r);
                    let next = theirs[j..].iter().map(|&x| (x, time(x))).find(|(_, tx)| tx.is_some());
                    let followed = next.filter(|(_, tx)| tx.is_some_and(|tx| tx - t <= *within));
                    match (then, followed) {
                        (true, Some((x, tx))) => {
                            add(&mut ev, vec![r, x], format!("followed after {}", d(tx.unwrap_or(t) - t)))
                        }
                        (false, None) if end.is_some_and(|e| e - t >= *within) => {
                            add(&mut ev, vec![r], format!("nothing followed within {}", d(*within)))
                        }
                        _ => {}
                    }
                }
                if as_fact { fact(count.to_string(), ev) } else { issue(count, ev) }
            }
            Test::Value(p) => {
                let Some(series) = values else { return (Verdict::Fact(None), Vec::new()) };
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
                    return if as_fact { (Verdict::Fact(None), Vec::new()) } else { (Verdict::Ok, Vec::new()) };
                };
                let text = match v {
                    ValueRef::Num(n) => n.to_string(),
                    ValueRef::Enum(t) => String::from_utf8_lossy(t).into_owned(),
                };
                let ev = vec![Evidence { rows: vec![r], note: format!("the {}: {text}", p.as_str()) }];
                if as_fact {
                    fact(text, ev)
                } else {
                    // A value that isn't a number holds no condition.
                    match text.trim().parse::<f64>() {
                        Ok(v) => match self.cond {
                            Some((op, n)) if op.test(v, n) => (Verdict::Issue(1), ev),
                            _ => (Verdict::Ok, Vec::new()),
                        },
                        Err(_) => (Verdict::Ok, Vec::new()),
                    }
                }
            }
        }
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

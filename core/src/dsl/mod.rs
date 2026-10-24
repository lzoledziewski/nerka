//! NFD — the Nerka Filter DSL.
//!
//! ```text
//! level >= ERROR AND raw ~ /SSL|CERT/i AND NOT raw ~ /healthcheck/
//! ```
//!
//! Grammar (handwritten recursive descent, see doc/architecture-analysis.md
//! for why not PEG):
//!
//! ```text
//! expr    := or
//! or      := and ( ("OR" | "||") and )*
//! and     := unary ( ("AND" | "&&") unary )*
//! unary   := ("NOT" | "!") unary | primary
//! primary := "(" expr ")"
//!          | REGEX                      -- shorthand for  raw ~ REGEX
//!          | STRING                     -- shorthand for  raw contains STRING
//!          | FIELD OP VALUE
//!          | SIGNAL OP VALUE
//! FIELD   := raw | level | WORD         -- WORD: a field the source supplies
//! SIGNAL  := "@" NAME [ "." VIEW ]      -- a signal the tab defines
//! VIEW    := state | event | changed    -- default state; event and
//!                                          changed may stand alone
//! OP      := == | != | >= | <= | > | < | ~ | !~
//! VALUE   := REGEX | STRING | WORD      -- numbers are WORDs: 42, 0x2a
//! REGEX   := /pattern/flags             -- flags: i (ignore case)
//! STRING  := "text"                     -- \" and \\ escapes
//! ```
//!
//! Source fields (DLT `ecu`, `apid`, …) are parsed generically and resolved
//! by [`Query::bind`] against the source's schema, which also checks the
//! operator and value against the field's kind: text fields take `==`,
//! `!=`, `~`, `!~`; numeric fields take every comparison. A line without a
//! value for a field only satisfies `!=` and `!~`. As with `raw`, a STRING
//! after `~` / `!~` is a literal substring. Fields marked
//! [`FieldDef::costly`] (DLT `payload`, which decodes the message) are
//! evaluated after the cheap conditions.
//!
//! Signals (`@speed > 100`, `crate::signal`) are values that rows carry,
//! defined outside the language, per tab; the `@` keeps them apart from
//! source fields. `@x` is the signal's state (the value it holds at the
//! row), `@x.event` a value received on the row, `@x.changed` an event
//! that changed the state; the last two also stand alone (any event).
//! [`Query::with_signals`] resolves them and types the comparison by the
//! signal's kind: `num` takes every comparison, `enum` takes `==`, `!=`,
//! `~`, `!~`, `time` takes every comparison with a quoted date-time
//! (`@time >= "2026-10-17 10:00:01.5"`) or time of day (`"10:00:00"`:
//! compared with the value's time of day). As with fields, a row without a value only satisfies `!=`
//! and `!~`, and an unknown signal is an error.
//!
//! The search boxes' Auto mode (`auto.rs`, `doc/search-modes.md`) reads plain
//! words as text and rewrites them into this grammar; NFD itself has no
//! bare text.
//!
//! Keywords and field names are case-insensitive. Evaluation reorders AND/OR operands so
//! cheap checks run before regexes (spec §3.5).

mod auto;
mod parser;

use crate::event::{Event, FieldDef, FieldId, FieldKind, FieldValue, Level};
use crate::signal::{SignalDef, SignalKind, SignalSeries, SignalView, Signals, ValueRef, source_key, view_at};
use crate::time::{TIME_OF_DAY_END, parse_time};
use crate::{LogSource, Progress};
use regex::bytes::Regex;
use std::sync::Arc;

pub use auto::{AutoRead, Plain, read_auto, read_auto_in, read_auto_with};
pub use parser::parse;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Ge,
    Le,
    Gt,
    Lt,
}

impl CmpOp {
    fn test<T: Ord>(self, a: T, b: T) -> bool {
        match self {
            CmpOp::Eq => a == b,
            CmpOp::Ne => a != b,
            CmpOp::Ge => a >= b,
            CmpOp::Le => a <= b,
            CmpOp::Gt => a > b,
            CmpOp::Lt => a < b,
        }
    }

    fn test_f64(self, a: f64, b: f64) -> bool {
        match self {
            CmpOp::Eq => a == b,
            CmpOp::Ne => a != b,
            CmpOp::Ge => a >= b,
            CmpOp::Le => a <= b,
            CmpOp::Gt => a > b,
            CmpOp::Lt => a < b,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Cond {
    /// `raw ~ /re/`
    RawMatch(Regex),
    /// `raw == "text"` (whole line equality)
    RawEq(Vec<u8>),
    /// `level OP LEVEL`; lines without a detectable level only satisfy `!=`.
    Level(CmpOp, Level),
    /// `FIELD OP VALUE` on a source-supplied field.
    Field(FieldCond),
    /// `@SIGNAL OP VALUE`.
    Signal(SignalCond),
}

#[derive(Debug, Clone)]
pub struct SignalCond {
    /// Lower-case name, without the `@`.
    pub name: String,
    pub view: SignalView,
    /// Byte offsets of the `@` and the operator in the query.
    pub pos: usize,
    pub op_pos: usize,
    /// Set by [`Query::with_signals`]; its query is bound with the rest.
    pub def: Option<Arc<SignalDef>>,
    /// Typed by [`Query::with_signals`]: `Float` for `num`, `Str` /
    /// `Match` for `enum`; `Any` when the view stands alone.
    pub test: FieldTest,
    /// The signal's events in the source searched, set by
    /// [`Query::prepare`]; without them rows look them up in the cache.
    pub series: Option<Arc<SignalSeries>>,
}

#[derive(Debug, Clone)]
pub struct FieldCond {
    /// Lower-case field name.
    pub name: String,
    /// Byte offsets of the field name and operator in the query.
    pub pos: usize,
    pub op_pos: usize,
    /// Resolved by [`Query::bind`]; unbound conditions never match.
    pub id: Option<FieldId>,
    pub test: FieldTest,
    /// Set by [`Query::bind`] from [`FieldDef::costly`].
    pub costly: bool,
}

#[derive(Debug, Clone)]
pub enum FieldTest {
    /// As parsed: operator, value text and its position. Typed by `bind`.
    Literal(CmpOp, String, usize),
    /// Text field, `==` or `!=`.
    Str(CmpOp, Vec<u8>),
    Num(CmpOp, u64),
    /// A `num` signal.
    Float(CmpOp, f64),
    /// No comparison: the signal has a value (`@x.event`).
    Any,
    /// A `time` signal: seconds, and whether only the time of day counts.
    Time(CmpOp, f64, bool),
    /// `~` (`!~` is `NOT ~`).
    Match(Regex),
}

#[derive(Debug, Clone)]
pub enum Expr {
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    Cond(Cond),
}

impl Expr {
    pub fn eval(&self, ev: &mut Event<'_>) -> bool {
        match self {
            Expr::And(xs) => xs.iter().all(|x| x.eval(ev)),
            Expr::Or(xs) => xs.iter().any(|x| x.eval(ev)),
            Expr::Not(x) => !x.eval(ev),
            Expr::Cond(Cond::RawMatch(re)) => re.is_match(ev.raw()),
            Expr::Cond(Cond::RawEq(s)) => ev.raw() == s.as_slice(),
            Expr::Cond(Cond::Level(op, want)) => match ev.level() {
                Some(l) => op.test(l, *want),
                None => *op == CmpOp::Ne,
            },
            Expr::Cond(Cond::Field(f)) => match (f.id.and_then(|id| ev.field(id)), &f.test) {
                (Some(FieldValue::Str(v)), FieldTest::Str(op, want)) => op.test(&*v, want.as_slice()),
                (Some(FieldValue::U64(v)), FieldTest::Num(op, want)) => op.test(v, *want),
                (Some(FieldValue::Str(v)), FieldTest::Match(re)) => re.is_match(&v),
                (None, FieldTest::Str(op, _) | FieldTest::Num(op, _)) => *op == CmpOp::Ne,
                _ => false,
            },
            Expr::Cond(Cond::Signal(s)) => s.eval(ev),
        }
    }

    /// Resolve field names against `schema` and type their values.
    fn bind(&mut self, schema: &[FieldDef]) -> Result<(), DslError> {
        match self {
            Expr::And(xs) | Expr::Or(xs) => xs.iter_mut().try_for_each(|x| x.bind(schema)),
            Expr::Not(x) => x.bind(schema),
            Expr::Cond(Cond::Field(f)) => f.bind(schema),
            Expr::Cond(Cond::Signal(s)) => {
                let Some(def) = &s.def else {
                    return Err(DslError { msg: format!("unknown signal '@{}'", s.name), pos: s.pos });
                };
                let bound = def.bind(schema).map_err(|e| DslError {
                    msg: format!("signal '@{}': {} (at column {} of its query)", s.name, e.msg, e.pos + 1),
                    pos: s.pos,
                })?;
                s.def = Some(Arc::new(bound));
                Ok(())
            }
            Expr::Cond(_) => Ok(()),
        }
    }

    /// Resolve `@name` conditions against `signals` and type their values.
    fn attach(&mut self, signals: &Signals) -> Result<(), DslError> {
        match self {
            Expr::And(xs) | Expr::Or(xs) => xs.iter_mut().try_for_each(|x| x.attach(signals)),
            Expr::Not(x) => x.attach(signals),
            Expr::Cond(Cond::Signal(s)) => s.attach(signals),
            Expr::Cond(_) => Ok(()),
        }
    }

    /// Source fields the expression uses (lower-case, each once, in order;
    /// `raw` and `level` aren't source fields).
    pub fn field_names(&self) -> Vec<String> {
        fn walk(e: &Expr, out: &mut Vec<String>) {
            match e {
                Expr::And(xs) | Expr::Or(xs) => xs.iter().for_each(|x| walk(x, out)),
                Expr::Not(x) => walk(x, out),
                Expr::Cond(Cond::Field(f)) if !out.contains(&f.name) => out.push(f.name.clone()),
                Expr::Cond(_) => {}
            }
        }
        let mut out = Vec::new();
        walk(self, &mut out);
        out
    }

    /// Rough relative cost used to order operands.
    fn cost(&self) -> u32 {
        match self {
            Expr::Cond(Cond::RawEq(_)) => 1,
            Expr::Cond(Cond::Field(f)) => {
                if f.costly {
                    10
                } else {
                    1
                }
            }
            Expr::Cond(Cond::Level(..)) => 2,
            Expr::Cond(Cond::RawMatch(_)) => 10,
            // Runs the signal's whole query, then its capture.
            // A lookup in the signal's events (collected beforehand).
            Expr::Cond(Cond::Signal(_)) => 3,
            Expr::Not(x) => x.cost(),
            Expr::And(xs) | Expr::Or(xs) => xs.iter().map(Expr::cost).sum(),
        }
    }

    /// Flatten nested AND/OR and sort operands cheapest-first.
    fn optimize(self) -> Expr {
        match self {
            Expr::And(xs) => Self::flatten(xs, true),
            Expr::Or(xs) => Self::flatten(xs, false),
            Expr::Not(x) => match x.optimize() {
                Expr::Not(inner) => *inner,
                other => Expr::Not(Box::new(other)),
            },
            c => c,
        }
    }

    fn flatten(xs: Vec<Expr>, and: bool) -> Expr {
        let mut out = Vec::with_capacity(xs.len());
        for x in xs.into_iter().map(Expr::optimize) {
            match (x, and) {
                (Expr::And(inner), true) | (Expr::Or(inner), false) => out.extend(inner),
                (x, _) => out.push(x),
            }
        }
        out.sort_by_key(Expr::cost);
        match (out.len(), and) {
            (1, _) => out.pop().unwrap(),
            (_, true) => Expr::And(out),
            (_, false) => Expr::Or(out),
        }
    }
}

impl FieldCond {
    fn bind(&mut self, schema: &[FieldDef]) -> Result<(), DslError> {
        let Some(id) = schema.iter().position(|d| d.name == self.name) else {
            let mut known: Vec<&str> = vec!["raw", "level"];
            known.extend(schema.iter().map(|d| d.name));
            return Err(DslError {
                msg: format!("unknown field '{}' (this source has: {})", self.name, known.join(", ")),
                pos: self.pos,
            });
        };
        let name = &self.name;
        let fail = |msg: String, pos| Err(DslError { msg, pos });
        let test = std::mem::replace(&mut self.test, FieldTest::Num(CmpOp::Eq, 0));
        self.test = match (test, schema[id].kind) {
            (t @ (FieldTest::Match(_) | FieldTest::Str(..)), FieldKind::Str)
            | (t @ FieldTest::Num(..), FieldKind::U64) => t,
            (FieldTest::Literal(op @ (CmpOp::Eq | CmpOp::Ne), v, _), FieldKind::Str) => {
                FieldTest::Str(op, v.into_bytes())
            }
            (
                FieldTest::Literal(..)
                | FieldTest::Num(..)
                | FieldTest::Float(..)
                | FieldTest::Any
                | FieldTest::Time(..),
                FieldKind::Str,
            ) => {
                return fail(format!("'{name}' is text: use ==, !=, ~ or !~"), self.op_pos);
            }
            (FieldTest::Literal(op, v, pos), FieldKind::U64) => match parse_u64(&v) {
                Some(n) => FieldTest::Num(op, n),
                None => return fail(format!("'{name}' is a number but '{v}' isn't"), pos),
            },
            (
                FieldTest::Match(_) | FieldTest::Str(..) | FieldTest::Float(..) | FieldTest::Any | FieldTest::Time(..),
                FieldKind::U64,
            ) => {
                return fail(format!("'{name}' is a number: use ==, !=, >=, <=, >, <"), self.op_pos);
            }
        };
        self.id = Some(id);
        self.costly = schema[id].costly;
        Ok(())
    }
}

impl SignalCond {
    fn attach(&mut self, signals: &Signals) -> Result<(), DslError> {
        let Some(def) = signals.get(&self.name) else {
            let known = if signals.is_empty() {
                "no signals are defined".to_owned()
            } else {
                let names: Vec<String> = signals.iter().map(|d| format!("@{}", d.name())).collect();
                format!("defined: {}", names.join(", "))
            };
            return Err(DslError { msg: format!("unknown signal '@{}' ({known})", self.name), pos: self.pos });
        };
        let name = &self.name;
        let fail = |msg: String, pos| Err(DslError { msg, pos });
        let test = std::mem::replace(&mut self.test, FieldTest::Num(CmpOp::Eq, 0));
        self.test = match (test, def.kind()) {
            (t @ (FieldTest::Match(_) | FieldTest::Str(..)), SignalKind::Enum)
            | (t @ FieldTest::Float(..), SignalKind::Num)
            | (t @ FieldTest::Time(..), SignalKind::Time)
            | (t @ FieldTest::Any, _) => t,
            (FieldTest::Literal(op @ (CmpOp::Eq | CmpOp::Ne), v, _), SignalKind::Enum) => {
                FieldTest::Str(op, v.into_bytes())
            }
            (
                FieldTest::Literal(..) | FieldTest::Float(..) | FieldTest::Num(..) | FieldTest::Time(..),
                SignalKind::Enum,
            ) => {
                return fail(format!("'@{name}' is an enum: use ==, !=, ~ or !~"), self.op_pos);
            }
            (FieldTest::Literal(op, v, pos), SignalKind::Num) => match parse_f64(&v) {
                Some(n) => FieldTest::Float(op, n),
                None => return fail(format!("'@{name}' is a number but '{v}' isn't"), pos),
            },
            (FieldTest::Match(_) | FieldTest::Str(..) | FieldTest::Num(..) | FieldTest::Time(..), SignalKind::Num) => {
                return fail(format!("'@{name}' is a number: use ==, !=, >=, <=, >, <"), self.op_pos);
            }
            (FieldTest::Literal(op, v, pos), SignalKind::Time) => match parse_time(&v) {
                Some(t) => FieldTest::Time(op, t, t < TIME_OF_DAY_END),
                None => {
                    return fail(
                        format!("'@{name}' is a time but '{v}' isn't (\"2026-10-17 10:00:01.5\" or \"10:00:00\")"),
                        pos,
                    );
                }
            },
            (
                FieldTest::Match(_) | FieldTest::Str(..) | FieldTest::Num(..) | FieldTest::Float(..),
                SignalKind::Time,
            ) => {
                return fail(format!("'@{name}' is a time: use ==, !=, >=, <=, >, < with a quoted time"), self.op_pos);
            }
        };
        self.def = Some(def.clone());
        Ok(())
    }

    fn eval(&self, ev: &mut Event<'_>) -> bool {
        let Some(def) = &self.def else { return false };
        let line = ev.line();
        match ev.source() {
            // The source's own time: every line is an event.
            Some(src) if def.is_from_source() => {
                let t = src.time(line);
                let v = match self.view {
                    SignalView::Changed if line > 0 && src.time(line - 1) == t => None,
                    _ => t,
                };
                self.test(v.map(ValueRef::Num))
            }
            Some(src) => {
                let key = source_key(src);
                if let Some((events, row)) = self.series.as_ref().and_then(|s| s.locate(key, line)) {
                    return self.test(view_at(events, row, self.view));
                }
                // Not prepared for this source: the definition's cache.
                match def.series(src, &Progress::new()) {
                    Ok(series) => match series.locate(key, line) {
                        Some((events, row)) => self.test(view_at(events, row, self.view)),
                        None => self.test(None),
                    },
                    Err(_) => false,
                }
            }
            // A bare line: its own value is both its event and its state.
            None => {
                let v = def.value(ev);
                self.test(v.as_ref().map(|v| v.as_ref()))
            }
        }
    }

    fn test(&self, v: Option<ValueRef<'_>>) -> bool {
        match (v, &self.test) {
            (Some(_), FieldTest::Any) => true,
            (Some(ValueRef::Num(v)), FieldTest::Float(op, want)) => op.test_f64(v, *want),
            (Some(ValueRef::Num(v)), FieldTest::Time(op, want, of_day)) => {
                op.test_f64(if *of_day { v.rem_euclid(TIME_OF_DAY_END) } else { v }, *want)
            }
            (Some(ValueRef::Enum(v)), FieldTest::Str(op, want)) => op.test(v, want.as_slice()),
            (Some(ValueRef::Enum(v)), FieldTest::Match(re)) => re.is_match(v),
            (None, FieldTest::Str(op, _) | FieldTest::Float(op, _) | FieldTest::Time(op, ..)) => *op == CmpOp::Ne,
            _ => false,
        }
    }
}

/// A number compared with a `num` signal: decimal, float or `0x` hex.
fn parse_f64(s: &str) -> Option<f64> {
    parse_u64(s).map(|n| n as f64).or_else(|| s.parse::<f64>().ok().filter(|n| n.is_finite()))
}

/// Decimal or `0x` hexadecimal.
fn parse_u64(s: &str) -> Option<u64> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

/// A compiled, optimized query.
#[derive(Debug, Clone)]
pub struct Query {
    expr: Expr,
    source: String,
}

impl Query {
    pub fn compile(src: &str) -> Result<Query, DslError> {
        Ok(Query { expr: parse(src)?.optimize(), source: src.to_owned() })
    }

    /// Compile and [`bind`](Self::bind) in one step.
    pub fn compile_for(src: &str, schema: &[FieldDef]) -> Result<Query, DslError> {
        Query::compile_in(src, schema, &Signals::default())
    }

    /// Compile, resolve signals ([`with_signals`](Self::with_signals)) and
    /// bind in one step.
    pub fn compile_in(src: &str, schema: &[FieldDef], signals: &Signals) -> Result<Query, DslError> {
        Query::compile(src)?.with_signals(signals)?.bind(schema)
    }

    /// Resolve `@name` conditions against the tab's `signals`. Unknown
    /// signals and operators that don't fit the signal's kind are errors.
    /// Comes before [`bind`](Self::bind), which binds the signals' queries
    /// too and refuses unresolved signals.
    pub fn with_signals(mut self, signals: &Signals) -> Result<Query, DslError> {
        self.expr.attach(signals)?;
        // Costs are known only now.
        self.expr = self.expr.optimize();
        Ok(self)
    }

    /// Collect the events of the signals the query uses in `source` (a
    /// pass per signal and file, unless cached), so rows only look them
    /// up. Comes after [`bind`](Self::bind).
    pub fn prepare(mut self, source: &dyn LogSource, progress: &Progress) -> crate::Result<Query> {
        fn walk(e: &mut Expr, source: &dyn LogSource, progress: &Progress) -> crate::Result<()> {
            match e {
                Expr::And(xs) | Expr::Or(xs) => xs.iter_mut().try_for_each(|x| walk(x, source, progress)),
                Expr::Not(x) => walk(x, source, progress),
                Expr::Cond(Cond::Signal(s)) => {
                    if let Some(def) = &s.def {
                        s.series = Some(Arc::new(def.series(source, progress)?));
                    }
                    Ok(())
                }
                Expr::Cond(_) => Ok(()),
            }
        }
        walk(&mut self.expr, source, progress)?;
        Ok(self)
    }

    /// Progress units [`Self::prepare`] has to do on `source`.
    pub(crate) fn prepare_work(&self, source: &dyn LogSource) -> u64 {
        fn walk(e: &Expr, source: &dyn LogSource, seen: &mut Vec<String>) -> u64 {
            match e {
                Expr::And(xs) | Expr::Or(xs) => xs.iter().map(|x| walk(x, source, seen)).sum(),
                Expr::Not(x) => walk(x, source, seen),
                Expr::Cond(Cond::Signal(s)) if !seen.contains(&s.name) => {
                    seen.push(s.name.clone());
                    s.def.as_ref().map_or(0, |d| d.series_work(source))
                }
                Expr::Cond(_) => 0,
            }
        }
        walk(&self.expr, source, &mut Vec::new())
    }

    /// Signals the query uses (lower-case, each once, in order).
    pub fn signal_names(&self) -> Vec<String> {
        fn walk(e: &Expr, out: &mut Vec<String>) {
            match e {
                Expr::And(xs) | Expr::Or(xs) => xs.iter().for_each(|x| walk(x, out)),
                Expr::Not(x) => walk(x, out),
                Expr::Cond(Cond::Signal(s)) if !out.contains(&s.name) => out.push(s.name.clone()),
                Expr::Cond(_) => {}
            }
        }
        let mut out = Vec::new();
        walk(&self.expr, &mut out);
        out
    }

    /// Resolve source-supplied field names (`apid == "NAV"`) against the
    /// schema of the source the query will run on. Unknown fields and
    /// operators that don't fit the field's kind are errors. Binding a
    /// bound query again is fine.
    pub fn bind(mut self, schema: &[FieldDef]) -> Result<Query, DslError> {
        self.expr.bind(schema)?;
        // Costs are known only now.
        self.expr = self.expr.optimize();
        Ok(self)
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn expr(&self) -> &Expr {
        &self.expr
    }

    /// Evaluate on a bare line (no source fields).
    pub fn matches(&self, raw: &[u8]) -> bool {
        self.expr.eval(&mut Event::new(raw))
    }

    pub fn matches_event(&self, ev: &mut Event<'_>) -> bool {
        self.expr.eval(ev)
    }

    /// A regex every matching line must contain, if one exists. The engine
    /// uses it to scan the raw buffer and only evaluates the full query on
    /// candidate lines.
    pub fn prefilter(&self) -> Option<&Regex> {
        // An event condition that rows without an event fail (not `!=`)
        // needs its signal's query to match: its prefilter will do.
        fn of(e: &Expr) -> Option<&Regex> {
            match e {
                Expr::Cond(Cond::RawMatch(re)) => Some(re),
                // A state holds on rows without an event: no prefilter.
                Expr::Cond(Cond::Signal(s)) => match (s.view, &s.test) {
                    (SignalView::State, _)
                    | (
                        _,
                        FieldTest::Float(CmpOp::Ne, _) | FieldTest::Str(CmpOp::Ne, _) | FieldTest::Time(CmpOp::Ne, ..),
                    ) => None,
                    _ => s.def.as_ref().filter(|d| !d.is_from_source())?.query().prefilter(),
                },
                _ => None,
            }
        }
        match &self.expr {
            Expr::And(xs) => xs
                .iter()
                .find_map(|x| matches!(x, Expr::Cond(Cond::RawMatch(_))).then(|| of(x)).flatten())
                .or_else(|| xs.iter().find_map(of)),
            e => of(e),
        }
    }

    /// Regexes to highlight: positive (non-negated) raw matches.
    pub fn highlight_regexes(&self) -> Vec<Regex> {
        fn walk(e: &Expr, neg: bool, out: &mut Vec<Regex>) {
            match e {
                Expr::And(xs) | Expr::Or(xs) => xs.iter().for_each(|x| walk(x, neg, out)),
                Expr::Not(x) => walk(x, !neg, out),
                Expr::Cond(Cond::RawMatch(re)) if !neg => out.push(re.clone()),
                Expr::Cond(_) => {}
            }
        }
        let mut v = Vec::new();
        walk(&self.expr, false, &mut v);
        v
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{msg} (at column {})", .pos + 1)]
pub struct DslError {
    pub msg: String,
    /// Byte offset in the query string.
    pub pos: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> Query {
        Query::compile(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn spec_example() {
        let q = q("level >= ERROR\nAND raw ~ /SSL|CERT/\nAND NOT raw ~ /healthcheck/");
        assert!(q.matches(b"10:00 [ERROR] SSL handshake failed"));
        assert!(q.matches(b"10:00 [FATAL] CERT expired"));
        assert!(!q.matches(b"10:00 [WARN] SSL slow"));
        assert!(!q.matches(b"10:00 [ERROR] SSL healthcheck failed"));
        assert!(!q.matches(b"10:00 [ERROR] disk full"));
    }

    #[test]
    fn cheap_first_and_prefilter() {
        let q = q("raw ~ /SSL/ AND level >= ERROR");
        match q.expr() {
            Expr::And(xs) => assert!(matches!(xs[0], Expr::Cond(Cond::Level(..)))),
            e => panic!("{e:?}"),
        }
        assert!(q.prefilter().is_some());
        assert!(super::Query::compile("level >= ERROR OR raw ~ /x/").unwrap().prefilter().is_none());
    }

    #[test]
    fn shorthands_or_parens_flags() {
        let q = q(r#"(/timeout/i OR "refused") AND NOT level == DEBUG"#);
        assert!(q.matches(b"[INFO] Timeout waiting"));
        assert!(q.matches(b"[WARN] connection refused"));
        assert!(!q.matches(b"[DEBUG] timeout"));
        assert!(!q.matches(b"[INFO] ok"));
    }

    #[test]
    fn level_missing() {
        assert!(!q("level >= INFO").matches(b"plain line"));
        assert!(q("level != INFO").matches(b"plain line"));
    }

    #[test]
    fn highlight_skips_negated() {
        assert_eq!(q("/a/ AND NOT /b/ AND (/c/ OR level == INFO)").highlight_regexes().len(), 2);
    }

    #[test]
    fn errors_point_at_problem() {
        let e = Query::compile("level >= LOUD").unwrap_err();
        assert!(e.msg.contains("level"), "{e}");
        assert_eq!(e.pos, 9);
        assert!(Query::compile("raw ~ /(/").is_err());
        assert!(Query::compile("level >= ERROR AND").is_err());
        let e = Query::compile_for("service == api", &[]).unwrap_err();
        assert!(e.msg.contains("unknown field 'service'"), "{e}");
        assert_eq!(e.pos, 0);
        assert!(Query::compile("apid ~ 5").unwrap_err().msg.contains("regex"));
    }

    #[test]
    fn field_binding_errors_point_at_problem() {
        use crate::input::mem::SCHEMA;
        let bind = |s: &str| Query::compile_for(s, SCHEMA);
        assert!(bind("Apid == NAV AND mcnt >= 0x2a").is_ok());

        let e = bind("level >= INFO AND ctid == X").unwrap_err();
        assert_eq!((e.pos, e.msg.as_str()), (18, "unknown field 'ctid' (this source has: raw, level, apid, mcnt)"));
        let e = bind("apid >= NAV").unwrap_err();
        assert_eq!(e.pos, 5);
        assert!(e.msg.contains("is text"), "{e}");
        let e = bind("mcnt == ten").unwrap_err();
        assert_eq!(e.pos, 8);
        assert!(e.msg.contains("'ten' isn't"), "{e}");
        let e = bind("mcnt ~ /1/").unwrap_err();
        assert_eq!(e.pos, 5);
        assert!(e.msg.contains("is a number"), "{e}");
        let e = Query::compile("apid == (").unwrap_err();
        assert_eq!(e.pos, 8);
        let e = Query::compile("apid ~ NAV").unwrap_err();
        assert_eq!(e.pos, 7);
    }

    #[test]
    fn unbound_fields_never_match() {
        // Without binding (bare lines have no fields) only != holds.
        assert!(!q("apid == NAV").matches(b"NAV"));
        assert!(!q("apid != NAV").matches(b"NAV"));
        assert!(Query::compile_for("apid != NAV", crate::input::mem::SCHEMA).unwrap().matches(b"NAV"));
    }

    #[test]
    fn field_names_in_order_without_repeats() {
        let e = parse("APID == NAV AND (level >= WARN OR NOT ctid ~ /x/) AND raw ~ /y/ AND apid != X").unwrap();
        assert_eq!(e.field_names(), ["apid", "ctid"]);
        assert!(parse("level >= ERROR AND /x/").unwrap().field_names().is_empty());
    }

    #[test]
    fn costly_fields_run_last() {
        let schema = [
            FieldDef { name: "payload", kind: FieldKind::Str, costly: true },
            FieldDef { name: "apid", kind: FieldKind::Str, costly: false },
        ];
        let q = Query::compile_for(r#"payload ~ "a.b" AND apid == NAV AND /x/"#, &schema).unwrap();
        let Expr::And(xs) = q.expr() else { panic!("{:?}", q.expr()) };
        assert!(matches!(&xs[0], Expr::Cond(Cond::Field(f)) if f.name == "apid"));
        // A string after ~ is a literal substring, as for raw.
        assert!(matches!(&xs[1], Expr::Cond(Cond::Field(f)) if f.costly
            && matches!(&f.test, FieldTest::Match(re) if re.as_str() == r"a\.b")));
    }

    fn tab_signals() -> Signals {
        let mut s = Signals::default();
        s.set(SignalDef::parse_spec(r"speed=/speed=(?<speed>\d+)/").unwrap());
        s.set(SignalDef::parse_spec(r"gear:enum=/gear=(?<gear>\w)/").unwrap());
        s
    }

    #[test]
    fn signals_are_apart_from_fields() {
        let sig = tab_signals();
        let q = Query::compile_in("@Speed > 50 AND apid == NAV", crate::input::mem::SCHEMA, &sig).unwrap();
        assert_eq!(q.signal_names(), ["speed"]);
        assert_eq!(q.expr().field_names(), ["apid"]);
        // A field named like a signal is still a field.
        let e = Query::compile_in("speed > 50", crate::input::mem::SCHEMA, &sig).unwrap_err();
        assert!(e.msg.contains("unknown field 'speed'"), "{e}");
        let q = Query::compile_in("@speed >= 1.5e1 AND @gear != D", &[], &sig).unwrap();
        assert!(q.matches(b"speed=20 gear=N"));
        assert!(!q.matches(b"speed=20 gear=D"));
        assert!(!q.matches(b"speed=12 gear=N"));
    }

    #[test]
    fn signal_errors_point_at_problem() {
        let sig = tab_signals();
        let c = |s: &str| Query::compile_in(s, &[], &sig).unwrap_err();
        let e = c("level >= INFO AND @rpm > 1");
        assert_eq!((e.pos, e.msg.as_str()), (18, "unknown signal '@rpm' (defined: @speed, @gear)"));
        let e = Query::compile_for("@rpm > 1", &[]).unwrap_err();
        assert_eq!((e.pos, e.msg.as_str()), (0, "unknown signal '@rpm' (no signals are defined)"));
        let e = c("@gear > D");
        assert_eq!(e.pos, 6);
        assert!(e.msg.contains("is an enum"), "{e}");
        let e = c("@speed ~ /1/");
        assert_eq!(e.pos, 7);
        assert!(e.msg.contains("is a number"), "{e}");
        let e = c("@speed == ten");
        assert_eq!(e.pos, 10);
        assert!(e.msg.contains("'ten' isn't"), "{e}");
        // Parse errors.
        let e = Query::compile("@ > 1").unwrap_err();
        assert_eq!((e.pos, e.msg.as_str()), (1, "expected a signal name after '@'"));
        let e = Query::compile("/x/ AND @speed").unwrap_err();
        assert_eq!(e.pos, 14);
        assert!(e.msg.contains("expected an operator after '@speed'"), "{e}");
        let e = Query::compile("@speed > (").unwrap_err();
        assert_eq!(e.pos, 9);
        let e = Query::compile("@gear ~ D").unwrap_err();
        assert_eq!(e.pos, 8);
        // Views.
        let e = Query::compile("@gear.now == D").unwrap_err();
        assert_eq!((e.pos, e.msg.as_str()), (6, "unknown signal view '.now' (state, event, changed)"));
        let e = Query::compile("@gear AND /x/").unwrap_err();
        assert_eq!(e.pos, 6);
        assert!(e.msg.contains("or @gear.event"), "{e}");
        let e = Query::compile("@gear.state").unwrap_err();
        assert_eq!(e.pos, 11);
        assert!(Query::compile("@gear.event AND NOT @gear.changed AND @gear.Changed == D").is_ok());
    }

    #[test]
    fn time_signal_errors() {
        let mut sig = Signals::default();
        sig.set(SignalDef::parse_spec(r"t:time=/^(?<t>\S+)/").unwrap());
        let c = |s: &str| Query::compile_in(s, &[], &sig);
        assert!(c(r#"@t >= "2026-10-17 10:00:01.5" AND @t < "10:30:00""#).is_ok());
        let e = c("@t > soon").unwrap_err();
        assert_eq!(e.pos, 5);
        assert!(e.msg.contains("is a time but 'soon' isn't"), "{e}");
        let e = c("@t ~ /10/").unwrap_err();
        assert_eq!(e.pos, 3);
        assert!(e.msg.contains("is a time"), "{e}");
        assert!(c("@t > 1729771200").is_ok(), "seconds since the epoch");
        // On a bare line a time of day is itself.
        assert!(c(r#"@t >= "10:00:00""#).unwrap().matches(b"10:00:01 x"));
    }

    #[test]
    fn views_on_a_bare_line() {
        // Without a source a line's own value is its event and its state.
        let q = Query::compile_in("@gear.event AND @gear.changed AND @gear == N", &[], &tab_signals()).unwrap();
        assert!(q.matches(b"gear=N"));
        assert!(!q.matches(b"gear=D"));
        assert!(!q.matches(b"no gear"));
    }
}

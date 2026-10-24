//! Signals: named values that rows carry (`doc/sources-and-signals.md` →
//! "What a signal is").
//!
//! v1 defines a signal by an explicit capture: an NFD query plus the named
//! regex group to take the value from. A row carries the value when the
//! query matches it and the group captured something. NFD refers to
//! signals as `@name` ([`crate::dsl`]); definitions live outside the
//! language, per tab.
//!
//! A row that carries a value is an **event**; the **state** is the value
//! the signal holds, set by each event until the next one (on the event
//! row itself it is already the new value). [`SignalDef::series`] collects
//! a signal's events in one pass per file and caches them with the
//! definition; the state at a row is then the last event at or before it.
//! In a view of several files each file has its own events, so a state
//! never carries over into the next file.
//!
//! Spec text (CLI `--signal`, one line):
//!
//! ```text
//! spec  := NAME [ "(" GROUP ")" ] [ ":" KIND ] "=" QUERY
//! KIND  := num | enum                  -- default num
//! GROUP := regex group name            -- default NAME
//! ```
//!
//! e.g. `speed=/speed=(?<speed>\d+)/` or `gear(g):enum=apid == NAV AND payload ~ /gear (?<g>\w)/`.

use crate::dsl::{Cond, DslError, Expr, FieldTest, Query};
use crate::engine::search::{Filter, scan};
use crate::event::{Event, FieldDef, FieldValue};
use crate::{LineNo, LogSource, Progress};
use rayon::prelude::*;
use regex::bytes::Regex;
use std::sync::{Arc, Mutex};

/// Which side of a signal a condition looks at (`@x`, `@x.event`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalView {
    /// The value the signal holds at the row (`@x`, `@x.state`).
    State,
    /// A value received on the row (`@x.event`).
    Event,
    /// An event that changed the state (`@x.changed`).
    Changed,
}

impl SignalView {
    pub fn parse(s: &str) -> Option<SignalView> {
        match s.to_ascii_lowercase().as_str() {
            "state" => Some(SignalView::State),
            "event" => Some(SignalView::Event),
            "changed" => Some(SignalView::Changed),
            _ => None,
        }
    }
}

/// A value borrowed from a series or a row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ValueRef<'a> {
    Num(f64),
    Enum(&'a [u8]),
}

impl ValueRef<'_> {
    pub fn to_value(self) -> SignalValue {
        match self {
            ValueRef::Num(n) => SignalValue::Num(n),
            ValueRef::Enum(t) => SignalValue::Enum(t.to_vec()),
        }
    }
}

impl SignalValue {
    pub fn as_ref(&self) -> ValueRef<'_> {
        match self {
            SignalValue::Num(n) => ValueRef::Num(*n),
            SignalValue::Enum(t) => ValueRef::Enum(t),
        }
    }
}

/// The events of one signal in one file: rows ascending, one value each.
#[derive(Debug)]
pub struct Events {
    rows: Vec<LineNo>,
    values: Values,
}

#[derive(Debug)]
enum Values {
    Num(Vec<f64>),
    /// Interned: an id per event into `table`.
    Enum {
        ids: Vec<u32>,
        table: Vec<Vec<u8>>,
    },
}

impl Events {
    fn build(found: Vec<(LineNo, SignalValue)>, kind: SignalKind) -> Events {
        let rows = found.iter().map(|(r, _)| *r).collect();
        let values = match kind {
            SignalKind::Num => Values::Num(
                found.iter().map(|(_, v)| if let SignalValue::Num(n) = v { *n } else { f64::NAN }).collect(),
            ),
            SignalKind::Enum => {
                let mut index = std::collections::HashMap::new();
                let mut table = Vec::new();
                let ids = found
                    .into_iter()
                    .map(|(_, v)| {
                        let text = match v {
                            SignalValue::Enum(t) => t,
                            SignalValue::Num(n) => n.to_string().into_bytes(),
                        };
                        *index.entry(text).or_insert_with_key(|t: &Vec<u8>| {
                            table.push(t.clone());
                            (table.len() - 1) as u32
                        })
                    })
                    .collect();
                Values::Enum { ids, table }
            }
        };
        Events { rows, values }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Row and value of the `i`-th event.
    pub fn get(&self, i: usize) -> (LineNo, ValueRef<'_>) {
        let v = match &self.values {
            Values::Num(v) => ValueRef::Num(v[i]),
            Values::Enum { ids, table } => ValueRef::Enum(&table[ids[i] as usize]),
        };
        (self.rows[i], v)
    }

    /// Index of the event on `row`.
    fn event(&self, row: LineNo) -> Option<usize> {
        self.rows.binary_search(&row).ok()
    }

    /// Index of the event that set the state at `row`.
    fn state(&self, row: LineNo) -> Option<usize> {
        self.rows.partition_point(|&r| r <= row).checked_sub(1)
    }

    /// The `i`-th event changed the state (the first one always does).
    fn changed(&self, i: usize) -> bool {
        i == 0
            || match &self.values {
                Values::Num(v) => v[i] != v[i - 1],
                Values::Enum { ids, .. } => ids[i] != ids[i - 1],
            }
    }
}

/// Identity of a source for the cache: its address and line count.
pub(crate) fn source_key(source: &dyn LogSource) -> (usize, LineNo) {
    (source.data_id(), source.line_count())
}

/// A signal's events in a source: one [`Events`] per file.
#[derive(Debug, Clone)]
pub struct SignalSeries {
    key: (usize, LineNo),
    /// (first line in the source, key of the file's own source, events).
    parts: Vec<(LineNo, (usize, LineNo), Arc<Events>)>,
}

impl SignalSeries {
    /// The events and the file-local row for `row` of the source `key`
    /// (the whole source, or one of its files, as the engine scans them).
    pub(crate) fn locate(&self, key: (usize, LineNo), row: LineNo) -> Option<(&Events, LineNo)> {
        if key == self.key {
            let i = self.parts.partition_point(|p| p.0 <= row).checked_sub(1)?;
            let (first, _, ev) = &self.parts[i];
            return Some((ev, row - first));
        }
        self.parts.iter().find(|p| p.1 == key).map(|(_, _, ev)| (&**ev, row))
    }

    fn whole(&self, row: LineNo) -> Option<(&Events, LineNo, LineNo)> {
        let i = self.parts.partition_point(|p| p.0 <= row).checked_sub(1)?;
        let (first, _, ev) = &self.parts[i];
        Some((ev, row - first, *first))
    }

    /// The state at `row`: the event that set it (its row) and its value.
    pub fn state(&self, row: LineNo) -> Option<(LineNo, ValueRef<'_>)> {
        let (ev, local, first) = self.whole(row)?;
        ev.state(local).map(|i| ev.get(i)).map(|(r, v)| (r + first, v))
    }

    /// The value received on `row`, if it is an event.
    pub fn event(&self, row: LineNo) -> Option<ValueRef<'_>> {
        let (ev, local, _) = self.whole(row)?;
        ev.event(local).map(|i| ev.get(i).1)
    }

    /// Whether the event on `row` (if any) changed the state.
    pub fn changed(&self, row: LineNo) -> bool {
        self.whole(row).is_some_and(|(ev, local, _)| ev.event(local).is_some_and(|i| ev.changed(i)))
    }

    /// Every event, in row order: (row in the source, value).
    pub fn iter(&self) -> impl Iterator<Item = (LineNo, ValueRef<'_>)> {
        self.parts
            .iter()
            .flat_map(|(first, _, ev)| (0..ev.len()).map(move |i| ev.get(i)).map(move |(r, v)| (r + first, v)))
    }

    pub fn len(&self) -> usize {
        self.parts.iter().map(|p| p.2.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Look up `row` of the source `key` in `events`: the value of `view`.
pub(crate) fn view_at(events: &Events, row: LineNo, view: SignalView) -> Option<ValueRef<'_>> {
    match view {
        SignalView::State => events.state(row).map(|i| events.get(i).1),
        SignalView::Event => events.event(row).map(|i| events.get(i).1),
        SignalView::Changed => events.event(row).filter(|&i| events.changed(i)).map(|i| events.get(i).1),
    }
}

/// Files' events built so far, shared by every copy of a definition.
type Cache = Arc<Mutex<Vec<((usize, LineNo), Arc<Events>)>>>;

/// Files kept in a definition's cache at most.
const CACHED_FILES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// The capture read as a number (decimal, float or `0x` hex).
    Num,
    /// The captured text.
    Enum,
}

impl SignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SignalKind::Num => "num",
            SignalKind::Enum => "enum",
        }
    }

    pub fn parse(s: &str) -> Option<SignalKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "num" => Some(SignalKind::Num),
            "enum" => Some(SignalKind::Enum),
            _ => None,
        }
    }
}

/// The value a row carries.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalValue {
    Num(f64),
    Enum(Vec<u8>),
}

impl std::fmt::Display for SignalValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignalValue::Num(n) => write!(f, "{n}"),
            SignalValue::Enum(t) => f.write_str(&String::from_utf8_lossy(t)),
        }
    }
}

/// What is wrong with a definition, by the part it is in.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignalError {
    #[error("name: {0}")]
    Name(String),
    #[error("group: {0}")]
    Group(String),
    #[error("query: {0}")]
    Query(DslError),
    /// A spec line that isn't `NAME[(GROUP)][:KIND]=QUERY`.
    #[error("{0}")]
    Spec(String),
}

#[derive(Debug, Clone)]
pub struct SignalDef {
    name: String,
    kind: SignalKind,
    group: String,
    query: Query,
    cache: Cache,
}

impl SignalDef {
    /// `group` defaults to `name`. The query is compiled but not bound:
    /// that happens per source, with the filter that uses the signal.
    pub fn new(name: &str, kind: SignalKind, query: &str, group: Option<&str>) -> Result<SignalDef, SignalError> {
        let name = name.trim();
        if !is_name(name) {
            return Err(SignalError::Name(format!("'{name}' isn't a name (letters, digits and _, not first a digit)")));
        }
        let group = group.map(str::trim).filter(|g| !g.is_empty()).unwrap_or(name);
        let query = Query::compile(query).map_err(SignalError::Query)?;
        if uses_signals(query.expr()) {
            return Err(SignalError::Query(DslError { msg: "a signal can't use other signals".into(), pos: 0 }));
        }
        let mut groups = Vec::new();
        positive_regexes(query.expr(), false, &mut |re| {
            groups.extend(re.capture_names().flatten().map(str::to_owned));
        });
        if !groups.iter().any(|g| g == group) {
            let have = if groups.is_empty() {
                "the query has no named groups: write (?<name>…)".to_owned()
            } else {
                format!("the query has: {}", groups.join(", "))
            };
            return Err(SignalError::Group(format!("no regex captures '{group}' ({have})")));
        }
        Ok(SignalDef { name: name.to_ascii_lowercase(), kind, group: group.to_owned(), query, cache: Cache::default() })
    }

    /// Parse one spec line (module docs).
    pub fn parse_spec(spec: &str) -> Result<SignalDef, SignalError> {
        let bad = || SignalError::Spec(format!("'{spec}' isn't NAME[(GROUP)][:num|enum]=QUERY"));
        let (head, query) = spec.split_once('=').ok_or_else(bad)?;
        let (head, kind) = match head.split_once(':') {
            Some((h, k)) => (
                h,
                SignalKind::parse(k)
                    .ok_or_else(|| SignalError::Spec(format!("unknown kind '{}' (num, enum)", k.trim())))?,
            ),
            None => (head, SignalKind::Num),
        };
        let (name, group) = match head.split_once('(') {
            Some((n, g)) => (n, Some(g.trim().strip_suffix(')').ok_or_else(bad)?)),
            None => (head, None),
        };
        SignalDef::new(name, kind, query, group)
    }

    /// The spec line [`Self::parse_spec`] reads back.
    pub fn spec(&self) -> String {
        let group = if self.group == self.name { String::new() } else { format!("({})", self.group) };
        format!("{}{group}:{}={}", self.name, self.kind.as_str(), self.query.source())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> SignalKind {
        self.kind
    }

    pub fn group(&self) -> &str {
        &self.group
    }

    pub fn query(&self) -> &Query {
        &self.query
    }

    /// The definition with its query bound to `schema` (see [`Query::bind`]).
    pub fn bind(&self, schema: &[FieldDef]) -> Result<SignalDef, DslError> {
        Ok(SignalDef { query: self.query.clone().bind(schema)?, ..self.clone() })
    }

    /// The value row `ev` carries, if any.
    pub fn value(&self, ev: &mut Event<'_>) -> Option<SignalValue> {
        // A query that is only the capturing regex matches when it captures.
        let only_regex = matches!(self.query.expr(), Expr::Cond(Cond::RawMatch(_)));
        if !only_regex && !self.query.matches_event(ev) {
            return None;
        }
        let text = capture(self.query.expr(), &self.group, ev)?;
        match self.kind {
            SignalKind::Num => parse_num(&text).map(SignalValue::Num),
            SignalKind::Enum => Some(SignalValue::Enum(text)),
        }
    }

    /// The signal's events in `source`, one pass per file on first use,
    /// then from the cache this definition (and every copy of it) keeps.
    pub fn series(&self, source: &dyn LogSource, progress: &Progress) -> crate::Result<SignalSeries> {
        let mut parts = Vec::new();
        match source.parts() {
            Some(ps) => {
                for p in ps {
                    let src = p.source.as_ref();
                    parts.push((p.first, source_key(src), self.events(src, progress)?));
                }
            }
            None => parts.push((0, source_key(source), self.events(source, progress)?)),
        }
        Ok(SignalSeries { key: source_key(source), parts })
    }

    /// The series if every file's events are cached already (a UI can
    /// show values without waiting).
    pub fn cached_series(&self, source: &dyn LogSource) -> Option<SignalSeries> {
        let leaves: Vec<(LineNo, (usize, LineNo))> = match source.parts() {
            Some(ps) => ps.iter().map(|p| (p.first, source_key(p.source.as_ref()))).collect(),
            None => vec![(0, source_key(source))],
        };
        let parts = leaves
            .into_iter()
            .map(|(first, key)| self.cached(key).map(|ev| (first, key, ev)))
            .collect::<Option<Vec<_>>>()?;
        Some(SignalSeries { key: source_key(source), parts })
    }

    /// Progress units [`Self::series`] still has to do on `source`.
    pub(crate) fn series_work(&self, source: &dyn LogSource) -> u64 {
        match source.parts() {
            Some(ps) => ps.iter().map(|p| self.series_work(p.source.as_ref())).sum(),
            None if self.cached(source_key(source)).is_some() => 0,
            None => {
                self.bind(source.schema()).map_or(0, |d| crate::engine::search::work(source, &Filter::Query(d.query)))
            }
        }
    }

    fn cached(&self, key: (usize, LineNo)) -> Option<Arc<Events>> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.iter().find(|(k, _)| *k == key).map(|(_, ev)| ev.clone())
    }

    /// One file's events (a source without parts).
    fn events(&self, source: &dyn LogSource, progress: &Progress) -> crate::Result<Arc<Events>> {
        let key = source_key(source);
        if let Some(ev) = self.cached(key) {
            return Ok(ev);
        }
        // Built without holding the lock: a scan runs on the thread pool.
        let def = self.bind(source.schema())?;
        let rows = scan(source, &Filter::Query(def.query.clone()), progress)?;
        let found: Vec<(LineNo, SignalValue)> =
            rows.par_iter().filter_map(|&r| def.value(&mut Event::from_source(source, r)).map(|v| (r, v))).collect();
        let ev = Arc::new(Events::build(found, self.kind));
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if !cache.iter().any(|(k, _)| *k == key) {
            if cache.len() >= CACHED_FILES {
                cache.remove(0);
            }
            cache.push((key, ev.clone()));
        }
        Ok(ev)
    }
}

/// The signals a tab defines, by unique name.
#[derive(Debug, Clone, Default)]
pub struct Signals(Vec<Arc<SignalDef>>);

impl Signals {
    pub fn get(&self, name: &str) -> Option<&Arc<SignalDef>> {
        self.0.iter().find(|d| d.name.eq_ignore_ascii_case(name))
    }

    /// Add `def`, replacing a signal of the same name.
    pub fn set(&mut self, def: SignalDef) {
        match self.0.iter_mut().find(|d| d.name == def.name) {
            Some(d) => *d = Arc::new(def),
            None => self.0.push(Arc::new(def)),
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.0.retain(|d| !d.name.eq_ignore_ascii_case(name));
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<SignalDef>> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

fn is_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// Decimal, float or `0x` hex, surrounding blanks ignored; finite only.
fn parse_num(text: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(text).ok()?.trim();
    let n = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok()? as f64,
        None => s.parse::<f64>().ok()?,
    };
    n.is_finite().then_some(n)
}

fn uses_signals(e: &Expr) -> bool {
    match e {
        Expr::And(xs) | Expr::Or(xs) => xs.iter().any(uses_signals),
        Expr::Not(x) => uses_signals(x),
        Expr::Cond(c) => matches!(c, Cond::Signal(_)),
    }
}

/// Every regex not under a NOT, raw or field, in evaluation order.
fn positive_regexes(e: &Expr, neg: bool, f: &mut dyn FnMut(&Regex)) {
    match e {
        Expr::And(xs) | Expr::Or(xs) => xs.iter().for_each(|x| positive_regexes(x, neg, f)),
        Expr::Not(x) => positive_regexes(x, !neg, f),
        Expr::Cond(Cond::RawMatch(re)) if !neg => f(re),
        Expr::Cond(Cond::Field(fc)) if !neg => {
            if let FieldTest::Match(re) = &fc.test {
                f(re)
            }
        }
        Expr::Cond(_) => {}
    }
}

/// The text `group` captured in the first positive regex that has the
/// group and matches its text (the line, or its field).
fn capture(e: &Expr, group: &str, ev: &mut Event<'_>) -> Option<Vec<u8>> {
    match e {
        Expr::And(xs) | Expr::Or(xs) => xs.iter().find_map(|x| capture(x, group, ev)),
        Expr::Not(_) => None,
        Expr::Cond(Cond::RawMatch(re)) if has_group(re, group) => {
            re.captures(ev.raw()).and_then(|c| c.name(group)).map(|m| m.as_bytes().to_vec())
        }
        Expr::Cond(Cond::Field(fc)) => match (&fc.test, fc.id.and_then(|id| ev.field(id))) {
            (FieldTest::Match(re), Some(FieldValue::Str(v))) if has_group(re, group) => {
                re.captures(&v).and_then(|c| c.name(group)).map(|m| m.as_bytes().to_vec())
            }
            _ => None,
        },
        Expr::Cond(_) => None,
    }
}

fn has_group(re: &Regex, group: &str) -> bool {
    re.capture_names().flatten().any(|g| g == group)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(spec: &str, line: &str) -> Option<SignalValue> {
        SignalDef::parse_spec(spec).unwrap().value(&mut Event::new(line.as_bytes()))
    }

    #[test]
    fn values_from_captures() {
        assert_eq!(value(r"speed=/speed=(?<speed>[\d.]+)/", "x speed=12.5 y"), Some(SignalValue::Num(12.5)));
        assert_eq!(value(r"id=/id=(?<id>\S+)/", "id=0x1f"), Some(SignalValue::Num(31.0)));
        assert_eq!(value(r"speed=/speed=(?<speed>\S+)/", "speed=fast"), None, "not a number");
        assert_eq!(value(r"speed=/speed=(?<speed>\S+)/", "nothing"), None);
        assert_eq!(value(r"gear(g):enum=/gear (?<g>\w)/", "gear D"), Some(SignalValue::Enum(b"D".to_vec())));
        // The whole query has to match, not only the capturing regex.
        assert_eq!(value(r"v=/v=(?<v>\d+)/ AND /ok/", "v=1 bad"), None);
        assert_eq!(value(r"v=/v=(?<v>\d+)/ AND /ok/", "v=1 ok"), Some(SignalValue::Num(1.0)));
        // Under OR, the branch that matched gives the value.
        assert_eq!(value(r"v=/a=(?<v>\d+)/ OR /b=(?<v>\d+)/", "b=7"), Some(SignalValue::Num(7.0)));
        assert_eq!(value(r"v=/v=(?<v>\d+)?/", "v="), None, "group took no part");
    }

    #[test]
    fn spec_round_trips() {
        for spec in [r"speed:num=/speed=(?<speed>\d+)/", r"gear(g):enum=/gear (?<g>\w)/"] {
            assert_eq!(SignalDef::parse_spec(spec).unwrap().spec(), spec);
        }
        let d = SignalDef::parse_spec(r" Speed =/(?<Speed>\d)/").unwrap();
        assert_eq!((d.name(), d.group(), d.kind()), ("speed", "Speed", SignalKind::Num));
    }

    #[test]
    fn definition_errors() {
        let e = |s: &str| SignalDef::parse_spec(s).unwrap_err();
        assert!(matches!(e("speed"), SignalError::Spec(_)));
        assert!(matches!(e(r"1x=/(?<x>.)/"), SignalError::Name(_)));
        assert!(matches!(e(r"x:float=/(?<x>.)/"), SignalError::Spec(m) if m.contains("'float'")));
        assert!(matches!(e(r"x(y=/(?<y>.)/"), SignalError::Spec(_)));
        assert!(matches!(e(r"x=/(\d)/"), SignalError::Group(m) if m.contains("no named groups")));
        assert!(matches!(e(r"x=/(?<y>\d)/"), SignalError::Group(m) if m.contains("has: y")));
        assert!(matches!(e(r"x=NOT /(?<x>\d)/"), SignalError::Group(_)), "a negated regex captures nothing");
        assert!(matches!(e(r"x=/(?<x>\d)/ AND @y > 1"), SignalError::Query(q) if q.msg.contains("other signals")));
        assert!(matches!(e(r"x=/(?<x>\d)/ AND"), SignalError::Query(_)));
    }
}

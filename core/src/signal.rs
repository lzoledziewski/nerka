//! Signals: named values that rows carry (`doc/sources-and-signals.md` →
//! "What a signal is").
//!
//! v1 defines a signal by an explicit capture: an NFD query plus the named
//! regex group to take the value from. A row carries the value when the
//! query matches it and the group captured something. NFD refers to
//! signals as `@name` ([`crate::dsl`]); definitions live outside the
//! language, per tab.
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
use crate::event::{Event, FieldDef, FieldValue};
use regex::bytes::Regex;
use std::sync::Arc;

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
        Ok(SignalDef { name: name.to_ascii_lowercase(), kind, group: group.to_owned(), query })
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

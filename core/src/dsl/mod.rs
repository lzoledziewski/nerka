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
//! FIELD   := raw | level
//! OP      := == | != | >= | <= | > | < | ~ | !~
//! VALUE   := REGEX | STRING | WORD
//! REGEX   := /pattern/flags             -- flags: i (ignore case)
//! STRING  := "text"                     -- \" and \\ escapes
//! ```
//!
//! Keywords are case-insensitive. Evaluation reorders AND/OR operands so
//! cheap checks run before regexes (spec §3.5).

mod parser;

use crate::event::{Event, Level};
use regex::bytes::Regex;

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
}

#[derive(Debug, Clone)]
pub enum Cond {
    /// `raw ~ /re/`
    RawMatch(Regex),
    /// `raw == "text"` (whole line equality)
    RawEq(Vec<u8>),
    /// `level OP LEVEL`; lines without a detectable level only satisfy `!=`.
    Level(CmpOp, Level),
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
            Expr::Cond(Cond::RawMatch(re)) => re.is_match(ev.raw),
            Expr::Cond(Cond::RawEq(s)) => ev.raw == s.as_slice(),
            Expr::Cond(Cond::Level(op, want)) => match ev.level() {
                Some(l) => op.test(l, *want),
                None => *op == CmpOp::Ne,
            },
        }
    }

    /// Rough relative cost used to order operands.
    fn cost(&self) -> u32 {
        match self {
            Expr::Cond(Cond::RawEq(_)) => 1,
            Expr::Cond(Cond::Level(..)) => 2,
            Expr::Cond(Cond::RawMatch(_)) => 10,
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

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn expr(&self) -> &Expr {
        &self.expr
    }

    pub fn matches(&self, raw: &[u8]) -> bool {
        self.expr.eval(&mut Event::new(raw))
    }

    /// A regex every matching line must contain, if one exists. The engine
    /// uses it to scan the raw buffer and only evaluates the full query on
    /// candidate lines.
    pub fn prefilter(&self) -> Option<&Regex> {
        match &self.expr {
            Expr::Cond(Cond::RawMatch(re)) => Some(re),
            Expr::And(xs) => xs.iter().find_map(|x| match x {
                Expr::Cond(Cond::RawMatch(re)) => Some(re),
                _ => None,
            }),
            _ => None,
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
        assert!(Query::compile("service == api").unwrap_err().msg.contains("not supported"));
    }
}

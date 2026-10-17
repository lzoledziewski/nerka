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
//! FIELD   := raw | level | WORD         -- WORD: a field the source supplies
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
//! value for a field only satisfies `!=` and `!~`.
//!
//! Keywords and field names are case-insensitive. Evaluation reorders AND/OR operands so
//! cheap checks run before regexes (spec §3.5).

mod parser;

use crate::event::{Event, FieldDef, FieldId, FieldKind, FieldValue, Level};
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
    /// `FIELD OP VALUE` on a source-supplied field.
    Field(FieldCond),
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
}

#[derive(Debug, Clone)]
pub enum FieldTest {
    /// As parsed: operator, value text and its position. Typed by `bind`.
    Literal(CmpOp, String, usize),
    /// Text field, `==` or `!=`.
    Str(CmpOp, Vec<u8>),
    Num(CmpOp, u64),
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
                (Some(FieldValue::Str(v)), FieldTest::Str(op, want)) => op.test(v, want.as_slice()),
                (Some(FieldValue::U64(v)), FieldTest::Num(op, want)) => op.test(v, *want),
                (Some(FieldValue::Str(v)), FieldTest::Match(re)) => re.is_match(v),
                (None, FieldTest::Str(op, _) | FieldTest::Num(op, _)) => *op == CmpOp::Ne,
                _ => false,
            },
        }
    }

    /// Resolve field names against `schema` and type their values.
    fn bind(&mut self, schema: &[FieldDef]) -> Result<(), DslError> {
        match self {
            Expr::And(xs) | Expr::Or(xs) => xs.iter_mut().try_for_each(|x| x.bind(schema)),
            Expr::Not(x) => x.bind(schema),
            Expr::Cond(Cond::Field(f)) => f.bind(schema),
            Expr::Cond(_) => Ok(()),
        }
    }

    /// Rough relative cost used to order operands.
    fn cost(&self) -> u32 {
        match self {
            Expr::Cond(Cond::RawEq(_)) => 1,
            Expr::Cond(Cond::Field(_)) => 1,
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
            (FieldTest::Literal(..) | FieldTest::Num(..), FieldKind::Str) => {
                return fail(format!("'{name}' is text: use ==, !=, ~ or !~"), self.op_pos);
            }
            (FieldTest::Literal(op, v, pos), FieldKind::U64) => match parse_u64(&v) {
                Some(n) => FieldTest::Num(op, n),
                None => return fail(format!("'{name}' is a number but '{v}' isn't"), pos),
            },
            (FieldTest::Match(_) | FieldTest::Str(..), FieldKind::U64) => {
                return fail(format!("'{name}' is a number: use ==, !=, >=, <=, >, <"), self.op_pos);
            }
        };
        self.id = Some(id);
        Ok(())
    }
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
        Query::compile(src)?.bind(schema)
    }

    /// Resolve source-supplied field names (`apid == "NAV"`) against the
    /// schema of the source the query will run on. Unknown fields and
    /// operators that don't fit the field's kind are errors. Binding a
    /// bound query again is fine.
    pub fn bind(mut self, schema: &[FieldDef]) -> Result<Query, DslError> {
        self.expr.bind(schema)?;
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
        let e = Query::compile_for("service == api", &[]).unwrap_err();
        assert!(e.msg.contains("unknown field 'service'"), "{e}");
        assert_eq!(e.pos, 0);
        assert!(Query::compile(r#"apid ~ "x""#).unwrap_err().msg.contains("regex"));
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
}

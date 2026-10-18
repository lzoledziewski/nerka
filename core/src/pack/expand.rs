//! Expansion: a selection from the [`Library`] becomes plain NFD.
//!
//! Each picked query stands for its parent chain ANDed with its own text:
//! `(root) AND (…) AND (query)`. Included queries are OR'ed, excluded ones
//! are added as `AND NOT`. The result is one line of NFD that compiles on
//! its own, plus provenance: the byte range each query occupies.

use super::{Ctx, Library, render};
use crate::dsl::DslError;
use crate::{Filter, Query, SearchSet};
use std::collections::BTreeMap;
use std::ops::Range;

/// One pick from the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// A query by qualified id (`vision.frame_drops`).
    Query(String),
    /// Every query with this label or one below it, resolved when expanded
    /// (see [`Selection::snapshot`]).
    Label(String),
}

/// What was picked for Filter or for Search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub include: Vec<Pick>,
    pub exclude: Vec<Pick>,
}

impl Selection {
    /// The same selection with every label replaced by the queries that
    /// carry it now. A query labelled later doesn't join a snapshot.
    pub fn snapshot(&self, lib: &Library) -> Result<Selection, ExpandError> {
        let freeze = |picks: &[Pick]| -> Result<Vec<Pick>, ExpandError> {
            let mut out = Vec::new();
            for id in resolve(lib, picks)? {
                out.push(Pick::Query(id));
            }
            Ok(out)
        };
        Ok(Selection { include: freeze(&self.include)?, exclude: freeze(&self.exclude)? })
    }
}

/// A parameter value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamValue {
    pub value: String,
    /// Insert as regex syntax inside `/…/` instead of escaping it (only `/`
    /// is still escaped, so the value can't end the regex). In strings and
    /// bare values a parameter is always a literal.
    pub raw: bool,
}

impl ParamValue {
    pub fn literal(v: impl Into<String>) -> Self {
        Self { value: v.into(), raw: false }
    }
    pub fn raw(v: impl Into<String>) -> Self {
        Self { value: v.into(), raw: true }
    }
}

/// Parameter values by name, shared by every picked query.
pub type Params = BTreeMap<String, ParamValue>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Include,
    Exclude,
}

/// Where one picked query sits in [`Expansion::nfd`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Qualified id of the picked query.
    pub query: String,
    pub role: Role,
    /// The query's full meaning (parent chain AND own text). `nfd[range]`
    /// is valid NFD on its own.
    pub range: Range<usize>,
    /// The parent chain from the root down to the query itself: qualified
    /// id and the range of that query's own (parenthesized) text.
    pub chain: Vec<(String, Range<usize>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    /// Plain NFD, one line.
    pub nfd: String,
    /// Included parts first, in pick order, then excluded ones.
    pub parts: Vec<Part>,
}

impl Expansion {
    /// The standalone NFD of a part.
    pub fn part_nfd(&self, part: &Part) -> &str {
        &self.nfd[part.range.clone()]
    }

    pub fn parts_with(&self, role: Role) -> impl Iterator<Item = &Part> {
        self.parts.iter().filter(move |p| p.role == role)
    }

    /// The parts as search queries: hits are reported per included part,
    /// in the order of [`parts_with`](Self::parts_with)`(Role::Include)`.
    pub fn search_set(&self) -> Result<SearchSet, DslError> {
        let compile = |role| {
            self.parts_with(role).map(|p| Query::compile(self.part_nfd(p)).map(Filter::Query)).collect::<Result<_, _>>()
        };
        Ok(SearchSet { include: compile(Role::Include)?, exclude: compile(Role::Exclude)? })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExpandError {
    #[error("nothing selected")]
    Empty,
    #[error("unknown query '{0}'")]
    UnknownQuery(String),
    #[error("no query has the label '{0}'")]
    UnknownLabel(String),
    #[error("query '{query}' refines '{parent}', which doesn't exist (not loaded or renamed)")]
    OrphanedChild { query: String, parent: String },
    #[error("query '{0}' is its own ancestor (parent cycle)")]
    ParentCycle(String),
    #[error("parameter '${param}' of query '{query}' has no value")]
    MissingParam { query: String, param: String },
    #[error("query '{query}' with these parameter values isn't valid NFD: {err}")]
    Invalid { query: String, err: DslError },
}

/// Qualified ids of the picks, labels resolved, in order, without duplicates.
fn resolve(lib: &Library, picks: &[Pick]) -> Result<Vec<String>, ExpandError> {
    let mut out: Vec<String> = Vec::new();
    for p in picks {
        let ids = match p {
            Pick::Query(id) => {
                lib.get(id).ok_or_else(|| ExpandError::UnknownQuery(id.clone()))?;
                vec![id.clone()]
            }
            Pick::Label(l) => match lib.with_label(l) {
                v if v.is_empty() => return Err(ExpandError::UnknownLabel(l.clone())),
                v => v,
            },
        };
        for id in ids {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// Expand a selection into plain NFD. Labels resolve against `lib` now.
pub fn expand(sel: &Selection, lib: &Library, params: &Params) -> Result<Expansion, ExpandError> {
    let include = resolve(lib, &sel.include)?;
    let exclude = resolve(lib, &sel.exclude)?;
    if include.is_empty() && exclude.is_empty() {
        return Err(ExpandError::Empty);
    }
    let mut nfd = String::new();
    let mut parts = Vec::new();
    let group = include.len() > 1 && !exclude.is_empty();
    if group {
        nfd.push('(');
    }
    for (i, id) in include.iter().enumerate() {
        if i > 0 {
            nfd.push_str(" OR ");
        }
        // AND binds tighter than OR, so a chain needs no parentheses here.
        parts.push(part(lib, id, Role::Include, params, &mut nfd)?);
    }
    if group {
        nfd.push(')');
    }
    for (i, id) in exclude.iter().enumerate() {
        if i > 0 || !include.is_empty() {
            nfd.push_str(" AND ");
        }
        nfd.push_str("NOT ");
        let chain = chain_len(lib, id)?;
        if chain > 1 {
            nfd.push('(');
        }
        parts.push(part(lib, id, Role::Exclude, params, &mut nfd)?);
        if chain > 1 {
            nfd.push(')');
        }
    }
    Ok(Expansion { nfd, parts })
}

/// Qualified ids from the root to `id`.
fn chain_of(lib: &Library, id: &str) -> Result<Vec<String>, ExpandError> {
    let mut chain = vec![id.to_owned()];
    let mut cur = id.to_owned();
    loop {
        let (pack, q) = lib.get(&cur).ok_or_else(|| ExpandError::UnknownQuery(cur.clone()))?;
        let Some(parent) = pack.parent_of(q) else { break };
        if chain.contains(&parent) {
            return Err(ExpandError::ParentCycle(id.to_owned()));
        }
        if lib.get(&parent).is_none() {
            return Err(ExpandError::OrphanedChild { query: cur, parent });
        }
        chain.push(parent.clone());
        cur = parent;
    }
    chain.reverse();
    Ok(chain)
}

fn chain_len(lib: &Library, id: &str) -> Result<usize, ExpandError> {
    chain_of(lib, id).map(|c| c.len())
}

/// Append `(root) AND … AND (query)` to `nfd`.
fn part(lib: &Library, id: &str, role: Role, params: &Params, nfd: &mut String) -> Result<Part, ExpandError> {
    let start = nfd.len();
    let mut chain = Vec::new();
    for (i, qid) in chain_of(lib, id)?.into_iter().enumerate() {
        if i > 0 {
            nfd.push_str(" AND ");
        }
        let (_, q) = lib.get(&qid).expect("chain_of checked it");
        let text = render(
            &q.text,
            true,
            |name, ctx, _| match params.get(name) {
                Some(v) => Ok(substitute(v, ctx)),
                None => Err(ExpandError::MissingParam { query: qid.clone(), param: name.to_owned() }),
            },
            |_| unreachable!("the pack parser rejects an unterminated ${{"),
        )?;
        let s = nfd.len();
        nfd.push('(');
        nfd.push_str(&text);
        nfd.push(')');
        if let Err(err) = crate::dsl::parse(&nfd[s..]) {
            let err = DslError { pos: err.pos.saturating_sub(1).min(text.len()), ..err };
            return Err(ExpandError::Invalid { query: qid, err });
        }
        chain.push((qid, s..nfd.len()));
    }
    Ok(Part { query: id.to_owned(), role, range: start..nfd.len(), chain })
}

/// A parameter value as NFD text for its context.
fn substitute(v: &ParamValue, ctx: Ctx) -> String {
    let mut out = String::with_capacity(v.value.len() + 8);
    match ctx {
        Ctx::Regex => {
            for c in v.value.chars() {
                match c {
                    // `\x2f`, not `\/`: a value ending in `\` must not
                    // escape the closing delimiter.
                    '/' => out.push_str("\\x2f"),
                    '\\' if !v.raw => out.push_str("\\x5c"),
                    c if !v.raw && regex_syntax_meta(c) => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
        }
        Ctx::Str => push_string_body(&mut out, &v.value),
        Ctx::Bare => {
            out.push('"');
            push_string_body(&mut out, &v.value);
            out.push('"');
        }
    }
    out
}

fn push_string_body(out: &mut String, s: &str) {
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
}

/// Characters `regex::escape` escapes.
fn regex_syntax_meta(c: char) -> bool {
    let mut buf = [0u8; 4];
    let s = c.encode_utf8(&mut buf);
    regex::escape(s) != *s
}

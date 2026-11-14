//! Expansion: a selection from the [`Library`] becomes plain NFD.
//!
//! Each picked query stands for its parent chain ANDed with its own text:
//! `(root) AND (…) AND (query)`. Included queries are OR'ed, required ones
//! are ANDed to that (`(a OR b) AND r`), excluded ones are added as
//! `AND NOT`. The result is one line of NFD that compiles on
//! its own, plus provenance: the byte range each query occupies.
//!
//! A pick whose ancestor is picked in the same role adds nothing
//! (`P OR P AND C` is `P`, `NOT P AND NOT (P AND C)` is `NOT P`), so it is
//! left out of the NFD. It is still a [`Part`], [`covered`](Part::covered)
//! by that ancestor, with its own text kept aside.

use super::{Ctx, Library, render};
use crate::dsl::DslError;
use crate::event::FieldDef;
use crate::{Filter, Query, SearchSet};
use std::collections::BTreeMap;
use std::ops::Range;

/// One pick from the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// A query by qualified id (`vision.frame_drops`).
    Query(String),
    /// A query with arguments of its own, over the shared ones: one
    /// instance of it (`query-instances.md`). Several picks of one query
    /// with different arguments are several instances.
    QueryWith(String, Params),
    /// Every query with this label or one below it, resolved when expanded
    /// (see [`Selection::snapshot`]).
    Label(String),
}

impl Pick {
    /// A query as given on a command line: `ID`, or `ID(NAME=VALUE, …)`,
    /// an instance of it with its own arguments (`query-instances.md`). A
    /// value holding `,`, `)` or spaces is quoted (`"…"`, with `\"` and
    /// `\\`). Query names may hold parentheses: `(…)` counts as arguments
    /// only after the id of a query `lib` has.
    pub fn parse_query(lib: &Library, s: &str) -> Result<Pick, String> {
        let Some((id, rest)) = s.split_once('(').filter(|(id, r)| r.ends_with(')') && lib.get(id.trim()).is_some())
        else {
            return Ok(Pick::Query(s.to_owned()));
        };
        let inner = &rest[..rest.len() - 1];
        let mut args = Params::new();
        let mut chars = inner.chars().peekable();
        loop {
            let mut name = String::new();
            let mut eq = false;
            for c in chars.by_ref() {
                if c == '=' {
                    eq = true;
                    break;
                }
                name.push(c);
            }
            let name = name.trim();
            if name.is_empty() && !eq {
                break;
            }
            if !eq || !super::is_name(name) {
                return Err(format!("{s}: expected NAME=VALUE arguments, got '{name}'"));
            }
            while chars.next_if(|c| *c == ' ').is_some() {}
            let mut value = String::new();
            if chars.next_if_eq(&'"').is_some() {
                loop {
                    match chars.next() {
                        Some('\\') => value.extend(chars.next()),
                        Some('"') => break,
                        Some(c) => value.push(c),
                        None => return Err(format!("{s}: missing closing '\"'")),
                    }
                }
                while chars.next_if(|c| *c == ' ').is_some() {}
                if !matches!(chars.next(), Some(',') | None) {
                    return Err(format!("{s}: expected ',' after a quoted value"));
                }
            } else {
                value = chars.by_ref().take_while(|&c| c != ',').collect::<String>().trim().to_owned();
            }
            args.insert(name.to_owned(), ParamValue::literal(value));
        }
        Ok(Pick::QueryWith(id.trim().to_owned(), args))
    }
}

/// What was picked for Filter or for Search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub include: Vec<Pick>,
    /// Every kept line must match these too (AND).
    pub require: Vec<Pick>,
    pub exclude: Vec<Pick>,
    /// Queries that don't filter: they give signals (their named groups)
    /// and can colour lines.
    pub signal: Vec<Pick>,
}

impl Selection {
    /// The same selection with every label replaced by the queries that
    /// carry it now. A query labelled later doesn't join a snapshot.
    pub fn snapshot(&self, lib: &Library) -> Result<Selection, ExpandError> {
        let freeze = |picks: &[Pick]| -> Result<Vec<Pick>, ExpandError> {
            let mut out = Vec::new();
            for p in picks {
                match p {
                    Pick::Label(_) => out.extend(
                        resolve(lib, std::slice::from_ref(p), &Params::new())?.into_iter().map(|r| Pick::Query(r.id)),
                    ),
                    p => out.push(p.clone()),
                }
            }
            Ok(out)
        };
        Ok(Selection {
            include: freeze(&self.include)?,
            require: freeze(&self.require)?,
            exclude: freeze(&self.exclude)?,
            signal: freeze(&self.signal)?,
        })
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
    /// ANDed with the includes: lines must match it too.
    Require,
    Exclude,
    /// Not in the filter: signals and line colours only.
    Signal,
}

/// Where one picked query sits in [`Expansion::nfd`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Qualified id of the picked query.
    pub query: String,
    pub role: Role,
    /// The pick it comes from: its index in its role's list of the
    /// [`Selection`] (a label's queries share one). Instances of a query
    /// share the id, not the pick.
    pub pick: usize,
    /// The pick's own arguments (an instance's; empty: the shared ones).
    pub args: Params,
    /// The query's full meaning (parent chain AND own text), valid NFD on
    /// its own: a range of [`Expansion::nfd`], or of
    /// [`Expansion::aside`] when the part is covered.
    pub range: Range<usize>,
    /// The parent chain from the root down to the query itself: qualified
    /// id and the range of that query's own (parenthesized) text, in the
    /// same string as `range`.
    pub chain: Vec<(String, Range<usize>)>,
    /// The picked ancestor (same role) that makes this pick redundant: the
    /// part isn't in `nfd`.
    pub covered: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    /// Plain NFD, one line.
    pub nfd: String,
    /// Included parts first, in pick order, then required ones, then
    /// excluded ones, then the signal ones (in `aside`).
    pub parts: Vec<Part>,
    /// The texts of covered parts, which `nfd` leaves out.
    pub aside: String,
}

impl Expansion {
    /// The standalone NFD of a part.
    pub fn part_nfd(&self, part: &Part) -> &str {
        let text = if part.covered.is_some() || part.role == Role::Signal { &self.aside } else { &self.nfd };
        &text[part.range.clone()]
    }

    pub fn parts_with(&self, role: Role) -> impl Iterator<Item = &Part> {
        self.parts.iter().filter(move |p| p.role == role)
    }

    /// The parts as search queries: hits are reported per included part,
    /// in the order of [`parts_with`](Self::parts_with)`(Role::Include)`.
    /// Required parts are ANDed to each included one (alone, they are the
    /// only one).
    pub fn search_set(&self) -> Result<SearchSet, DslError> {
        let required: Vec<&str> = self.parts_with(Role::Require).map(|p| self.part_nfd(p)).collect();
        let with_required = |nfd: &str| {
            let mut s = format!("({nfd})");
            for r in &required {
                s += &format!(" AND ({r})");
            }
            s
        };
        let mut include: Vec<Filter> = self
            .parts_with(Role::Include)
            .map(|p| Query::compile(&with_required(self.part_nfd(p))).map(Filter::Query))
            .collect::<Result<_, _>>()?;
        if include.is_empty() && !required.is_empty() {
            let all = required.iter().map(|r| format!("({r})")).collect::<Vec<_>>().join(" AND ");
            include.push(Filter::Query(Query::compile(&all)?));
        }
        let exclude = self
            .parts_with(Role::Exclude)
            .map(|p| Query::compile(self.part_nfd(p)).map(Filter::Query))
            .collect::<Result<_, _>>()?;
        Ok(SearchSet { include, exclude })
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

/// A picked query, resolved: its id, the pick it comes from, the pick's own
/// arguments and the parameters it expands with (its own over the shared).
struct Resolved {
    id: String,
    pick: usize,
    args: Params,
    effective: Params,
}

/// The picks' queries, labels resolved, in order, without duplicates: a
/// query with the same effective arguments as an earlier one adds nothing
/// (instances of it with other arguments do).
fn resolve(lib: &Library, picks: &[Pick], shared: &Params) -> Result<Vec<Resolved>, ExpandError> {
    let mut out: Vec<Resolved> = Vec::new();
    for (pick, p) in picks.iter().enumerate() {
        let known = |id: &String| lib.get(id).map(|_| ()).ok_or_else(|| ExpandError::UnknownQuery(id.clone()));
        let (ids, args) = match p {
            Pick::Query(id) => {
                known(id)?;
                (vec![id.clone()], Params::new())
            }
            Pick::QueryWith(id, args) => {
                known(id)?;
                (vec![id.clone()], args.clone())
            }
            Pick::Label(l) => match lib.with_label(l) {
                v if v.is_empty() => return Err(ExpandError::UnknownLabel(l.clone())),
                v => (v, Params::new()),
            },
        };
        let mut effective = shared.clone();
        effective.extend(args.clone());
        for id in ids {
            if !out.iter().any(|r| r.id == id && r.effective == effective) {
                out.push(Resolved { id, pick, args: args.clone(), effective: effective.clone() });
            }
        }
    }
    Ok(out)
}

/// Expand a selection into plain NFD. Labels resolve against `lib` now;
/// `params` are the shared parameter values, a pick's own over them.
pub fn expand(sel: &Selection, lib: &Library, params: &Params) -> Result<Expansion, ExpandError> {
    let include = resolve(lib, &sel.include, params)?;
    let require = resolve(lib, &sel.require, params)?;
    let exclude = resolve(lib, &sel.exclude, params)?;
    let signal = resolve(lib, &sel.signal, params)?;
    if include.is_empty() && require.is_empty() && exclude.is_empty() && signal.is_empty() {
        return Err(ExpandError::Empty);
    }
    let include_cov = covers(lib, &include)?;
    let exclude_cov = covers(lib, &exclude)?;
    let (mut nfd, mut aside) = (String::new(), String::new());
    let mut parts = Vec::new();
    let shown = include_cov.iter().filter(|c| c.is_none()).count();
    let group = shown > 1 && (!require.is_empty() || exclude_cov.iter().any(|c| c.is_none()));
    if group {
        nfd.push('(');
    }
    let mut first = true;
    for (r, covered) in include.iter().zip(include_cov) {
        if covered.is_some() {
            parts.push(Part { covered, ..part(lib, r, Role::Include, &mut aside)? });
            continue;
        }
        if !first {
            nfd.push_str(" OR ");
        }
        first = false;
        // AND binds tighter than OR, so a chain needs no parentheses here.
        parts.push(part(lib, r, Role::Include, &mut nfd)?);
    }
    if group {
        nfd.push(')');
    }
    let mut first = shown == 0;
    // Required parts: AND binds tighter than OR, and the includes are
    // grouped when there are several, so a chain needs no parentheses.
    for r in &require {
        if !first {
            nfd.push_str(" AND ");
        }
        first = false;
        parts.push(part(lib, r, Role::Require, &mut nfd)?);
    }
    for (r, covered) in exclude.iter().zip(exclude_cov) {
        if covered.is_some() {
            parts.push(Part { covered, ..part(lib, r, Role::Exclude, &mut aside)? });
            continue;
        }
        if !first {
            nfd.push_str(" AND ");
        }
        first = false;
        nfd.push_str("NOT ");
        let chain = chain_len(lib, &r.id)?;
        if chain > 1 {
            nfd.push('(');
        }
        parts.push(part(lib, r, Role::Exclude, &mut nfd)?);
        if chain > 1 {
            nfd.push(')');
        }
    }
    // Signal queries don't filter: their texts go aside.
    for r in &signal {
        if !aside.is_empty() {
            aside.push(' ');
        }
        parts.push(part(lib, r, Role::Signal, &mut aside)?);
    }
    Ok(Expansion { nfd, parts, aside })
}

/// For each pick: the nearest ancestor of it that is also picked, with the
/// same effective arguments (another instance's parent doesn't cover it).
fn covers(lib: &Library, picks: &[Resolved]) -> Result<Vec<Option<String>>, ExpandError> {
    picks
        .iter()
        .map(|r| {
            let chain = chain_of(lib, &r.id)?;
            let picked = |a: &&String| picks.iter().any(|o| o.id == **a && o.effective == r.effective);
            Ok(chain[..chain.len() - 1].iter().rev().find(picked).cloned())
        })
        .collect()
}

impl Library {
    /// Qualified ids from the root of `id`'s parent chain down to `id`;
    /// empty if the query is unknown or its chain is broken.
    pub fn chain(&self, id: &str) -> Vec<String> {
        chain_of(self, id).unwrap_or_default()
    }

    /// Source fields that query `id` (with its parent chain) uses but
    /// `schema` doesn't have, in order. Such a query can't run on that
    /// source ("unknown field"). Unknown or broken queries give none (they
    /// fail elsewhere with their own error).
    pub fn missing_fields(&self, id: &str, schema: &[FieldDef]) -> Vec<String> {
        self.missing_in(self.chain(id), schema)
    }

    /// Like [`missing_fields`](Self::missing_fields), for the query's own
    /// text only (without its parents).
    pub fn missing_own_fields(&self, id: &str, schema: &[FieldDef]) -> Vec<String> {
        self.missing_in([id.to_owned()], schema)
    }

    fn missing_in(&self, ids: impl IntoIterator<Item = String>, schema: &[FieldDef]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for qid in ids {
            let Some((_, q)) = self.get(&qid) else { continue };
            // Parameters as placeholder values: only the fields matter (`"I"`
            // is a level too).
            let text = render(
                &q.text,
                false,
                |_, ctx, _| Ok::<_, ()>(if ctx == Ctx::Bare { "\"I\"".into() } else { "x".into() }),
                |_| (),
            );
            let Some(expr) = text.ok().and_then(|t| crate::dsl::parse(&t).ok()) else { continue };
            for f in expr.field_names() {
                if !schema.iter().any(|d| d.name == f) && !out.contains(&f) {
                    out.push(f);
                }
            }
        }
        out
    }

    /// Parameter names the queries need, their parent chains included, in
    /// first-use order. Unknown ids and broken chains are skipped (expanding
    /// reports them).
    pub fn params_needed<'a>(&self, ids: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for id in ids {
            for qid in chain_of(self, id).unwrap_or_default() {
                for p in self.get(&qid).map(|(_, q)| q.params.as_slice()).unwrap_or_default() {
                    if !out.contains(p) {
                        out.push(p.clone());
                    }
                }
            }
        }
        out
    }
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
fn part(lib: &Library, r: &Resolved, role: Role, nfd: &mut String) -> Result<Part, ExpandError> {
    let (id, params) = (r.id.as_str(), &r.effective);
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
    let (pick, args) = (r.pick, r.args.clone());
    Ok(Part { query: id.to_owned(), role, pick, args, range: start..nfd.len(), chain, covered: None })
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

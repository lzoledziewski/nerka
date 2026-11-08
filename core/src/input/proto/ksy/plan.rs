//! Straight decoders for objects inside lists.
//!
//! The general reader ([`super::run`]) evaluates a definition lazily, field
//! by field, which suits headers (their positions are memoized across
//! messages) but costs a lookup per step inside list elements, where
//! nothing can be memoized. A type used inside a list is compiled here,
//! once per definition, into a [`Plan`]: its attributes in order, each a
//! concrete read (a number, bits, bytes of a size, a nested object, a case
//! table), and its conditions, sizes, instances and template as small
//! expressions over "field `i` of this object or of the one `up` levels
//! above". Anything on the fixed part of the message (`_root.msbf`) stays a
//! normal expression ([`Fast::Static`]). An element is then decoded once,
//! in order, into a record of values ([`super::run`]).
//!
//! A type whose features a plan can't express (checks, `contents`, lists
//! inside it, positional instances, `_io` beyond its size) has no plan and
//! is read by the general reader: the results are the same either way.

use super::expr::{BinOp, UnOp};
use super::model::{CExpr, Def, Endian, EndianRule, InstKind, Kind, Lit, NodeId, Repeat, Size};
use super::template::Piece;

/// A way down from a record: through the objects in these attributes
/// (each of the node given).
pub type Down = Box<[(usize, NodeId)]>;

/// An expression over a record and the records above it.
#[derive(Debug, Clone, PartialEq)]
pub enum Fast {
    Lit(Lit),
    /// Attribute `i` of a record: `up` levels above this one (0: this
    /// one), then down through the objects in attributes `down` (each must
    /// be of the node given: a switch may have picked another case).
    Field {
        up: u8,
        down: Down,
        i: usize,
    },
    /// Value instance `k` of such a record.
    Inst {
        up: u8,
        down: Down,
        k: usize,
    },
    /// On the fixed part of the message: the general reader's, memoized.
    Static(CExpr),
    /// The size of a record's stream, in bytes.
    IoSize {
        up: u8,
        down: Down,
    },
    Length(Box<Fast>),
    Index(Box<Fast>, Box<Fast>),
    ToInt(Box<Fast>),
    Unary(UnOp, Box<Fast>),
    Binary(BinOp, Box<Fast>, Box<Fast>),
    Cond(Box<Fast>, Box<Fast>, Box<Fast>),
}

/// The byte order of a number: fixed, or that of a node on the fixed part
/// of the message (DLT's payload: MSBF).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Order {
    Fixed(Endian),
    Of(NodeId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PSize {
    /// Bytes.
    Fixed(u64),
    /// Bytes, from an expression.
    Expr(Fast),
    /// To the end of the stream.
    Eos,
    /// Up to and including the first such byte.
    Terminated(u8),
}

/// How an attribute is read.
#[derive(Debug, Clone, PartialEq)]
pub enum Read {
    Int {
        bytes: u8,
        signed: bool,
        order: Order,
    },
    Float {
        bytes: u8,
        order: Order,
    },
    Bits(u8),
    Bytes {
        size: PSize,
        text: bool,
        terminator: Option<u8>,
    },
    /// A nested object (with a stream of its own if sized).
    Obj {
        node: NodeId,
        size: Option<PSize>,
    },
    Switch {
        on: Fast,
        cases: Vec<(Lit, Read)>,
        default: Option<Box<Read>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PAttr {
    pub cond: Option<Fast>,
    pub read: Read,
    pub bits: bool,
    pub enum_id: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub attrs: Vec<PAttr>,
    /// Value instances, by their index.
    pub insts: Vec<Fast>,
    pub text: Option<Vec<Piece<Fast>>>,
}

/// Plans for the element types of every list, and the types inside them;
/// `None` where a type has none.
pub fn compile(def: &Def) -> Vec<Option<Plan>> {
    let mut c = Compiler { def, plans: vec![State::Todo; def.nodes.len()] };
    for node in &def.nodes {
        for a in &node.attrs {
            if a.repeat != Repeat::No
                && let Kind::User(e) = a.kind
            {
                c.plan(e);
            }
        }
    }
    c.plans
        .into_iter()
        .map(|s| match s {
            State::Done(p) => p.map(|p| *p),
            _ => None,
        })
        .collect()
}

#[derive(Clone)]
enum State {
    Todo,
    Busy,
    Done(Option<Box<Plan>>),
}

struct Compiler<'d> {
    def: &'d Def,
    plans: Vec<State>,
}

impl Compiler<'_> {
    /// Node `n`'s plan, compiled once; whether it has one.
    fn plan(&mut self, n: NodeId) -> bool {
        match &self.plans[n] {
            State::Done(p) => return p.is_some(),
            State::Busy => return false,
            State::Todo => {}
        }
        self.plans[n] = State::Busy;
        let p = self.build(n).map(Box::new);
        let ok = p.is_some();
        self.plans[n] = State::Done(p);
        ok
    }

    fn build(&mut self, n: NodeId) -> Option<Plan> {
        let node = &self.def.nodes[n];
        let mut attrs = Vec::new();
        for a in &node.attrs {
            if a.repeat != Repeat::No || !a.checks.is_empty() || a.contents.is_some() || a.partial.is_some() {
                return None;
            }
            let cond = match &a.cond {
                Some(c) => Some(self.expr(n, c)?),
                None => None,
            };
            let read = self.read(n, &a.kind, &a.size, a.terminator)?;
            attrs.push(PAttr { cond, read, bits: a.is_bits(), enum_id: a.enum_id });
        }
        let mut insts = Vec::new();
        for i in &node.insts {
            // A condition on a value instance is part of its value.
            let InstKind::Value(v) = &i.kind else { return None };
            let v = self.expr(n, v)?;
            insts.push(match &i.cond {
                Some(c) => Fast::Cond(Box::new(self.expr(n, c)?), Box::new(v), Box::new(Fast::Lit(Lit::Bool(false)))),
                None => v,
            });
        }
        let text = match &node.text {
            Some(t) => Some(self.template(n, t)?),
            None => None,
        };
        Some(Plan { attrs, insts, text })
    }

    fn template(&mut self, n: NodeId, t: &[Piece<CExpr>]) -> Option<Vec<Piece<Fast>>> {
        t.iter()
            .map(|p| {
                Some(match p {
                    Piece::Text(s) => Piece::Text(s.clone()),
                    Piece::Value(e, f) => Piece::Value(self.expr(n, e)?, f.clone()),
                    Piece::Group(alts) => {
                        Piece::Group(alts.iter().map(|a| self.template(n, a)).collect::<Option<Vec<_>>>()?)
                    }
                })
            })
            .collect()
    }

    fn read(&mut self, n: NodeId, kind: &Kind, size: &Size, terminator: Option<u8>) -> Option<Read> {
        Some(match kind {
            Kind::Int { bytes, signed, endian } => {
                Read::Int { bytes: *bytes, signed: *signed, order: self.order(n, *endian, *bytes > 1)? }
            }
            Kind::Float { bytes, endian } => Read::Float { bytes: *bytes, order: self.order(n, *endian, true)? },
            Kind::Bits(k) => Read::Bits(*k),
            Kind::Bytes | Kind::Str => Read::Bytes {
                size: self.size(n, size)?,
                text: matches!(kind, Kind::Str),
                terminator: if matches!(size, Size::Terminated(_)) { None } else { terminator },
            },
            Kind::User(c) => {
                if !self.plan(*c) {
                    return None;
                }
                let size = match size {
                    Size::Child => None,
                    s => Some(self.size(n, s)?),
                };
                Read::Obj { node: *c, size }
            }
            Kind::Switch(sw) => {
                let on = self.expr(n, &sw.on)?;
                let mut cases = Vec::new();
                for (k, case) in &sw.cases {
                    cases.push((k.clone(), self.read(n, case, size, terminator)?));
                }
                let default = match &sw.default {
                    Some(d) => Some(Box::new(self.read(n, d, size, terminator)?)),
                    None => None,
                };
                Read::Switch { on, cases, default }
            }
        })
    }

    fn size(&mut self, n: NodeId, size: &Size) -> Option<PSize> {
        Some(match size {
            Size::Fixed(v) => PSize::Fixed(*v),
            Size::Expr(e) => PSize::Expr(self.expr(n, e)?),
            Size::Eos => PSize::Eos,
            Size::Terminated(t) => PSize::Terminated(*t),
            Size::Bits(_) | Size::Child => return None,
        })
    }

    /// The byte order of a number in node `n`: its own, or the rule of the
    /// nearest node above with one (fixed, or on the fixed part of the
    /// message).
    fn order(&self, n: NodeId, own: Option<Endian>, needed: bool) -> Option<Order> {
        if let Some(e) = own {
            return Some(Order::Fixed(e));
        }
        if !needed {
            return Some(Order::Fixed(Endian::Be));
        }
        let mut m = n;
        loop {
            let node = &self.def.nodes[m];
            match &node.endian {
                Some(EndianRule::Fixed(e)) => return Some(Order::Fixed(*e)),
                Some(EndianRule::Switch { .. }) if !node.dynamic => return Some(Order::Of(m)),
                Some(EndianRule::Switch { .. }) => return None,
                None => m = node.parent?,
            }
        }
    }

    /// Where node `m` is from node `n`, inside the same list element: how
    /// many records up, then which attributes down (to an object of which
    /// node).
    fn path(&self, n: NodeId, m: NodeId) -> Option<(u8, Down)> {
        let mut down = Vec::new();
        let mut at = m;
        loop {
            if let Some(up) = self.up(n, at) {
                down.reverse();
                return Some((up, down.into_boxed_slice()));
            }
            let node = &self.def.nodes[at];
            // Through attributes only (not instances), and not out of the
            // list element.
            let (p, i) = match node.place {
                super::model::Place::Attr(p, i) | super::model::Place::Case(p, i, _) => (p, i),
                _ => return None,
            };
            if node.anchor == Some(at) || down.len() > 8 {
                return None;
            }
            down.push((i, at));
            at = p;
        }
    }

    /// How many records above node `n` node `m` is, if it is one of them
    /// (inside the same list element).
    fn up(&self, n: NodeId, m: NodeId) -> Option<u8> {
        let (mut k, mut at) = (0u8, n);
        loop {
            if at == m {
                return Some(k);
            }
            let node = &self.def.nodes[at];
            // The list element itself: above it is the fixed part.
            if node.anchor == Some(at) {
                return None;
            }
            at = node.parent?;
            k = k.checked_add(1)?;
        }
    }

    fn expr(&mut self, n: NodeId, e: &CExpr) -> Option<Fast> {
        let rec = |c: &mut Self, x: &CExpr| c.expr(n, x).map(Box::new);
        Some(match e {
            CExpr::Lit(l) => Fast::Lit(l.clone()),
            CExpr::Attr(m, i) => match self.path(n, *m) {
                Some((up, down)) => Fast::Field { up, down, i: *i },
                None if !self.def.nodes[*m].dynamic => Fast::Static(e.clone()),
                None => return None,
            },
            CExpr::Inst(m, k) => match self.path(n, *m) {
                Some((up, down)) => {
                    if !matches!(self.def.nodes[*m].insts[*k].kind, InstKind::Value(_)) {
                        return None;
                    }
                    Fast::Inst { up, down, k: *k }
                }
                None if !self.def.nodes[*m].dynamic => Fast::Static(e.clone()),
                None => return None,
            },
            CExpr::IoSize(m) => match self.path(n, *m) {
                Some((up, down)) => Fast::IoSize { up, down },
                None if !self.def.nodes[*m].dynamic => Fast::Static(e.clone()),
                None => return None,
            },
            CExpr::Origin(_) => Fast::Static(e.clone()),
            CExpr::Length(a) => Fast::Length(rec(self, a)?),
            CExpr::Index(a, i) => Fast::Index(rec(self, a)?, rec(self, i)?),
            CExpr::ToInt(a) => Fast::ToInt(rec(self, a)?),
            CExpr::Unary(op, a) => Fast::Unary(*op, rec(self, a)?),
            CExpr::Binary(op, a, b) => Fast::Binary(*op, rec(self, a)?, rec(self, b)?),
            CExpr::Cond(c, a, b) => Fast::Cond(rec(self, c)?, rec(self, a)?, rec(self, b)?),
            CExpr::In(..) | CExpr::Underscore | CExpr::Rest => return None,
        })
    }
}

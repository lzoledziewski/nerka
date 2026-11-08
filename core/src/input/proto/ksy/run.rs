//! Reading a message with a compiled definition ([`super::model::Def`]).
//!
//! Everything is read lazily and in place: asking for `body.ext.apid`
//! finds where `ext` starts, where `apid` is in it, and reads those bytes;
//! nothing else is parsed.
//!
//! Where an attribute (or a node) starts depends on the attributes before
//! it: their `if`s and sizes. That computation is memoized ([`Memo`]): if
//! it read a single byte of the message (as flag-dependent headers do: all
//! of DLT's optional fields depend on `htyp`), its result is stored in a
//! 256-entry table under that byte's value, and from then on it costs one
//! byte read and a lookup. The table fills itself as messages are read,
//! each entry once, from any thread. A computation is a function of what it
//! reads, so the same byte value gives the same result; one whose first
//! read had another position, or that read more (a length, the message's
//! size), is marked and evaluated each time.

use super::expr::{BinOp, UnOp};
use super::model::{
    Attr, CExpr, Check, Def, Endian, EndianRule, InstKind, Kind, Lit, NodeId, OriginField, Place, Size, Ty,
};
use crate::input::proto::Origin;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::sync::OnceLock;

/// A memoized position: see the module documentation.
pub struct Memo {
    shape: OnceLock<Shape>,
    entries: OnceLock<Box<[OnceLock<Entry>]>>,
}

#[derive(Debug, Clone, Copy)]
enum Shape {
    /// Read nothing: the same for every message.
    Const(Option<i64>),
    /// Keyed by the byte at this offset.
    Keyed(usize),
    /// Read no single byte first (a length): never memoized.
    Never,
}

#[derive(Debug, Clone, Copy)]
enum Entry {
    Known(Option<i64>),
    /// For this byte value the computation reads more: evaluated each time.
    Generic,
}

impl Memo {
    pub fn new() -> Memo {
        Memo { shape: OnceLock::new(), entries: OnceLock::new() }
    }
}

impl Default for Memo {
    fn default() -> Self {
        Memo::new()
    }
}

/// What a computation read.
#[derive(Debug, Clone, Copy, Default)]
struct Trace {
    first: Option<usize>,
    /// Another byte too, or something that isn't a byte (a length).
    many: bool,
}

/// No value: the message is too short, a read is outside its stream, a
/// check failed, or nesting went too deep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stop;

pub type R<T> = Result<T, Stop>;

#[derive(Debug, Clone, PartialEq)]
pub enum Val<'m> {
    Null,
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Bytes of the message.
    Msg(&'m [u8]),
    /// Bytes of the definition (interned).
    Lit(&'static [u8]),
    Own(Vec<u8>),
}

impl<'m> Val<'m> {
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Val::Msg(b) => Some(b),
            Val::Lit(b) => Some(b),
            Val::Own(b) => Some(b),
            _ => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Val::Null => false,
            Val::Bool(b) => *b,
            Val::Int(v) => *v != 0,
            Val::Float(v) => *v != 0.0,
            v => v.bytes().is_some_and(|b| !b.is_empty()),
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Val::Int(v) => Some(*v as f64),
            Val::Float(v) => Some(*v),
            _ => None,
        }
    }
}

fn lit<'m>(l: &Lit) -> Val<'m> {
    match l {
        Lit::Int(v) => Val::Int(*v),
        Lit::Float(v) => Val::Float(*v),
        Lit::Bool(v) => Val::Bool(*v),
        Lit::Bytes(b) => Val::Lit(b),
    }
}

/// Nesting deeper than this (instances using each other in a circle) has
/// no value.
const MAX_DEPTH: u32 = 200;

/// One message being read.
pub struct Eval<'d, 'm> {
    def: &'d Def,
    msg: &'m [u8],
    origin: Origin<'m>,
    /// The message's length in bytes, from this expression (framing), else
    /// `msg`'s.
    length: Option<&'d CExpr>,
    root_end: Cell<Option<i64>>,
    /// Check that reads stay inside their streams (splitting a datagram).
    /// A message that was split is known to fit, so reading its fields
    /// only stays inside `msg`.
    checked: bool,
    trace: Cell<Trace>,
    depth: Cell<u32>,
    underscore: RefCell<Val<'m>>,
    /// Per node (the first [`CACHED`]): where it starts and where its
    /// stream ends, once known, with what finding them read.
    starts: [Cell<(i64, Trace)>; CACHED],
    io_ends: [Cell<(i64, Trace)>; CACHED],
}

/// Nodes whose start and stream end a message keeps.
const CACHED: usize = 8;
const UNKNOWN: i64 = i64::MIN;

impl<'d, 'm> Eval<'d, 'm> {
    pub fn new(def: &'d Def, msg: &'m [u8], origin: Origin<'m>, length: Option<&'d CExpr>, checked: bool) -> Self {
        Eval {
            def,
            msg,
            origin,
            length,
            root_end: Cell::new(None),
            checked,
            trace: Cell::new(Trace::default()),
            depth: Cell::new(0),
            underscore: RefCell::new(Val::Null),
            starts: [const { Cell::new((UNKNOWN, Trace { first: None, many: false })) }; CACHED],
            io_ends: [const { Cell::new((UNKNOWN, Trace { first: None, many: false })) }; CACHED],
        }
    }

    /// The message's length in bytes, from the framing expression, if it
    /// fits `msg`.
    pub fn length(&self) -> R<usize> {
        Ok((self.root_end()? / 8) as usize)
    }

    // --- reading bytes, and what was read --------------------------------

    fn record(&self, at: usize) {
        let mut t = self.trace.get();
        match t.first {
            None => t.first = Some(at),
            Some(p) if p == at => {}
            Some(_) => t.many = true,
        }
        self.trace.set(t);
    }

    fn record_many(&self) {
        let mut t = self.trace.get();
        t.many = true;
        self.trace.set(t);
    }

    fn byte(&self, at: usize) -> R<u8> {
        self.record(at);
        self.msg.get(at).copied().ok_or(Stop)
    }

    fn slice(&self, from: usize, to: usize) -> R<&'m [u8]> {
        if to > from {
            self.record(from);
            if to - from > 1 {
                self.record_many();
            }
        }
        self.msg.get(from..to).ok_or(Stop)
    }

    /// `f`, with what it reads recorded apart; then added to the outer
    /// record.
    fn traced(&self, f: impl FnOnce(&Self) -> R<Option<i64>>) -> R<(Option<i64>, Trace)> {
        let outer = self.trace.replace(Trace::default());
        let r = f(self);
        let inner = self.trace.replace(outer);
        self.replay(inner);
        r.map(|v| (v, inner))
    }

    /// `f` without recording what it reads (bounds checks: they decide
    /// whether there is a value, not which).
    fn untraced<T>(&self, f: impl FnOnce(&Self) -> R<T>) -> R<T> {
        let outer = self.trace.get();
        let r = f(self);
        self.trace.set(outer);
        r
    }

    fn memo(&self, m: &Memo, f: impl FnOnce(&Self) -> R<Option<i64>>) -> R<Option<i64>> {
        match m.shape.get() {
            Some(Shape::Const(v)) => Ok(*v),
            Some(&Shape::Keyed(p)) => {
                let b = self.byte(p)?;
                let entry = &m.entries.get().ok_or(Stop)?[b as usize];
                match entry.get() {
                    Some(Entry::Known(v)) => Ok(*v),
                    Some(Entry::Generic) => f(self),
                    None => {
                        let (v, t) = self.traced(f)?;
                        let only_p = !t.many && t.first.is_none_or(|q| q == p);
                        let _ = entry.set(if only_p { Entry::Known(v) } else { Entry::Generic });
                        Ok(v)
                    }
                }
            }
            Some(Shape::Never) => f(self),
            None => {
                let (v, t) = self.traced(f)?;
                match t.first {
                    None if !t.many => {
                        let _ = m.shape.set(Shape::Const(v));
                    }
                    None => {
                        let _ = m.shape.set(Shape::Never);
                    }
                    Some(p) => {
                        let entries = m.entries.get_or_init(|| (0..256).map(|_| OnceLock::new()).collect());
                        if let Some(&b) = self.msg.get(p) {
                            let _ = entries[b as usize].set(if t.many { Entry::Generic } else { Entry::Known(v) });
                        }
                        let _ = m.shape.set(Shape::Keyed(p));
                    }
                }
                Ok(v)
            }
        }
    }

    fn enter(&self) -> R<()> {
        let d = self.depth.get() + 1;
        if d > MAX_DEPTH {
            return Err(Stop);
        }
        self.depth.set(d);
        Ok(())
    }

    fn leave(&self) {
        self.depth.set(self.depth.get() - 1);
    }

    fn nested<T>(&self, f: impl FnOnce(&Self) -> R<T>) -> R<T> {
        self.enter()?;
        let r = f(self);
        self.leave();
        r
    }

    // --- where things are -------------------------------------------------

    /// The message's length in bits.
    fn root_end(&self) -> R<i64> {
        // Always a dependency on more than a byte.
        self.record_many();
        if let Some(v) = self.root_end.get() {
            return Ok(v);
        }
        let v = match self.length {
            Some(e) => {
                // Reading the length itself is bounded by the buffer.
                self.root_end.set(Some(self.msg.len() as i64 * 8));
                let n = self.int(e);
                self.root_end.set(None);
                let n = n?;
                if n < 0 || n as usize > self.msg.len() {
                    return Err(Stop);
                }
                n * 8
            }
            None => self.msg.len() as i64 * 8,
        };
        self.root_end.set(Some(v));
        Ok(v)
    }

    /// Where node `n` starts, in bits from the message's start.
    pub fn node_start(&self, n: NodeId) -> R<i64> {
        let node = &self.def.nodes[n];
        match node.place {
            Place::Root => Ok(0),
            Place::Attr(p, i) => self.kept(self.starts.get(n), |e| {
                e.nested(|e| {
                    e.memo(&node.start, |e| {
                        let s = e.node_start(p)?;
                        Ok(e.attr_pos(p, i)?.map(|q| s + q))
                    })?
                    .ok_or(Stop)
                })
            }),
            Place::Inst(p, k) => self.nested(|e| {
                let inst = &e.def.nodes[p].insts[k];
                if let Some(c) = &inst.cond
                    && !e.eval(c)?.truthy()
                {
                    return Err(Stop);
                }
                let InstKind::Read { pos, .. } = &inst.kind else { return Err(Stop) };
                let s = e.io_start(p)?;
                Ok(s + e.int(pos)?.checked_mul(8).ok_or(Stop)?)
            }),
        }
    }

    /// Node `n`'s stream: where it starts and ends, in bits.
    pub fn io(&self, n: NodeId) -> R<(i64, i64)> {
        Ok((self.io_start(n)?, self.io_end(n)?))
    }

    /// Where node `n`'s stream starts: the node itself if it is sized, else
    /// the stream it reads from.
    fn io_start(&self, n: NodeId) -> R<i64> {
        let node = &self.def.nodes[n];
        match node.parent {
            None => Ok(0),
            Some(_) if node.sized => self.node_start(n),
            Some(p) => self.io_start(p),
        }
    }

    /// Where node `n`'s stream ends; kept for the message (bounds checks
    /// ask for it again and again).
    fn io_end(&self, n: NodeId) -> R<i64> {
        let node = &self.def.nodes[n];
        let Some(parent) = node.parent else { return self.root_end() };
        if !node.sized {
            return self.io_end(parent);
        }
        self.kept(self.io_ends.get(n), |e| {
            e.nested(|e| {
                let s = e.node_start(n)?;
                let size = match node.place {
                    Place::Attr(p, i) => e.attr_size(p, &e.def.nodes[p].attrs[i], s)?,
                    Place::Inst(p, k) => match &e.def.nodes[p].insts[k].kind {
                        InstKind::Read { read, .. } => e.attr_size(p, read, s)?,
                        InstKind::Value(_) => return Err(Stop),
                    },
                    Place::Root => return Err(Stop),
                };
                Ok(s + size)
            })
        })
    }

    /// `f`'s result, kept in `slot` for the rest of the message. What `f`
    /// read is kept too and recorded again on every use, so memo tables
    /// learn the same dependencies as without the slot.
    fn kept(&self, slot: Option<&Cell<(i64, Trace)>>, f: impl FnOnce(&Self) -> R<i64>) -> R<i64> {
        let Some(slot) = slot else { return f(self) };
        let (v, t) = slot.get();
        if v != UNKNOWN {
            self.replay(t);
            return Ok(v);
        }
        let outer = self.trace.replace(Trace::default());
        let r = f(self);
        let inner = self.trace.replace(outer);
        self.replay(inner);
        let v = r?;
        slot.set((v, inner));
        Ok(v)
    }

    fn replay(&self, t: Trace) {
        if let Some(p) = t.first {
            self.record(p);
        }
        if t.many {
            self.record_many();
        }
    }

    pub fn present(&self, n: NodeId, i: usize) -> R<bool> {
        let a = &self.def.nodes[n].attrs[i];
        match &a.cond {
            None => Ok(true),
            // Memoized like positions: an `if` on a flag reads one byte.
            Some(c) => Ok(self.memo(&a.there, |e| Ok(Some(i64::from(e.eval(c)?.truthy()))))? == Some(1)),
        }
    }

    /// Where attribute `i` starts in node `n` (bits), if it is there.
    pub fn attr_pos(&self, n: NodeId, i: usize) -> R<Option<i64>> {
        let a = &self.def.nodes[n].attrs[i];
        self.nested(|e| {
            e.memo(&a.pos, |e| {
                if !e.present(n, i)? {
                    return Ok(None);
                }
                let p = e.end_of(n, i)?;
                Ok(Some(if a.is_bits() { p } else { align(p) }))
            })
        })
    }

    /// Bits from node `n`'s start to the end of its first `upto` attributes.
    fn end_of(&self, n: NodeId, upto: usize) -> R<i64> {
        let attrs = &self.def.nodes[n].attrs;
        let mut p = 0i64;
        for (j, b) in attrs[..upto].iter().enumerate() {
            if !self.present(n, j)? {
                continue;
            }
            if !b.is_bits() {
                p = align(p);
            }
            let size = match (&b.kind, &b.size) {
                (Kind::Int { bytes, .. }, _) => i64::from(*bytes) * 8,
                (Kind::Bits(k), _) => i64::from(*k),
                (_, Size::Fixed(v)) => (*v as i64).checked_mul(8).ok_or(Stop)?,
                // Sizes that need to know where they are.
                _ => {
                    let abs = self.node_start(n)? + p;
                    self.attr_size(n, b, abs)?
                }
            };
            p = p.checked_add(size).ok_or(Stop)?;
        }
        Ok(p)
    }

    /// Size in bits of attribute `a` of node `n`, starting at `abs`.
    fn attr_size(&self, n: NodeId, a: &Attr, abs: i64) -> R<i64> {
        Ok(match (&a.kind, &a.size) {
            (Kind::Int { bytes, .. }, _) => i64::from(*bytes) * 8,
            (Kind::Bits(k), _) => i64::from(*k),
            (_, Size::Fixed(v)) => (*v as i64).checked_mul(8).ok_or(Stop)?,
            (_, Size::Bits(k)) => i64::from(*k),
            (_, Size::Expr(e)) => {
                let v = self.int(e)?;
                if v < 0 {
                    return Err(Stop);
                }
                v.checked_mul(8).ok_or(Stop)?
            }
            (_, Size::Eos) => {
                let end = self.io_end(n)?;
                if end < abs {
                    return Err(Stop);
                }
                end - abs
            }
            (_, Size::Terminated(t)) => {
                let end = self.io_end(n)?;
                let (from, to) = (byte_at(abs)?, (end / 8) as usize);
                self.record_many();
                let hay = self.msg.get(from..to.min(self.msg.len())).ok_or(Stop)?;
                let i = memchr::memchr(*t, hay).ok_or(Stop)?;
                (i as i64 + 1) * 8
            }
            (Kind::User(c), Size::Child) => self.end_of(*c, self.def.nodes[*c].attrs.len())?,
            (_, Size::Child) => return Err(Stop),
        })
    }

    fn endian(&self, n: NodeId) -> R<Endian> {
        let node = &self.def.nodes[n];
        match &node.endian {
            Some(EndianRule::Fixed(e)) => Ok(*e),
            Some(EndianRule::Switch { on, cases, default }) => {
                let v = self.eval(on)?;
                cases
                    .iter()
                    .find(|(k, _)| compare(&v, &lit(k)) == Some(Ordering::Equal))
                    .map(|(_, e)| *e)
                    .or(*default)
                    .ok_or(Stop)
            }
            None => self.endian(node.parent.ok_or(Stop)?),
        }
    }

    // --- values -------------------------------------------------------------

    /// Attribute `i` of node `n`.
    pub fn attr_val(&self, n: NodeId, i: usize) -> R<Val<'m>> {
        let Some(rel) = self.attr_pos(n, i)? else { return Ok(Val::Null) };
        let abs = self.node_start(n)? + rel;
        self.read_at(n, &self.def.nodes[n].attrs[i], abs, self.checked)
    }

    fn read_at(&self, n: NodeId, a: &Attr, abs: i64, check_io: bool) -> R<Val<'m>> {
        let size = self.attr_size(n, a, abs)?;
        let end = abs.checked_add(size).ok_or(Stop)?;
        if check_io {
            let (s, e) = self.untraced(|x| Ok((x.io_start(n)?, x.io_end(n)?)))?;
            if abs < s || end > e {
                return Err(Stop);
            }
        }
        match &a.kind {
            Kind::Int { bytes, signed, endian } => {
                let at = byte_at(abs)?;
                let b = self.slice(at, at + usize::from(*bytes))?;
                let endian = match endian {
                    Some(e) => *e,
                    None => self.endian(n)?,
                };
                let mut v: u64 = 0;
                match endian {
                    Endian::Be => b.iter().for_each(|&x| v = (v << 8) | u64::from(x)),
                    Endian::Le => b.iter().rev().for_each(|&x| v = (v << 8) | u64::from(x)),
                }
                let bits = u32::from(*bytes) * 8;
                let v = if *signed && bits < 64 { ((v << (64 - bits)) as i64) >> (64 - bits) } else { v as i64 };
                Ok(Val::Int(v))
            }
            Kind::Bits(k) => {
                let v = self.bits(abs, u32::from(*k))?;
                Ok(if *k == 1 { Val::Bool(v != 0) } else { Val::Int(v as i64) })
            }
            Kind::Bytes | Kind::Str | Kind::User(_) => {
                let from = byte_at(abs)?;
                let to = byte_at(align(end))?;
                let mut s = self.slice(from, to)?;
                match a.size {
                    Size::Terminated(_) => s = &s[..s.len().saturating_sub(1)],
                    _ => {
                        if let Some(t) = a.terminator
                            && let Some(i) = memchr::memchr(t, s)
                        {
                            s = &s[..i];
                        }
                    }
                }
                Ok(Val::Msg(s))
            }
        }
    }

    /// `k` bits from bit `abs`, most significant first.
    fn bits(&self, abs: i64, k: u32) -> R<u64> {
        if abs < 0 || !(1..=64).contains(&k) {
            return Err(Stop);
        }
        let first = byte_at(abs - abs % 8)?;
        let skip = (abs % 8) as u32;
        let n_bytes = (skip + k).div_ceil(8) as usize;
        let b = self.slice(first, first + n_bytes)?;
        let mut v: u128 = 0;
        for &x in b {
            v = (v << 8) | u128::from(x);
        }
        let total = n_bytes as u32 * 8;
        let v = (v >> (total - skip - k)) & ((1u128 << k) - 1);
        Ok(v as u64)
    }

    pub fn inst_val(&self, n: NodeId, k: usize) -> R<Val<'m>> {
        let inst = &self.def.nodes[n].insts[k];
        if let Some(ty) = inst.small {
            let v = self.memo(&inst.memo, |e| {
                Ok(match e.inst_eval(n, k)? {
                    Val::Int(v) => Some(v),
                    Val::Bool(b) => Some(i64::from(b)),
                    // Not what its type said (an attribute that isn't there).
                    _ => None,
                })
            })?;
            return Ok(match (v, ty) {
                (None, _) => Val::Null,
                (Some(v), Ty::Bool) => Val::Bool(v != 0),
                (Some(v), _) => Val::Int(v),
            });
        }
        self.inst_eval(n, k)
    }

    fn inst_eval(&self, n: NodeId, k: usize) -> R<Val<'m>> {
        let inst = &self.def.nodes[n].insts[k];
        self.nested(|e| {
            if let Some(c) = &inst.cond
                && !e.eval(c)?.truthy()
            {
                return Ok(Val::Null);
            }
            match &inst.kind {
                InstKind::Value(x) => e.eval(x),
                InstKind::Read { pos, read } => {
                    let s = e.io_start(n)?;
                    let abs = s + e.int(pos)?.checked_mul(8).ok_or(Stop)?;
                    e.read_at(n, read, abs, true)
                }
            }
        })
    }

    fn int(&self, e: &CExpr) -> R<i64> {
        match self.eval(e)? {
            Val::Int(v) => Ok(v),
            Val::Bool(b) => Ok(i64::from(b)),
            _ => Err(Stop),
        }
    }

    pub fn eval(&self, e: &CExpr) -> R<Val<'m>> {
        Ok(match e {
            CExpr::Lit(l) => lit(l),
            CExpr::Attr(n, i) => self.nested(|x| x.attr_val(*n, *i))?,
            CExpr::Inst(n, k) => self.inst_val(*n, *k)?,
            CExpr::IoSize(n) => {
                let (s, e) = self.io(*n)?;
                Val::Int((e - s) / 8)
            }
            CExpr::Length(a) => match self.eval(a)?.bytes() {
                Some(b) => Val::Int(b.len() as i64),
                None => Val::Null,
            },
            CExpr::Index(a, i) => {
                let v = self.eval(a)?;
                let i = self.eval(i)?;
                match (v.bytes(), i) {
                    (Some(b), Val::Int(i)) if i >= 0 => {
                        b.get(i as usize).map_or(Val::Null, |&x| Val::Int(i64::from(x)))
                    }
                    _ => Val::Null,
                }
            }
            CExpr::Unary(op, a) => {
                let v = self.eval(a)?;
                match (op, v) {
                    (UnOp::Not, v) => Val::Bool(!v.truthy()),
                    (UnOp::Neg, Val::Int(x)) => Val::Int(x.wrapping_neg()),
                    (UnOp::Neg, Val::Float(x)) => Val::Float(-x),
                    (UnOp::BitNot, Val::Int(x)) => Val::Int(!x),
                    _ => Val::Null,
                }
            }
            CExpr::Binary(BinOp::And, a, b) => Val::Bool(self.eval(a)?.truthy() && self.eval(b)?.truthy()),
            CExpr::Binary(BinOp::Or, a, b) => Val::Bool(self.eval(a)?.truthy() || self.eval(b)?.truthy()),
            CExpr::Binary(op, a, b) => {
                let (a, b) = (self.eval(a)?, self.eval(b)?);
                binary(*op, a, b)
            }
            CExpr::Cond(c, a, b) => {
                if self.eval(c)?.truthy() {
                    self.eval(a)?
                } else {
                    self.eval(b)?
                }
            }
            CExpr::Underscore => self.underscore.borrow().clone(),
            CExpr::Origin(f) => match f {
                OriginField::Channel => {
                    let c: &'m [u8] = self.origin.channel;
                    let n = c.iter().position(|&b| b == 0).unwrap_or(c.len());
                    Val::Msg(&c[..n])
                }
                OriginField::Secs => Val::Int(i64::from(self.origin.secs)),
                OriginField::Usecs => Val::Int(i64::from(self.origin.usecs)),
            },
        })
    }

    // --- checking a whole message -------------------------------------------

    /// Every attribute of every node (not instances) fits its stream and
    /// passes its checks.
    pub fn validate(&self) -> R<()> {
        self.validate_node(0)
    }

    /// One pass over the node's attributes in order, as a sequential reader
    /// would make (positions add up as it goes; values are read only where
    /// a check needs them).
    fn validate_node(&self, n: NodeId) -> R<()> {
        self.enter()?;
        let node = &self.def.nodes[n];
        let start = self.node_start(n)?;
        let end = self.io_end(n)?;
        let mut p = 0i64;
        for (i, a) in node.attrs.iter().enumerate() {
            if !self.present(n, i)? {
                continue;
            }
            if !a.is_bits() {
                p = align(p);
            }
            let abs = start + p;
            let size = self.attr_size(n, a, abs)?;
            p = p.checked_add(size).ok_or(Stop)?;
            if start + p > end {
                return Err(Stop);
            }
            if let Kind::User(c) = a.kind {
                self.validate_node(c)?;
            }
            if a.contents.is_none() && a.checks.is_empty() {
                continue;
            }
            let v = self.read_at(n, a, abs, false)?;
            if let Some(c) = a.contents
                && v.bytes() != Some(c)
            {
                return Err(Stop);
            }
            for check in &a.checks {
                *self.underscore.borrow_mut() = v.clone();
                let ok = match check {
                    Check::Eq(e) => compare(&v, &self.eval(e)?) == Some(Ordering::Equal),
                    Check::Min(e) => compare(&v, &self.eval(e)?).is_some_and(|o| o != Ordering::Less),
                    Check::Max(e) => compare(&v, &self.eval(e)?).is_some_and(|o| o != Ordering::Greater),
                    Check::AnyOf(list) => {
                        let mut any = false;
                        for e in list {
                            any |= compare(&v, &self.eval(e)?) == Some(Ordering::Equal);
                        }
                        any
                    }
                    Check::Expr(e) => self.eval(e)?.truthy(),
                };
                if !ok {
                    return Err(Stop);
                }
            }
        }
        self.leave();
        Ok(())
    }
}

fn align(bits: i64) -> i64 {
    (bits + 7) & !7
}

fn byte_at(bits: i64) -> R<usize> {
    if bits < 0 || bits % 8 != 0 {
        return Err(Stop);
    }
    Ok((bits / 8) as usize)
}

/// Integer arithmetic as Kaitai has it: wrapping; `/` and `%` towards
/// negative infinity, `%` never negative; nothing for a zero divisor.
pub fn int_op(op: BinOp, x: i64, y: i64) -> Option<Lit> {
    Some(Lit::Int(match op {
        BinOp::Add => x.wrapping_add(y),
        BinOp::Sub => x.wrapping_sub(y),
        BinOp::Mul => x.wrapping_mul(y),
        BinOp::Div if y != 0 => x.wrapping_div_euclid(y),
        BinOp::Mod if y != 0 => x.wrapping_rem_euclid(y),
        BinOp::BitAnd => x & y,
        BinOp::BitOr => x | y,
        BinOp::BitXor => x ^ y,
        BinOp::Shl => {
            if (0..64).contains(&y) {
                x.wrapping_shl(y as u32)
            } else {
                0
            }
        }
        BinOp::Shr => {
            if (0..64).contains(&y) {
                x >> y
            } else if x < 0 {
                -1
            } else {
                0
            }
        }
        _ => return None,
    }))
}

fn binary<'m>(op: BinOp, a: Val<'m>, b: Val<'m>) -> Val<'m> {
    match op {
        BinOp::Eq => return Val::Bool(compare(&a, &b) == Some(Ordering::Equal)),
        BinOp::Ne => return Val::Bool(compare(&a, &b) != Some(Ordering::Equal)),
        BinOp::Lt => return Val::Bool(compare(&a, &b) == Some(Ordering::Less)),
        BinOp::Le => return Val::Bool(matches!(compare(&a, &b), Some(Ordering::Less | Ordering::Equal))),
        BinOp::Gt => return Val::Bool(compare(&a, &b) == Some(Ordering::Greater)),
        BinOp::Ge => return Val::Bool(matches!(compare(&a, &b), Some(Ordering::Greater | Ordering::Equal))),
        _ => {}
    }
    match (&a, &b) {
        (Val::Int(x), Val::Int(y)) => {
            return match int_op(op, *x, *y) {
                Some(Lit::Int(v)) => Val::Int(v),
                _ => Val::Null,
            };
        }
        (Val::Bool(x), Val::Bool(y)) => {
            return match op {
                BinOp::BitAnd => Val::Bool(x & y),
                BinOp::BitOr => Val::Bool(x | y),
                BinOp::BitXor => Val::Bool(x ^ y),
                _ => Val::Null,
            };
        }
        _ => {}
    }
    if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
        return match op {
            BinOp::Add => Val::Float(x + y),
            BinOp::Sub => Val::Float(x - y),
            BinOp::Mul => Val::Float(x * y),
            BinOp::Div => Val::Float(x / y),
            BinOp::Mod => Val::Float(x.rem_euclid(y)),
            _ => Val::Null,
        };
    }
    if op == BinOp::Add
        && let (Some(x), Some(y)) = (a.bytes(), b.bytes())
    {
        return Val::Own([x, y].concat());
    }
    Val::Null
}

/// Numbers with numbers, bytes with bytes, booleans with booleans; else
/// nothing (a missing value compares with nothing).
pub fn compare(a: &Val<'_>, b: &Val<'_>) -> Option<Ordering> {
    match (a, b) {
        (Val::Int(x), Val::Int(y)) => Some(x.cmp(y)),
        (Val::Bool(x), Val::Bool(y)) => Some(x.cmp(y)),
        (Val::Null, _) | (_, Val::Null) => None,
        _ => {
            if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
                return x.partial_cmp(&y);
            }
            Some(a.bytes()?.cmp(b.bytes()?))
        }
    }
}

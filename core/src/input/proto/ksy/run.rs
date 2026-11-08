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
//!
//! Elements of lists (`repeat`) have a place per element: reading one
//! pushes a **frame** (the node and where this element of it is), and
//! nodes inside it find their place from there. Nothing inside a list is
//! memoized.

use super::expr::{BinOp, UnOp};
use super::model::{
    Attr, CExpr, Check, Def, Endian, EndianRule, InstKind, Kind, Lit, NodeId, OriginField, Place, Repeat, Size, Switch,
    Template, Ty,
};
use super::plan::{Fast, Order, PSize, Read};
use super::template::{Fmt, Piece};
use crate::input::proto::Origin;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Write;
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

/// An object (a user type's node) of this message: an index into the
/// message's frames.
pub type FrameId = u32;

/// Where an object is in this message.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Frame {
    node: NodeId,
    /// Bits from the message's start.
    start: i64,
    /// Its stream's end, if it has a stream of its own.
    end: Option<i64>,
    /// The object around it, for `_parent` and its members.
    up: Option<FrameId>,
}

/// List attribute `i` of node `n` at `abs`, read in the context `ctx`: its
/// elements come from its scan when they are asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListRef {
    n: NodeId,
    i: usize,
    abs: i64,
    ctx: Option<FrameId>,
}

/// A partial attribute that couldn't be read: the bytes it had.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UndecodedRef {
    n: NodeId,
    i: usize,
    from: usize,
    to: usize,
    ctx: Option<FrameId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Val<'m> {
    Null,
    Int(i64),
    /// An unsigned 64-bit number above `i64::MAX`, kept exact.
    U64(u64),
    Float(f64),
    /// A 32-bit float, shown as one (`0.1`, not `0.10000000149011612`).
    F32(f32),
    Bool(bool),
    /// A number of an enum ([`Def::enums`]).
    Enum(i64, usize),
    /// Bytes of the message.
    Msg(&'m [u8]),
    /// Text (`str`) of the message.
    Text(&'m [u8]),
    /// Bytes of the definition (interned).
    Lit(&'static [u8]),
    Own(Vec<u8>),
    Obj(FrameId),
    /// An object decoded by its plan ([`super::plan`]): an index into the
    /// message's records.
    Rec(u32),
    List(ListRef),
    Undecoded(UndecodedRef),
}

impl<'m> Val<'m> {
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Val::Msg(b) | Val::Text(b) => Some(b),
            Val::Lit(b) => Some(b),
            Val::Own(b) => Some(b),
            _ => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Val::Null | Val::Undecoded(_) => false,
            Val::Bool(b) => *b,
            Val::Int(v) | Val::Enum(v, _) => *v != 0,
            Val::U64(v) => *v != 0,
            Val::Float(v) => *v != 0.0,
            Val::F32(v) => *v != 0.0,
            Val::Obj(_) | Val::Rec(_) | Val::List(_) => true,
            v => v.bytes().is_some_and(|b| !b.is_empty()),
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Val::Int(v) | Val::Enum(v, _) => Some(*v),
            Val::U64(v) => Some(*v as i64),
            Val::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Val::Int(v) | Val::Enum(v, _) => Some(*v as f64),
            Val::U64(v) => Some(*v as f64),
            Val::Float(v) => Some(*v),
            Val::F32(v) => Some(f64::from(*v)),
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
/// A list longer than this has no value (partial: is cut there).
const MAX_ITEMS: usize = 1 << 16;

/// Nodes whose start and stream end a message keeps.
const CACHED: usize = 8;
const UNKNOWN: i64 = i64::MIN;

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
    rest: RefCell<Option<&'m [u8]>>,
    /// The object being read inside (a list element or one around it).
    cur: Cell<Option<FrameId>>,
    /// Per node (the first [`CACHED`]): where it starts and where its
    /// stream ends, once known, with what finding them read.
    starts: [Cell<(i64, Trace)>; CACHED],
    io_ends: [Cell<(i64, Trace)>; CACHED],
    /// The byte order of the node plans read numbers in, found once.
    order: Cell<Option<(NodeId, Endian)>>,
    /// What reading objects inside lists keeps for the message: taken from
    /// the thread's pool when first needed ([`Scratch`]).
    sc: RefCell<Option<Scratch>>,
}

/// What reading one message's objects keeps, reused from message to message
/// on a thread: reading allocates only while these grow (allocating is
/// most of what a payload costs otherwise).
#[derive(Default)]
struct Scratch {
    /// What was found inside list elements: by what it is ([`Found`]),
    /// node, attribute or instance, and where the element (or, for
    /// layouts, the object) starts. Reading one element asks the same
    /// things many times.
    found: HashMap<(Found, NodeId, usize, i64), Kept, Fx>,
    /// Elements checked, and lists scanned: by node, attribute and where
    /// they start. Showing a partial attribute checks it whole, then reads
    /// it: the second time is a lookup.
    elements: HashMap<(NodeId, usize, i64), R<Element>, Fx>,
    scans: HashMap<(NodeId, usize, i64), ScanRef, Fx>,
    /// The elements of the lists scanned: where each is, its object node,
    /// its record if its plan decoded it.
    items: Vec<Item>,
    frames: Vec<Frame>,
    recs: Vec<Rec>,
    rvals: Vec<Kept>,
    /// Objects inside lists read in order ([`Eval::layout`]): by node and
    /// where the object starts, an index into `layout_heads`; each head's
    /// attributes are slots of `slots` (position, end).
    layouts: HashMap<(NodeId, i64), usize, Fx>,
    layout_heads: Vec<LayoutHead>,
    slots: Vec<(Option<i64>, i64)>,
}

/// How far an object's layout is read.
#[derive(Debug, Clone, Copy)]
struct LayoutHead {
    first: u32,
    /// Attributes whose position is known, and whose end is.
    placed: u32,
    ended: u32,
    failed: bool,
}

impl Scratch {
    fn clear(&mut self) {
        self.found.clear();
        self.elements.clear();
        self.scans.clear();
        self.items.clear();
        self.frames.clear();
        self.recs.clear();
        self.rvals.clear();
        self.layouts.clear();
        self.layout_heads.clear();
        self.slots.clear();
    }
}

thread_local! {
    static POOL: RefCell<Vec<Scratch>> = const { RefCell::new(Vec::new()) };
}

impl Drop for Eval<'_, '_> {
    fn drop(&mut self) {
        if let Some(mut sc) = self.sc.get_mut().take() {
            sc.clear();
            let _ = POOL.try_with(|p| {
                let mut p = p.borrow_mut();
                if p.len() < 4 {
                    p.push(sc);
                }
            });
        }
    }
}

/// A value kept in [`Scratch::found`]: without the message's lifetime
/// (bytes by offsets), so the scratch area outlives the message.
#[derive(Debug, Clone, Copy)]
enum Kept {
    Null,
    Int(i64),
    U64(u64),
    Float(f64),
    F32(f32),
    Bool(bool),
    Enum(i64, usize),
    Msg(usize, usize),
    Text(usize, usize),
    Lit(&'static [u8]),
    Obj(FrameId),
    Rec(u32),
    List(ListRef),
    Undecoded(UndecodedRef),
    /// A record's instance not computed yet.
    Missing,
}

/// An object decoded by its plan: where it is, its stream, the record
/// around it, and its values (attributes, then instances) in
/// [`Scratch::rvals`].
#[derive(Debug, Clone, Copy)]
struct Rec {
    node: NodeId,
    start: i64,
    end: i64,
    io: (i64, i64),
    parent: Option<u32>,
    first: u32,
}

/// An element checked: where it ends, its object node if it is one.
type Element = (i64, Option<NodeId>);

/// A list's elements in [`Scratch::items`], and where the list ends.
#[derive(Debug, Clone, Copy)]
struct ScanRef {
    first: u32,
    len: u32,
    end: i64,
}

/// A list element: where it starts and ends, its object node, its record.
type Item = (i64, i64, Option<NodeId>, Option<u32>);

/// A list's elements while it is scanned: on the stack up to 16.
struct Items {
    inline: [Item; 16],
    len: usize,
    spill: Vec<Item>,
}

impl Items {
    fn new() -> Items {
        Items { inline: [(0, 0, None, None); 16], len: 0, spill: Vec::new() }
    }

    fn push(&mut self, x: Item) {
        if self.len < self.inline.len() {
            self.inline[self.len] = x;
        } else {
            self.spill.push(x);
        }
        self.len += 1;
    }

    fn all(&self) -> impl Iterator<Item = &Item> {
        self.inline[..self.len.min(self.inline.len())].iter().chain(&self.spill)
    }
}

/// A cheap hasher for the small keys of one message's caches (no
/// dependency; FxHash's multiply-rotate).
#[derive(Default, Clone, Copy)]
struct Fx(u64);

impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u64(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_usize(&mut self, x: usize) {
        self.write_u64(x as u64);
    }

    fn write_i64(&mut self, x: i64) {
        self.write_u64(x as u64);
    }

    fn write_u8(&mut self, x: u8) {
        self.write_u64(u64::from(x));
    }

    fn write_isize(&mut self, x: isize) {
        self.write_u64(x as u64);
    }
}

impl std::hash::BuildHasher for Fx {
    type Hasher = Fx;

    fn build_hasher(&self) -> Fx {
        Fx(0)
    }
}

/// What [`Scratch::found`] keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Found {
    Present,
    Start,
    Attr,
    Inst,
}

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
            rest: RefCell::new(None),
            cur: Cell::new(None),
            starts: [const { Cell::new((UNKNOWN, Trace { first: None, many: false })) }; CACHED],
            io_ends: [const { Cell::new((UNKNOWN, Trace { first: None, many: false })) }; CACHED],
            sc: RefCell::new(None),
            order: Cell::new(None),
        }
    }

    /// The scratch area, taken from the thread's pool on first use.
    fn sc(&self) -> std::cell::RefMut<'_, Scratch> {
        std::cell::RefMut::map(self.sc.borrow_mut(), |sc| {
            sc.get_or_insert_with(|| POOL.try_with(|p| p.borrow_mut().pop()).ok().flatten().unwrap_or_default())
        })
    }

    /// `v` as [`Scratch::found`] keeps it; not bytes owned or not in the
    /// message.
    fn keep(&self, v: &Val<'m>) -> Option<Kept> {
        let at = |b: &[u8]| {
            let base = self.msg.as_ptr() as usize;
            let from = (b.as_ptr() as usize).checked_sub(base)?;
            (from + b.len() <= self.msg.len()).then_some((from, from + b.len()))
        };
        Some(match v {
            Val::Null => Kept::Null,
            Val::Int(x) => Kept::Int(*x),
            Val::U64(x) => Kept::U64(*x),
            Val::Float(x) => Kept::Float(*x),
            Val::F32(x) => Kept::F32(*x),
            Val::Bool(x) => Kept::Bool(*x),
            Val::Enum(x, e) => Kept::Enum(*x, *e),
            Val::Msg(b) => {
                let (a, z) = at(b)?;
                Kept::Msg(a, z)
            }
            Val::Text(b) => {
                let (a, z) = at(b)?;
                Kept::Text(a, z)
            }
            Val::Lit(b) => Kept::Lit(b),
            Val::Obj(f) => Kept::Obj(*f),
            Val::Rec(r) => Kept::Rec(*r),
            Val::List(l) => Kept::List(*l),
            Val::Undecoded(u) => Kept::Undecoded(*u),
            Val::Own(_) => return None,
        })
    }

    fn unkeep(&self, k: Kept) -> Val<'m> {
        match k {
            Kept::Null => Val::Null,
            Kept::Int(x) => Val::Int(x),
            Kept::U64(x) => Val::U64(x),
            Kept::Float(x) => Val::Float(x),
            Kept::F32(x) => Val::F32(x),
            Kept::Bool(x) => Val::Bool(x),
            Kept::Enum(x, e) => Val::Enum(x, e),
            Kept::Msg(a, z) => Val::Msg(&self.msg[a..z]),
            Kept::Text(a, z) => Val::Text(&self.msg[a..z]),
            Kept::Lit(b) => Val::Lit(b),
            Kept::Obj(f) => Val::Obj(f),
            Kept::Rec(r) => Val::Rec(r),
            Kept::Missing => Val::Null,
            Kept::List(l) => Val::List(l),
            Kept::Undecoded(u) => Val::Undecoded(u),
        }
    }

    fn found_set(&self, what: Found, n: NodeId, i: usize, at: i64, v: Kept) {
        let mut sc = self.sc();
        reserve(&mut sc.found);
        sc.found.insert((what, n, i, at), v);
    }

    /// Where the list element node `n` is in starts, if it is in one being
    /// read.
    fn anchor(&self, n: NodeId) -> Option<i64> {
        let e = self.def.nodes[n].anchor?;
        self.frame_of(e).map(|(s, _)| s)
    }

    /// `f` for a node inside a list element, kept for the message.
    fn found(&self, what: Found, n: NodeId, i: usize, f: impl FnOnce(&Self) -> R<Val<'m>>) -> R<Val<'m>> {
        let Some(at) = self.anchor(n) else { return f(self) };
        let key = (what, n, i, at);
        let hit = self.sc().found.get(&key).copied();
        if let Some(k) = hit {
            self.record_many();
            return Ok(self.unkeep(k));
        }
        let v = f(self)?;
        if let Some(k) = self.keep(&v) {
            self.found_set(key.0, key.1, key.2, key.3, k);
        }
        Ok(v)
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

    fn replay(&self, t: Trace) {
        if let Some(p) = t.first {
            self.record(p);
        }
        if t.many {
            self.record_many();
        }
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

    fn nested<T>(&self, f: impl FnOnce(&Self) -> R<T>) -> R<T> {
        let d = self.depth.get() + 1;
        if d > MAX_DEPTH {
            return Err(Stop);
        }
        self.depth.set(d);
        let r = f(self);
        self.depth.set(d - 1);
        r
    }

    // --- frames -------------------------------------------------------------

    /// Where node `n` is, if a frame says (an object being read).
    fn frame_of(&self, n: NodeId) -> Option<(i64, Option<i64>)> {
        let mut f = self.cur.get()?;
        let sc = self.sc.borrow();
        let frames = &sc.as_ref()?.frames;
        loop {
            let fr = frames.get(f as usize)?;
            if fr.node == n {
                return Some((fr.start, fr.end));
            }
            f = fr.up?;
        }
    }

    fn fr(&self, id: FrameId) -> Option<Frame> {
        self.sc.borrow().as_ref()?.frames.get(id as usize).copied()
    }

    fn with_frame<T>(&self, fr: Option<FrameId>, f: impl FnOnce(&Self) -> T) -> T {
        let old = self.cur.replace(fr);
        let r = f(self);
        self.cur.set(old);
        r
    }

    fn frame(&self, node: NodeId, start: i64, end: Option<i64>) -> FrameId {
        let up = self.cur.get();
        let mut sc = self.sc();
        if sc.frames.capacity() == 0 {
            sc.frames.reserve(32);
        }
        sc.frames.push(Frame { node, start, end, up });
        (sc.frames.len() - 1) as FrameId
    }

    /// Whether node `n` is an element of a list (placed by a frame only).
    fn is_element(&self, n: NodeId) -> bool {
        match self.def.nodes[n].place {
            Place::Attr(p, i) | Place::Case(p, i, _) => self.def.nodes[p].attrs[i].repeat != Repeat::No,
            _ => false,
        }
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
        if let Some((s, _)) = self.frame_of(n) {
            return Ok(s);
        }
        let node = &self.def.nodes[n];
        match node.place {
            Place::Root => Ok(0),
            Place::Attr(p, i) | Place::Case(p, i, _) => {
                if self.def.nodes[p].attrs[i].repeat != Repeat::No {
                    return Err(Stop);
                }
                let case_taken = |e: &Self| match node.place {
                    Place::Case(_, _, c) => Ok(e.case_index(p, i)? == Some(c)),
                    _ => Ok(true),
                };
                if node.dynamic {
                    // Kept with the case check: a case not taken has no start.
                    let v = self.found(Found::Start, n, 0, |e| {
                        if !case_taken(e)? {
                            return Ok(Val::Null);
                        }
                        e.nested(|e| Ok(Val::Int(e.node_start(p)? + e.attr_pos(p, i)?.ok_or(Stop)?)))
                    })?;
                    return v.as_i64().ok_or(Stop);
                }
                // Kept with the case check: a case taken is checked once a
                // message (one not taken has no start, and isn't kept).
                self.kept(self.starts.get(n), |e| {
                    if !case_taken(e)? {
                        return Err(Stop);
                    }
                    e.nested(|e| {
                        e.memo(&node.start, |e| {
                            let s = e.node_start(p)?;
                            Ok(e.attr_pos(p, i)?.map(|q| s + q))
                        })?
                        .ok_or(Stop)
                    })
                })
            }
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
        if let Some((_, Some(end))) = self.frame_of(n) {
            return Ok(end);
        }
        let slot = if node.dynamic { None } else { self.io_ends.get(n) };
        self.kept(slot, |e| {
            e.nested(|e| {
                let s = e.node_start(n)?;
                let size = match node.place {
                    Place::Attr(p, i) | Place::Case(p, i, _) => {
                        let a = &e.def.nodes[p].attrs[i];
                        e.sized_size(p, a, s)?
                    }
                    Place::Inst(p, k) => match &e.def.nodes[p].insts[k].kind {
                        InstKind::Read { read, .. } => e.sized_size(p, read, s)?,
                        InstKind::Value(_) => return Err(Stop),
                    },
                    Place::Root => return Err(Stop),
                };
                Ok(s + size)
            })
        })
    }

    /// The size an attribute's `size` / `size-eos` gives (a user type's
    /// stream), in bits.
    fn sized_size(&self, n: NodeId, a: &Attr, abs: i64) -> R<i64> {
        match &a.size {
            Size::Fixed(v) => (*v as i64).checked_mul(8).ok_or(Stop),
            Size::Expr(e) => {
                let v = self.int(e)?;
                if v < 0 {
                    return Err(Stop);
                }
                v.checked_mul(8).ok_or(Stop)
            }
            Size::Eos => {
                let end = self.io_end(n)?;
                if end < abs { Err(Stop) } else { Ok(end - abs) }
            }
            _ => Err(Stop),
        }
    }

    pub fn present(&self, n: NodeId, i: usize) -> R<bool> {
        let node = &self.def.nodes[n];
        let a = &node.attrs[i];
        match &a.cond {
            None => Ok(true),
            Some(c) if node.dynamic => {
                Ok(self.found(Found::Present, n, i, |e| Ok(Val::Bool(e.eval(c)?.truthy())))?.truthy())
            }
            // Memoized like positions: an `if` on a flag reads one byte.
            Some(c) => Ok(self.memo(&a.there, |e| Ok(Some(i64::from(e.eval(c)?.truthy()))))? == Some(1)),
        }
    }

    /// Where attribute `i` starts in node `n` (bits), if it is there.
    pub fn attr_pos(&self, n: NodeId, i: usize) -> R<Option<i64>> {
        let node = &self.def.nodes[n];
        let a = &node.attrs[i];
        let compute = |e: &Self| -> R<Option<i64>> {
            if !e.present(n, i)? {
                return Ok(None);
            }
            let p = e.end_of(n, i)?;
            Ok(Some(if a.is_bits() { p } else { align(p) }))
        };
        if node.dynamic {
            let start = self.node_start(n)?;
            return self.nested(|e| e.layout(n, start, i + 1, false)).map(|l| l.0);
        }
        self.nested(|e| e.memo(&a.pos, compute))
    }

    /// An object inside a list read in order, as a sequential parser would:
    /// where each of its attributes is and where each ends, kept for the
    /// message and extended as far as asked. With `end`, attribute
    /// `upto - 1` is read whole; else only where it starts is needed (a
    /// nested object asks that while its parent measures it). That
    /// attribute's position, and where it ends (0 if not asked).
    fn layout(&self, n: NodeId, start: i64, upto: usize, end: bool) -> R<(Option<i64>, i64)> {
        let attrs = &self.def.nodes[n].attrs;
        if upto == 0 {
            return Ok((None, 0));
        }
        // One lookup for the object; its attributes are slots after that.
        let at = {
            let mut sc = self.sc();
            match sc.layouts.get(&(n, start)) {
                Some(&at) => at,
                None => {
                    let at = sc.layout_heads.len();
                    let first = sc.slots.len() as u32;
                    sc.layout_heads.push(LayoutHead { first, placed: 0, ended: 0, failed: false });
                    let len = sc.slots.len() + attrs.len();
                    sc.slots.resize(len, (None, 0));
                    reserve(&mut sc.layouts);
                    sc.layouts.insert((n, start), at);
                    at
                }
            }
        };
        let head = |e: &Self| e.sc().layout_heads[at];
        let slot = |e: &Self, j: usize| {
            let sc = e.sc();
            sc.slots[sc.layout_heads[at].first as usize + j]
        };
        loop {
            let h = head(self);
            let (placed, ended) = (h.placed as usize, h.ended as usize);
            if ended >= upto {
                return Ok(slot(self, upto - 1));
            }
            if !end && placed >= upto {
                return Ok((slot(self, upto - 1).0, 0));
            }
            if h.failed || placed > ended {
                // Can't be read, or attribute `ended` is being measured and
                // its end (or a later one) is asked: it depends on itself.
                return Err(Stop);
            }
            let next = if placed == 0 { 0 } else { slot(self, placed - 1).1 };
            // Attribute `placed`: nothing borrowed while it is read (its
            // `if` and size may read the attributes before it).
            let b = &attrs[placed];
            let pos = match self.present(n, placed) {
                Ok(true) => Some(if b.is_bits() { next } else { align(next) }),
                Ok(false) => None,
                Err(Stop) => return self.layout_failed(at),
            };
            {
                let mut sc = self.sc();
                if sc.layout_heads[at].placed as usize != placed {
                    continue;
                }
                let first = sc.layout_heads[at].first as usize;
                sc.slots[first + placed].0 = pos;
                sc.layout_heads[at].placed += 1;
            }
            let size = match pos {
                None => Ok(0),
                Some(p) => (|| {
                    Ok(match (&b.kind, &b.size, &b.repeat) {
                        (Kind::Int { bytes, .. } | Kind::Float { bytes, .. }, _, Repeat::No) => i64::from(*bytes) * 8,
                        (Kind::Bits(k), _, Repeat::No) => i64::from(*k),
                        (_, Size::Fixed(v), Repeat::No) if b.partial.is_none() => {
                            (*v as i64).checked_mul(8).ok_or(Stop)?
                        }
                        _ => self.attr_size(n, b, start + p)?,
                    })
                })(),
            };
            let size = match size {
                Ok(v) => v,
                Err(Stop) => return self.layout_failed(at),
            };
            let mut sc = self.sc();
            if sc.layout_heads[at].ended as usize == placed {
                let first = sc.layout_heads[at].first as usize;
                sc.slots[first + placed].1 = pos.unwrap_or(next).checked_add(size).ok_or(Stop)?;
                sc.layout_heads[at].ended += 1;
            }
        }
    }

    fn layout_failed(&self, at: usize) -> R<(Option<i64>, i64)> {
        self.sc().layout_heads[at].failed = true;
        Err(Stop)
    }

    /// Bits from node `n`'s start to the end of its first `upto` attributes.
    fn end_of(&self, n: NodeId, upto: usize) -> R<i64> {
        if self.def.nodes[n].dynamic {
            let start = self.node_start(n)?;
            return self.nested(|e| e.layout(n, start, upto, true)).map(|l| l.1);
        }
        let attrs = &self.def.nodes[n].attrs;
        let mut p = 0i64;
        for (j, b) in attrs[..upto].iter().enumerate() {
            if !self.present(n, j)? {
                continue;
            }
            if !b.is_bits() {
                p = align(p);
            }
            let size = match (&b.kind, &b.size, &b.repeat) {
                (Kind::Int { bytes, .. } | Kind::Float { bytes, .. }, _, Repeat::No) => i64::from(*bytes) * 8,
                (Kind::Bits(k), _, Repeat::No) => i64::from(*k),
                (_, Size::Fixed(v), Repeat::No) if b.partial.is_none() => (*v as i64).checked_mul(8).ok_or(Stop)?,
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

    /// Size in bits of attribute `a` of node `n` starting at `abs`: all its
    /// elements if it repeats; 0 for a partial one that can't be read.
    fn attr_size(&self, n: NodeId, a: &Attr, abs: i64) -> R<i64> {
        if a.repeat != Repeat::No {
            return Ok(self.scan(n, a, abs)?.end - abs);
        }
        if a.partial.is_some() {
            return Ok(match self.element(n, a, abs) {
                Ok((end, _)) => end - abs,
                Err(Stop) => 0,
            });
        }
        self.elem_size(n, a, &a.kind, abs)
    }

    /// Size in bits of one value of kind `kind` of attribute `a` at `abs`.
    fn elem_size(&self, n: NodeId, a: &Attr, kind: &Kind, abs: i64) -> R<i64> {
        Ok(match (kind, &a.size) {
            (Kind::Int { bytes, .. } | Kind::Float { bytes, .. }, _) => i64::from(*bytes) * 8,
            (Kind::Bits(k), _) => i64::from(*k),
            (Kind::Switch(sw), Size::Child) => {
                let k = self.chosen(sw)?;
                return self.elem_size(n, a, k, abs);
            }
            (_, Size::Fixed(v)) => (*v as i64).checked_mul(8).ok_or(Stop)?,
            (_, Size::Bits(k)) => i64::from(*k),
            (_, Size::Expr(_) | Size::Eos) => self.sized_size(n, a, abs)?,
            (_, Size::Terminated(t)) => {
                let end = self.io_end(n)?;
                let (from, to) = (byte_at(abs)?, (end / 8) as usize);
                self.record_many();
                let hay = self.msg.get(from..to.min(self.msg.len())).ok_or(Stop)?;
                let i = memchr::memchr(*t, hay).ok_or(Stop)?;
                (i as i64 + 1) * 8
            }
            (Kind::User(c), Size::Child) => self.extent(*c, abs)?,
            (_, Size::Child) => return Err(Stop),
        })
    }

    /// How long an unsized object of node `c` at `abs` is: its attributes.
    fn extent(&self, c: NodeId, abs: i64) -> R<i64> {
        let len = self.def.nodes[c].attrs.len();
        if self.is_element(c) {
            self.with_frame(Some(self.frame(c, abs, None)), |e| e.end_of(c, len))
        } else {
            self.end_of(c, len)
        }
    }

    /// The case of switch attribute `i` of node `n` that is read: its index
    /// (the default is `cases.len()`), or none.
    fn case_index(&self, n: NodeId, i: usize) -> R<Option<usize>> {
        let Kind::Switch(sw) = &self.def.nodes[n].attrs[i].kind else { return Ok(None) };
        self.switch_index(sw)
    }

    fn switch_index(&self, sw: &Switch) -> R<Option<usize>> {
        let v = self.eval(&sw.on)?;
        Ok(sw
            .cases
            .iter()
            .position(|(k, _)| compare(&v, &lit(k)) == Some(Ordering::Equal))
            .or(sw.default.as_ref().map(|_| sw.cases.len())))
    }

    fn chosen<'s>(&self, sw: &'s Switch) -> R<&'s Kind> {
        match self.switch_index(sw)? {
            Some(c) if c < sw.cases.len() => Ok(&sw.cases[c].1),
            Some(_) => sw.default.as_ref().ok_or(Stop),
            None => Err(Stop),
        }
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

    // --- lists --------------------------------------------------------------

    /// The elements of list attribute `a` of node `n` starting at `abs`. A
    /// partial list stops before the first element that can't be read.
    fn scan(&self, n: NodeId, a: &Attr, abs: i64) -> R<ScanRef> {
        let i = self.def.nodes[n].attrs.iter().position(|x| std::ptr::eq(x, a)).unwrap_or(usize::MAX);
        let key = (n, i, abs);
        let hit = self.sc().scans.get(&key).copied();
        if let Some(s) = hit {
            self.record_many();
            return Ok(s);
        }
        let (items, end) = self.scan_list(n, a, abs)?;
        let mut sc = self.sc();
        let first = sc.items.len() as u32;
        if sc.items.capacity() == 0 {
            sc.items.reserve(64);
        }
        sc.items.extend(items.all().copied());
        let s = ScanRef { first, len: items.len as u32, end };
        reserve(&mut sc.scans);
        sc.scans.insert(key, s);
        Ok(s)
    }

    /// Element `k` of a scanned list: where it is, its object node.
    fn item(&self, s: ScanRef, k: usize) -> Option<Item> {
        if k >= s.len as usize {
            return None;
        }
        self.sc().items.get(s.first as usize + k).copied()
    }

    fn scan_list(&self, n: NodeId, a: &Attr, abs: i64) -> R<(Items, i64)> {
        let count = match &a.repeat {
            Repeat::Expr(e) => Some(self.int(e)?.max(0) as usize),
            _ => None,
        };
        let io_end = self.io_end(n)?;
        let (mut pos, mut items) = (abs, Items::new());
        // Elements with a plan: decoded once each, in order.
        if let Kind::User(c) = a.kind
            && self.def.plans[c].is_some()
            && matches!(a.repeat, Repeat::Expr(_) | Repeat::Eos)
        {
            let io_start = self.io_start(n)?;
            loop {
                if count.is_some_and(|c| items.len >= c) || (a.repeat == Repeat::Eos && pos >= io_end) {
                    break;
                }
                if items.len >= MAX_ITEMS {
                    if a.partial.is_some() {
                        break;
                    }
                    return Err(Stop);
                }
                let io = match &a.size {
                    Size::Child => (io_start, io_end),
                    _ => {
                        let size = self.sized_size(n, a, pos)?;
                        (pos, pos.checked_add(size).ok_or(Stop)?)
                    }
                };
                match self.decode(c, pos, io, None) {
                    Ok(r) => {
                        let end = if a.size == Size::Child { self.rec(r).end } else { io.1 };
                        if end > io_end {
                            if a.partial.is_some() {
                                break;
                            }
                            return Err(Stop);
                        }
                        items.push((pos, end, Some(c), Some(r)));
                        let progressed = end > pos;
                        pos = end;
                        if !progressed && count.is_none() {
                            break;
                        }
                    }
                    Err(Stop) if a.partial.is_some() => break,
                    Err(Stop) => return Err(Stop),
                }
            }
            return Ok((items, pos));
        }
        loop {
            if count.is_some_and(|c| items.len >= c) || (a.repeat == Repeat::Eos && pos >= io_end) {
                break;
            }
            if items.len >= MAX_ITEMS {
                if a.partial.is_some() {
                    break;
                }
                return Err(Stop);
            }
            match self.element(n, a, pos) {
                Ok((end, node)) => {
                    items.push((pos, end, node, None));
                    let progressed = end > pos;
                    let start = pos;
                    pos = end;
                    if let Repeat::Until(u) = &a.repeat {
                        let v = self.elem_val(n, a, start, end, node)?;
                        *self.underscore.borrow_mut() = v;
                        if self.eval(u)?.truthy() {
                            break;
                        }
                    }
                    if !progressed && count.is_none() {
                        break;
                    }
                }
                Err(Stop) if a.partial.is_some() => break,
                Err(Stop) => return Err(Stop),
            }
        }
        Ok((items, pos))
    }

    /// One element (or the value of a non-list attribute) at `abs`, checked:
    /// it fits its stream, its checks pass, an object's attributes are all
    /// read. Its end, and its object node if it is one.
    fn element(&self, n: NodeId, a: &Attr, abs: i64) -> R<(i64, Option<NodeId>)> {
        let i = self.def.nodes[n].attrs.iter().position(|x| std::ptr::eq(x, a)).unwrap_or(usize::MAX);
        let key = (n, i, abs);
        if i != usize::MAX {
            let hit = self.sc().elements.get(&key).copied();
            if let Some(r) = hit {
                self.record_many();
                return r;
            }
        }
        let r = self.check_element(n, a, abs);
        if i != usize::MAX {
            let mut sc = self.sc();
            reserve(&mut sc.elements);
            sc.elements.insert(key, r);
        }
        r
    }

    fn check_element(&self, n: NodeId, a: &Attr, abs: i64) -> R<(i64, Option<NodeId>)> {
        self.nested(|e| {
            let kind = match &a.kind {
                Kind::Switch(sw) => e.chosen(sw)?,
                k => k,
            };
            let size = e.elem_size(n, a, kind, abs)?;
            let end = abs.checked_add(size).ok_or(Stop)?;
            let (s, io_end) = e.untraced(|x| Ok((x.io_start(n)?, x.io_end(n)?)))?;
            if abs < s || end > io_end {
                return Err(Stop);
            }
            match kind {
                Kind::User(c) => {
                    let sized = e.def.nodes[*c].sized;
                    let fr = e.frame(*c, abs, sized.then_some(end));
                    e.with_frame(Some(fr), |e| e.validate_node(*c))?;
                    Ok((end, Some(*c)))
                }
                _ => {
                    if a.contents.is_some() || !a.checks.is_empty() {
                        let v = e.read_kind(n, a, kind, abs, end)?;
                        e.check(a, &v)?;
                    }
                    Ok((end, None))
                }
            }
        })
    }

    /// The value of one element.
    fn elem_val(&self, n: NodeId, a: &Attr, start: i64, end: i64, node: Option<NodeId>) -> R<Val<'m>> {
        match node {
            Some(c) => {
                let sized = self.def.nodes[c].sized;
                Ok(Val::Obj(self.frame(c, start, sized.then_some(end))))
            }
            None => {
                let kind = match &a.kind {
                    Kind::Switch(sw) => self.chosen(sw)?,
                    k => k,
                };
                self.read_kind(n, a, kind, start, end)
            }
        }
    }

    // --- values -------------------------------------------------------------

    /// Attribute `i` of node `n`.
    pub fn attr_val(&self, n: NodeId, i: usize) -> R<Val<'m>> {
        let Some(rel) = self.attr_pos(n, i)? else { return Ok(Val::Null) };
        let abs = self.node_start(n)? + rel;
        let a = &self.def.nodes[n].attrs[i];
        if a.repeat != Repeat::No {
            // Scanned now (a list that can't be read has no value); its
            // elements are read when asked for.
            self.scan(n, a, abs)?;
            return Ok(Val::List(ListRef { n, i, abs, ctx: self.cur.get() }));
        }
        if a.partial.is_some() {
            return Ok(match self.element(n, a, abs) {
                Ok((end, node)) => self.elem_val(n, a, abs, end, node)?,
                Err(Stop) => {
                    let io_end = self.io_end(n)?;
                    let from = byte_at(abs)?;
                    let to = ((io_end / 8) as usize).min(self.msg.len()).max(from);
                    Val::Undecoded(UndecodedRef { n, i, from, to, ctx: self.cur.get() })
                }
            });
        }
        let kind = match &a.kind {
            Kind::Switch(sw) => self.chosen(sw)?,
            k => k,
        };
        let size = self.elem_size(n, a, kind, abs)?;
        let end = abs.checked_add(size).ok_or(Stop)?;
        if self.checked {
            let (s, e) = self.untraced(|x| Ok((x.io_start(n)?, x.io_end(n)?)))?;
            if abs < s || end > e {
                return Err(Stop);
            }
        }
        match kind {
            Kind::User(c) => self.elem_val(n, a, abs, end, Some(*c)),
            _ => self.read_kind(n, a, kind, abs, end),
        }
    }

    fn read_kind(&self, n: NodeId, a: &Attr, kind: &Kind, abs: i64, end: i64) -> R<Val<'m>> {
        let enumed = |v: i64| match a.enum_id {
            Some(id) => Val::Enum(v, id),
            None => Val::Int(v),
        };
        match kind {
            Kind::Int { bytes, signed, endian } => {
                let v = self.number(n, abs, *bytes, *endian)?;
                let bits = u32::from(*bytes) * 8;
                Ok(if *signed && bits < 64 {
                    enumed(((v << (64 - bits)) as i64) >> (64 - bits))
                } else if !*signed && v > i64::MAX as u64 && a.enum_id.is_none() {
                    Val::U64(v)
                } else {
                    enumed(v as i64)
                })
            }
            Kind::Float { bytes, endian } => {
                let v = self.number(n, abs, *bytes, *endian)?;
                Ok(if *bytes == 4 { Val::F32(f32::from_bits(v as u32)) } else { Val::Float(f64::from_bits(v)) })
            }
            Kind::Bits(k) => {
                let v = self.bits(abs, u32::from(*k))?;
                Ok(if *k == 1 && a.enum_id.is_none() { Val::Bool(v != 0) } else { enumed(v as i64) })
            }
            Kind::Bytes | Kind::Str => {
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
                Ok(if matches!(kind, Kind::Str) { Val::Text(s) } else { Val::Msg(s) })
            }
            Kind::User(c) => self.elem_val(n, a, abs, end, Some(*c)),
            Kind::Switch(sw) => {
                let k = self.chosen(sw)?;
                self.read_kind(n, a, k, abs, end)
            }
        }
    }

    /// A `bytes`-long number at `abs`, in the given or the node's order.
    fn number(&self, n: NodeId, abs: i64, bytes: u8, endian: Option<Endian>) -> R<u64> {
        let at = byte_at(abs)?;
        let b = self.slice(at, at + usize::from(bytes))?;
        let endian = match endian {
            Some(e) => e,
            None => self.endian(n)?,
        };
        let mut v: u64 = 0;
        match endian {
            Endian::Be => b.iter().for_each(|&x| v = (v << 8) | u64::from(x)),
            Endian::Le => b.iter().rev().for_each(|&x| v = (v << 8) | u64::from(x)),
        }
        Ok(v)
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
        let node = &self.def.nodes[n];
        let inst = &node.insts[k];
        if let Some(ty) = inst.small
            && !node.dynamic
        {
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
                    let size = e.elem_size(n, read, &read.kind, abs)?;
                    let end = abs.checked_add(size).ok_or(Stop)?;
                    let io_end = e.untraced(|x| x.io_end(n))?;
                    if abs < s || end > io_end {
                        return Err(Stop);
                    }
                    e.read_kind(n, read, &read.kind, abs, end)
                }
            }
        })
    }

    fn int(&self, e: &CExpr) -> R<i64> {
        self.eval(e)?.as_i64().ok_or(Stop)
    }

    pub fn eval(&self, e: &CExpr) -> R<Val<'m>> {
        Ok(match e {
            CExpr::Lit(l) => lit(l),
            CExpr::Attr(n, i) if self.def.nodes[*n].dynamic => {
                self.found(Found::Attr, *n, *i, |x| x.nested(|x| x.attr_val(*n, *i)))?
            }
            CExpr::Attr(n, i) => self.nested(|x| x.attr_val(*n, *i))?,
            CExpr::Inst(n, k) if self.def.nodes[*n].dynamic => {
                self.found(Found::Inst, *n, *k, |x| x.inst_val(*n, *k))?
            }
            CExpr::Inst(n, k) => self.inst_val(*n, *k)?,
            CExpr::IoSize(n) => {
                let (s, e) = self.io(*n)?;
                Val::Int((e - s) / 8)
            }
            CExpr::Length(a) => match self.eval(a)? {
                Val::List(l) => Val::Int(self.list_scan(&l)?.len as i64),
                v => v.bytes().map_or(Val::Null, |b| Val::Int(b.len() as i64)),
            },
            CExpr::Index(a, i) => {
                let v = self.eval(a)?;
                let Some(i) = self.eval(i)?.as_i64().filter(|i| *i >= 0) else { return Ok(Val::Null) };
                match v {
                    Val::List(l) => self.list_item(&l, i as usize)?.unwrap_or(Val::Null),
                    v => v.bytes().and_then(|b| b.get(i as usize)).map_or(Val::Null, |&x| Val::Int(i64::from(x))),
                }
            }
            CExpr::In(obj, inner) => match self.eval(obj)? {
                Val::Obj(fr) => self.with_frame(Some(fr), |e| e.eval(inner))?,
                Val::Rec(r) => {
                    let fr = self.rec_frame(r);
                    self.with_frame(Some(fr), |e| e.eval(inner))?
                }
                _ => Val::Null,
            },
            CExpr::ToInt(a) => self.eval(a)?.as_i64().map_or(Val::Null, Val::Int),
            CExpr::Unary(op, a) => {
                let v = self.eval(a)?;
                match (op, v) {
                    (UnOp::Not, v) => Val::Bool(!v.truthy()),
                    (UnOp::Neg, Val::Float(x)) => Val::Float(-x),
                    (UnOp::Neg, Val::F32(x)) => Val::F32(-x),
                    (UnOp::Neg, v) => v.as_i64().map_or(Val::Null, |x| Val::Int(x.wrapping_neg())),
                    (UnOp::BitNot, v) => v.as_i64().map_or(Val::Null, |x| Val::Int(!x)),
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
            CExpr::Rest => self.rest.borrow().map_or(Val::Null, Val::Msg),
            CExpr::Origin(f) => match f {
                OriginField::Channel => {
                    let c: &'m [u8] = self.origin.channel;
                    let n = c.iter().position(|&b| b == 0).unwrap_or(c.len());
                    Val::Text(&c[..n])
                }
                OriginField::Secs => Val::Int(i64::from(self.origin.secs)),
                OriginField::Usecs => Val::Int(i64::from(self.origin.usecs)),
            },
        })
    }

    // --- records: objects decoded by their plans ------------------------------

    fn rec(&self, r: u32) -> Rec {
        self.sc().recs[r as usize]
    }

    /// Decode an object of node `c` at `start` in the stream `io`, by its
    /// plan: its attributes in order into a new record.
    fn decode(&self, c: NodeId, start: i64, io: (i64, i64), parent: Option<u32>) -> R<u32> {
        let plan = self.def.plans[c].as_ref().ok_or(Stop)?;
        let id = {
            let mut sc = self.sc();
            if sc.recs.capacity() == 0 {
                sc.recs.reserve(32);
                sc.rvals.reserve(256);
            }
            let first = sc.rvals.len() as u32;
            let n = plan.attrs.len() + plan.insts.len();
            sc.rvals.extend(std::iter::repeat_n(Kept::Missing, n));
            sc.recs.push(Rec { node: c, start, end: start, io, parent, first });
            (sc.recs.len() - 1) as u32
        };
        self.nested(|e| {
            let mut pos = start;
            for (j, a) in plan.attrs.iter().enumerate() {
                if let Some(cond) = &a.cond
                    && !e.fast(cond, id)?.truthy()
                {
                    e.set_rval(id, j, Kept::Null);
                    continue;
                }
                if !a.bits {
                    pos = align(pos);
                }
                let (v, end) = e.read_plan(&a.read, a.enum_id, id, pos, io)?;
                if end > io.1 {
                    return Err(Stop);
                }
                e.set_rval(id, j, v);
                pos = end;
            }
            e.sc().recs[id as usize].end = pos;
            Ok(id)
        })
    }

    fn set_rval(&self, r: u32, j: usize, v: Kept) {
        let mut sc = self.sc();
        let first = sc.recs[r as usize].first as usize;
        sc.rvals[first + j] = v;
    }

    /// One attribute of record `id` at `pos`: its value and where it ends.
    fn read_plan(&self, read: &Read, enum_id: Option<usize>, id: u32, pos: i64, io: (i64, i64)) -> R<(Kept, i64)> {
        let enumed = |v: i64| match enum_id {
            Some(e) => Kept::Enum(v, e),
            None => Kept::Int(v),
        };
        Ok(match read {
            Read::Int { bytes, signed, order } => {
                let v = self.number_in(pos, *bytes, *order)?;
                let bits = u32::from(*bytes) * 8;
                let v = if *signed && bits < 64 {
                    enumed(((v << (64 - bits)) as i64) >> (64 - bits))
                } else if !*signed && v > i64::MAX as u64 && enum_id.is_none() {
                    Kept::U64(v)
                } else {
                    enumed(v as i64)
                };
                (v, pos + i64::from(*bytes) * 8)
            }
            Read::Float { bytes, order } => {
                let v = self.number_in(pos, *bytes, *order)?;
                let v = if *bytes == 4 { Kept::F32(f32::from_bits(v as u32)) } else { Kept::Float(f64::from_bits(v)) };
                (v, pos + i64::from(*bytes) * 8)
            }
            Read::Bits(k) => {
                let v = self.bits(pos, u32::from(*k))?;
                let v = if *k == 1 && enum_id.is_none() { Kept::Bool(v != 0) } else { enumed(v as i64) };
                (v, pos + i64::from(*k))
            }
            Read::Bytes { size, text, terminator } => {
                let from = byte_at(pos)?;
                let (to, cut_last) = match size {
                    PSize::Terminated(t) => {
                        let hay = self.msg.get(from..((io.1 / 8) as usize).min(self.msg.len())).ok_or(Stop)?;
                        (from + memchr::memchr(*t, hay).ok_or(Stop)? + 1, true)
                    }
                    s => (from + self.psize(s, id, pos, io)? as usize, false),
                };
                let mut b = self.msg.get(from..to).ok_or(Stop)?;
                let mut z = to;
                if cut_last {
                    z = to - 1;
                } else if let Some(t) = terminator
                    && let Some(i) = memchr::memchr(*t, b)
                {
                    z = from + i;
                }
                b = &self.msg[from..z];
                let _ = b;
                (if *text { Kept::Text(from, z) } else { Kept::Msg(from, z) }, to as i64 * 8)
            }
            Read::Obj { node, size } => {
                let child_io = match size {
                    Some(s) => {
                        let len = self.psize(s, id, pos, io)?;
                        (pos, pos.checked_add(len.checked_mul(8).ok_or(Stop)?).ok_or(Stop)?)
                    }
                    None => io,
                };
                let r = self.decode(*node, pos, child_io, Some(id))?;
                let end = if size.is_some() { child_io.1 } else { self.rec(r).end };
                (Kept::Rec(r), end)
            }
            Read::Switch { on, cases, default } => {
                let v = self.fast(on, id)?;
                let case = cases
                    .iter()
                    .find(|(k, _)| compare(&v, &lit(k)) == Some(Ordering::Equal))
                    .map(|(_, r)| r)
                    .or(default.as_deref())
                    .ok_or(Stop)?;
                return self.read_plan(case, enum_id, id, pos, io);
            }
        })
    }

    /// A size in bytes.
    fn psize(&self, s: &PSize, id: u32, pos: i64, io: (i64, i64)) -> R<i64> {
        Ok(match s {
            PSize::Fixed(v) => *v as i64,
            PSize::Expr(f) => {
                let v = self.fast(f, id)?.as_i64().ok_or(Stop)?;
                if v < 0 {
                    return Err(Stop);
                }
                v
            }
            PSize::Eos => {
                if io.1 < pos {
                    return Err(Stop);
                }
                (io.1 - pos) / 8
            }
            PSize::Terminated(_) => return Err(Stop),
        })
    }

    /// A number in the given order; a node's order is found once per message.
    fn number_in(&self, pos: i64, bytes: u8, order: Order) -> R<u64> {
        let endian = match order {
            Order::Fixed(e) => e,
            Order::Of(n) => match self.order.get() {
                Some((m, e)) if m == n => e,
                _ => {
                    let e = self.endian(n)?;
                    self.order.set(Some((n, e)));
                    e
                }
            },
        };
        let at = byte_at(pos)?;
        let b = self.msg.get(at..at + usize::from(bytes)).ok_or(Stop)?;
        let mut v: u64 = 0;
        match endian {
            Endian::Be => b.iter().for_each(|&x| v = (v << 8) | u64::from(x)),
            Endian::Le => b.iter().rev().for_each(|&x| v = (v << 8) | u64::from(x)),
        }
        Ok(v)
    }

    /// The record `up` levels above record `id`, then down through the
    /// objects in attributes `down`; none if one isn't there (absent, or
    /// another case of a switch).
    fn rec_at(&self, mut id: u32, up: u8, down: &[(usize, NodeId)]) -> Option<u32> {
        let sc = self.sc();
        for _ in 0..up {
            id = sc.recs[id as usize].parent?;
        }
        for &(attr, node) in down {
            let r = sc.recs[id as usize];
            match sc.rvals[r.first as usize + attr] {
                Kept::Rec(c) if sc.recs[c as usize].node == node => id = c,
                _ => return None,
            }
        }
        Some(id)
    }

    /// A plan's expression on record `id`.
    fn fast(&self, f: &Fast, id: u32) -> R<Val<'m>> {
        Ok(match f {
            Fast::Lit(l) => lit(l),
            Fast::Field { up, down, i } => {
                let Some(r) = self.rec_at(id, *up, down) else { return Ok(Val::Null) };
                let k = {
                    let sc = self.sc();
                    sc.rvals[sc.recs[r as usize].first as usize + i]
                };
                self.unkeep(k)
            }
            Fast::Inst { up, down, k } => {
                let Some(r) = self.rec_at(id, *up, down) else { return Ok(Val::Null) };
                let rec = self.rec(r);
                let plan = self.def.plans[rec.node].as_ref().ok_or(Stop)?;
                let slot = rec.first as usize + plan.attrs.len() + k;
                let kept = self.sc().rvals[slot];
                if !matches!(kept, Kept::Missing) {
                    return Ok(self.unkeep(kept));
                }
                let v = self.nested(|e| e.fast(&plan.insts[*k], r))?;
                if let Some(kv) = self.keep(&v) {
                    self.sc().rvals[slot] = kv;
                }
                v
            }
            Fast::Static(e) => self.eval(e)?,
            Fast::IoSize { up, down } => {
                let Some(r) = self.rec_at(id, *up, down) else { return Ok(Val::Null) };
                let r = self.rec(r);
                Val::Int((r.io.1 - r.io.0) / 8)
            }
            Fast::Length(a) => self.fast(a, id)?.bytes().map_or(Val::Null, |b| Val::Int(b.len() as i64)),
            Fast::Index(a, i) => {
                let v = self.fast(a, id)?;
                match (v.bytes(), self.fast(i, id)?.as_i64()) {
                    (Some(b), Some(i)) if i >= 0 => b.get(i as usize).map_or(Val::Null, |&x| Val::Int(i64::from(x))),
                    _ => Val::Null,
                }
            }
            Fast::ToInt(a) => self.fast(a, id)?.as_i64().map_or(Val::Null, Val::Int),
            Fast::Unary(op, a) => {
                let v = self.fast(a, id)?;
                match (op, v) {
                    (UnOp::Not, v) => Val::Bool(!v.truthy()),
                    (UnOp::Neg, Val::Float(x)) => Val::Float(-x),
                    (UnOp::Neg, Val::F32(x)) => Val::F32(-x),
                    (UnOp::Neg, v) => v.as_i64().map_or(Val::Null, |x| Val::Int(x.wrapping_neg())),
                    (UnOp::BitNot, v) => v.as_i64().map_or(Val::Null, |x| Val::Int(!x)),
                }
            }
            Fast::Binary(BinOp::And, a, b) => Val::Bool(self.fast(a, id)?.truthy() && self.fast(b, id)?.truthy()),
            Fast::Binary(BinOp::Or, a, b) => Val::Bool(self.fast(a, id)?.truthy() || self.fast(b, id)?.truthy()),
            Fast::Binary(op, a, b) => {
                let (a, b) = (self.fast(a, id)?, self.fast(b, id)?);
                binary(*op, a, b)
            }
            Fast::Cond(c, a, b) => {
                if self.fast(c, id)?.truthy() {
                    self.fast(a, id)?
                } else {
                    self.fast(b, id)?
                }
            }
        })
    }

    /// A plan's template on record `id`; whether all its values were there.
    fn fast_template(&self, t: &[Piece<Fast>], id: u32, out: &mut Vec<u8>) -> bool {
        let node = self.rec(id).node;
        let mut all = true;
        for piece in t {
            match piece {
                Piece::Text(s) => out.extend_from_slice(s.as_bytes()),
                Piece::Value(e, f) => {
                    let v = self.fast(e, id).unwrap_or(Val::Null);
                    all &= self.text(&v, f, Some(node), out);
                }
                Piece::Group(alts) => {
                    let at = out.len();
                    for alt in alts {
                        if self.fast_template(alt, id, out) {
                            break;
                        }
                        out.truncate(at);
                    }
                }
            }
        }
        all
    }

    /// A record as a frame, for the general reader (`obj.member`).
    fn rec_frame(&self, id: u32) -> FrameId {
        let rec = self.rec(id);
        let up = rec.parent.map(|p| self.rec_frame(p));
        let sized = self.def.nodes[rec.node].sized;
        self.with_frame(up.or(self.cur.get()), |e| e.frame(rec.node, rec.start, sized.then_some(rec.end)))
    }

    /// A list value's scan, in its context.
    fn list_scan(&self, l: &ListRef) -> R<ScanRef> {
        let a = &self.def.nodes[l.n].attrs[l.i];
        self.with_frame(l.ctx, |e| e.scan(l.n, a, l.abs))
    }

    /// Element `k` of a list value.
    fn list_item(&self, l: &ListRef, k: usize) -> R<Option<Val<'m>>> {
        let s = self.list_scan(l)?;
        let Some((start, end, node, rec)) = self.item(s, k) else { return Ok(None) };
        if let Some(r) = rec {
            return Ok(Some(Val::Rec(r)));
        }
        let a = &self.def.nodes[l.n].attrs[l.i];
        self.with_frame(l.ctx, |e| e.elem_val(l.n, a, start, end, node)).map(Some)
    }

    // --- checking a whole message -------------------------------------------

    /// Every attribute of every node (not instances, not partial ones) fits
    /// its stream and passes its checks.
    pub fn validate(&self) -> R<()> {
        self.validate_node(0)
    }

    /// One pass over the node's attributes in order, as a sequential reader
    /// would make (positions add up as it goes; values are read only where
    /// a check needs them).
    fn validate_node(&self, n: NodeId) -> R<()> {
        self.nested(|e| {
            let node = &e.def.nodes[n];
            let start = e.node_start(n)?;
            let end = e.io_end(n)?;
            let mut p = 0i64;
            for (i, a) in node.attrs.iter().enumerate() {
                if !e.present(n, i)? {
                    continue;
                }
                if !a.is_bits() {
                    p = align(p);
                }
                let abs = start + p;
                // A partial attribute never fails, and last it moves
                // nothing: not even read (splitting never decodes a
                // payload). A list checks its elements as it reads them.
                if a.partial.is_some() && i + 1 == node.attrs.len() {
                    continue;
                }
                if a.partial.is_some() || a.repeat != Repeat::No {
                    p = p.checked_add(e.attr_size(n, a, abs)?).ok_or(Stop)?;
                    if start + p > end {
                        return Err(Stop);
                    }
                    continue;
                }
                let kind = match &a.kind {
                    Kind::Switch(sw) => e.chosen(sw)?,
                    k => k,
                };
                let size = e.elem_size(n, a, kind, abs)?;
                p = p.checked_add(size).ok_or(Stop)?;
                if start + p > end {
                    return Err(Stop);
                }
                if let Kind::User(c) = kind {
                    e.validate_node(*c)?;
                }
                if a.contents.is_none() && a.checks.is_empty() {
                    continue;
                }
                let v = e.read_kind(n, a, kind, abs, abs + size)?;
                e.check(a, &v)?;
            }
            Ok(())
        })
    }

    fn check(&self, a: &Attr, v: &Val<'m>) -> R<()> {
        if let Some(c) = a.contents
            && v.bytes() != Some(c)
        {
            return Err(Stop);
        }
        for check in &a.checks {
            *self.underscore.borrow_mut() = v.clone();
            let ok = match check {
                Check::Eq(e) => compare(v, &self.eval(e)?) == Some(Ordering::Equal),
                Check::Min(e) => compare(v, &self.eval(e)?).is_some_and(|o| o != Ordering::Less),
                Check::Max(e) => compare(v, &self.eval(e)?).is_some_and(|o| o != Ordering::Greater),
                Check::AnyOf(list) => {
                    let mut any = false;
                    for e in list {
                        any |= compare(v, &self.eval(e)?) == Some(Ordering::Equal);
                    }
                    any
                }
                Check::Expr(e) => self.eval(e)?.truthy(),
            };
            if !ok {
                return Err(Stop);
            }
        }
        Ok(())
    }

    // --- text -----------------------------------------------------------------

    /// Write `v` as text in the format `fmt`; `node` is the type whose
    /// template asks (its byte order for numbers made of bytes). Whether
    /// anything was written: a missing or empty value hides its `[…]`.
    pub fn text(&self, v: &Val<'m>, fmt: &Fmt, node: Option<NodeId>, out: &mut Vec<u8>) -> bool {
        let start = out.len();
        let _ = self.nested(|e| {
            e.write_text(v, fmt, node, out);
            Ok(())
        });
        out.len() > start
    }

    fn write_text(&self, v: &Val<'m>, fmt: &Fmt, node: Option<NodeId>, out: &mut Vec<u8>) {
        match v {
            Val::Null => {}
            Val::Int(x) => int_text(*x, fmt, out),
            Val::U64(x) => match fmt {
                Fmt::Hex => {
                    let _ = write!(out, "{x:#x}");
                }
                Fmt::Name => {}
                _ => {
                    let _ = write!(out, "{x}");
                }
            },
            Val::Enum(x, id) => {
                let name = self.def.enums.get(*id).and_then(|e| e.values.iter().find(|(v, _)| v == x)).map(|(_, l)| *l);
                match (fmt, name) {
                    (Fmt::Name | Fmt::Plain, Some(l)) => out.extend_from_slice(l),
                    (Fmt::Name, None) => {}
                    _ => int_text(*x, fmt, out),
                }
            }
            Val::Float(x) => {
                let _ = write!(out, "{x}");
            }
            Val::F32(x) => {
                let _ = write!(out, "{x}");
            }
            Val::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
            // Text of the message, and names and strings of the definition.
            Val::Text(b) | Val::Lit(b) => match fmt {
                Fmt::Hex => hex(b, out),
                Fmt::Str => out.extend_from_slice(String::from_utf8_lossy(strip_nul(b)).as_bytes()),
                _ => out.extend_from_slice(String::from_utf8_lossy(b).as_bytes()),
            },
            Val::Msg(_) | Val::Own(_) => {
                let b = v.bytes().unwrap_or_default();
                match fmt {
                    // Bytes as they are (a `str` is decoded; bytes aren't).
                    Fmt::Str => out.extend_from_slice(strip_nul(b)),
                    Fmt::Dec | Fmt::SDec if b.len() <= 16 => {
                        let endian = node.and_then(|n| self.endian(n).ok()).unwrap_or(Endian::Be);
                        let mut be = [0u8; 16];
                        match endian {
                            Endian::Be => be[16 - b.len()..].copy_from_slice(b),
                            Endian::Le => b.iter().enumerate().for_each(|(i, &x)| be[15 - i] = x),
                        }
                        let u = u128::from_be_bytes(be);
                        let _ = if *fmt == Fmt::SDec && !b.is_empty() {
                            let shift = 128 - 8 * b.len() as u32;
                            write!(out, "{}", ((u << shift) as i128) >> shift)
                        } else {
                            write!(out, "{u}")
                        };
                    }
                    _ => hex(b, out),
                }
            }
            Val::Obj(id) => {
                let Some(fr) = self.fr(*id) else { return };
                let c = fr.node;
                self.with_frame(Some(*id), |e| match &e.def.nodes[c].text {
                    Some(t) => {
                        e.template(t, c, out);
                    }
                    None => {
                        let end = fr.end.or_else(|| e.extent(c, fr.start).ok().map(|x| fr.start + x));
                        if let (Some(end), Ok(from)) = (end, byte_at(fr.start))
                            && let Ok(to) = byte_at(align(end))
                            && let Some(b) = e.msg.get(from..to)
                        {
                            hex(b, out);
                        }
                    }
                });
            }
            Val::Rec(r) => {
                let rec = self.rec(*r);
                match self.def.plans[rec.node].as_ref().and_then(|p| p.text.as_ref()) {
                    Some(t) => {
                        self.fast_template(t, *r, out);
                    }
                    None => {
                        if let (Ok(from), Ok(to)) = (byte_at(rec.start), byte_at(align(rec.end)))
                            && let Some(b) = self.msg.get(from..to)
                        {
                            hex(b, out);
                        }
                    }
                }
            }
            Val::List(l) => {
                let sep = match fmt {
                    Fmt::Sep(s) => s.as_bytes(),
                    _ => b" ",
                };
                let Ok(s) = self.list_scan(l) else { return };
                let start = out.len();
                for k in 0..s.len as usize {
                    if k > 0 {
                        out.extend_from_slice(sep);
                    }
                    if let Ok(Some(item)) = self.list_item(l, k) {
                        self.write_text(&item, &Fmt::Plain, node, out);
                    }
                }
                let a = &self.def.nodes[l.n].attrs[l.i];
                if let Some(t) = &a.partial {
                    let io_end = self.with_frame(l.ctx, |e| e.io_end(l.n));
                    if let (Ok(io_end), Ok(from)) = (io_end, byte_at(s.end))
                        && let Some(rest) = self.msg.get(from..((io_end / 8) as usize).min(self.msg.len()))
                        && !rest.is_empty()
                    {
                        if out.len() > start {
                            out.extend_from_slice(sep);
                        }
                        self.partial(t, l.n, rest, l.ctx, out);
                    }
                }
            }
            Val::Undecoded(u) => {
                if let Some(t) = &self.def.nodes[u.n].attrs[u.i].partial {
                    let rest = self.msg.get(u.from..u.to).unwrap_or(&[]);
                    self.partial(t, u.n, rest, u.ctx, out);
                }
            }
        }
    }

    /// A `-nerka-partial` template with `_rest`.
    fn partial(&self, t: &Template, n: NodeId, rest: &'m [u8], ctx: Option<FrameId>, out: &mut Vec<u8>) {
        let old_rest = self.rest.replace(Some(rest));
        let old_cur = self.cur.replace(ctx);
        self.template(t, n, out);
        self.cur.set(old_cur);
        *self.rest.borrow_mut() = old_rest;
    }

    /// Render a template of node `n`; whether all its values were there.
    fn template(&self, t: &Template, n: NodeId, out: &mut Vec<u8>) -> bool {
        let mut all = true;
        for piece in t {
            match piece {
                Piece::Text(s) => out.extend_from_slice(s.as_bytes()),
                Piece::Value(e, f) => {
                    let v = self.eval(e).unwrap_or(Val::Null);
                    all &= self.text(&v, f, Some(n), out);
                }
                Piece::Group(alts) => {
                    // Each alternative straight into `out`, taken back if
                    // a value of it is missing.
                    let at = out.len();
                    for alt in alts {
                        if self.template(alt, n, out) {
                            break;
                        }
                        out.truncate(at);
                    }
                }
            }
        }
        all
    }
}

/// Room for what one message's objects keep, made once (allocating is the
/// costly part of reading a payload).
fn reserve<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, V, Fx>) {
    if map.capacity() == 0 {
        map.reserve(32);
    }
}

fn int_text(x: i64, fmt: &Fmt, out: &mut Vec<u8>) {
    match fmt {
        Fmt::Hex => {
            let _ = write!(out, "{:#x}", x as u64);
        }
        Fmt::Name => {}
        _ => {
            let _ = write!(out, "{x}");
        }
    }
}

/// Bytes as hex pairs separated by spaces.
pub fn hex(b: &[u8], out: &mut Vec<u8>) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for (i, &x) in b.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        out.extend_from_slice(&[DIGITS[(x >> 4) as usize], DIGITS[(x & 15) as usize]]);
    }
}

fn strip_nul(b: &[u8]) -> &[u8] {
    b.strip_suffix(b"\0").unwrap_or(b)
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
    let is_int = |v: &Val<'_>| matches!(v, Val::Int(_) | Val::U64(_) | Val::Enum(..));
    if is_int(&a) && is_int(&b) {
        return match (a.as_i64(), b.as_i64()) {
            (Some(x), Some(y)) => match int_op(op, x, y) {
                Some(Lit::Int(v)) => Val::Int(v),
                _ => Val::Null,
            },
            _ => Val::Null,
        };
    }
    if let (Val::Bool(x), Val::Bool(y)) = (&a, &b) {
        return match op {
            BinOp::BitAnd => Val::Bool(x & y),
            BinOp::BitOr => Val::Bool(x | y),
            BinOp::BitXor => Val::Bool(x ^ y),
            _ => Val::Null,
        };
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
        (Val::Null, _) | (_, Val::Null) => None,
        (Val::Bool(x), Val::Bool(y)) => Some(x.cmp(y)),
        (Val::U64(x), Val::U64(y)) => Some(x.cmp(y)),
        (Val::U64(x), y) if y.as_i64().is_some() && !matches!(y, Val::Bool(_)) => {
            Some(i128::from(*x).cmp(&i128::from(y.as_i64()?)))
        }
        (x, Val::U64(y)) if x.as_i64().is_some() && !matches!(x, Val::Bool(_)) => {
            Some(i128::from(x.as_i64()?).cmp(&i128::from(*y)))
        }
        (Val::Int(_) | Val::Enum(..), Val::Int(_) | Val::Enum(..)) => Some(a.as_i64()?.cmp(&b.as_i64()?)),
        _ => {
            if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
                return x.partial_cmp(&y);
            }
            Some(a.bytes()?.cmp(b.bytes()?))
        }
    }
}

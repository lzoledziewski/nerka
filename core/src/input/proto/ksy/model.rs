//! A definition compiled: the static tree of nodes with every name bound.
//!
//! There are no arrays (`repeat`) yet, so every attribute of every nested
//! type has one place: the root type is node 0, an attribute (or a
//! positional instance) of a user type is a child node. Expressions are
//! bound per node, so `_parent`, `body.ext.apid` and the like become node
//! and attribute numbers when the file loads.

use super::KsyError;
use super::expr::{self, BinOp, Expr, UnOp};
use super::run::Memo;
use super::yaml::{Key, Node, Value};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

pub type NodeId = usize;

/// At most this many nodes (types used inside types).
const MAX_NODES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Be,
    Le,
}

/// A constant.
#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Int(i64),
    Float(f64),
    Bool(bool),
    Bytes(&'static [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginField {
    Channel,
    Secs,
    Usecs,
}

/// A bound expression.
#[derive(Debug, Clone, PartialEq)]
pub enum CExpr {
    Lit(Lit),
    /// Seq attribute `i` of node `n`.
    Attr(NodeId, usize),
    /// Instance `k` of node `n`.
    Inst(NodeId, usize),
    /// `_io.size` of node `n`'s stream, in bytes.
    IoSize(NodeId),
    Length(Box<CExpr>),
    Index(Box<CExpr>, Box<CExpr>),
    Unary(UnOp, Box<CExpr>),
    Binary(BinOp, Box<CExpr>, Box<CExpr>),
    Cond(Box<CExpr>, Box<CExpr>, Box<CExpr>),
    /// `_`: the value a `valid` check is about.
    Underscore,
    /// What the container knows (`-nerka:` only).
    Origin(OriginField),
}

/// What an expression gives, as far as is known when it is bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Bytes,
    Any,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Int {
        bytes: u8,
        signed: bool,
        endian: Option<Endian>,
    },
    Bits(u8),
    Bytes,
    Str,
    /// A user type: the node it is.
    User(NodeId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Size {
    /// Bytes.
    Fixed(u64),
    Bits(u8),
    /// Bytes, from an expression.
    Expr(CExpr),
    /// To the end of the stream.
    Eos,
    /// Up to and including the first `terminator` byte.
    Terminated(u8),
    /// An unsized user type: as long as its attributes.
    Child,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    Eq(CExpr),
    Min(CExpr),
    Max(CExpr),
    AnyOf(Vec<CExpr>),
    Expr(CExpr),
}

pub struct Attr {
    pub id: String,
    pub kind: Kind,
    pub size: Size,
    pub cond: Option<CExpr>,
    pub checks: Vec<Check>,
    pub contents: Option<&'static [u8]>,
    /// For bytes and strings with a size: cut at the first such byte.
    pub terminator: Option<u8>,
    /// Where it starts in its node, in bits (`None`: not there).
    pub pos: Memo,
    /// Whether it is there (its `if`): 1 or 0.
    pub there: Memo,
}

impl Attr {
    pub fn is_bits(&self) -> bool {
        matches!(self.kind, Kind::Bits(_))
    }
}

pub enum InstKind {
    Value(CExpr),
    /// Read at `pos` bytes into the node's stream, as `read` says.
    Read {
        pos: CExpr,
        read: Box<Attr>,
    },
}

pub struct Inst {
    pub id: String,
    pub cond: Option<CExpr>,
    pub kind: InstKind,
    /// A value instance that is a number or a boolean: memoized like
    /// positions (`has_ext: (htyp & 1) != 0` reads one byte).
    pub small: Option<Ty>,
    pub memo: Memo,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EndianRule {
    Fixed(Endian),
    Switch { on: CExpr, cases: Vec<(Lit, Endian)>, default: Option<Endian> },
}

/// Where a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Root,
    Attr(NodeId, usize),
    Inst(NodeId, usize),
}

pub struct NodeDef {
    pub type_name: String,
    pub place: Place,
    pub parent: Option<NodeId>,
    pub attrs: Vec<Attr>,
    pub insts: Vec<Inst>,
    /// Its own; `None`: its parent's.
    pub endian: Option<EndianRule>,
    /// It has a stream of its own (a size, or an instance with one).
    pub sized: bool,
    /// Where it starts, in bits from the message's start.
    pub start: Memo,
}

pub struct Def {
    pub nodes: Vec<NodeDef>,
}

/// Byte strings that live as long as the process: names a definition maps
/// to, string constants. Interned, so loading a definition again adds
/// nothing.
pub fn intern(b: &[u8]) -> &'static [u8] {
    static ALL: OnceLock<Mutex<HashSet<&'static [u8]>>> = OnceLock::new();
    let mut all = ALL.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = all.get(b) {
        return s;
    }
    let s: &'static [u8] = Box::leak(b.to_vec().into_boxed_slice());
    all.insert(s);
    s
}

// ---------------------------------------------------------------------------
// Reading the YAML into types (names unbound).

struct TypeSpec {
    name: String,
    parent: Option<usize>,
    children: Vec<(String, usize)>,
    endian: Option<EndianSpec>,
    attrs: Vec<AttrSpec>,
    insts: Vec<InstSpec>,
}

enum EndianSpec {
    Fixed(Endian),
    Switch { on: Src, cases: Vec<(Node, Endian)>, default: Option<Endian> },
}

/// An expression with where its text is.
#[derive(Clone)]
pub struct Src {
    pub expr: Expr,
    pub line: usize,
    pub col: usize,
}

enum TypeRef {
    Int { bytes: u8, signed: bool, endian: Option<Endian> },
    Bits(u8),
    Str,
    Strz,
    Bytes,
    User(String),
}

struct AttrSpec {
    id: String,
    line: usize,
    col: usize,
    ty: TypeRef,
    size: Option<Src>,
    size_eos: bool,
    cond: Option<Src>,
    valid: Vec<(String, Vec<Src>)>,
    contents: Option<Vec<u8>>,
    terminator: Option<u8>,
}

struct InstSpec {
    id: String,
    line: usize,
    col: usize,
    cond: Option<Src>,
    value: Option<Src>,
    pos: Option<Src>,
    read: Option<AttrSpec>,
}

pub fn err(line: usize, col: usize, message: impl Into<String>) -> KsyError {
    KsyError { line, col, message: message.into() }
}

fn node_err(n: &Node, message: impl Into<String>) -> KsyError {
    err(n.line, n.col, message)
}

fn key_err(k: &Key, message: impl Into<String>) -> KsyError {
    err(k.line, k.col, message)
}

pub fn as_map<'n>(n: &'n Node, what: &str) -> Result<&'n [(Key, Node)], KsyError> {
    match &n.value {
        Value::Map(e) => Ok(e),
        Value::Null => Ok(&[]),
        _ => Err(node_err(n, format!("{what} must be a mapping"))),
    }
}

pub fn as_seq<'n>(n: &'n Node, what: &str) -> Result<&'n [Node], KsyError> {
    match &n.value {
        Value::Seq(e) => Ok(e),
        Value::Null => Ok(&[]),
        _ => Err(node_err(n, format!("{what} must be a list"))),
    }
}

pub fn as_text<'n>(n: &'n Node, what: &str) -> Result<&'n str, KsyError> {
    match &n.value {
        Value::Scalar { text, .. } => Ok(text),
        _ => Err(node_err(n, format!("{what} must be a single value"))),
    }
}

/// A value that is an expression: a number or `true` written as YAML is
/// one too.
pub fn as_expr(n: &Node, what: &str) -> Result<Src, KsyError> {
    let text = match &n.value {
        Value::Scalar { text, quoted: false } => text.clone(),
        // A quoted YAML value is still an expression in Kaitai; the quotes
        // are YAML's.
        Value::Scalar { text, quoted: true } => text.clone(),
        _ => return Err(node_err(n, format!("{what} must be an expression"))),
    };
    let quoted = matches!(n.value, Value::Scalar { quoted: true, .. });
    let expr = expr::parse(&text).map_err(|e| err(n.line, n.col + e.at + usize::from(quoted), e.message))?;
    Ok(Src { expr, line: n.line, col: n.col })
}

/// The keys of `entries` must be among `known` (or start with `-`).
fn known_keys(entries: &[(Key, Node)], known: &[&str], what: &str) -> Result<(), KsyError> {
    for (k, _) in entries {
        if !k.text.starts_with('-') && !known.contains(&k.text.as_str()) {
            return Err(key_err(k, format!("`{}` isn't read in {what} (yet)", k.text)));
        }
    }
    Ok(())
}

fn get<'n>(entries: &'n [(Key, Node)], key: &str) -> Option<&'n Node> {
    entries.iter().find(|(k, _)| k.text == key).map(|(_, v)| v)
}

const META_KEYS: [&str; 12] = [
    "id",
    "title",
    "doc",
    "ks-version",
    "license",
    "application",
    "file-extension",
    "xref",
    "endian",
    "bit-endian",
    "encoding",
    "tags",
];

fn read_types(root: &Node) -> Result<Vec<TypeSpec>, KsyError> {
    let mut types = Vec::new();
    read_type(root, "root".into(), None, &mut types, 0)?;
    Ok(types)
}

fn read_type(
    n: &Node,
    name: String,
    parent: Option<usize>,
    types: &mut Vec<TypeSpec>,
    depth: usize,
) -> Result<usize, KsyError> {
    if depth > 32 || types.len() > 256 {
        return Err(node_err(n, "too many nested types"));
    }
    let entries = as_map(n, "a type")?;
    let top = parent.is_none();
    let known: &[&str] = &["meta", "seq", "instances", "types", "doc", "doc-ref"];
    for (k, _) in entries {
        if matches!(k.text.as_str(), "enums") {
            return Err(key_err(k, "enums aren't read yet"));
        }
        if matches!(k.text.as_str(), "params") {
            return Err(key_err(k, "params aren't read yet"));
        }
    }
    known_keys(entries, known, if top { "a definition" } else { "a type" })?;
    let id = types.len();
    types.push(TypeSpec { name, parent, children: Vec::new(), endian: None, attrs: Vec::new(), insts: Vec::new() });
    if let Some(meta) = get(entries, "meta") {
        let m = as_map(meta, "meta")?;
        known_keys(m, &META_KEYS, "meta")?;
        if let Some((k, _)) = m.iter().find(|(k, _)| k.text == "imports") {
            return Err(key_err(k, "imports aren't read yet"));
        }
        if let Some(e) = get(m, "endian") {
            types[id].endian = Some(read_endian(e)?);
        }
        if let Some(b) = get(m, "bit-endian")
            && as_text(b, "bit-endian")? != "be"
        {
            return Err(node_err(b, "only `bit-endian: be` is read yet"));
        }
    }
    if let Some(seq) = get(entries, "seq") {
        let mut attrs = Vec::new();
        for a in as_seq(seq, "seq")? {
            let attr = read_attr(a, false)?;
            if attrs.iter().any(|x: &AttrSpec| x.id == attr.id) {
                return Err(err(attr.line, attr.col, format!("`{}` is given twice", attr.id)));
            }
            attrs.push(attr);
        }
        types[id].attrs = attrs;
    }
    if let Some(insts) = get(entries, "instances") {
        let mut out: Vec<InstSpec> = Vec::new();
        for (k, v) in as_map(insts, "instances")? {
            if types[id].attrs.iter().any(|a| a.id == k.text) {
                return Err(key_err(k, format!("`{}` is an attribute already", k.text)));
            }
            out.push(read_inst(k, v)?);
        }
        types[id].insts = out;
    }
    if let Some(ts) = get(entries, "types") {
        for (k, v) in as_map(ts, "types")? {
            let child = read_type(v, k.text.clone(), Some(id), types, depth + 1)?;
            types[id].children.push((k.text.clone(), child));
        }
    }
    Ok(id)
}

fn endian_name(n: &Node) -> Result<Endian, KsyError> {
    match as_text(n, "endian")? {
        "be" => Ok(Endian::Be),
        "le" => Ok(Endian::Le),
        other => Err(node_err(n, format!("endian is `be` or `le`, not `{other}`"))),
    }
}

fn read_endian(n: &Node) -> Result<EndianSpec, KsyError> {
    if let Value::Scalar { .. } = n.value {
        return Ok(EndianSpec::Fixed(endian_name(n)?));
    }
    let m = as_map(n, "endian")?;
    known_keys(m, &["switch-on", "cases"], "endian")?;
    let on = as_expr(get(m, "switch-on").ok_or_else(|| node_err(n, "endian needs `switch-on`"))?, "switch-on")?;
    let mut cases = Vec::new();
    let mut default = None;
    for (k, v) in as_map(get(m, "cases").ok_or_else(|| node_err(n, "endian needs `cases`"))?, "cases")? {
        let e = endian_name(v)?;
        if k.text == "_" {
            default = Some(e);
        } else {
            let key = Node { value: Value::Scalar { text: k.text.clone(), quoted: false }, line: k.line, col: k.col };
            cases.push((key, e));
        }
    }
    Ok(EndianSpec::Switch { on, cases, default })
}

const ATTR_KEYS: [&str; 14] = [
    "id",
    "type",
    "size",
    "size-eos",
    "if",
    "contents",
    "valid",
    "terminator",
    "encoding",
    "doc",
    "doc-ref",
    "pad-right",
    "include",
    "consume",
];

fn read_attr(n: &Node, instance: bool) -> Result<AttrSpec, KsyError> {
    let m = as_map(n, "an attribute")?;
    for (k, _) in m {
        let not_yet = match k.text.as_str() {
            "repeat" | "repeat-expr" | "repeat-until" => Some("repeat"),
            "process" => Some("process"),
            "enum" => Some("enum"),
            "eos-error" => Some("eos-error"),
            _ => None,
        };
        if let Some(what) = not_yet {
            return Err(key_err(k, format!("{what} isn't read yet")));
        }
    }
    let mut keys: Vec<&str> = ATTR_KEYS.to_vec();
    if instance {
        keys.retain(|k| *k != "id");
        keys.extend(["pos", "value"]);
    }
    known_keys(m, &keys, "an attribute")?;
    for (k, v) in m {
        if matches!(k.text.as_str(), "pad-right" | "include" | "consume") {
            return Err(node_err(v, format!("{} isn't read yet", k.text)));
        }
    }
    let id = if instance {
        String::new()
    } else {
        let idn = get(m, "id").ok_or_else(|| node_err(n, "an attribute needs an `id`"))?;
        let id = as_text(idn, "id")?;
        if !is_name(id) {
            return Err(node_err(idn, format!("`{id}` isn't a name (lower case, digits, `_`)")));
        }
        id.to_owned()
    };
    let ty = match get(m, "type") {
        None => TypeRef::Bytes,
        Some(t) => type_ref(t)?,
    };
    let size = get(m, "size").map(|s| as_expr(s, "size")).transpose()?;
    let size_eos = match get(m, "size-eos") {
        Some(v) => match as_text(v, "size-eos")? {
            "true" => true,
            "false" => false,
            _ => return Err(node_err(v, "size-eos is `true` or `false`")),
        },
        None => false,
    };
    if size.is_some() && size_eos {
        return Err(node_err(n, "give `size` or `size-eos`, not both"));
    }
    let cond = get(m, "if").map(|c| as_expr(c, "if")).transpose()?;
    let contents = get(m, "contents").map(read_contents).transpose()?;
    let terminator = match get(m, "terminator") {
        Some(t) => Some(byte_value(t)?),
        None => None,
    };
    let mut valid = Vec::new();
    if let Some(v) = get(m, "valid") {
        match &v.value {
            Value::Map(entries) => {
                known_keys(entries, &["eq", "min", "max", "any-of", "expr"], "valid")?;
                for (k, e) in entries {
                    let list = if k.text == "any-of" {
                        as_seq(e, "any-of")?.iter().map(|x| as_expr(x, "any-of")).collect::<Result<_, _>>()?
                    } else {
                        vec![as_expr(e, &k.text)?]
                    };
                    valid.push((k.text.clone(), list));
                }
            }
            _ => valid.push(("eq".to_owned(), vec![as_expr(v, "valid")?])),
        }
    }
    let (line, col) = (n.line, n.col);
    if contents.is_some() && !matches!(ty, TypeRef::Bytes) {
        return Err(node_err(n, "`contents` takes no `type`"));
    }
    match &ty {
        TypeRef::Int { .. } | TypeRef::Bits(_) if size.is_some() || size_eos || terminator.is_some() => {
            return Err(node_err(n, "a number takes no `size`, `size-eos` or `terminator`"));
        }
        TypeRef::Str if size.is_none() && !size_eos && terminator.is_none() => {
            return Err(node_err(n, "a `str` needs `size`, `size-eos` or `terminator`"));
        }
        TypeRef::Bytes if contents.is_none() && size.is_none() && !size_eos && terminator.is_none() => {
            return Err(node_err(n, "bytes need `size`, `size-eos`, `terminator` or `contents`"));
        }
        _ => {}
    }
    Ok(AttrSpec { id, line, col, ty, size, size_eos, cond, valid, contents, terminator })
}

fn read_inst(k: &Key, v: &Node) -> Result<InstSpec, KsyError> {
    if !is_name(&k.text) {
        return Err(key_err(k, format!("`{}` isn't a name (lower case, digits, `_`)", k.text)));
    }
    let m = as_map(v, "an instance")?;
    let cond = get(m, "if").map(|c| as_expr(c, "if")).transpose()?;
    if let Some(value) = get(m, "value") {
        known_keys(m, &["value", "if", "doc", "doc-ref"], "a value instance")?;
        return Ok(InstSpec {
            id: k.text.clone(),
            line: k.line,
            col: k.col,
            cond,
            value: Some(as_expr(value, "value")?),
            pos: None,
            read: None,
        });
    }
    let pos = get(m, "pos").ok_or_else(|| key_err(k, "an instance needs `value` or `pos`"))?;
    let pos = as_expr(pos, "pos")?;
    let mut read = read_attr(v, true)?;
    read.cond = None;
    Ok(InstSpec { id: k.text.clone(), line: k.line, col: k.col, cond, value: None, pos: Some(pos), read: Some(read) })
}

fn is_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase() || f == '_')
        && s.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn type_ref(n: &Node) -> Result<TypeRef, KsyError> {
    let t = as_text(n, "type")?;
    if let Value::Map(_) = n.value {
        return Err(node_err(n, "switch-on types aren't read yet"));
    }
    let suffix = |rest: &str| -> Option<Option<Endian>> {
        match rest {
            "" => Some(None),
            "be" => Some(Some(Endian::Be)),
            "le" => Some(Some(Endian::Le)),
            _ => None,
        }
    };
    let b = t.as_bytes();
    if (b.first() == Some(&b'u') || b.first() == Some(&b's'))
        && let Some(&d) = b.get(1)
        && matches!(d, b'1' | b'2' | b'4' | b'8')
        && let Some(endian) = suffix(&t[2..])
    {
        return Ok(TypeRef::Int { bytes: d - b'0', signed: b[0] == b's', endian });
    }
    if let Some(rest) = t.strip_prefix('b') {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            let tail = &rest[digits.len()..];
            let n_bits: u32 = digits.parse().unwrap_or(0);
            return match tail {
                "" | "be" if (1..=64).contains(&n_bits) => Ok(TypeRef::Bits(n_bits as u8)),
                "le" => Err(node_err(n, "little-endian bit fields aren't read yet")),
                _ => Err(node_err(n, format!("`{t}` isn't a type"))),
            };
        }
    }
    match t {
        "str" => Ok(TypeRef::Str),
        "strz" => Ok(TypeRef::Strz),
        "f4" | "f8" | "f4be" | "f4le" | "f8be" | "f8le" => Err(node_err(n, "floats aren't read yet")),
        _ if t.contains('(') => Err(node_err(n, "types with parameters aren't read yet")),
        _ if t.contains("::") => Err(node_err(n, "types of other files aren't read yet")),
        _ if is_name(t) => Ok(TypeRef::User(t.to_owned())),
        _ => Err(node_err(n, format!("`{t}` isn't a type"))),
    }
}

fn byte_value(n: &Node) -> Result<u8, KsyError> {
    let t = as_text(n, "a byte")?;
    parse_int(t).and_then(|v| u8::try_from(v).ok()).ok_or_else(|| node_err(n, format!("`{t}` isn't a byte (0..255)")))
}

pub fn parse_int(t: &str) -> Option<i64> {
    match expr::parse(t).ok()? {
        Expr::Int(v) => Some(v),
        _ => None,
    }
}

fn read_contents(n: &Node) -> Result<Vec<u8>, KsyError> {
    let mut out = Vec::new();
    let mut add = |item: &Node| -> Result<(), KsyError> {
        match &item.value {
            Value::Scalar { text, quoted: false } if parse_int(text).is_some() => out.push(byte_value(item)?),
            Value::Scalar { text, .. } => out.extend_from_slice(text.as_bytes()),
            _ => return Err(node_err(item, "contents are bytes or strings")),
        }
        Ok(())
    };
    match &n.value {
        Value::Seq(items) => items.iter().try_for_each(&mut add)?,
        _ => add(n)?,
    }
    if out.is_empty() {
        return Err(node_err(n, "contents are empty"));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Nodes and binding.

struct Builder<'t> {
    types: &'t [TypeSpec],
    nodes: Vec<NodeDef>,
    /// Per node, its type.
    node_types: Vec<usize>,
}

impl<'t> Builder<'t> {
    /// The type `name` as seen from type `from` (its own types, then its
    /// parents').
    fn lookup(&self, from: usize, name: &str) -> Option<usize> {
        let mut t = Some(from);
        while let Some(i) = t {
            if let Some((_, c)) = self.types[i].children.iter().find(|(n, _)| n == name) {
                return Some(*c);
            }
            t = self.types[i].parent;
        }
        None
    }

    /// Make the node of type `ty`, and its children.
    fn make(&mut self, ty: usize, place: Place, parent: Option<NodeId>, sized: bool) -> Result<NodeId, KsyError> {
        if self.nodes.len() >= MAX_NODES {
            return Err(err(1, 1, format!("more than {MAX_NODES} nodes (types used inside types)")));
        }
        // A type inside itself would never end.
        let mut up = parent;
        while let Some(p) = up {
            if self.node_types[p] == ty {
                let t = &self.types[ty];
                return Err(err(1, 1, format!("type `{}` contains itself; recursive types aren't read yet", t.name)));
            }
            up = self.nodes[p].parent;
        }
        let id = self.nodes.len();
        let types = self.types;
        let spec = &types[ty];
        self.nodes.push(NodeDef {
            type_name: spec.name.clone(),
            place,
            parent,
            attrs: Vec::new(),
            insts: Vec::new(),
            endian: None,
            sized,
            start: Memo::new(),
        });
        self.node_types.push(ty);
        // Children first (their numbers), expressions later.
        let mut attrs = Vec::new();
        for (i, a) in spec.attrs.iter().enumerate() {
            let kind = self.kind(ty, a, Place::Attr(id, i), id)?;
            attrs.push(Attr {
                id: a.id.clone(),
                kind,
                size: Size::Fixed(0),
                cond: None,
                checks: Vec::new(),
                contents: a.contents.as_deref().map(intern),
                terminator: terminator(a),
                pos: Memo::new(),
                there: Memo::new(),
            });
        }
        let mut insts = Vec::new();
        for (k, s) in spec.insts.iter().enumerate() {
            let kind = match &s.read {
                Some(r) => {
                    let kind = self.kind(ty, r, Place::Inst(id, k), id)?;
                    InstKind::Read {
                        pos: CExpr::Lit(Lit::Int(0)),
                        read: Box::new(Attr {
                            id: s.id.clone(),
                            kind,
                            size: Size::Fixed(0),
                            cond: None,
                            checks: Vec::new(),
                            contents: r.contents.as_deref().map(intern),
                            terminator: terminator(r),
                            pos: Memo::new(),
                            there: Memo::new(),
                        }),
                    }
                }
                None => InstKind::Value(CExpr::Lit(Lit::Int(0))),
            };
            insts.push(Inst { id: s.id.clone(), cond: None, kind, small: None, memo: Memo::new() });
        }
        self.nodes[id].attrs = attrs;
        self.nodes[id].insts = insts;
        Ok(id)
    }

    fn kind(&mut self, ty: usize, a: &AttrSpec, place: Place, node: NodeId) -> Result<Kind, KsyError> {
        Ok(match &a.ty {
            TypeRef::Int { bytes, signed, endian } => Kind::Int { bytes: *bytes, signed: *signed, endian: *endian },
            TypeRef::Bits(n) => Kind::Bits(*n),
            TypeRef::Str | TypeRef::Strz => Kind::Str,
            TypeRef::Bytes => Kind::Bytes,
            TypeRef::User(name) => {
                let t = self
                    .lookup(ty, name)
                    .ok_or_else(|| err(a.line, a.col, format!("there is no type `{name}` here")))?;
                let sized = a.size.is_some() || a.size_eos;
                Kind::User(self.make(t, place, Some(node), sized)?)
            }
        })
    }

    /// Bind every expression of every node.
    fn bind_all(&mut self) -> Result<(), KsyError> {
        let types = self.types;
        for id in 0..self.nodes.len() {
            let spec = &types[self.node_types[id]];
            let endian = match &spec.endian {
                None => None,
                Some(EndianSpec::Fixed(e)) => Some(EndianRule::Fixed(*e)),
                Some(EndianSpec::Switch { on, cases, default }) => {
                    let on = self.bind(on, id, false)?.0;
                    let mut out = Vec::new();
                    for (k, e) in cases {
                        out.push((self.lit_key(k)?, *e));
                    }
                    Some(EndianRule::Switch { on, cases: out, default: *default })
                }
            };
            self.nodes[id].endian = endian;
            for (i, a) in spec.attrs.iter().enumerate() {
                let (size, cond, checks) = self.bind_attr(a, id)?;
                let attr = &mut self.nodes[id].attrs[i];
                attr.size = size;
                attr.cond = cond;
                attr.checks = checks;
            }
            for (k, s) in spec.insts.iter().enumerate() {
                let cond = s.cond.as_ref().map(|c| self.bind(c, id, false)).transpose()?.map(|b| b.0);
                let kind = match (&s.value, &s.pos, &s.read) {
                    (Some(v), _, _) => InstKind::Value(self.bind(v, id, false)?.0),
                    (None, Some(pos), Some(r)) => {
                        let pos = self.bind(pos, id, false)?.0;
                        let (size, _, checks) = self.bind_attr(r, id)?;
                        let made = std::mem::replace(
                            &mut self.nodes[id].insts[k].kind,
                            InstKind::Value(CExpr::Lit(Lit::Int(0))),
                        );
                        let InstKind::Read { mut read, .. } = made else {
                            return Err(err(s.line, s.col, "an instance needs `value` or `pos`"));
                        };
                        read.size = size;
                        read.checks = checks;
                        if let Kind::User(c) = read.kind {
                            self.nodes[c].sized = !matches!(read.size, Size::Child);
                        }
                        InstKind::Read { pos, read }
                    }
                    _ => return Err(err(s.line, s.col, "an instance needs `value` or `pos`")),
                };
                let small = match (&kind, &s.value) {
                    (InstKind::Value(_), Some(v)) => match self.bind(v, id, false)?.1 {
                        t @ (Ty::Int | Ty::Bool) => Some(t),
                        _ => None,
                    },
                    _ => None,
                };
                let inst = &mut self.nodes[id].insts[k];
                inst.cond = cond;
                inst.kind = kind;
                inst.small = small;
            }
        }
        self.check_endians()
    }

    fn bind_attr(&self, a: &AttrSpec, id: NodeId) -> Result<(Size, Option<CExpr>, Vec<Check>), KsyError> {
        let size = match (&a.ty, &a.size, a.size_eos) {
            (TypeRef::Int { bytes, .. }, _, _) => Size::Fixed(u64::from(*bytes)),
            (TypeRef::Bits(n), _, _) => Size::Bits(*n),
            (_, Some(s), _) => match self.bind(s, id, false)?.0 {
                CExpr::Lit(Lit::Int(v)) if v >= 0 => Size::Fixed(v as u64),
                CExpr::Lit(_) => return Err(err(s.line, s.col, "a size must be a whole number, 0 or more")),
                e => Size::Expr(e),
            },
            (_, None, true) => Size::Eos,
            (TypeRef::Strz, None, false) => Size::Terminated(a.terminator.unwrap_or(0)),
            (TypeRef::Str | TypeRef::Bytes, None, false) if a.contents.is_some() => {
                Size::Fixed(a.contents.as_ref().map_or(0, |c| c.len() as u64))
            }
            (TypeRef::Str | TypeRef::Bytes, None, false) => Size::Terminated(a.terminator.unwrap_or(0)),
            (TypeRef::User(_), None, false) => Size::Child,
        };
        let cond = a.cond.as_ref().map(|c| self.bind(c, id, false)).transpose()?.map(|b| b.0);
        let mut checks = Vec::new();
        for (key, list) in &a.valid {
            let bound: Vec<CExpr> =
                list.iter().map(|e| self.bind(e, id, true).map(|b| b.0)).collect::<Result<_, _>>()?;
            checks.push(match key.as_str() {
                "eq" => Check::Eq(bound.into_iter().next().unwrap_or(CExpr::Lit(Lit::Bool(true)))),
                "min" => Check::Min(bound.into_iter().next().unwrap_or(CExpr::Lit(Lit::Int(0)))),
                "max" => Check::Max(bound.into_iter().next().unwrap_or(CExpr::Lit(Lit::Int(0)))),
                "any-of" => Check::AnyOf(bound),
                _ => Check::Expr(bound.into_iter().next().unwrap_or(CExpr::Lit(Lit::Bool(true)))),
            });
        }
        Ok((size, cond, checks))
    }

    /// A case key of `switch-on`: a number, `true` / `false`, or a string.
    fn lit_key(&self, k: &Node) -> Result<Lit, KsyError> {
        let src = as_expr(k, "a case")?;
        match self.bind(&src, 0, false)?.0 {
            CExpr::Lit(l) => Ok(l),
            _ => Err(node_err(k, "a case must be a constant")),
        }
    }

    /// Every multi-byte number without `be` / `le` has an endianness from
    /// its node or a node above it.
    fn check_endians(&self) -> Result<(), KsyError> {
        for (id, n) in self.nodes.iter().enumerate() {
            let reads = n.attrs.iter().chain(n.insts.iter().filter_map(|i| match &i.kind {
                InstKind::Read { read, .. } => Some(&**read),
                InstKind::Value(_) => None,
            }));
            for a in reads {
                if let Kind::Int { bytes, endian: None, .. } = a.kind
                    && bytes > 1
                    && !self.has_endian(id)
                {
                    let spec = &self.types[self.node_types[id]];
                    let at = spec.attrs.iter().find(|s| s.id == a.id).map_or((1, 1), |s| (s.line, s.col));
                    return Err(err(
                        at.0,
                        at.1,
                        format!("`{}` needs an endianness: `meta: endian`, or a type like u2be", a.id),
                    ));
                }
            }
        }
        Ok(())
    }

    fn has_endian(&self, mut id: NodeId) -> bool {
        loop {
            if self.nodes[id].endian.is_some() {
                return true;
            }
            match self.nodes[id].parent {
                Some(p) => id = p,
                None => return false,
            }
        }
    }

    /// Bind `src` in node `at`. `underscore`: `_` is allowed (`valid`).
    pub fn bind(&self, src: &Src, at: NodeId, underscore: bool) -> Result<(CExpr, Ty), KsyError> {
        let scope = Scope { b: self, underscore, origin: false };
        scope.bind(&src.expr, at).map_err(|m| err(src.line, src.col, m))
    }
}

/// What a name is while binding.
#[derive(Debug, Clone)]
enum Bound {
    Value(CExpr, Ty),
    Obj(NodeId),
    Io(NodeId),
    Origin,
}

pub struct Scope<'b, 't> {
    b: &'b Builder<'t>,
    underscore: bool,
    /// `origin` may be used (`-nerka:`).
    origin: bool,
}

impl Scope<'_, '_> {
    fn bind(&self, e: &Expr, at: NodeId) -> Result<(CExpr, Ty), String> {
        match self.bound(e, at, 0)? {
            Bound::Value(c, t) => Ok((fold(c), t)),
            Bound::Obj(n) => {
                // A user type as a value: its bytes (Kaitai's `_raw_…`).
                match self.b.nodes[n].place {
                    Place::Attr(p, i) => Ok((CExpr::Attr(p, i), Ty::Bytes)),
                    Place::Inst(p, k) => Ok((CExpr::Inst(p, k), Ty::Bytes)),
                    Place::Root => Err("`_root` isn't a value".into()),
                }
            }
            Bound::Io(_) => Err("`_io` isn't a value; `_io.size` is".into()),
            Bound::Origin => Err("`origin` isn't a value; `origin.channel` is".into()),
        }
    }

    fn value(&self, e: &Expr, at: NodeId, depth: usize) -> Result<(CExpr, Ty), String> {
        match self.bound(e, at, depth)? {
            Bound::Value(c, t) => Ok((c, t)),
            Bound::Obj(n) => match self.b.nodes[n].place {
                Place::Attr(p, i) => Ok((CExpr::Attr(p, i), Ty::Bytes)),
                Place::Inst(p, k) => Ok((CExpr::Inst(p, k), Ty::Bytes)),
                Place::Root => Err("`_root` isn't a value".into()),
            },
            Bound::Io(_) => Err("`_io` isn't a value; `_io.size` is".into()),
            Bound::Origin => Err("`origin` isn't a value; `origin.channel` is".into()),
        }
    }

    fn member(&self, n: NodeId, name: &str) -> Result<Bound, String> {
        let node = &self.b.nodes[n];
        match name {
            "_root" => return Ok(Bound::Obj(0)),
            "_parent" => return node.parent.map(Bound::Obj).ok_or_else(|| "the root has no `_parent`".to_owned()),
            "_io" => return Ok(Bound::Io(n)),
            _ => {}
        }
        if let Some(i) = node.attrs.iter().position(|a| a.id == name) {
            return Ok(match &node.attrs[i].kind {
                Kind::User(c) => Bound::Obj(*c),
                k => Bound::Value(CExpr::Attr(n, i), kind_ty(k)),
            });
        }
        if let Some(k) = node.insts.iter().position(|a| a.id == name) {
            return Ok(match &node.insts[k].kind {
                InstKind::Read { read, .. } => match read.kind {
                    Kind::User(c) => Bound::Obj(c),
                    _ => Bound::Value(CExpr::Inst(n, k), kind_ty(&read.kind)),
                },
                // Its type is known once it is bound; until then, any.
                InstKind::Value(_) => Bound::Value(CExpr::Inst(n, k), self.inst_ty(n, k)),
            });
        }
        Err(format!("`{name}` isn't an attribute or instance of `{}`", node.type_name))
    }

    /// The type of value instance `k` of node `n`, from its expression.
    fn inst_ty(&self, n: NodeId, k: usize) -> Ty {
        let spec = &self.b.types[self.b.node_types[n]].insts[k];
        match &spec.value {
            Some(v) => {
                // Bound again without its type; cycles end at the depth limit.
                thread_local!(static DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
                let d = DEPTH.with(|d| d.get());
                if d > 16 {
                    return Ty::Any;
                }
                DEPTH.with(|x| x.set(d + 1));
                let t = self.value(&v.expr, n, 0).map_or(Ty::Any, |b| b.1);
                DEPTH.with(|x| x.set(d));
                t
            }
            None => Ty::Any,
        }
    }

    fn bound(&self, e: &Expr, at: NodeId, depth: usize) -> Result<Bound, String> {
        if depth > 128 {
            return Err("nested too deeply".into());
        }
        let d = depth + 1;
        Ok(match e {
            Expr::Int(v) => Bound::Value(CExpr::Lit(Lit::Int(*v)), Ty::Int),
            Expr::Float(v) => Bound::Value(CExpr::Lit(Lit::Float(*v)), Ty::Float),
            Expr::Bool(v) => Bound::Value(CExpr::Lit(Lit::Bool(*v)), Ty::Bool),
            Expr::Str(s) => Bound::Value(CExpr::Lit(Lit::Bytes(intern(s))), Ty::Bytes),
            Expr::Name(n) if n == "_" => {
                if !self.underscore {
                    return Err("`_` is only for `valid`".into());
                }
                Bound::Value(CExpr::Underscore, Ty::Any)
            }
            Expr::Name(n) if n == "origin" && self.origin => Bound::Origin,
            Expr::Name(n) => self.member(at, n)?,
            Expr::Attr(base, name) => match self.bound(base, at, d)? {
                Bound::Obj(n) => self.member(n, name)?,
                Bound::Io(n) => match name.as_str() {
                    "size" => Bound::Value(CExpr::IoSize(n), Ty::Int),
                    _ => return Err(format!("`_io.{name}` isn't read yet (only `_io.size`)")),
                },
                Bound::Origin => Bound::Value(
                    CExpr::Origin(match name.as_str() {
                        "channel" => OriginField::Channel,
                        "secs" => OriginField::Secs,
                        "usecs" => OriginField::Usecs,
                        _ => return Err(format!("`origin.{name}` isn't known (channel, secs, usecs)")),
                    }),
                    if name == "channel" { Ty::Bytes } else { Ty::Int },
                ),
                Bound::Value(c, t) => match name.as_str() {
                    "length" | "size" if matches!(t, Ty::Bytes | Ty::Any) => {
                        Bound::Value(CExpr::Length(Box::new(c)), Ty::Int)
                    }
                    _ => return Err(format!("`.{name}` isn't read yet on a value")),
                },
            },
            Expr::Index(a, i) => {
                let (a, _) = self.value(a, at, d)?;
                let (i, _) = self.value(i, at, d)?;
                Bound::Value(CExpr::Index(Box::new(a), Box::new(i)), Ty::Int)
            }
            Expr::Unary(op, a) => {
                let (a, t) = self.value(a, at, d)?;
                let ty = match op {
                    UnOp::Not => Ty::Bool,
                    UnOp::BitNot => Ty::Int,
                    UnOp::Neg => t,
                };
                Bound::Value(CExpr::Unary(*op, Box::new(a)), ty)
            }
            Expr::Binary(op, a, b) => {
                let (a, ta) = self.value(a, at, d)?;
                let (b, tb) = self.value(b, at, d)?;
                let ty = match op {
                    BinOp::Or | BinOp::And | BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        Ty::Bool
                    }
                    BinOp::Add if ta == Ty::Bytes || tb == Ty::Bytes => Ty::Bytes,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                        if ta == Ty::Float || tb == Ty::Float =>
                    {
                        Ty::Float
                    }
                    _ if ta == Ty::Any || tb == Ty::Any => Ty::Any,
                    _ => Ty::Int,
                };
                Bound::Value(CExpr::Binary(*op, Box::new(a), Box::new(b)), ty)
            }
            Expr::Cond(c, a, b) => {
                let (c, _) = self.value(c, at, d)?;
                let (a, ta) = self.value(a, at, d)?;
                let (b, tb) = self.value(b, at, d)?;
                Bound::Value(CExpr::Cond(Box::new(c), Box::new(a), Box::new(b)), if ta == tb { ta } else { Ty::Any })
            }
        })
    }
}

/// The byte a sized string or byte run is cut at.
fn terminator(a: &AttrSpec) -> Option<u8> {
    match a.ty {
        TypeRef::Strz => Some(a.terminator.unwrap_or(0)),
        _ => a.terminator,
    }
}

fn kind_ty(k: &Kind) -> Ty {
    match k {
        Kind::Int { .. } => Ty::Int,
        Kind::Bits(1) => Ty::Bool,
        Kind::Bits(_) => Ty::Int,
        Kind::Bytes | Kind::Str | Kind::User(_) => Ty::Bytes,
    }
}

/// Constant arithmetic done once (`len - 4` stays, `16 - 4` doesn't).
fn fold(e: CExpr) -> CExpr {
    match e {
        CExpr::Binary(op, a, b) => {
            let (a, b) = (fold(*a), fold(*b));
            if let (CExpr::Lit(Lit::Int(x)), CExpr::Lit(Lit::Int(y))) = (&a, &b)
                && let Some(v) = super::run::int_op(op, *x, *y)
            {
                return CExpr::Lit(v);
            }
            CExpr::Binary(op, Box::new(a), Box::new(b))
        }
        CExpr::Unary(UnOp::BitNot, a) => match fold(*a) {
            CExpr::Lit(Lit::Int(v)) => CExpr::Lit(Lit::Int(!v)),
            a => CExpr::Unary(UnOp::BitNot, Box::new(a)),
        },
        e => e,
    }
}

/// The definition's structure, from its YAML.
pub fn build(root: &Node) -> Result<(Def, Binder), KsyError> {
    let types = read_types(root)?;
    let mut b = Builder { types: &types, nodes: Vec::new(), node_types: Vec::new() };
    b.make(0, Place::Root, None, false)?;
    b.bind_all()?;
    let Builder { nodes, node_types, .. } = b;
    Ok((Def { nodes }, Binder { types, node_types }))
}

/// Binds the `-nerka:` section's expressions after [`build`].
pub struct Binder {
    types: Vec<TypeSpec>,
    node_types: Vec<usize>,
}

impl Binder {
    /// Bind `src` at the root, where `origin` is known too.
    pub fn bind_root(&self, def: &mut Def, src: &Src) -> Result<(CExpr, Ty), KsyError> {
        let nodes = std::mem::take(&mut def.nodes);
        let b = Builder { types: &self.types, nodes, node_types: self.node_types.clone() };
        let scope = Scope { b: &b, underscore: false, origin: true };
        let r = scope.bind(&src.expr, 0).map_err(|m| err(src.line, src.col, m));
        def.nodes = b.nodes;
        r
    }
}

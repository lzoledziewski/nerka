//! Protocols described in a file: a Kaitai Struct (`.ksy`) structure and a
//! `-nerka:` section saying what Nerka shows of it. Design, the language
//! subset and the section: `doc/protocol-definitions.md`.
//!
//! [`KsyProtocol`] is a [`Protocol`] like the native ones: captures route
//! UDP ports to it ([`super::LoadedProtocol`]), and its fields, level and
//! columns come from the definition, read in place (`run`).

pub mod expr;
mod model;
mod plan;
mod run;
pub mod template;
pub mod yaml;

use super::{NO_CHANNEL, Origin, Protocol};
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue, Level};
use crate::input::ColumnDef;
use crate::input::capture::route::Match;
use model::{CExpr, Def, Lit, Src, Ty, as_expr, as_map, as_seq, as_text, err, intern, parse_int};
use run::{Eval, Val};
use std::borrow::Cow;
use std::fmt;
use std::path::Path;
use template::Fmt;
use yaml::{Node, Value};

/// What is wrong with a definition, and where (1-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KsyError {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl fmt::Display for KsyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}: {}", self.line, self.col, self.message)
    }
}

impl From<yaml::YamlError> for KsyError {
    fn from(e: yaml::YamlError) -> Self {
        KsyError { line: e.line, col: e.col, message: e.message }
    }
}

/// A protocol read from a definition.
pub struct KsyProtocol {
    name: String,
    def: Def,
    framing: Framing,
    schema: Vec<FieldDef>,
    fields: Vec<Spec>,
    level: Option<Spec>,
    /// The names `level` maps to, read as levels when the file loads.
    level_names: Vec<(&'static [u8], Level)>,
    uptime: Option<Spec>,
    columns: Vec<ColumnDef>,
    cells: Vec<Cell>,
    /// Which UDP datagrams are its messages, by default (`match:`).
    routes: Vec<Match>,
}

enum Framing {
    /// One message per datagram, without a length.
    Datagram,
    /// Messages back to back, each this many bytes long.
    Length(CExpr),
}

/// A value of the `-nerka:` section.
struct Spec {
    from: CExpr,
    cond: Option<CExpr>,
    map: Option<Vec<(Lit, &'static [u8])>>,
    otherwise: Option<Box<Spec>>,
    decimals: Option<usize>,
    hex: bool,
    ty: Ty,
}

enum Cell {
    Field(FieldId),
    Own(Spec),
}

/// Fields NFD has of its own, or that a source adds.
const RESERVED: [&str; 6] = ["raw", "level", "bookmark", "proto", "file", "nfd"];
const NATIVE: [&str; 3] = ["dlt", "someip", "syslog"];

impl KsyProtocol {
    /// Read a definition from its text.
    pub fn parse(text: &str) -> Result<KsyProtocol, KsyError> {
        let root = yaml::parse(text)?;
        let entries = as_map(&root, "a definition")?;
        // The structure first: its errors say more than a missing section.
        let (mut def, binder) = model::build(&root)?;
        let nerka = entries
            .iter()
            .find(|(k, _)| k.text == "-nerka")
            .map(|(_, v)| v)
            .ok_or_else(|| err(root.line.max(1), root.col.max(1), "a definition needs a `-nerka:` section"))?;
        let mut bind = |src: &Src| binder.bind_root(&mut def, src);
        let n = as_map(nerka, "-nerka")?;
        for (k, _) in n {
            if !["name", "framing", "fields", "level", "uptime", "columns", "match", "doc"].contains(&k.text.as_str()) {
                return Err(err(k.line, k.col, format!("`{}` isn't read in -nerka", k.text)));
            }
        }
        let get = |key: &str| n.iter().find(|(k, _)| k.text == key).map(|(_, v)| v);

        let name_node = get("name").ok_or_else(|| err(nerka.line, nerka.col, "-nerka needs a `name`"))?;
        let name = as_text(name_node, "name")?.to_owned();
        if name.is_empty()
            || name.len() > 16
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(err(name_node.line, name_node.col, "a name is 1 to 16 letters, digits, `_` or `-`"));
        }
        if NATIVE.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(err(name_node.line, name_node.col, format!("`{name}` is a native protocol's name")));
        }

        let framing = match get("framing") {
            None => return Err(err(nerka.line, nerka.col, "-nerka needs `framing`: `datagram` or `{ length: … }`")),
            Some(f) => match &f.value {
                Value::Scalar { text, .. } if text == "datagram" => Framing::Datagram,
                Value::Map(m) => {
                    let len = m
                        .iter()
                        .find(|(k, _)| k.text == "length")
                        .ok_or_else(|| err(f.line, f.col, "framing needs `length`"))?;
                    if let Some((k, _)) = m.iter().find(|(k, _)| k.text != "length") {
                        return Err(err(k.line, k.col, format!("`{}` isn't read in framing", k.text)));
                    }
                    Framing::Length(bind(&as_expr(&len.1, "length")?)?.0)
                }
                _ => return Err(err(f.line, f.col, "framing is `datagram` or `{ length: … }`")),
            },
        };

        let mut schema = Vec::new();
        let mut fields = Vec::new();
        if let Some(fs) = get("fields") {
            for (k, v) in as_map(fs, "fields")? {
                let name = &k.text;
                let ok = name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                    && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.');
                if !ok {
                    return Err(err(
                        k.line,
                        k.col,
                        format!("`{name}` isn't a field name (lower case, digits, `_`, `.`)"),
                    ));
                }
                if RESERVED.contains(&name.as_str()) {
                    return Err(err(k.line, k.col, format!("`{name}` is a name NFD has already")));
                }
                if name.starts_with("net.") {
                    return Err(err(k.line, k.col, format!("`{name}`: `net.` fields are the capture's own")));
                }
                let (spec, extra) = spec(v, &mut bind, &["kind", "costly"])?;
                let kind = match extra.kind {
                    Some(k) => k,
                    None if spec.map.is_some() => FieldKind::Str,
                    None => match spec.ty {
                        Ty::Int | Ty::Bool => FieldKind::U64,
                        Ty::Float => FieldKind::F64,
                        Ty::Bytes | Ty::Any | Ty::Obj | Ty::List(_) => FieldKind::Str,
                    },
                };
                schema.push(FieldDef::owned(name.clone(), kind, extra.costly || spec.hex));
                fields.push(spec);
            }
        }

        let level = get("level").map(|l| spec(l, &mut bind, &[]).map(|s| s.0)).transpose()?;
        let mut level_names = Vec::new();
        let mut s = level.as_ref();
        while let Some(spec) = s {
            for (_, name) in spec.map.iter().flatten() {
                match std::str::from_utf8(name).ok().and_then(Level::parse) {
                    Some(l) => level_names.push((*name, l)),
                    None => {
                        let at = get("level").map_or((nerka.line, nerka.col), |n| (n.line, n.col));
                        let shown = String::from_utf8_lossy(name);
                        return Err(err(
                            at.0,
                            at.1,
                            format!("`{shown}` isn't a level (fatal, error, warn, info, debug, trace, …)"),
                        ));
                    }
                }
            }
            s = spec.otherwise.as_deref();
        }
        let uptime = get("uptime").map(|u| spec(u, &mut bind, &[]).map(|s| s.0)).transpose()?;

        let cols = get("columns").ok_or_else(|| err(nerka.line, nerka.col, "-nerka needs `columns`"))?;
        let cols = as_seq(cols, "columns")?;
        if cols.is_empty() {
            return Err(err(nerka.line, nerka.col, "-nerka needs at least one column"));
        }
        let mut columns = Vec::new();
        let mut cells = Vec::new();
        for (i, c) in cols.iter().enumerate() {
            let last = i + 1 == cols.len();
            let (col_name, width, cell) = match &c.value {
                Value::Scalar { text, .. } => {
                    let id = schema
                        .iter()
                        .position(|f| f.name == text.as_str())
                        .ok_or_else(|| err(c.line, c.col, format!("there is no field `{text}`; give `from:`")))?;
                    (text.clone(), None, Cell::Field(id))
                }
                Value::Map(m) => {
                    let name = m
                        .iter()
                        .find(|(k, _)| k.text == "name")
                        .ok_or_else(|| err(c.line, c.col, "a column needs a `name`"))?;
                    let col_name = as_text(&name.1, "name")?.to_owned();
                    let width = match m.iter().find(|(k, _)| k.text == "width") {
                        Some((_, w)) => Some(
                            parse_int(as_text(w, "width")?)
                                .and_then(|v| u16::try_from(v).ok())
                                .ok_or_else(|| err(w.line, w.col, "a width is a number of characters"))?,
                        ),
                        None => None,
                    };
                    let rest: Vec<_> =
                        m.iter().filter(|(k, _)| k.text != "name" && k.text != "width").cloned().collect();
                    let cell = if rest.is_empty() {
                        let id = schema.iter().position(|f| f.name == col_name.as_str()).ok_or_else(|| {
                            err(c.line, c.col, format!("there is no field `{col_name}`; give `from:`"))
                        })?;
                        Cell::Field(id)
                    } else {
                        let node = Node { value: Value::Map(rest), line: c.line, col: c.col };
                        Cell::Own(spec(&node, &mut bind, &[])?.0)
                    };
                    (col_name, width, cell)
                }
                _ => return Err(err(c.line, c.col, "a column is a field's name or `{ name: …, from: … }`")),
            };
            let width = if last {
                0
            } else {
                width.unwrap_or(match &cell {
                    Cell::Field(id) if schema[*id].kind == FieldKind::Str => 8,
                    _ => 10,
                })
            };
            columns.push(ColumnDef { name: Cow::Owned(col_name), width });
            cells.push(cell);
        }
        let mut routes = Vec::new();
        if let Some(m) = get("match") {
            for r in as_seq(m, "match")? {
                let rule = match &r.value {
                    Value::Map(entries) => {
                        let mut rule = Match::default();
                        for (k, v) in entries {
                            rule.set(&k.text, as_text(v, "a rule's value")?).map_err(|e| err(v.line, v.col, e))?;
                        }
                        rule
                    }
                    _ => as_text(r, "a rule")?.parse().map_err(|e: String| err(r.line, r.col, e))?,
                };
                routes.push(rule);
            }
        }
        Ok(KsyProtocol { name, def, framing, schema, fields, level, level_names, uptime, columns, cells, routes })
    }

    /// Read a definition file.
    pub fn load(path: impl AsRef<Path>) -> crate::Result<KsyProtocol> {
        let path = path.as_ref();
        // The file is named (review 2026-11-08, R3: "I/O error" alone).
        let io = |e: std::io::Error| crate::Error::opening(path, e.into());
        let meta = std::fs::metadata(path).map_err(io)?;
        if meta.len() > yaml::MAX_BYTES as u64 {
            return Err(crate::Error::Format(format!("{}: larger than {} KiB", path.display(), yaml::MAX_BYTES >> 10)));
        }
        let text = std::fs::read_to_string(path).map_err(io)?;
        KsyProtocol::parse(&text).map_err(|e| crate::Error::Format(format!("{}: {e}", path.display())))
    }

    /// The rules it gives itself: which UDP datagrams are its messages.
    pub fn routes(&self) -> &[Match] {
        &self.routes
    }

    fn length(&self) -> Option<&CExpr> {
        match &self.framing {
            Framing::Length(e) => Some(e),
            Framing::Datagram => None,
        }
    }

    fn reader<'s, 'm>(&'s self, msg: &'m [u8], at: &Origin<'m>) -> Eval<'s, 'm> {
        Eval::new(&self.def, msg, *at, self.length(), false)
    }

    fn cell_text(&self, e: &Eval<'_, '_>, cell: &Cell, one_line: bool, out: &mut Vec<u8>) {
        let spec = match cell {
            Cell::Field(id) => &self.fields[*id],
            Cell::Own(s) => s,
        };
        let start = out.len();
        write_spec(e, spec, out);
        clean(out, start, one_line);
    }

    fn cells(&self, msg: &[u8], at: &Origin<'_>, one_line: bool) -> Vec<Vec<u8>> {
        let e = self.reader(msg, at);
        self.cells
            .iter()
            .map(|c| {
                let mut out = Vec::new();
                self.cell_text(&e, c, one_line, &mut out);
                out
            })
            .collect()
    }
}

/// Control characters in a cell: on one line, all of them as spaces; in a
/// detailed view, line breaks (`\r\n` and a lone `\r` as `\n`) and tabs
/// kept. As native DLT shows payloads.
fn clean(out: &mut Vec<u8>, start: usize, one_line: bool) {
    if one_line {
        for b in &mut out[start..] {
            if *b < 0x20 || *b == 0x7F {
                *b = b' ';
            }
        }
        return;
    }
    let text = out.split_off(start);
    let mut it = text.iter().copied().peekable();
    while let Some(b) = it.next() {
        match b {
            b'\r' => {
                it.next_if_eq(&b'\n');
                out.push(b'\n');
            }
            b'\n' | b'\t' => out.push(b),
            b if b < 0x20 || b == 0x7F => out.push(b' '),
            b => out.push(b),
        }
    }
}

/// A section value as text: numbers with its `decimals`, bytes as hex if
/// it says `render: hex`, objects by their templates.
fn write_spec(e: &Eval<'_, '_>, spec: &Spec, out: &mut Vec<u8>) {
    let v = eval_spec(e, spec);
    write_spec_val(e, spec, &v, out);
}

/// What a value of the section says besides its value.
#[derive(Default)]
struct Extra {
    kind: Option<FieldKind>,
    costly: bool,
}

/// A value: an expression, or `{ from, if, map, else, decimals, render }`
/// (and `extra` keys).
fn spec(
    n: &Node,
    bind: &mut impl FnMut(&Src) -> Result<(CExpr, Ty), KsyError>,
    extra: &[&str],
) -> Result<(Spec, Extra), KsyError> {
    spec_at(n, bind, extra, 0)
}

fn spec_at(
    n: &Node,
    bind: &mut impl FnMut(&Src) -> Result<(CExpr, Ty), KsyError>,
    extra: &[&str],
    depth: usize,
) -> Result<(Spec, Extra), KsyError> {
    if depth > 16 {
        return Err(err(n.line, n.col, "`else` nested too deeply"));
    }
    if let Value::Scalar { .. } = n.value {
        let (from, ty) = bind(&as_expr(n, "a value")?)?;
        return Ok((
            Spec { from, cond: None, map: None, otherwise: None, decimals: None, hex: false, ty },
            Extra::default(),
        ));
    }
    let m = as_map(n, "a value")?;
    for (k, _) in m {
        let known = ["from", "if", "map", "else", "decimals", "render"].contains(&k.text.as_str())
            || extra.contains(&k.text.as_str());
        if !known {
            return Err(err(k.line, k.col, format!("`{}` isn't read in a value", k.text)));
        }
    }
    let get = |key: &str| m.iter().find(|(k, _)| k.text == key).map(|(_, v)| v);
    let from_node = get("from").ok_or_else(|| err(n.line, n.col, "a value needs `from`"))?;
    let (from, ty) = bind(&as_expr(from_node, "from")?)?;
    let cond = get("if").map(|c| as_expr(c, "if").and_then(|s| bind(&s))).transpose()?.map(|b| b.0);
    let map = match get("map") {
        None => None,
        Some(mn) => {
            let mut out = Vec::new();
            for (k, v) in as_map(mn, "map")? {
                let key = match k.text.as_str() {
                    "true" => Lit::Bool(true),
                    "false" => Lit::Bool(false),
                    t => parse_int(t).map_or_else(|| Lit::Bytes(intern(t.as_bytes())), Lit::Int),
                };
                out.push((key, intern(as_text(v, "a name")?.as_bytes())));
            }
            Some(out)
        }
    };
    let otherwise = get("else").map(|e| spec_at(e, bind, &[], depth + 1).map(|s| Box::new(s.0))).transpose()?;
    let decimals = match get("decimals") {
        Some(d) => Some(
            parse_int(as_text(d, "decimals")?)
                .and_then(|v| usize::try_from(v).ok())
                .filter(|&v| v <= 12)
                .ok_or_else(|| err(d.line, d.col, "decimals is 0 to 12"))?,
        ),
        None => None,
    };
    let hex = match get("render") {
        Some(r) => match as_text(r, "render")? {
            "hex" => true,
            "text" => false,
            other => return Err(err(r.line, r.col, format!("render is `hex` or `text`, not `{other}`"))),
        },
        None => false,
    };
    let mut x = Extra::default();
    if let Some(k) = get("kind") {
        x.kind = Some(match as_text(k, "kind")? {
            "str" => FieldKind::Str,
            "u64" => FieldKind::U64,
            "f64" => FieldKind::F64,
            "name" => FieldKind::Name,
            other => return Err(err(k.line, k.col, format!("kind is str, u64, f64 or name, not `{other}`"))),
        });
    }
    if let Some(c) = get("costly") {
        x.costly = match as_text(c, "costly")? {
            "true" => true,
            "false" => false,
            _ => return Err(err(c.line, c.col, "costly is `true` or `false`")),
        };
    }
    Ok((Spec { from, cond, map, otherwise, decimals, hex, ty }, x))
}

/// A section value for one message; nothing if it has none.
fn eval_spec<'m>(e: &Eval<'_, 'm>, s: &Spec) -> Val<'m> {
    let v = (|| {
        if let Some(c) = &s.cond
            && !e.eval(c).ok()?.truthy()
        {
            return None;
        }
        let v = e.eval(&s.from).ok()?;
        match &s.map {
            None => Some(v),
            Some(map) => map
                .iter()
                .find(|(k, _)| run::compare(&v, &lit_val(k)) == Some(std::cmp::Ordering::Equal))
                .map(|(_, name)| Val::Lit(name)),
        }
    })();
    match v {
        Some(Val::Null) | None => s.otherwise.as_ref().map_or(Val::Null, |o| eval_spec(e, o)),
        Some(v) => v,
    }
}

/// [`write_spec`] for a value already evaluated.
fn write_spec_val<'m>(e: &Eval<'_, 'm>, spec: &Spec, v: &Val<'m>, out: &mut Vec<u8>) {
    use std::io::Write;
    if let Some(d) = spec.decimals
        && let Some(x) = v.as_f64()
        && !matches!(v, Val::Enum(..))
    {
        let _ = write!(out, "{x:.d$}");
        return;
    }
    e.text(v, if spec.hex { &Fmt::Hex } else { &Fmt::Plain }, None, out);
}

fn lit_val(l: &Lit) -> Val<'static> {
    match l {
        Lit::Int(v) => Val::Int(*v),
        Lit::Float(v) => Val::Float(*v),
        Lit::Bool(v) => Val::Bool(*v),
        Lit::Bytes(b) => Val::Lit(b),
    }
}

impl Protocol for KsyProtocol {
    fn name(&self) -> &str {
        &self.name
    }

    fn columns(&self) -> &[ColumnDef] {
        &self.columns
    }

    fn schema(&self) -> &[FieldDef] {
        &self.schema
    }

    fn split(&self, datagram: &[u8], found: &mut Vec<usize>) -> usize {
        let origin = Origin { secs: 0, usecs: 0, channel: NO_CHANNEL };
        match &self.framing {
            Framing::Datagram => {
                let e = Eval::new(&self.def, datagram, origin, None, true);
                if datagram.is_empty() || e.validate().is_err() {
                    return datagram.len();
                }
                found.push(0);
                0
            }
            Framing::Length(len) => {
                let mut pos = 0;
                while pos < datagram.len() {
                    // The length, then everything inside it, in one reading.
                    let e = Eval::new(&self.def, &datagram[pos..], origin, Some(len), true);
                    let n = match e.length() {
                        Ok(n) if n > 0 => n,
                        _ => break,
                    };
                    if e.validate().is_err() {
                        break;
                    }
                    found.push(pos);
                    pos += n;
                }
                datagram.len() - pos
            }
        }
    }

    fn delimited(&self) -> bool {
        matches!(self.framing, Framing::Length(_))
    }

    fn render(&self, msg: &[u8], at: &Origin<'_>, out: &mut Vec<u8>) {
        for (i, cell) in self.cells(msg, at, true).into_iter().enumerate() {
            if i > 0 {
                out.push(b'\t');
            }
            out.extend_from_slice(&cell);
        }
    }

    fn detail(&self, msg: &[u8], at: &Origin<'_>) -> Vec<String> {
        self.cells(msg, at, false).into_iter().map(|c| String::from_utf8_lossy(&c).into_owned()).collect()
    }

    fn field<'m>(&self, msg: &'m [u8], at: &Origin<'m>, id: FieldId) -> Option<FieldValue<'m>> {
        let spec = self.fields.get(id)?;
        let kind = self.schema[id].kind;
        let e = self.reader(msg, at);
        let v = eval_spec(&e, spec);
        let as_text = |v: &Val<'m>| {
            let mut out = Vec::new();
            write_spec_val(&e, spec, v, &mut out);
            clean(&mut out, 0, true);
            FieldValue::Str(out.into())
        };
        if spec.hex || spec.decimals.is_some() {
            return (v != Val::Null).then(|| as_text(&v));
        }
        Some(match (kind, v) {
            (_, Val::Null) => return None,
            (FieldKind::U64, Val::U64(x)) => FieldValue::U64(x),
            (FieldKind::U64, v) if v.as_i64().is_some() => FieldValue::U64(v.as_i64()? as u64),
            (FieldKind::U64, v) => FieldValue::U64(v.as_f64()? as u64),
            (FieldKind::F64, v) => FieldValue::F64(v.as_f64()?),
            // As they are: only cells show control characters as spaces.
            (_, Val::Msg(b) | Val::Text(b)) => FieldValue::Str(Cow::Borrowed(b)),
            (_, Val::Lit(b)) => FieldValue::Str(Cow::Borrowed(b)),
            (_, v) => as_text(&v),
        })
    }

    fn level(&self, msg: &[u8], at: &Origin<'_>) -> Option<Level> {
        let v = eval_spec(&self.reader(msg, at), self.level.as_ref()?);
        let b = v.bytes()?;
        match self.level_names.iter().find(|(n, _)| *n == b) {
            Some((_, l)) => Some(*l),
            None => Level::parse(std::str::from_utf8(b).ok()?),
        }
    }

    fn uptime(&self, msg: &[u8], at: &Origin<'_>) -> Option<f64> {
        eval_spec(&self.reader(msg, at), self.uptime.as_ref()?).as_f64()
    }
}

#[cfg(test)]
mod tests;

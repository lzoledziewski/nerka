//! FIBEX descriptions of DLT non-verbose messages: the catalog that turns a
//! non-verbose payload (message id + raw argument bytes) into text, and
//! supplies the APID, CTID, type and level such a message doesn't carry.
//!
//! The convention (COVESA dlt-daemon / dlt-viewer examples, FIBEX 3.x):
//!
//! ```text
//! FRAME ID="ID_<message id>"                  one message
//!   PDU-INSTANCES: PDU-REF + SEQUENCE-NUMBER  its pieces, in order
//!   MANUFACTURER-EXTENSION: MESSAGE_TYPE, MESSAGE_INFO (level),
//!     APPLICATION_ID, CONTEXT_ID, MESSAGE_SOURCE_FILE, MESSAGE_LINE_NUMBER
//! PDU: ho:DESC (static text) and/or SIGNAL-INSTANCES (typed arguments)
//! SIGNAL: CODING-REF -> CODING: ho:CODED-TYPE ho:BASE-DATA-TYPE
//!   (A_UINT8 … A_INT64, A_FLOAT32/64, A_ASCIISTRING, A_UNICODE2STRING,
//!   A_BYTEFIELD) and ho:BIT-LENGTH
//! ```
//!
//! Signals that aren't defined (some files only refer to them) are read by
//! the conventional names: `S_BOOL`, `S_UINT8` … `S_SINT64`, `S_FLOA16/32/64`,
//! `S_STRG_ASCII`, `S_STRG_UTF8`, `S_RAWD`.
//!
//! On the wire a non-verbose argument has no type info: numbers are their
//! bytes in the message's byte order, strings and raw data a 16-bit length
//! and then the bytes (a string's final NUL dropped).
//!
//! A message id is unique per ECU. A file that names exactly one ECU binds
//! its frames to it; looking a message up prefers its ECU, then frames
//! bound to none, then any (ECU names in FIBEX and in logs don't always
//! agree), and among those the one whose APID / CTID match the message's.

use crate::event::Level;
use crate::signal::{SignalKind, SignalValue};
use quick_xml::events::Event as Xml;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// The type of one non-verbose argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    Bool,
    /// Unsigned, signed or floating point, of this many bytes.
    Uint(u8),
    Sint(u8),
    Float(u8),
    /// Length-prefixed text.
    Str,
    /// Length-prefixed bytes, shown as hex.
    Raw,
}

/// A piece of a message: static text, or an argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    Arg(ArgType),
    /// An argument whose type the file doesn't say (its signal id): the
    /// rest of the payload can't be read past it.
    Unknown(String),
}

/// One non-verbose message as FIBEX describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The ECU the file binds it to, if any.
    pub ecu: Option<String>,
    pub apid: Option<String>,
    pub ctid: Option<String>,
    /// DLT MSTP / MTIN (from `MESSAGE_TYPE` / `MESSAGE_INFO`).
    pub mstp: Option<u8>,
    pub mtin: Option<u8>,
    /// Where it is logged: file and line.
    pub source: Option<(String, u32)>,
    pub pieces: Vec<Piece>,
}

impl Frame {
    /// The level of a log message (`MESSAGE_INFO`).
    pub fn level(&self) -> Option<Level> {
        if self.mstp != Some(0) {
            return None;
        }
        Some(match self.mtin? {
            1 => Level::Fatal,
            2 => Level::Error,
            3 => Level::Warn,
            4 => Level::Info,
            5 => Level::Debug,
            6 => Level::Trace,
            _ => return None,
        })
    }

    /// The value of argument `index` (counting arguments only) in a payload
    /// (`data`: after the message id): numbers as numbers, the rest as
    /// text.
    pub fn arg(&self, data: &[u8], big_endian: bool, index: usize) -> Option<SignalValue> {
        let mut r = Reader { b: data, be: big_endian };
        let mut n = 0;
        for p in &self.pieces {
            let ty = match p {
                Piece::Text(_) => continue,
                Piece::Arg(ty) => *ty,
                Piece::Unknown(_) => return None,
            };
            if n == index {
                return r.value(ty);
            }
            // Skip it: its text isn't needed.
            r.arg(ty, &mut Vec::new())?;
            n += 1;
        }
        None
    }

    /// The message as a template: its texts, and each argument as its
    /// type (`Free size: <u16>`).
    pub fn template(&self) -> String {
        let mut out = String::new();
        for p in &self.pieces {
            if !out.is_empty() && !out.ends_with(char::is_whitespace) {
                out.push(' ');
            }
            match p {
                Piece::Text(t) => out.push_str(t),
                Piece::Arg(ty) => out.push_str(&format!("<{}>", ty.name())),
                Piece::Unknown(s) => out.push_str(&format!("<{s}?>")),
            }
        }
        out
    }

    /// Append the text of a payload (`data`: after the message id) to
    /// `out`. Pieces are separated by a space unless the text before ends
    /// in one; what can't be read is shown as hex after `<undecoded>`.
    pub fn render(&self, data: &[u8], big_endian: bool, out: &mut Vec<u8>) {
        let mut r = Reader { b: data, be: big_endian };
        let start = out.len();
        let sep = |out: &mut Vec<u8>| {
            if out.len() > start && !out.last().is_some_and(u8::is_ascii_whitespace) {
                out.push(b' ');
            }
        };
        for p in &self.pieces {
            match p {
                Piece::Text(t) => {
                    sep(out);
                    out.extend_from_slice(t.as_bytes());
                }
                Piece::Arg(ty) => {
                    let (before, len) = (r.b, out.len());
                    sep(out);
                    if r.arg(*ty, out).is_none() {
                        out.truncate(len);
                        r.b = before;
                        break;
                    }
                }
                Piece::Unknown(_) => break,
            }
        }
        if !r.b.is_empty() {
            sep(out);
            out.extend_from_slice(b"<undecoded> ");
            for (i, x) in r.b.iter().enumerate() {
                let _ = write!(out, "{}{x:02x}", if i > 0 { " " } else { "" });
            }
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    be: bool,
}

impl ArgType {
    /// `u16`, `i32`, `f32`, `bool`, `string`, `raw`.
    pub fn name(self) -> String {
        match self {
            ArgType::Bool => "bool".into(),
            ArgType::Uint(n) => format!("u{}", u32::from(n) * 8),
            ArgType::Sint(n) => format!("i{}", u32::from(n) * 8),
            ArgType::Float(n) => format!("f{}", u32::from(n) * 8),
            ArgType::Str => "string".into(),
            ArgType::Raw => "raw".into(),
        }
    }

    /// The kind of signal it makes: numbers are `num`, the rest `enum`.
    pub fn kind(self) -> SignalKind {
        match self {
            ArgType::Uint(_) | ArgType::Sint(_) | ArgType::Float(_) => SignalKind::Num,
            _ => SignalKind::Enum,
        }
    }
}

impl<'a> Reader<'a> {
    /// One argument as a value.
    fn value(&mut self, ty: ArgType) -> Option<SignalValue> {
        let num = |u: u128, ty| -> f64 {
            match ty {
                ArgType::Sint(n) => {
                    let shift = 128 - 8 * u32::from(n);
                    (((u << shift) as i128) >> shift) as f64
                }
                ArgType::Float(2) => f64::from(half(u as u16)),
                ArgType::Float(4) => f64::from(f32::from_bits(u as u32)),
                ArgType::Float(_) => f64::from_bits(u as u64),
                _ => u as f64,
            }
        };
        Some(match ty {
            ArgType::Uint(n) | ArgType::Sint(n) | ArgType::Float(n) => SignalValue::Num(num(self.number(n)?, ty)),
            _ => {
                let mut out = Vec::new();
                self.arg(ty, &mut out)?;
                SignalValue::Enum(out)
            }
        })
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.b.len() < n {
            return None;
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Some(h)
    }

    /// Up to 16 bytes as a big-endian, right-aligned 128-bit number.
    fn number(&mut self, size: u8) -> Option<u128> {
        let v = self.take(size as usize)?;
        let mut be = [0u8; 16];
        if self.be {
            be[16 - v.len()..].copy_from_slice(v);
        } else {
            v.iter().enumerate().for_each(|(i, &b)| be[15 - i] = b);
        }
        Some(u128::from_be_bytes(be))
    }

    fn arg(&mut self, ty: ArgType, out: &mut Vec<u8>) -> Option<()> {
        match ty {
            ArgType::Bool => out.extend_from_slice(if self.take(1)?[0] != 0 { b"true" } else { b"false" }),
            ArgType::Uint(n) => write!(out, "{}", self.number(n)?).ok()?,
            ArgType::Sint(n) => {
                let shift = 128 - 8 * u32::from(n);
                write!(out, "{}", ((self.number(n)? << shift) as i128) >> shift).ok()?
            }
            ArgType::Float(n) => {
                let u = self.number(n)?;
                match n {
                    2 => write!(out, "{}", half(u as u16)),
                    4 => write!(out, "{}", f32::from_bits(u as u32)),
                    8 => write!(out, "{}", f64::from_bits(u as u64)),
                    _ => return None,
                }
                .ok()?
            }
            ArgType::Str | ArgType::Raw => {
                let len = self.number(2)? as usize;
                let data = self.take(len)?;
                if ty == ArgType::Raw {
                    for (i, x) in data.iter().enumerate() {
                        write!(out, "{}{x:02x}", if i > 0 { " " } else { "" }).ok()?;
                    }
                } else {
                    let s = data.strip_suffix(b"\0").unwrap_or(data);
                    out.extend_from_slice(String::from_utf8_lossy(s).as_bytes());
                }
            }
        }
        Some(())
    }
}

/// An IEEE half-precision float.
fn half(h: u16) -> f32 {
    let (sign, exp, frac) = (u32::from(h >> 15), u32::from((h >> 10) & 0x1F), u32::from(h & 0x3FF));
    let v = match exp {
        0 => frac as f32 * 2f32.powi(-24),
        31 if frac == 0 => f32::INFINITY,
        31 => f32::NAN,
        e => (1.0 + frac as f32 / 1024.0) * 2f32.powi(e as i32 - 15),
    };
    if sign == 1 { -v } else { v }
}

#[derive(Debug, thiserror::Error)]
pub enum FibexError {
    #[error("{file}: {msg} (at byte {pos})")]
    Xml { file: String, pos: u64, msg: String },
    #[error("{file}: {err}")]
    Io { file: String, err: std::io::Error },
}

/// Non-verbose messages by id, from one or more FIBEX files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fibex {
    frames: HashMap<u32, Vec<Frame>>,
    /// The files it was read from.
    pub files: Vec<String>,
}

impl Fibex {
    /// Read FIBEX files into one catalog.
    pub fn load(paths: &[impl AsRef<Path>]) -> Result<Fibex, FibexError> {
        let mut f = Fibex::default();
        for p in paths {
            let p = p.as_ref();
            let file = p.display().to_string();
            let text = std::fs::read(p).map_err(|err| FibexError::Io { file: file.clone(), err })?;
            f.add(&file, &String::from_utf8_lossy(&text))?;
        }
        Ok(f)
    }

    /// Add the messages of one FIBEX document (`name` for messages).
    pub fn add(&mut self, name: &str, xml: &str) -> Result<(), FibexError> {
        let raw = parse(name, xml)?;
        for (mid, frame) in raw.resolve() {
            self.frames.entry(mid).or_default().push(frame);
        }
        self.files.push(name.to_owned());
        Ok(())
    }

    /// The number of messages described.
    pub fn len(&self) -> usize {
        self.frames.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Every argument of every message, sorted by ECU, APID, CTID and
    /// message id, each with a signal name that is unique in the list.
    pub fn arguments(&self) -> Vec<Argument> {
        let mut frames: Vec<(u32, &Frame)> =
            self.frames.iter().flat_map(|(mid, fs)| fs.iter().map(move |f| (*mid, f))).collect();
        frames.sort_by(|a, b| (&a.1.ecu, &a.1.apid, &a.1.ctid, a.0).cmp(&(&b.1.ecu, &b.1.apid, &b.1.ctid, b.0)));
        let mut out: Vec<Argument> = Vec::new();
        for (mid, f) in frames {
            // The text before an argument, else the label of the one before
            // it (`Temperature measurement <u8> <f32>`: both are about it).
            let (mut label, mut last) = (String::new(), String::new());
            let mut index = 0;
            for p in &f.pieces {
                match p {
                    Piece::Text(t) => label = t.clone(),
                    Piece::Arg(ty) => {
                        out.push(Argument {
                            mid,
                            ecu: f.ecu.clone(),
                            apid: f.apid.clone(),
                            ctid: f.ctid.clone(),
                            index,
                            label: {
                                let l = last_phrase(&std::mem::take(&mut label));
                                if !l.is_empty() {
                                    last = l;
                                }
                                last.clone()
                            },
                            name: String::new(),
                            ty: *ty,
                            message: f.template(),
                        });
                        index += 1;
                    }
                    Piece::Unknown(_) => break,
                }
            }
        }
        // Names from the text before the argument (`Free size:` ->
        // `free_size`), else from the message; made unique.
        let base: Vec<String> = out
            .iter()
            .map(|a| match snake(&a.label) {
                s if s.is_empty() => format!("m{}_{}", a.mid, a.index),
                s => s,
            })
            .collect();
        // A label used once is the name; else the message id and the
        // argument's place say which, and the ECU if that's not enough.
        let count = |n: &str| base.iter().filter(|b| *b == n).count();
        let with_mids: Vec<String> =
            out.iter().zip(&base).map(|(a, b)| format!("{b}_m{}_{}", a.mid, a.index)).collect();
        let count_mid = |n: &str| with_mids.iter().filter(|b| *b == n).count();
        let mut used = std::collections::HashSet::new();
        for (i, a) in out.iter_mut().enumerate() {
            let with_mid = with_mids[i].clone();
            let mut candidates = Vec::new();
            if count(&base[i]) == 1 {
                candidates.push(base[i].clone());
            }
            if count_mid(&with_mid) == 1 {
                candidates.push(with_mid.clone());
            }
            candidates.push(format!("{}_{with_mid}", snake(a.ecu.as_deref().unwrap_or("any"))));
            let name = candidates.into_iter().find(|c| !used.contains(c)).unwrap_or_else(|| {
                (2..).map(|k| format!("{with_mid}_{k}")).find(|c| !used.contains(c)).expect("a free name")
            });
            used.insert(name.clone());
            a.name = name;
        }
        out
    }

    /// The description of message `mid` from `ecu` (module docs: the best
    /// match), given the message's own APID / CTID if it has them.
    pub fn frame(&self, mid: u32, ecu: &[u8], apid: Option<&[u8]>, ctid: Option<&[u8]>) -> Option<&Frame> {
        let all = self.frames.get(&mid)?;
        if all.len() == 1 {
            return all.first();
        }
        let same = |a: &Option<String>, b: Option<&[u8]>| match (a, b) {
            (Some(a), Some(b)) => a.as_bytes() == b,
            _ => true,
        };
        let rank = |f: &Frame| {
            let ecu_rank = match &f.ecu {
                Some(e) if e.as_bytes() == ecu => 0,
                None => 1,
                Some(_) => 2,
            };
            (ecu_rank, !(same(&f.apid, apid) && same(&f.ctid, ctid)))
        };
        all.iter().min_by_key(|f| rank(f))
    }
}

/// One argument of a described message, as a possible signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument {
    pub mid: u32,
    pub ecu: Option<String>,
    pub apid: Option<String>,
    pub ctid: Option<String>,
    /// Its place among the message's arguments.
    pub index: usize,
    /// The text before it in the message (`Free size`).
    pub label: String,
    /// A signal name, unique among all arguments (`free_size`).
    pub name: String,
    pub ty: ArgType,
    /// The whole message as a template ([`Frame::template`]).
    pub message: String,
}

/// The text that names an argument: the last phrase before it, without a
/// final `:` / `=` (`Buffer near limit. Free size:` -> `Free size`).
fn last_phrase(text: &str) -> String {
    let t = text.trim().trim_end_matches([':', '=']).trim();
    t.rsplit(['.', ';', ',']).find(|p| !p.trim().is_empty()).unwrap_or("").trim().to_owned()
}

/// `Free size` -> `free_size`; empty if nothing of it is a name.
fn snake(label: &str) -> String {
    let mut out = String::new();
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_owned();
    match out.chars().next() {
        Some(c) if c.is_ascii_digit() => format!("_{out}"),
        _ => out,
    }
}

/// What a document says, before its references are resolved.
#[derive(Default)]
struct Raw {
    ecus: Vec<String>,
    frames: Vec<RawFrame>,
    pdus: HashMap<String, RawPdu>,
    /// Signal id -> coding id.
    signals: HashMap<String, String>,
    codings: HashMap<String, RawCoding>,
}

#[derive(Default)]
struct RawFrame {
    id: String,
    short_name: String,
    /// (sequence number, PDU id).
    pdus: Vec<(u32, String)>,
    ext: HashMap<String, String>,
}

#[derive(Default)]
struct RawPdu {
    desc: String,
    /// (sequence number, signal id).
    signals: Vec<(u32, String)>,
}

#[derive(Default)]
struct RawCoding {
    short_name: String,
    base: String,
    bits: Option<u32>,
}

impl Raw {
    fn resolve(mut self) -> Vec<(u32, Frame)> {
        let ecu = (self.ecus.len() == 1).then(|| self.ecus[0].clone());
        let mut out = Vec::new();
        for mut f in std::mem::take(&mut self.frames) {
            let Some(mid) = message_id(&f.id).or_else(|| message_id(&f.short_name)) else { continue };
            f.pdus.sort_by_key(|p| p.0);
            let mut pieces = Vec::new();
            for (_, pdu_id) in &f.pdus {
                let Some(pdu) = self.pdus.get(pdu_id) else { continue };
                if !pdu.desc.is_empty() {
                    pieces.push(Piece::Text(pdu.desc.clone()));
                }
                let mut sigs = pdu.signals.clone();
                sigs.sort_by_key(|s| s.0);
                for (_, sig) in sigs {
                    pieces.push(match self.arg_type(&sig) {
                        Some(t) => Piece::Arg(t),
                        None => Piece::Unknown(sig),
                    });
                }
            }
            let ext = |k: &str| f.ext.get(k).filter(|v| !v.is_empty()).cloned();
            let mstp = ext("MESSAGE_TYPE").and_then(|t| match t.as_str() {
                "DLT_TYPE_LOG" => Some(0),
                "DLT_TYPE_APP_TRACE" => Some(1),
                "DLT_TYPE_NW_TRACE" => Some(2),
                "DLT_TYPE_CONTROL" => Some(3),
                _ => None,
            });
            let mtin = ext("MESSAGE_INFO").and_then(|i| {
                Some(match i.as_str() {
                    "DLT_LOG_FATAL" => 1,
                    "DLT_LOG_ERROR" => 2,
                    "DLT_LOG_WARN" => 3,
                    "DLT_LOG_INFO" => 4,
                    "DLT_LOG_DEBUG" => 5,
                    "DLT_LOG_VERBOSE" => 6,
                    _ => return None,
                })
            });
            let source = ext("MESSAGE_SOURCE_FILE")
                .map(|file| (file, ext("MESSAGE_LINE_NUMBER").and_then(|l| l.parse().ok()).unwrap_or(0)));
            out.push((
                mid,
                Frame {
                    ecu: ecu.clone(),
                    apid: ext("APPLICATION_ID"),
                    ctid: ext("CONTEXT_ID"),
                    mstp,
                    mtin,
                    source,
                    pieces,
                },
            ));
        }
        out
    }

    /// A signal's type: from its coding, else from its conventional name.
    fn arg_type(&self, signal: &str) -> Option<ArgType> {
        let coding = self.signals.get(signal).and_then(|c| self.codings.get(c).map(|x| (c, x)));
        if let Some((id, c)) = coding {
            // A boolean is coded as an 8-bit unsigned; only its name says so.
            if id.eq_ignore_ascii_case("BOOL") || c.short_name.eq_ignore_ascii_case("BOOL") {
                return Some(ArgType::Bool);
            }
            let bytes = |default: u8| c.bits.map_or(default, |b| (b / 8) as u8);
            let width =
                |s: &str| s.trim_start_matches(|ch: char| !ch.is_ascii_digit()).parse::<u8>().ok().map(|b| b / 8);
            let t = match c.base.as_str() {
                b if b.starts_with("A_UINT") => ArgType::Uint(bytes(width(b)?)),
                b if b.starts_with("A_INT") => ArgType::Sint(bytes(width(b)?)),
                b if b.starts_with("A_FLOAT") => ArgType::Float(bytes(width(b)?)),
                "A_ASCIISTRING" | "A_UNICODE2STRING" => ArgType::Str,
                "A_BYTEFIELD" => ArgType::Raw,
                _ => return conventional(signal),
            };
            return matches!(
                t,
                ArgType::Uint(1 | 2 | 4 | 8)
                    | ArgType::Sint(1 | 2 | 4 | 8)
                    | ArgType::Float(2 | 4 | 8)
                    | ArgType::Str
                    | ArgType::Raw
                    | ArgType::Bool
            )
            .then_some(t);
        }
        conventional(signal)
    }
}

/// `S_UINT32` and the like.
fn conventional(signal: &str) -> Option<ArgType> {
    let s = signal.strip_prefix("S_").unwrap_or(signal).to_ascii_uppercase();
    Some(match s.as_str() {
        "BOOL" => ArgType::Bool,
        "UINT8" => ArgType::Uint(1),
        "UINT16" => ArgType::Uint(2),
        "UINT32" => ArgType::Uint(4),
        "UINT64" => ArgType::Uint(8),
        "SINT8" => ArgType::Sint(1),
        "SINT16" => ArgType::Sint(2),
        "SINT32" => ArgType::Sint(4),
        "SINT64" => ArgType::Sint(8),
        "FLOA16" => ArgType::Float(2),
        "FLOA32" => ArgType::Float(4),
        "FLOA64" => ArgType::Float(8),
        "STRG_ASCII" | "STRG_UTF8" => ArgType::Str,
        "RAWD" => ArgType::Raw,
        _ => return None,
    })
}

/// `ID_123` (or `ID_0x7b`) -> 123.
fn message_id(id: &str) -> Option<u32> {
    let n = id.trim().strip_prefix("ID_")?;
    match n.strip_prefix("0x").or_else(|| n.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16).ok(),
        None => n.parse().ok(),
    }
}

/// The text of a predefined or character entity reference (`amp`, `#38`).
fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix('x') {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(code)?
        }
    })
}

/// An attribute value with its entity references replaced (an unknown one
/// is kept as written).
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        match rest.find(';').and_then(|j| Some((j, entity(&rest[1..j])?))) {
            Some((j, c)) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Where the reader is in a document, and what it has read.
#[derive(Default)]
struct State {
    raw: Raw,
    /// Element names (without namespace prefix) from the root down.
    path: Vec<String>,
    frame: Option<RawFrame>,
    pdu: Option<(String, RawPdu)>,
    coding: Option<(String, RawCoding)>,
    signal: Option<String>,
    /// An instance being read: (sequence number, reference).
    instance: (u32, String),
}

/// Read a document, keeping only what non-verbose messages need.
fn parse(file: &str, xml: &str) -> Result<Raw, FibexError> {
    let mut rd = quick_xml::Reader::from_str(xml);
    let mut st = State::default();
    let mut text = String::new();
    let err = |rd: &quick_xml::Reader<&[u8]>, msg: String| FibexError::Xml {
        file: file.to_owned(),
        pos: rd.buffer_position(),
        msg,
    };
    loop {
        let ev = rd.read_event().map_err(|e| err(&rd, e.to_string()))?;
        match ev {
            Xml::Start(ref e) | Xml::Empty(ref e) => {
                let name = e.local_name().into_inner().to_owned();
                let attr = |key: &str| {
                    e.attributes()
                        .flatten()
                        .find(|a| a.key.local_name().into_inner() == key)
                        .map(|a| unescape(&a.value))
                };
                st.start(&name, attr);
                text.clear();
                if matches!(ev, Xml::Start(_)) {
                    st.path.push(name);
                } else {
                    st.end(&name, "");
                }
            }
            Xml::Text(t) => text.push_str(&t.xml10_content()),
            Xml::CData(t) => text.push_str(&t.xml10_content()),
            Xml::GeneralRef(r) => {
                let name = r.xml10_content();
                match entity(&name) {
                    Some(c) => text.push(c),
                    None => return Err(err(&rd, format!("unknown entity '&{name};'"))),
                }
            }
            Xml::End(e) => {
                let name = e.local_name().into_inner().to_owned();
                if st.path.last() != Some(&name) {
                    return Err(err(&rd, format!("unexpected end of '{name}'")));
                }
                st.path.pop();
                let t = std::mem::take(&mut text);
                st.end(&name, &t);
            }
            Xml::Eof => break,
            _ => {}
        }
    }
    Ok(st.raw)
}

impl State {
    /// An element `name` started (`path`: its ancestors).
    fn start(&mut self, name: &str, attr: impl Fn(&str) -> Option<String>) {
        let parent = self.path.last().map(String::as_str);
        match name {
            "ECU" if parent == Some("ECUS") => self.raw.ecus.extend(attr("ID")),
            "FRAME" => self.frame = Some(RawFrame { id: attr("ID").unwrap_or_default(), ..Default::default() }),
            "PDU" if parent == Some("PDUS") => self.pdu = Some((attr("ID").unwrap_or_default(), RawPdu::default())),
            "SIGNAL" if parent == Some("SIGNALS") => self.signal = attr("ID"),
            "CODING" if parent == Some("CODINGS") => {
                self.coding = Some((attr("ID").unwrap_or_default(), RawCoding::default()))
            }
            "PDU-INSTANCE" | "SIGNAL-INSTANCE" => self.instance = (0, String::new()),
            "PDU-REF" | "SIGNAL-REF" => self.instance.1 = attr("ID-REF").unwrap_or_default(),
            "CODING-REF" => {
                if let (Some(s), Some(c)) = (&self.signal, attr("ID-REF")) {
                    self.raw.signals.insert(s.clone(), c);
                }
            }
            "CODED-TYPE" => {
                if let Some((_, c)) = &mut self.coding {
                    c.base = attr("BASE-DATA-TYPE").unwrap_or_default();
                }
            }
            _ => {}
        }
    }

    /// An element `name` ended (`path`: its ancestors), with `text` its text.
    fn end(&mut self, name: &str, text: &str) {
        let parent = self.path.last().map(String::as_str);
        match name {
            "FRAME" => self.raw.frames.extend(self.frame.take()),
            "PDU" if parent == Some("PDUS") => {
                if let Some((id, p)) = self.pdu.take() {
                    self.raw.pdus.insert(id, p);
                }
            }
            "SIGNAL" if parent == Some("SIGNALS") => self.signal = None,
            "CODING" if parent == Some("CODINGS") => {
                if let Some((id, c)) = self.coding.take() {
                    self.raw.codings.insert(id, c);
                }
            }
            "SEQUENCE-NUMBER" => self.instance.0 = text.trim().parse().unwrap_or(0),
            "PDU-INSTANCE" => {
                if let Some(f) = &mut self.frame {
                    f.pdus.push(std::mem::take(&mut self.instance));
                }
            }
            "SIGNAL-INSTANCE" => {
                if let Some((_, p)) = &mut self.pdu {
                    p.signals.push(std::mem::take(&mut self.instance));
                }
            }
            "DESC" if parent == Some("PDU") => {
                if let Some((_, p)) = &mut self.pdu {
                    p.desc = text.to_owned();
                }
            }
            "SHORT-NAME" => match (parent, self.frame.as_mut(), self.coding.as_mut()) {
                (Some("FRAME"), Some(f), _) => f.short_name = text.trim().to_owned(),
                (Some("CODING"), _, Some((_, c))) => c.short_name = text.trim().to_owned(),
                _ => {}
            },
            "BIT-LENGTH" => {
                if let Some((_, c)) = &mut self.coding {
                    c.bits = text.trim().parse().ok();
                }
            }
            // Children of the frame's MANUFACTURER-EXTENSION.
            _ if parent == Some("MANUFACTURER-EXTENSION")
                && self.path.len() >= 2
                && self.path[self.path.len() - 2] == "FRAME" =>
            {
                if let Some(f) = &mut self.frame {
                    f.ext.insert(name.to_owned(), text.trim().to_owned());
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;

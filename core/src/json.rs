//! JSON for scripts and agents: a value, a writer and a parser, hand-written
//! (owner, 2026-11-24, `docs/dev/ai.md`: no `serde`, spec §3.1).
//!
//! The CLI's `--json` builds [`Value`]s and writes them on one line each
//! (its `Display`); the MCP bridge (later) parses requests with
//! [`parse`]. Reading JSON *logs* is another thing: `input/json.rs` walks a
//! record's bytes without building a tree.
//!
//! - Objects keep their keys in the order given (output reads the same
//!   every run); [`Value::get`] finds the first of a key.
//! - Numbers are whole (`u64`, `i64`) when they are, so line numbers and
//!   counts stay exact; a float that isn't finite is written `null`.
//! - The parser is strict RFC 8259 (no comments, no trailing commas, one
//!   value, surrogate pairs joined) and refuses nesting deeper than
//!   [`MAX_DEPTH`], so a hostile input can't overflow the stack.

use std::fmt::{self, Write as _};

/// Nesting [`parse`] accepts at most.
pub const MAX_DEPTH: usize = 128;

/// A JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// A whole number ≥ 0.
    UInt(u64),
    /// A whole number < 0.
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    /// Keys in the order given.
    Object(Vec<(String, Value)>),
}

impl Value {
    /// An object from `(key, value)` pairs, in their order.
    pub fn object<K: Into<String>>(pairs: impl IntoIterator<Item = (K, Value)>) -> Value {
        Value::Object(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// An empty object, for [`Value::push`].
    pub fn new_object() -> Value {
        Value::Object(Vec::new())
    }

    /// Add `key: value` to an object (nothing on other values).
    pub fn push(&mut self, key: impl Into<String>, value: impl Into<Value>) -> &mut Value {
        if let Value::Object(pairs) = self {
            pairs.push((key.into(), value.into()));
        }
        self
    }

    /// The value of `key` in an object (its first, if given twice).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// A whole number ≥ 0 (a float only if it is one exactly).
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::UInt(n) => Some(n),
            Value::Float(f) if f >= 0.0 && f.fract() == 0.0 && f < u64::MAX as f64 => Some(f as u64),
            _ => None,
        }
    }

    /// Any number.
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::UInt(n) => Some(n as f64),
            Value::Int(n) => Some(n as f64),
            Value::Float(f) => Some(f),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Write the value compactly (no line breaks) to `out`.
    pub fn write_to(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::UInt(n) => {
                let _ = write!(out, "{n}");
            }
            Value::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Value::Float(f) if !f.is_finite() => out.push_str("null"),
            // A whole float keeps its `.0`, so it reads back as a float.
            Value::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => {
                let _ = write!(out, "{f:.1}");
            }
            Value::Float(f) => {
                let _ = write!(out, "{f}");
            }
            Value::Str(s) => write_str(s, out),
            Value::Array(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write_to(out);
                }
                out.push(']');
            }
            Value::Object(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(k, out);
                    out.push(':');
                    v.write_to(out);
                }
                out.push('}');
            }
        }
    }
}

/// Compact JSON, one line.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = String::new();
        self.write_to(&mut s);
        f.write_str(&s)
    }
}

/// `s` as a JSON string, quotes included.
pub fn string(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    write_str(s, &mut o);
    o
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}

impl From<u64> for Value {
    fn from(n: u64) -> Value {
        Value::UInt(n)
    }
}

impl From<usize> for Value {
    fn from(n: usize) -> Value {
        Value::UInt(n as u64)
    }
}

impl From<u32> for Value {
    fn from(n: u32) -> Value {
        Value::UInt(u64::from(n))
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Value {
        if n >= 0 { Value::UInt(n as u64) } else { Value::Int(n) }
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Value {
        Value::Float(f)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::Str(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Value {
        Value::Str(s)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(o: Option<T>) -> Value {
        o.map_or(Value::Null, Into::into)
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Value {
        Value::Array(v.into_iter().map(Into::into).collect())
    }
}

/// Why [`parse`] refused its input, and where (a byte offset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    pub offset: usize,
    pub msg: &'static str,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.msg, self.offset)
    }
}

impl std::error::Error for JsonError {}

/// One JSON value (whitespace around it allowed, nothing else).
pub fn parse(text: &str) -> Result<Value, JsonError> {
    let mut p = Parser { b: text.as_bytes(), i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i < p.b.len() {
        return Err(p.err("text after the value"));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, msg: &'static str) -> JsonError {
        JsonError { offset: self.i, msg }
    }

    fn ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.b.get(self.i) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self, w: &str, v: Value) -> Result<Value, JsonError> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(self.err("not a JSON value"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth >= MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        match self.b.get(self.i) {
            None => Err(self.err("a value expected, the text ended")),
            Some(b'n') => self.word("null", Value::Null),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'"') => self.string().map(Value::Str),
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat(b']') {
                    return Ok(Value::Array(items));
                }
                loop {
                    self.ws();
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(b']') {
                        return Ok(Value::Array(items));
                    }
                    if !self.eat(b',') {
                        return Err(self.err("`,` or `]` expected"));
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut pairs = Vec::new();
                self.ws();
                if self.eat(b'}') {
                    return Ok(Value::Object(pairs));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return Err(self.err("a key (a string) expected"));
                    }
                    let k = self.string()?;
                    self.ws();
                    if !self.eat(b':') {
                        return Err(self.err("`:` expected"));
                    }
                    self.ws();
                    let v = self.value(depth + 1)?;
                    pairs.push((k, v));
                    self.ws();
                    if self.eat(b'}') {
                        return Ok(Value::Object(pairs));
                    }
                    if !self.eat(b',') {
                        return Err(self.err("`,` or `}` expected"));
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("not a JSON value")),
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while self.b.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.i;
        let neg = self.eat(b'-');
        let int_start = self.i;
        if self.digits() == 0 {
            return Err(self.err("a digit expected"));
        }
        if self.b[int_start] == b'0' && self.i - int_start > 1 {
            return Err(JsonError { offset: int_start, msg: "a number can't start with 0" });
        }
        let mut float = false;
        if self.eat(b'.') {
            float = true;
            if self.digits() == 0 {
                return Err(self.err("a digit expected after `.`"));
            }
        }
        if let Some(b'e' | b'E') = self.b.get(self.i) {
            float = true;
            self.i += 1;
            if let Some(b'+' | b'-') = self.b.get(self.i) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return Err(self.err("a digit expected in the exponent"));
            }
        }
        // Only ASCII was consumed.
        let text = std::str::from_utf8(&self.b[start..self.i]).expect("ASCII");
        if !float {
            if neg {
                if let Ok(n) = text.parse::<i64>() {
                    return Ok(if n == 0 { Value::UInt(0) } else { Value::Int(n) });
                }
            } else if let Ok(n) = text.parse::<u64>() {
                return Ok(Value::UInt(n));
            }
        }
        // Too big for a whole number, or with a fraction or exponent.
        text.parse::<f64>().map(Value::Float).map_err(|_| JsonError { offset: start, msg: "not a number" })
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let h = self.b.get(self.i..self.i + 4).ok_or_else(|| self.err("four hex digits expected after \\u"))?;
        let s = std::str::from_utf8(h).map_err(|_| self.err("four hex digits expected after \\u"))?;
        let n = u32::from_str_radix(s, 16).map_err(|_| self.err("four hex digits expected after \\u"))?;
        if !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(self.err("four hex digits expected after \\u"));
        }
        self.i += 4;
        Ok(n)
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.i += 1; // the opening quote
        let mut out = String::new();
        loop {
            // A run of plain bytes at once (the input is valid UTF-8).
            let start = self.i;
            while let Some(&c) = self.b.get(self.i) {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.b[start..self.i]).expect("cut at ASCII bytes"));
            match self.b.get(self.i) {
                None => return Err(self.err("the string isn't closed")),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let e = *self.b.get(self.i).ok_or_else(|| self.err("the string isn't closed"))?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let at = self.i - 2;
                            let hi = self.hex4()?;
                            let c = match hi {
                                0xD800..=0xDBFF => {
                                    if !(self.eat(b'\\') && self.eat(b'u')) {
                                        return Err(JsonError { offset: at, msg: "a lone surrogate" });
                                    }
                                    let lo = self.hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&lo) {
                                        return Err(JsonError { offset: at, msg: "a lone surrogate" });
                                    }
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                }
                                0xDC00..=0xDFFF => return Err(JsonError { offset: at, msg: "a lone surrogate" }),
                                c => c,
                            };
                            out.push(char::from_u32(c).expect("a scalar value"));
                        }
                        _ => return Err(JsonError { offset: self.i - 1, msg: "an unknown escape" }),
                    }
                }
                Some(_) => return Err(self.err("a control character in a string (write it \\u00XX)")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_compact_in_key_order() {
        let mut v = Value::new_object();
        v.push("line", 3u64).push("raw", "a \"b\"\n\u{1}").push("t", 1.5).push("n", Value::Null);
        v.push("list", vec![1u64, 2]).push("neg", -4i64).push("whole", 2.0).push("nan", f64::NAN);
        assert_eq!(
            v.to_string(),
            r#"{"line":3,"raw":"a \"b\"\n\u0001","t":1.5,"n":null,"list":[1,2],"neg":-4,"whole":2.0,"nan":null}"#
        );
        assert_eq!(string("żółw\t"), "\"żółw\\t\"");
    }

    #[test]
    fn parses_what_it_writes() {
        let v = Value::object([
            ("a", Value::from(u64::MAX)),
            ("b", Value::from(-7i64)),
            ("c", Value::from(0.25)),
            ("d", Value::from("x\u{0}y\"\\é😀")),
            ("e", Value::Array(vec![Value::Null, true.into(), Value::new_object()])),
        ]);
        assert_eq!(parse(&v.to_string()).unwrap(), v);
    }

    #[test]
    fn parses_json_rpc() {
        let v = parse(r#" {"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"count","arguments":{}}} "#)
            .unwrap();
        assert_eq!(v.get("id").and_then(Value::as_u64), Some(1));
        assert_eq!(v.get("method").and_then(Value::as_str), Some("tools/call"));
        assert_eq!(v.get("params").and_then(|p| p.get("name")).and_then(Value::as_str), Some("count"));
    }

    #[test]
    fn numbers() {
        assert_eq!(parse("0").unwrap(), Value::UInt(0));
        assert_eq!(parse("-0").unwrap(), Value::UInt(0));
        assert_eq!(parse("18446744073709551616").unwrap(), Value::Float(18446744073709551616.0));
        assert_eq!(parse("1e3").unwrap(), Value::Float(1000.0));
        assert_eq!(parse("-1.5E-2").unwrap(), Value::Float(-0.015));
        assert_eq!(parse("2.0").unwrap().as_u64(), Some(2));
    }

    #[test]
    fn escapes_and_surrogates() {
        assert_eq!(parse(r#""😀 é \/ \b""#).unwrap(), Value::Str("😀 é / \u{8}".into()));
        assert_eq!(parse(r#""\ud83d""#).unwrap_err().msg, "a lone surrogate");
        assert_eq!(parse(r#""\ude00""#).unwrap_err().msg, "a lone surrogate");
        assert_eq!(parse(r#""\u12g4""#).unwrap_err().msg, "four hex digits expected after \\u");
        assert_eq!(parse(r#""\u+123""#).unwrap_err().msg, "four hex digits expected after \\u");
    }

    #[test]
    fn errors_say_where() {
        let e = |s: &str| parse(s).unwrap_err();
        assert_eq!(e("[1,]"), JsonError { offset: 3, msg: "not a JSON value" });
        assert_eq!(e("{\"a\" 1}"), JsonError { offset: 5, msg: "`:` expected" });
        assert_eq!(e("{a:1}"), JsonError { offset: 1, msg: "a key (a string) expected" });
        assert_eq!(e("01"), JsonError { offset: 0, msg: "a number can't start with 0" });
        assert_eq!(e("1 2"), JsonError { offset: 2, msg: "text after the value" });
        assert_eq!(e("\"a\nb\""), JsonError { offset: 2, msg: "a control character in a string (write it \\u00XX)" });
        assert_eq!(e("\"abc"), JsonError { offset: 4, msg: "the string isn't closed" });
        assert_eq!(e(""), JsonError { offset: 0, msg: "a value expected, the text ended" });
        assert_eq!(e("nul"), JsonError { offset: 0, msg: "not a JSON value" });
        assert_eq!(e("-"), JsonError { offset: 1, msg: "a digit expected" });
        assert_eq!(e("1."), JsonError { offset: 2, msg: "a digit expected after `.`" });
        assert_eq!(e("[1 2]"), JsonError { offset: 3, msg: "`,` or `]` expected" });
    }

    #[test]
    fn deep_nesting_is_refused_not_a_crash() {
        let deep = "[".repeat(100_000);
        assert_eq!(parse(&deep).unwrap_err().msg, "nested too deeply");
        let ok = format!("{}{}", "[".repeat(MAX_DEPTH - 1), "]".repeat(MAX_DEPTH - 1));
        assert!(parse(&ok).is_ok());
    }
}

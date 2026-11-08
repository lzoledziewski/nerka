//! The YAML that `.ksy` files use: block mappings and sequences, flow
//! collections (`{ a: 1 }`, `[1, 2]`, over several lines too), plain and
//! quoted scalars, `|` / `>` blocks and comments. Not read: anchors,
//! aliases, tags, several documents, complex keys. Scalars stay text; the
//! reader of the definition decides what they are.
//!
//! Positions are 1-based lines and columns, for error messages.

use std::fmt;

/// A YAML value with where it starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub value: Value,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Nothing after a key or dash.
    Null,
    /// Text as written; `quoted` if it was in quotes (so `'1'` is no number).
    Scalar {
        text: String,
        quoted: bool,
    },
    Seq(Vec<Node>),
    /// Entries in file order (keys are unique).
    Map(Vec<(Key, Node)>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub text: String,
    pub line: usize,
    pub col: usize,
}

/// What is wrong, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlError {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

/// Files larger than this aren't definitions.
pub const MAX_BYTES: usize = 1 << 20;
const MAX_DEPTH: usize = 64;

/// Read a whole document.
pub fn parse(text: &str) -> Result<Node, YamlError> {
    if text.len() > MAX_BYTES {
        return Err(YamlError { line: 1, col: 1, message: format!("larger than {} KiB", MAX_BYTES >> 10) });
    }
    let mut p = Parser { lines: lines(text)?, at: 0 };
    p.skip_document_start();
    if p.at >= p.lines.len() {
        return Ok(Node { value: Value::Null, line: 1, col: 1 });
    }
    let indent = p.lines[p.at].indent;
    let node = p.block(indent, 0)?;
    if let Some(l) = p.lines.get(p.at) {
        return Err(l.error(l.indent, "unexpected text (indented less than its block, or a second document)"));
    }
    Ok(node)
}

/// One line without its comment: `indent` spaces, then `text` (trimmed at
/// the end); `raw` keeps the whole line for `|` blocks.
#[derive(Debug, Clone)]
struct Line {
    no: usize,
    indent: usize,
    text: String,
    raw: String,
    /// Nothing but spaces and a comment.
    blank: bool,
}

impl Line {
    fn error(&self, col0: usize, message: impl Into<String>) -> YamlError {
        YamlError { line: self.no, col: col0 + 1, message: message.into() }
    }
}

fn lines(text: &str) -> Result<Vec<Line>, YamlError> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let no = i + 1;
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let indent = raw.len() - raw.trim_start_matches(' ').len();
        if raw[indent..].starts_with('\t') {
            return Err(YamlError { line: no, col: indent + 1, message: "tabs can't indent YAML".into() });
        }
        let body = strip_comment(&raw[indent..]).trim_end().to_owned();
        out.push(Line { no, indent, blank: body.is_empty(), text: body, raw: raw.to_owned() });
    }
    Ok(out)
}

/// `text` without a `#` comment (one at the start, or after a space,
/// outside quotes).
fn strip_comment(text: &str) -> &str {
    let b = text.as_bytes();
    let (mut single, mut double) = (false, false);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\'' if !double => single = !single,
            b'"' if !single => double = !double,
            b'\\' if double => i += 1,
            b'#' if !single && !double && (i == 0 || b[i - 1] == b' ' || b[i - 1] == b'\t') => return &text[..i],
            _ => {}
        }
        i += 1;
    }
    text
}

struct Parser {
    lines: Vec<Line>,
    at: usize,
}

impl Parser {
    fn skip_blank(&mut self) {
        while self.lines.get(self.at).is_some_and(|l| l.blank) {
            self.at += 1;
        }
    }

    fn skip_document_start(&mut self) {
        self.skip_blank();
        if self.lines.get(self.at).is_some_and(|l| l.indent == 0 && (l.text == "---" || l.text.starts_with("--- "))) {
            let l = &mut self.lines[self.at];
            if l.text == "---" {
                self.at += 1;
            } else {
                l.text = l.text[4..].trim_start().to_owned();
                l.indent = 4;
            }
        }
        self.skip_blank();
    }

    /// The block that starts at the current line, indented `indent`.
    fn block(&mut self, indent: usize, depth: usize) -> Result<Node, YamlError> {
        self.skip_blank();
        let Some(l) = self.lines.get(self.at) else { return Ok(Node { value: Value::Null, line: 0, col: 0 }) };
        if depth > MAX_DEPTH {
            return Err(l.error(l.indent, "nested too deeply"));
        }
        if is_dash(&l.text) {
            self.seq(indent, depth)
        } else if map_key(&l.text).is_some() {
            self.map(indent, depth)
        } else {
            let (no, col, text) = (l.no, l.indent, l.text.clone());
            self.at += 1;
            let node = if text.starts_with('[') || text.starts_with('{') {
                self.flow(no, col, text, indent, depth)?
            } else {
                scalar(&text, no, col)?
            };
            self.skip_blank();
            if let Some(l) = self.lines.get(self.at)
                && l.indent > indent
            {
                return Err(l.error(l.indent, "a plain value can't continue on the next line here; quote it"));
            }
            Ok(node)
        }
    }

    fn seq(&mut self, indent: usize, depth: usize) -> Result<Node, YamlError> {
        let (line, col) = (self.lines[self.at].no, self.lines[self.at].indent + 1);
        let mut items = Vec::new();
        loop {
            self.skip_blank();
            let Some(l) = self.lines.get(self.at) else { break };
            if l.indent != indent || !is_dash(&l.text) {
                if l.indent > indent {
                    return Err(l.error(l.indent, "indented more than the items before it"));
                }
                break;
            }
            let rest = l.text[1..].trim_start().to_owned();
            if rest.is_empty() {
                let (no, dash) = (l.no, l.indent);
                self.at += 1;
                self.skip_blank();
                match self.lines.get(self.at) {
                    Some(n) if n.indent > indent => {
                        let i = n.indent;
                        items.push(self.block(i, depth + 1)?);
                    }
                    _ => items.push(Node { value: Value::Null, line: no, col: dash + 2 }),
                }
            } else {
                // The item starts on the dash's line: read it as if it
                // were a line of its own, indented where it starts.
                let col = l.indent + (l.text.len() - rest.len());
                let l = &mut self.lines[self.at];
                l.indent = col;
                l.text = rest;
                items.push(self.block(col, depth + 1)?);
            }
        }
        Ok(Node { value: Value::Seq(items), line, col })
    }

    fn map(&mut self, indent: usize, depth: usize) -> Result<Node, YamlError> {
        let (line, col) = (self.lines[self.at].no, self.lines[self.at].indent + 1);
        let mut entries: Vec<(Key, Node)> = Vec::new();
        loop {
            self.skip_blank();
            let Some(l) = self.lines.get(self.at) else { break };
            if l.indent != indent {
                if l.indent > indent {
                    return Err(l.error(l.indent, "indented more than the keys before it"));
                }
                break;
            }
            if is_dash(&l.text) {
                break;
            }
            let Some((key, rest)) = map_key(&l.text) else {
                return Err(l.error(l.indent, "expected `key: value`"));
            };
            let key = Key { text: unquote_key(&key, l.no, l.indent)?, line: l.no, col: l.indent + 1 };
            if entries.iter().any(|(k, _)| k.text == key.text) {
                return Err(l.error(l.indent, format!("`{}` is given twice", key.text)));
            }
            let (no, at_col) = (l.no, l.indent + (l.text.len() - rest.len()));
            self.at += 1;
            let value = if rest.is_empty() {
                self.skip_blank();
                match self.lines.get(self.at) {
                    Some(n) if n.indent > indent => {
                        let i = n.indent;
                        self.block(i, depth + 1)?
                    }
                    // A sequence may sit at its key's indent.
                    Some(n) if n.indent == indent && is_dash(&n.text) => self.seq(indent, depth + 1)?,
                    _ => Node { value: Value::Null, line: no, col: at_col + 1 },
                }
            } else if rest.starts_with('|') || rest.starts_with('>') {
                self.block_scalar(&rest, no, at_col, indent)?
            } else if rest.starts_with('[') || rest.starts_with('{') {
                self.flow(no, at_col, rest, indent, depth)?
            } else {
                let node = scalar(&rest, no, at_col)?;
                self.skip_blank();
                if let Some(n) = self.lines.get(self.at)
                    && n.indent > indent
                {
                    return Err(n.error(n.indent, "a plain value can't continue on the next line here; quote it"));
                }
                node
            };
            entries.push((key, value));
        }
        Ok(Node { value: Value::Map(entries), line, col })
    }

    /// `|` (lines kept) or `>` (lines joined by spaces): the lines indented
    /// more than the key.
    fn block_scalar(&mut self, head: &str, no: usize, col: usize, indent: usize) -> Result<Node, YamlError> {
        let style = head.as_bytes()[0];
        if !head[1..].chars().all(|c| matches!(c, '-' | '+')) {
            return Err(YamlError { line: no, col: col + 2, message: "indentation indicators aren't read".into() });
        }
        let keep_end = head.contains('+');
        let strip_end = head.contains('-');
        let mut body: Vec<&str> = Vec::new();
        let mut inner = None;
        while let Some(l) = self.lines.get(self.at) {
            let raw_indent = l.raw.len() - l.raw.trim_start_matches(' ').len();
            if l.raw.trim().is_empty() {
                body.push("");
                self.at += 1;
                continue;
            }
            if raw_indent <= indent {
                break;
            }
            let i = *inner.get_or_insert(raw_indent);
            if raw_indent < i {
                break;
            }
            body.push(&l.raw[i..]);
            self.at += 1;
        }
        while !keep_end && body.last() == Some(&"") {
            body.pop();
        }
        let mut text = if style == b'|' { body.join("\n") } else { fold(&body) };
        if !strip_end && !text.is_empty() {
            text.push('\n');
        }
        Ok(Node { value: Value::Scalar { text, quoted: true }, line: no, col: col + 1 })
    }

    /// A flow collection starting with `first` (on line `no`, column
    /// `col`); continues on the following lines until it is closed.
    fn flow(&mut self, no: usize, col: usize, first: String, indent: usize, depth: usize) -> Result<Node, YamlError> {
        let mut text = first;
        let mut starts = vec![(0usize, no, col)];
        while !balanced(&text) {
            let Some(l) = self.lines.get(self.at) else {
                return Err(YamlError { line: no, col: col + 1, message: "not closed".into() });
            };
            if !l.blank && l.indent <= indent {
                return Err(YamlError { line: no, col: col + 1, message: "not closed".into() });
            }
            text.push(' ');
            starts.push((text.len(), l.no, l.indent));
            text.push_str(&l.text);
            self.at += 1;
        }
        let mut f = Flow { s: text.as_bytes(), at: 0, starts: &starts, depth };
        let node = f.node()?;
        f.space();
        if f.at < f.s.len() {
            return Err(f.error("unexpected text after the closing bracket"));
        }
        Ok(node)
    }
}

fn fold(lines: &[&str]) -> String {
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push(if l.is_empty() || lines[i - 1].is_empty() { '\n' } else { ' ' });
        }
        out.push_str(l);
    }
    out
}

/// Are the brackets outside quotes closed?
fn balanced(text: &str) -> bool {
    let (mut depth, mut single, mut double, mut esc) = (0i32, false, false, false);
    for c in text.chars() {
        match c {
            _ if esc => esc = false,
            '\\' if double => esc = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '[' | '{' if !single && !double => depth += 1,
            ']' | '}' if !single && !double => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

fn is_dash(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

/// `key: rest` (`rest` may be empty): the key's text as written, if the
/// line is a mapping entry.
fn map_key(text: &str) -> Option<(String, String)> {
    if text.starts_with('[') || text.starts_with('{') {
        return None;
    }
    let b = text.as_bytes();
    let end = if b.first().is_some_and(|&c| c == b'\'' || c == b'"') { quoted_end(b, 0)? } else { 0 };
    let mut i = end;
    while i < b.len() {
        if b[i] == b':' && (i + 1 == b.len() || b[i + 1] == b' ') {
            return Some((text[..i].trim_end().to_owned(), text[i + 1..].trim_start().to_owned()));
        }
        if end > 0 {
            return None;
        }
        i += 1;
    }
    None
}

/// Index just past the closing quote of the string starting at `b[at]`.
fn quoted_end(b: &[u8], at: usize) -> Option<usize> {
    let q = b[at];
    let mut i = at + 1;
    while i < b.len() {
        match b[i] {
            b'\\' if q == b'"' => i += 1,
            b'\'' if q == b'\'' && b.get(i + 1) == Some(&b'\'') => i += 1,
            c if c == q => return Some(i + 1),
            _ => {}
        }
        i += 1;
    }
    None
}

fn unquote_key(key: &str, no: usize, col: usize) -> Result<String, YamlError> {
    match scalar(key, no, col)?.value {
        Value::Scalar { text, .. } => Ok(text),
        _ => Err(YamlError { line: no, col: col + 1, message: "a key must be a scalar".into() }),
    }
}

/// A scalar written on one line.
fn scalar(text: &str, no: usize, col: usize) -> Result<Node, YamlError> {
    let err = |message: &str| YamlError { line: no, col: col + 1, message: message.into() };
    let value = match text.as_bytes().first() {
        None => Value::Null,
        Some(b'\'' | b'"') => {
            let end = quoted_end(text.as_bytes(), 0).ok_or_else(|| err("the quote isn't closed"))?;
            if !text[end..].trim().is_empty() {
                return Err(err("text after the closing quote"));
            }
            Value::Scalar { text: unescape(&text[..end]).map_err(|m| err(&m))?, quoted: true }
        }
        Some(b'&') => return Err(err("anchors aren't read")),
        Some(b'*') => return Err(err("aliases aren't read")),
        Some(b'!') => return Err(err("tags aren't read")),
        _ if text == "~" || text == "null" || text == "Null" || text == "NULL" => Value::Null,
        _ => Value::Scalar { text: text.to_owned(), quoted: false },
    };
    Ok(Node { value, line: no, col: col + 1 })
}

/// The text of a quoted scalar (quotes included).
fn unescape(q: &str) -> Result<String, String> {
    let inner = &q[1..q.len() - 1];
    if q.starts_with('\'') {
        return Ok(inner.replace("''", "'"));
    }
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let e = chars.next().ok_or("a `\\` ends the string")?;
        out.push(match e {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '0' => '\0',
            '\\' => '\\',
            '"' => '"',
            '/' => '/',
            ' ' => ' ',
            'x' | 'u' => {
                let n = if e == 'x' { 2 } else { 4 };
                let hex: String = chars.by_ref().take(n).collect();
                let v = u32::from_str_radix(&hex, 16).map_err(|_| format!("`\\{e}{hex}` isn't an escape"))?;
                char::from_u32(v).ok_or_else(|| format!("`\\{e}{hex}` isn't a character"))?
            }
            _ => return Err(format!("`\\{e}` isn't an escape")),
        });
    }
    Ok(out)
}

/// A flow collection's text, joined from its lines.
struct Flow<'a> {
    s: &'a [u8],
    at: usize,
    /// Where each joined line starts in `s`: (offset, line, column).
    starts: &'a [(usize, usize, usize)],
    depth: usize,
}

impl Flow<'_> {
    fn pos(&self, at: usize) -> (usize, usize) {
        let &(off, line, col) = self.starts.iter().rev().find(|(o, _, _)| *o <= at).unwrap_or(&self.starts[0]);
        (line, col + (at - off) + 1)
    }

    fn error(&self, message: &str) -> YamlError {
        let (line, col) = self.pos(self.at);
        YamlError { line, col, message: message.into() }
    }

    fn space(&mut self) {
        while self.s.get(self.at) == Some(&b' ') {
            self.at += 1;
        }
    }

    fn node(&mut self) -> Result<Node, YamlError> {
        self.space();
        if self.depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        let (line, col) = self.pos(self.at);
        match self.s.get(self.at) {
            Some(b'[') => {
                self.at += 1;
                self.depth += 1;
                let mut items = Vec::new();
                loop {
                    self.space();
                    if self.s.get(self.at) == Some(&b']') {
                        self.at += 1;
                        break;
                    }
                    items.push(self.node()?);
                    self.space();
                    match self.s.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b']') => {}
                        _ => return Err(self.error("expected `,` or `]`")),
                    }
                }
                self.depth -= 1;
                Ok(Node { value: Value::Seq(items), line, col })
            }
            Some(b'{') => {
                self.at += 1;
                self.depth += 1;
                let mut entries: Vec<(Key, Node)> = Vec::new();
                loop {
                    self.space();
                    if self.s.get(self.at) == Some(&b'}') {
                        self.at += 1;
                        break;
                    }
                    let (kl, kc) = self.pos(self.at);
                    let key = match self.scalar(true)?.value {
                        Value::Scalar { text, .. } => text,
                        _ => return Err(self.error("expected a key")),
                    };
                    if entries.iter().any(|(k, _)| k.text == key) {
                        return Err(YamlError { line: kl, col: kc, message: format!("`{key}` is given twice") });
                    }
                    self.space();
                    let value = if self.s.get(self.at) == Some(&b':') {
                        self.at += 1;
                        self.space();
                        match self.s.get(self.at) {
                            Some(b',' | b'}') => Node { value: Value::Null, line: kl, col: kc },
                            _ => self.node()?,
                        }
                    } else {
                        Node { value: Value::Null, line: kl, col: kc }
                    };
                    entries.push((Key { text: key, line: kl, col: kc }, value));
                    self.space();
                    match self.s.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {}
                        _ => return Err(self.error("expected `,` or `}`")),
                    }
                }
                self.depth -= 1;
                Ok(Node { value: Value::Map(entries), line, col })
            }
            _ => self.scalar(false),
        }
    }

    /// A scalar inside a flow collection: quoted, or plain up to `,`, `]`,
    /// `}` (and `: ` for a key).
    fn scalar(&mut self, key: bool) -> Result<Node, YamlError> {
        self.space();
        let (line, col) = self.pos(self.at);
        let start = self.at;
        if let Some(&q) = self.s.get(self.at)
            && (q == b'\'' || q == b'"')
        {
            let end = quoted_end(self.s, self.at).ok_or_else(|| self.error("the quote isn't closed"))?;
            self.at = end;
            let text = std::str::from_utf8(&self.s[start..end]).unwrap_or_default();
            let text = unescape(text).map_err(|m| self.error(&m))?;
            return Ok(Node { value: Value::Scalar { text, quoted: true }, line, col });
        }
        while let Some(&c) = self.s.get(self.at) {
            let ends_key = key && c == b':' && matches!(self.s.get(self.at + 1), None | Some(b' ' | b',' | b'}'));
            if matches!(c, b',' | b']' | b'}') || ends_key {
                break;
            }
            if matches!(c, b'[' | b'{') {
                return Err(self.error("a bracket inside a plain value; quote the value"));
            }
            self.at += 1;
        }
        let text = std::str::from_utf8(&self.s[start..self.at]).unwrap_or_default().trim();
        if text.is_empty() {
            return Err(self.error("expected a value"));
        }
        let n = scalar(text, line, col - 1)?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests;

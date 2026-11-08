//! Text templates (`-nerka-text` on a type, `-nerka-partial` on an
//! attribute): how decoded values read as text. Nerka's own; Kaitai's
//! compiler ignores the keys, and the format hints are those of Kaitai's
//! Web IDE (`:dec`, `:hex`, `:sep=`, `:str`).
//!
//! ```text
//! template := piece*
//! piece    := TEXT | '{' EXPR (':' FORMAT)? '}' | '[' template ('|' template)* ']'
//! FORMAT   := dec | sdec | hex | str | name | sep=TEXT
//! ```
//!
//! `{expr}` is a Kaitai expression in the type's context, shown as its
//! value reads (numbers in decimal, `str` text as text, bytes as hex, a
//! user type by its own template, a list joined by spaces). `[a|b]` shows
//! the first alternative whose values are all there and not empty, or
//! nothing. `{{`, `}}`, `[[`, `]]` and `||` stand for the character itself.

use super::expr::{self, Expr};

/// How a value is shown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Fmt {
    #[default]
    Plain,
    /// Numbers in decimal; bytes as one unsigned number (the type's byte
    /// order).
    Dec,
    /// Bytes as one signed number.
    SDec,
    /// Numbers as `0x1f`; bytes as `1f 02`.
    Hex,
    /// Text: a `str` decoded as UTF-8, bytes as they are; one trailing
    /// NUL dropped.
    Str,
    /// An enum value's name; nothing if it has none.
    Name,
    /// A list joined by this text.
    Sep(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Piece<E> {
    Text(String),
    Value(E, Fmt),
    /// Alternatives: the first whose values are all there.
    Group(Vec<Vec<Piece<E>>>),
}

/// A parsed template: expressions with their offset in the text.
pub type Parsed = Vec<Piece<(Expr, usize)>>;

/// What is wrong, at a byte offset of the template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError {
    pub at: usize,
    pub message: String,
}

const MAX_DEPTH: usize = 16;

pub fn parse(text: &str) -> Result<Parsed, TemplateError> {
    if text.len() > expr::MAX_LEN {
        return Err(TemplateError { at: 0, message: format!("longer than {} characters", expr::MAX_LEN) });
    }
    let mut p = Parser { s: text, at: 0 };
    let (pieces, end) = p.pieces(0)?;
    match end {
        End::Eof => Ok(pieces),
        End::Close => {
            Err(TemplateError { at: p.at - 1, message: "`]` without `[` (write `]]` for the character)".into() })
        }
        End::Bar => {
            Err(TemplateError { at: p.at - 1, message: "`|` outside `[…]` (write `||` for the character)".into() })
        }
    }
}

enum End {
    Eof,
    Close,
    Bar,
}

struct Parser<'a> {
    s: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn err(&self, at: usize, message: impl Into<String>) -> TemplateError {
        TemplateError { at, message: message.into() }
    }

    fn pieces(&mut self, depth: usize) -> Result<(Parsed, End), TemplateError> {
        if depth > MAX_DEPTH {
            return Err(self.err(self.at, "`[…]` nested too deeply"));
        }
        let b = self.s.as_bytes();
        let mut out: Parsed = Vec::new();
        let mut text = String::new();
        let flush = |text: &mut String, out: &mut Parsed| {
            if !text.is_empty() {
                out.push(Piece::Text(std::mem::take(text)));
            }
        };
        while self.at < b.len() {
            let c = b[self.at];
            let doubled = b.get(self.at + 1) == Some(&c);
            // In a run of `]` inside a group, the group closes first:
            // `{x}]]]` is the end of the group, then a `]`.
            let run = b[self.at..].iter().take_while(|&&x| x == b']').count();
            match c {
                b']' if depth > 0 && run % 2 == 1 => {
                    self.at += 1;
                    flush(&mut text, &mut out);
                    return Ok((out, End::Close));
                }
                b'{' | b'}' | b'[' | b']' | b'|' if doubled => {
                    text.push(c as char);
                    self.at += 2;
                }
                b'{' => {
                    flush(&mut text, &mut out);
                    let start = self.at + 1;
                    let end = close_brace(b, start).ok_or_else(|| self.err(self.at, "`{` isn't closed"))?;
                    let inner = &self.s[start..end];
                    let (expr_text, fmt) = split_format(inner);
                    let e = expr::parse(expr_text).map_err(|e| self.err(start + e.at, e.message))?;
                    out.push(Piece::Value((e, start), fmt));
                    self.at = end + 1;
                }
                b'}' => return Err(self.err(self.at, "`}` without `{` (write `}}` for the character)")),
                b'[' => {
                    flush(&mut text, &mut out);
                    let open = self.at;
                    self.at += 1;
                    let mut alts = Vec::new();
                    loop {
                        let (alt, end) = self.pieces(depth + 1)?;
                        alts.push(alt);
                        match end {
                            End::Bar => continue,
                            End::Close => break,
                            End::Eof => return Err(self.err(open, "`[` isn't closed")),
                        }
                    }
                    out.push(Piece::Group(alts));
                }
                b']' => {
                    self.at += 1;
                    flush(&mut text, &mut out);
                    return Ok((out, End::Close));
                }
                b'|' if depth > 0 => {
                    self.at += 1;
                    flush(&mut text, &mut out);
                    return Ok((out, End::Bar));
                }
                b'|' => return Err(self.err(self.at, "`|` outside `[…]` (write `||` for the character)")),
                _ => {
                    let ch = self.s[self.at..].chars().next().unwrap_or(' ');
                    text.push(ch);
                    self.at += ch.len_utf8();
                }
            }
        }
        flush(&mut text, &mut out);
        Ok((out, End::Eof))
    }
}

/// The `}` closing the placeholder that starts at `from`, outside quotes.
fn close_brace(b: &[u8], from: usize) -> Option<usize> {
    let mut quote = None;
    let mut i = from;
    while i < b.len() {
        match (quote, b[i]) {
            (None, b'\'' | b'"') => quote = Some(b[i]),
            (Some(q), c) if c == q => quote = None,
            (Some(b'"'), b'\\') => i += 1,
            (None, b'}') => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// `expr:format` → the expression and its format, when the text after the
/// last `:` (outside quotes) is one; else all of it is the expression
/// (`a ? b : c`).
fn split_format(inner: &str) -> (&str, Fmt) {
    let b = inner.as_bytes();
    let mut quote = None;
    let mut last = None;
    let mut i = 0;
    while i < b.len() {
        match (quote, b[i]) {
            (None, b'\'' | b'"') => quote = Some(b[i]),
            (Some(q), c) if c == q => quote = None,
            (Some(b'"'), b'\\') => i += 1,
            // `::` is an enum, not a format.
            (None, b':') if b.get(i + 1) == Some(&b':') => i += 1,
            (None, b':') if i > 0 && b[i - 1] != b':' => last = Some(i),
            _ => {}
        }
        i += 1;
    }
    let Some(at) = last else { return (inner, Fmt::Plain) };
    let tail = &inner[at + 1..];
    let fmt = match tail.trim() {
        "dec" => Fmt::Dec,
        "sdec" => Fmt::SDec,
        "hex" => Fmt::Hex,
        "str" => Fmt::Str,
        "name" => Fmt::Name,
        _ if tail.starts_with("sep=") => Fmt::Sep(tail[4..].to_owned()),
        _ => return (inner, Fmt::Plain),
    };
    (&inner[..at], fmt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(p: &Parsed) -> String {
        p.iter()
            .map(|piece| match piece {
                Piece::Text(t) => format!("'{t}'"),
                Piece::Value((e, at), f) => format!("<{e:?}@{at}:{f:?}>"),
                Piece::Group(alts) => {
                    format!("[{}]", alts.iter().map(show).collect::<Vec<_>>().join("|"))
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn pieces_formats_and_groups() {
        let p = parse("[{name:str}=]{v}[ {unit:str}]").unwrap();
        assert_eq!(show(&p), "[<Name(\"name\")@2:Str> '='] <Name(\"v\")@14:Plain> [' ' <Name(\"unit\")@19:Str>]");
        let p = parse("[[[{sid:name}|service {sid:hex}]]]").unwrap();
        assert_eq!(show(&p), "'[' [<Name(\"sid\")@4:Name>|'service ' <Name(\"sid\")@23:Hex>] ']'");
        let p = parse("{args:sep=, }").unwrap();
        assert_eq!(show(&p), "<Name(\"args\")@1:Sep(\", \")>");
        // A conditional expression keeps its `:`; an enum its `::`.
        let p = parse("{a ? 'x' : 'y'}{k == e::v:dec}").unwrap();
        assert!(show(&p).contains("Cond") && show(&p).contains("EnumLit") && show(&p).ends_with(":Dec>"));
    }

    #[test]
    fn errors_say_where() {
        assert_eq!(parse("a {b").unwrap_err(), TemplateError { at: 2, message: "`{` isn't closed".into() });
        assert_eq!(parse("[a").unwrap_err().message, "`[` isn't closed");
        assert_eq!(parse("a]").unwrap_err().at, 1);
        assert_eq!(parse("a|b").unwrap_err().at, 1);
        assert_eq!(parse("{a +}").unwrap_err(), TemplateError { at: 4, message: "expected a value".into() });
        assert_eq!(parse(&"[a".repeat(40)).unwrap_err().message, "`[…]` nested too deeply");
    }
}

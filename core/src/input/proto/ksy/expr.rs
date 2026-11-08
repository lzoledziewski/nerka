//! Kaitai Struct's expression language (the part definitions use): the
//! parse tree, before names are bound ([`super::model`]).
//!
//! ```text
//! expr    := or ('?' expr ':' expr)?
//! or      := and ('or' and)*
//! and     := not ('and' not)*
//! not     := 'not' not | cmp
//! cmp     := bitor (('==' | '!=' | '<' | '<=' | '>' | '>=') bitor)?
//! bitor   := bitxor ('|' bitxor)*
//! bitxor  := bitand ('^' bitand)*
//! bitand  := shift ('&' shift)*
//! shift   := sum (('<<' | '>>') sum)*
//! sum     := term (('+' | '-') term)*
//! term    := unary (('*' | '/' | '%') unary)*
//! unary   := ('-' | '~') unary | postfix
//! postfix := atom ('.' NAME | '[' expr ']')*
//! atom    := INT | FLOAT | STRING | 'true' | 'false' | NAME | '(' expr ')'
//! ```
//!
//! As in Kaitai (and Python), comparisons bind more loosely than bit
//! operations: `a & 4 != 0` is `(a & 4) != 0`. Integers: decimal, `0x`,
//! `0b`, `0o`, with `_` between digits.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Vec<u8>),
    /// A bare name: an attribute, instance, `_root`, `_parent`, `_io`, `_`.
    Name(String),
    /// `a.b`
    Attr(Box<Expr>, String),
    /// `a[i]`
    Index(Box<Expr>, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `c ? a : b`
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    BitNot,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    BitOr,
    BitXor,
    BitAnd,
    Shl,
    Shr,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

/// What is wrong, at a byte offset of the expression's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprError {
    pub at: usize,
    pub message: String,
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at character {})", self.message, self.at + 1)
    }
}

const MAX_DEPTH: usize = 64;
/// Longer expressions aren't definitions.
pub const MAX_LEN: usize = 4096;

pub fn parse(text: &str) -> Result<Expr, ExprError> {
    if text.len() > MAX_LEN {
        return Err(ExprError { at: 0, message: format!("longer than {MAX_LEN} characters") });
    }
    let tokens = lex(text)?;
    let mut p = Parser { tokens: &tokens, at: 0, depth: 0, end: text.len() };
    let e = p.expr()?;
    if let Some(t) = p.tokens.get(p.at) {
        return Err(ExprError { at: t.at, message: format!("unexpected {}", t.tok) });
    }
    Ok(e)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Int(i64),
    Float(f64),
    Str(Vec<u8>),
    Name(String),
    Op(&'static str),
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Int(v) => write!(f, "number {v}"),
            Tok::Float(v) => write!(f, "number {v}"),
            Tok::Str(_) => write!(f, "string"),
            Tok::Name(n) => write!(f, "`{n}`"),
            Tok::Op(o) => write!(f, "`{o}`"),
        }
    }
}

struct Token {
    tok: Tok,
    at: usize,
}

const OPS: [&str; 26] = [
    "<<", ">>", "<=", ">=", "==", "!=", "::", "(", ")", "[", "]", ".", ",", "?", ":", "<", ">", "|", "^", "&", "+",
    "-", "*", "/", "%", "~",
];

fn lex(text: &str) -> Result<Vec<Token>, ExprError> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let at = i;
        if c.is_ascii_digit() {
            let (tok, n) = number(&text[i..]).map_err(|message| ExprError { at, message })?;
            out.push(Token { tok, at });
            i += n;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Token { tok: Tok::Name(text[at..i].to_owned()), at });
        } else if c == b'\'' || c == b'"' {
            let (s, n) = string(&b[i..]).map_err(|message| ExprError { at, message })?;
            out.push(Token { tok: Tok::Str(s), at });
            i += n;
        } else if let Some(op) = OPS.iter().find(|o| b[i..].starts_with(o.as_bytes())) {
            if *op == "::" {
                return Err(ExprError { at, message: "enums (`::`) aren't read yet".into() });
            }
            out.push(Token { tok: Tok::Op(op), at });
            i += op.len();
        } else {
            let ch = text[i..].chars().next().unwrap_or('?');
            return Err(ExprError { at, message: format!("unexpected `{ch}`") });
        }
    }
    Ok(out)
}

/// A number at the start of `s`, and its length.
fn number(s: &str) -> Result<(Tok, usize), String> {
    let b = s.as_bytes();
    let (radix, start) = match b {
        [b'0', b'x' | b'X', ..] => (16, 2),
        [b'0', b'b' | b'B', ..] => (2, 2),
        [b'0', b'o' | b'O', ..] => (8, 2),
        _ => (10, 0),
    };
    let mut i = start;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
        i += 1;
    }
    let mut float = false;
    if radix == 10 && b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        float = true;
        i += 1;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_' || b[i] == b'e' || b[i] == b'E') {
            i += 1;
        }
    }
    let digits: String = s[start..i].chars().filter(|&c| c != '_').collect();
    if float {
        return digits.parse().map(|v| (Tok::Float(v), i)).map_err(|_| format!("`{}` isn't a number", &s[..i]));
    }
    // Kaitai's integers are as wide as they need to be; here 64 bits,
    // u64 values kept by their bits.
    let v = u64::from_str_radix(&digits, radix).map_err(|_| format!("`{}` isn't a number", &s[..i]))?;
    Ok((Tok::Int(v as i64), i))
}

/// A quoted string at the start of `b` (`'` raw, `"` with escapes).
fn string(b: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let q = b[0];
    let mut out = Vec::new();
    let mut i = 1;
    while i < b.len() {
        let c = b[i];
        if c == q {
            return Ok((out, i + 1));
        }
        if c == b'\\' && q == b'"' {
            i += 1;
            let e = *b.get(i).ok_or("the string isn't closed")?;
            out.push(match e {
                b'n' => b'\n',
                b't' => b'\t',
                b'r' => b'\r',
                b'0' => 0,
                b'\\' | b'"' | b'\'' => e,
                _ => return Err(format!("`\\{}` isn't an escape", e as char)),
            });
        } else {
            out.push(c);
        }
        i += 1;
    }
    Err("the string isn't closed".into())
}

struct Parser<'t> {
    tokens: &'t [Token],
    at: usize,
    depth: usize,
    end: usize,
}

impl Parser<'_> {
    fn peek_op(&self, op: &str) -> bool {
        matches!(self.tokens.get(self.at), Some(Token { tok: Tok::Op(o), .. }) if *o == op)
    }

    fn peek_word(&self, w: &str) -> bool {
        matches!(self.tokens.get(self.at), Some(Token { tok: Tok::Name(n), .. }) if n == w)
    }

    fn error_here(&self, message: impl Into<String>) -> ExprError {
        let at = self.tokens.get(self.at).map_or(self.end, |t| t.at);
        ExprError { at, message: message.into() }
    }

    fn expect_op(&mut self, op: &str) -> Result<(), ExprError> {
        if self.peek_op(op) {
            self.at += 1;
            Ok(())
        } else {
            let what = self.tokens.get(self.at).map_or("the end".to_owned(), |t| t.tok.to_string());
            Err(self.error_here(format!("expected `{op}`, found {what}")))
        }
    }

    fn deeper(&mut self) -> Result<(), ExprError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH { Err(self.error_here("nested too deeply")) } else { Ok(()) }
    }

    fn expr(&mut self) -> Result<Expr, ExprError> {
        self.deeper()?;
        let c = self.or()?;
        let e = if self.peek_op("?") {
            self.at += 1;
            let a = self.expr()?;
            self.expect_op(":")?;
            let b = self.expr()?;
            Expr::Cond(Box::new(c), Box::new(a), Box::new(b))
        } else {
            c
        };
        self.depth -= 1;
        Ok(e)
    }

    fn or(&mut self) -> Result<Expr, ExprError> {
        let mut e = self.and()?;
        while self.peek_word("or") {
            self.at += 1;
            e = Expr::Binary(BinOp::Or, Box::new(e), Box::new(self.and()?));
        }
        Ok(e)
    }

    fn and(&mut self) -> Result<Expr, ExprError> {
        let mut e = self.not()?;
        while self.peek_word("and") {
            self.at += 1;
            e = Expr::Binary(BinOp::And, Box::new(e), Box::new(self.not()?));
        }
        Ok(e)
    }

    fn not(&mut self) -> Result<Expr, ExprError> {
        if self.peek_word("not") {
            self.at += 1;
            self.deeper()?;
            let e = Expr::Unary(UnOp::Not, Box::new(self.not()?));
            self.depth -= 1;
            return Ok(e);
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Result<Expr, ExprError> {
        let a = self.binary(0)?;
        const CMP: [(&str, BinOp); 6] = [
            ("==", BinOp::Eq),
            ("!=", BinOp::Ne),
            ("<=", BinOp::Le),
            (">=", BinOp::Ge),
            ("<", BinOp::Lt),
            (">", BinOp::Gt),
        ];
        for (op, b) in CMP {
            if self.peek_op(op) {
                self.at += 1;
                let rhs = self.binary(0)?;
                if CMP.iter().any(|(o, _)| self.peek_op(o)) {
                    return Err(self.error_here("comparisons don't chain; use `and`"));
                }
                return Ok(Expr::Binary(b, Box::new(a), Box::new(rhs)));
            }
        }
        Ok(a)
    }

    /// Bit operations, shifts and arithmetic, by level (loosest first).
    fn binary(&mut self, level: usize) -> Result<Expr, ExprError> {
        const LEVELS: [&[(&str, BinOp)]; 6] = [
            &[("|", BinOp::BitOr)],
            &[("^", BinOp::BitXor)],
            &[("&", BinOp::BitAnd)],
            &[("<<", BinOp::Shl), (">>", BinOp::Shr)],
            &[("+", BinOp::Add), ("-", BinOp::Sub)],
            &[("*", BinOp::Mul), ("/", BinOp::Div), ("%", BinOp::Mod)],
        ];
        let Some(ops) = LEVELS.get(level) else { return self.unary() };
        let mut e = self.binary(level + 1)?;
        'outer: loop {
            for &(op, b) in *ops {
                if self.peek_op(op) {
                    self.at += 1;
                    e = Expr::Binary(b, Box::new(e), Box::new(self.binary(level + 1)?));
                    continue 'outer;
                }
            }
            return Ok(e);
        }
    }

    fn unary(&mut self) -> Result<Expr, ExprError> {
        for (op, u) in [("-", UnOp::Neg), ("~", UnOp::BitNot)] {
            if self.peek_op(op) {
                self.at += 1;
                self.deeper()?;
                let inner = self.unary()?;
                self.depth -= 1;
                // Fold `-1` so the most negative number can be written.
                return Ok(match (u, inner) {
                    (UnOp::Neg, Expr::Int(v)) => Expr::Int(v.wrapping_neg()),
                    (UnOp::Neg, Expr::Float(v)) => Expr::Float(-v),
                    (u, e) => Expr::Unary(u, Box::new(e)),
                });
            }
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, ExprError> {
        let mut e = self.atom()?;
        loop {
            if self.peek_op(".") {
                self.at += 1;
                match self.tokens.get(self.at) {
                    Some(Token { tok: Tok::Name(n), .. }) => {
                        e = Expr::Attr(Box::new(e), n.clone());
                        self.at += 1;
                    }
                    _ => return Err(self.error_here("expected a name after `.`")),
                }
                if self.peek_op("(") {
                    return Err(self.error_here("methods with arguments aren't read yet"));
                }
            } else if self.peek_op("[") {
                self.at += 1;
                let i = self.expr()?;
                self.expect_op("]")?;
                e = Expr::Index(Box::new(e), Box::new(i));
            } else {
                return Ok(e);
            }
        }
    }

    fn atom(&mut self) -> Result<Expr, ExprError> {
        let Some(t) = self.tokens.get(self.at) else { return Err(self.error_here("expected a value")) };
        self.at += 1;
        Ok(match &t.tok {
            Tok::Int(v) => Expr::Int(*v),
            Tok::Float(v) => Expr::Float(*v),
            Tok::Str(s) => Expr::Str(s.clone()),
            Tok::Name(n) if n == "true" => Expr::Bool(true),
            Tok::Name(n) if n == "false" => Expr::Bool(false),
            Tok::Name(n) if matches!(n.as_str(), "and" | "or" | "not") => {
                self.at -= 1;
                return Err(self.error_here(format!("expected a value, found `{n}`")));
            }
            Tok::Name(n) => Expr::Name(n.clone()),
            Tok::Op("(") => {
                let e = self.expr()?;
                self.expect_op(")")?;
                e
            }
            Tok::Op("[") => {
                self.at -= 1;
                return Err(self.error_here("array literals aren't read yet"));
            }
            tok => {
                self.at -= 1;
                return Err(self.error_here(format!("expected a value, found {tok}")));
            }
        })
    }
}

#[cfg(test)]
mod tests;

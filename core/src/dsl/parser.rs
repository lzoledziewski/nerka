//! Lexer + recursive-descent parser for NFD.

use super::{CmpOp, Cond, DslError, Expr, FieldCond, FieldTest};
use crate::event::Level;
use regex::bytes::RegexBuilder;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    And,
    Or,
    Not,
    Op(OpTok),
    Word(String),
    Str(String),
    Regex { pat: String, ci: bool },
    End,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum OpTok {
    Cmp(CmpOp),
    Match,
    NotMatch,
}

struct Lexer<'a> {
    s: &'a str,
    pos: usize,
}

fn err<T>(msg: impl Into<String>, pos: usize) -> Result<T, DslError> {
    Err(DslError { msg: msg.into(), pos })
}

impl<'a> Lexer<'a> {
    fn peek_char(&self) -> Option<char> {
        self.s[self.pos..].chars().next()
    }

    fn next(&mut self) -> Result<(Tok, usize), DslError> {
        while self.peek_char().is_some_and(char::is_whitespace) {
            self.pos += self.peek_char().unwrap().len_utf8();
        }
        let start = self.pos;
        let rest = &self.s[self.pos..];
        let Some(c) = rest.chars().next() else { return Ok((Tok::End, start)) };

        let two = rest.get(..2).unwrap_or("");
        let op2 = match two {
            "==" => Some(Tok::Op(OpTok::Cmp(CmpOp::Eq))),
            "!=" => Some(Tok::Op(OpTok::Cmp(CmpOp::Ne))),
            ">=" => Some(Tok::Op(OpTok::Cmp(CmpOp::Ge))),
            "<=" => Some(Tok::Op(OpTok::Cmp(CmpOp::Le))),
            "!~" => Some(Tok::Op(OpTok::NotMatch)),
            "&&" => Some(Tok::And),
            "||" => Some(Tok::Or),
            _ => None,
        };
        if let Some(t) = op2 {
            self.pos += 2;
            return Ok((t, start));
        }
        let one = match c {
            '(' => Some(Tok::LParen),
            ')' => Some(Tok::RParen),
            '>' => Some(Tok::Op(OpTok::Cmp(CmpOp::Gt))),
            '<' => Some(Tok::Op(OpTok::Cmp(CmpOp::Lt))),
            '=' => Some(Tok::Op(OpTok::Cmp(CmpOp::Eq))),
            '~' => Some(Tok::Op(OpTok::Match)),
            '!' => Some(Tok::Not),
            _ => None,
        };
        if let Some(t) = one {
            self.pos += 1;
            return Ok((t, start));
        }
        if c == '/' || c == '"' {
            return self.delimited(c).map(|t| (t, start));
        }
        if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' {
            let len = rest
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '.' || ch == '-'))
                .unwrap_or(rest.len());
            self.pos += len;
            let w = &rest[..len];
            let t = match w.to_ascii_uppercase().as_str() {
                "AND" => Tok::And,
                "OR" => Tok::Or,
                "NOT" => Tok::Not,
                _ => Tok::Word(w.to_owned()),
            };
            return Ok((t, start));
        }
        err(format!("unexpected character '{c}'"), start)
    }

    /// `/regex/flags` or `"string"`. Inside, `\<delim>` escapes the delimiter;
    /// for regexes every other backslash is kept for the regex engine.
    fn delimited(&mut self, delim: char) -> Result<Tok, DslError> {
        let start = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek_char() else {
                return err(format!("unterminated {}", if delim == '/' { "regex" } else { "string" }), start);
            };
            self.pos += c.len_utf8();
            if c == delim {
                break;
            }
            if c == '\\' {
                match self.peek_char() {
                    Some(n) if n == delim => {
                        out.push(n);
                        self.pos += 1;
                    }
                    Some('\\') if delim == '"' => {
                        out.push('\\');
                        self.pos += 1;
                    }
                    _ => out.push('\\'),
                }
                continue;
            }
            out.push(c);
        }
        if delim == '"' {
            return Ok(Tok::Str(out));
        }
        let mut ci = false;
        while let Some(f) = self.peek_char().filter(char::is_ascii_alphabetic) {
            match f {
                'i' => ci = true,
                _ => return err(format!("unknown regex flag '{f}' (supported: i)"), self.pos),
            }
            self.pos += 1;
        }
        Ok(Tok::Regex { pat: out, ci })
    }
}

struct Parser<'a> {
    lex: Lexer<'a>,
    tok: Tok,
    pos: usize,
}

pub fn parse(src: &str) -> Result<Expr, DslError> {
    let mut lex = Lexer { s: src, pos: 0 };
    let (tok, pos) = lex.next()?;
    let mut p = Parser { lex, tok, pos };
    if p.tok == Tok::End {
        return err("empty query", 0);
    }
    let e = p.or()?;
    if p.tok != Tok::End {
        return err(format!("unexpected {}", describe(&p.tok)), p.pos);
    }
    Ok(e)
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::End => "end of query".into(),
        Tok::Word(w) => format!("'{w}'"),
        Tok::RParen => "')'".into(),
        t => format!("{t:?}").to_lowercase(),
    }
}

impl Parser<'_> {
    fn bump(&mut self) -> Result<Tok, DslError> {
        let (t, p) = self.lex.next()?;
        self.pos = p;
        Ok(std::mem::replace(&mut self.tok, t))
    }

    fn or(&mut self) -> Result<Expr, DslError> {
        let mut xs = vec![self.and()?];
        while self.tok == Tok::Or {
            self.bump()?;
            xs.push(self.and()?);
        }
        Ok(if xs.len() == 1 { xs.pop().unwrap() } else { Expr::Or(xs) })
    }

    fn and(&mut self) -> Result<Expr, DslError> {
        let mut xs = vec![self.unary()?];
        while self.tok == Tok::And {
            self.bump()?;
            xs.push(self.unary()?);
        }
        Ok(if xs.len() == 1 { xs.pop().unwrap() } else { Expr::And(xs) })
    }

    fn unary(&mut self) -> Result<Expr, DslError> {
        if self.tok == Tok::Not {
            self.bump()?;
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, DslError> {
        let pos = self.pos;
        match self.bump()? {
            Tok::LParen => {
                let e = self.or()?;
                if self.tok != Tok::RParen {
                    return err(format!("expected ')' but found {}", describe(&self.tok)), self.pos);
                }
                self.bump()?;
                Ok(e)
            }
            Tok::Regex { pat, ci } => Ok(Expr::Cond(Cond::RawMatch(regex(&pat, ci, pos)?))),
            Tok::Str(s) => Ok(Expr::Cond(Cond::RawMatch(regex(&regex::escape(&s), false, pos)?))),
            Tok::Word(field) => self.condition(&field, pos),
            t => err(format!("expected a condition but found {}", describe(&t)), pos),
        }
    }

    fn condition(&mut self, field: &str, field_pos: usize) -> Result<Expr, DslError> {
        let op_pos = self.pos;
        let Tok::Op(op) = self.bump()? else {
            return err(format!("expected an operator after '{field}' (==, !=, >=, <=, >, <, ~, !~)"), op_pos);
        };
        let val_pos = self.pos;
        let val = self.bump()?;
        match field.to_ascii_lowercase().as_str() {
            "raw" => {
                let (re_cond, negate) = match (op, val) {
                    (OpTok::Match | OpTok::NotMatch, Tok::Regex { pat, ci }) => {
                        (regex(&pat, ci, val_pos)?, op == OpTok::NotMatch)
                    }
                    (OpTok::Match | OpTok::NotMatch, Tok::Str(s)) => {
                        (regex(&regex::escape(&s), false, val_pos)?, op == OpTok::NotMatch)
                    }
                    (OpTok::Cmp(c @ (CmpOp::Eq | CmpOp::Ne)), Tok::Str(s) | Tok::Word(s)) => {
                        let e = Expr::Cond(Cond::RawEq(s.into_bytes()));
                        return Ok(if c == CmpOp::Ne { Expr::Not(Box::new(e)) } else { e });
                    }
                    (OpTok::Cmp(_), _) => return err("raw supports ~, !~, == and !=", op_pos),
                    (_, t) => return err(format!("expected /regex/ or \"text\" but found {}", describe(&t)), val_pos),
                };
                let e = Expr::Cond(Cond::RawMatch(re_cond));
                Ok(if negate { Expr::Not(Box::new(e)) } else { e })
            }
            "level" => {
                let OpTok::Cmp(c) = op else { return err("level supports ==, !=, >=, <=, >, <", op_pos) };
                let name = match val {
                    Tok::Word(w) | Tok::Str(w) => w,
                    t => return err(format!("expected a level name but found {}", describe(&t)), val_pos),
                };
                let Some(level) = Level::parse(&name) else {
                    return err(
                        format!("unknown level '{name}' (TRACE, DEBUG, INFO, NOTICE, WARN, ERROR, FATAL)"),
                        val_pos,
                    );
                };
                Ok(Expr::Cond(Cond::Level(c, level)))
            }
            _ => {
                let test = match (op, val) {
                    (OpTok::Match | OpTok::NotMatch, Tok::Regex { pat, ci }) => {
                        FieldTest::Match(regex(&pat, ci, val_pos)?)
                    }
                    (OpTok::Match | OpTok::NotMatch, t) => {
                        return err(format!("expected /regex/ but found {}", describe(&t)), val_pos);
                    }
                    (OpTok::Cmp(c), Tok::Word(w) | Tok::Str(w)) => FieldTest::Literal(c, w, val_pos),
                    (OpTok::Cmp(_), t) => return err(format!("expected a value but found {}", describe(&t)), val_pos),
                };
                let e = Expr::Cond(Cond::Field(FieldCond {
                    name: field.to_ascii_lowercase(),
                    pos: field_pos,
                    op_pos,
                    id: None,
                    test,
                }));
                Ok(if op == OpTok::NotMatch { Expr::Not(Box::new(e)) } else { e })
            }
        }
    }
}

fn regex(pat: &str, ci: bool, pos: usize) -> Result<regex::bytes::Regex, DslError> {
    RegexBuilder::new(pat)
        .case_insensitive(ci)
        .multi_line(true)
        .crlf(true)
        .build()
        .map_err(|e| DslError { msg: format!("invalid regex: {}", e.to_string().lines().last().unwrap_or("")), pos })
}

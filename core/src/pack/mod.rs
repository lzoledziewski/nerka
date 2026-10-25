//! Query packs: shareable libraries of NFD queries (see `doc/search-manager.md`).
//!
//! A pack is a text file of queries; its name is the namespace, so queries
//! are identified as `pack.query`. A query has NFD text, an optional parent
//! (its full meaning is `parent AND own text`), labels, parameters and an
//! optional colour. NFD itself knows nothing about packs: picking from a
//! [`Library`] expands into plain NFD ([`expand`]), with provenance.
//!
//! File format (`*.nerka-pack`, UTF-8):
//!
//! ```text
//! # A comment: '#' at the start of a line, or ' #' followed by a space.
//! pack vision                      # first non-comment line: the pack name
//!                                  # (quoted if it has spaces: pack "camera team")
//!
//! query cam_scope(ecu, cam)  labels: system/vision
//!     ecu == $ecu AND ctid == $cam
//!
//! query frame_drops(cam)  parent: cam_scope  labels: system/vision, perf  color: violet
//!     payload ~ /$cam frame drop/
//!
//! query "xyz logs"  parent: "vision.cam scope"
//!     apid == XYZ
//! ```
//!
//! ```text
//! file    := comment* "pack" PNAME query*
//! PNAME   := NAME | '"' text '"'                   -- quoted like a QNAME
//! query   := "query" QNAME [ "(" PARAM ("," PARAM)* ")" ] attr*  NEWLINE  body
//! QNAME   := NAME | '"' text '"'                   -- quoted: spaces allowed,
//!                                                     no . " # : at all
//! attr    := "parent:" (NAME | NAME "." NAME       -- same pack, or pack.query
//!                      | '"' [text "."] text '"')  -- quoted, for PNAMEs
//!                                                     and QNAMEs
//!          | "labels:" LABEL ("," LABEL)*          -- LABEL: seg ("/" seg)*
//!          | "color:" (lowercase name | "#" 6 hex digits)
//!          | "signals:" GROUP ":" KIND ("," GROUP ":" KIND)*  -- KIND: num, enum, time
//! body    := indented lines (the NFD text, kept verbatim), up to the next
//!            line that starts in column 1
//! NAME    := [A-Za-z_][A-Za-z0-9_]*                -- also PARAM
//! ```
//!
//! Inside the body, `$name` or `${name}` is a parameter. It must be
//! declared in the query's own parameter list. `$$` is a literal `$`, as is
//! a `$` not followed by a name (the regex anchor in `/down$/`), and `\$`
//! inside a regex or string. Comments are not recognised inside bodies.
//!
//! The body must be valid NFD once parameters are substituted; the parser
//! checks the syntax (field names can only be checked against a source).
//! Every error carries the file, line and column.

mod expand;
mod parser;
mod write;

use std::fmt;
use std::path::{Path, PathBuf};

pub use expand::{ExpandError, Expansion, ParamValue, Params, Part, Pick, Role, Selection, expand};
pub use parser::parse_pack;
pub use write::{create_pack_file, format_query, params_in, remove_query, rename_query, upsert_query};

/// File extension of pack files when a directory is loaded.
pub const PACK_EXTENSION: &str = "nerka-pack";

/// The per-user pack directory (`<config dir>/packs`).
pub fn default_dir() -> Option<PathBuf> {
    crate::config_dir().map(|d| d.join("packs"))
}

/// One query of a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackQuery {
    /// Short id, unique within the pack.
    pub id: String,
    /// As written: `name` (same pack) or `pack.name`.
    pub parent: Option<String>,
    pub labels: Vec<String>,
    /// Declared parameter names, in order.
    pub params: Vec<String>,
    pub color: Option<String>,
    /// Kinds pinned for the query's named groups (`signals: speed:num`);
    /// other groups get theirs from the values.
    pub signals: Vec<(String, crate::signal::SignalKind)>,
    /// NFD text with `$param` placeholders, dedented, lines joined by `\n`.
    pub text: String,
    /// 0-based line of the `query` header in the file.
    pub line: usize,
    /// 0-based line of the last body line (the block is `line..=end_line`).
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    pub name: String,
    /// Where it was loaded from (a display name for packs parsed from text).
    pub file: String,
    /// In file order.
    pub queries: Vec<PackQuery>,
}

impl Pack {
    pub fn query(&self, id: &str) -> Option<&PackQuery> {
        self.queries.iter().find(|q| q.id == id)
    }

    /// Qualified id of the query's parent (`pack.query`), if it has one.
    pub fn parent_of(&self, q: &PackQuery) -> Option<String> {
        q.parent.as_ref().map(|p| if p.contains('.') { p.clone() } else { format!("{}.{p}", self.name) })
    }
}

/// A pack file problem. Positions are 0-based; `Display` shows them 1-based
/// as `file:line:col: message`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct PackError {
    pub file: String,
    /// `(line, column)`, column in characters; `None` for whole-file
    /// problems such as I/O errors.
    pub pos: Option<(usize, usize)>,
    pub msg: String,
}

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pos {
            Some((l, c)) => write!(f, "{}:{}:{}: {}", self.file, l + 1, c + 1, self.msg),
            None => write!(f, "{}: {}", self.file, self.msg),
        }
    }
}

/// All loaded packs. Pack names must be unique.
#[derive(Debug, Clone, Default)]
pub struct Library {
    packs: Vec<Pack>,
}

impl Library {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn packs(&self) -> &[Pack] {
        &self.packs
    }

    pub fn add(&mut self, pack: Pack) -> Result<(), PackError> {
        if let Some(other) = self.packs.iter().find(|p| p.name == pack.name) {
            return Err(PackError {
                file: pack.file,
                pos: None,
                msg: format!("pack '{}' is already loaded from {}", pack.name, other.file),
            });
        }
        self.packs.push(pack);
        Ok(())
    }

    /// Load a pack file, or every `*.nerka-pack` file in a directory (not
    /// recursive, in file-name order).
    pub fn load(&mut self, path: &Path) -> Result<(), PackError> {
        let io_err = |e: std::io::Error| PackError { file: path.display().to_string(), pos: None, msg: e.to_string() };
        if path.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(path)
                .map_err(io_err)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == PACK_EXTENSION))
                .collect();
            files.sort();
            return files.iter().try_for_each(|f| self.load(f));
        }
        let text = std::fs::read_to_string(path).map_err(io_err)?;
        self.add(parse_pack(&path.display().to_string(), &text)?)
    }

    /// Like [`load`](Self::load), but a bad file doesn't stop the others in
    /// a directory: every pack that parses is added, and the errors are
    /// returned.
    pub fn load_lenient(&mut self, path: &Path) -> Vec<PackError> {
        if !path.is_dir() {
            return self.load(path).err().into_iter().collect();
        }
        let files = match std::fs::read_dir(path) {
            Ok(rd) => {
                let mut v: Vec<PathBuf> = rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == PACK_EXTENSION))
                    .collect();
                v.sort();
                v
            }
            Err(e) => return vec![PackError { file: path.display().to_string(), pos: None, msg: e.to_string() }],
        };
        files.iter().filter_map(|f| self.load(f).err()).collect()
    }

    /// Write `q` into the file of pack `pack` ([`upsert_query`]: replaced
    /// if its id exists, appended otherwise) and reload that pack. Nothing
    /// is written if the result wouldn't parse.
    pub fn save_query(&mut self, pack: &str, q: &PackQuery) -> Result<(), PackError> {
        self.rewrite(pack, |file, text| upsert_query(file, text, q))
    }

    /// Replace query `old` of pack `pack` with `q`, which may have a new
    /// name (in place, so the file keeps its order). Fails if another query
    /// already has `q`'s name.
    pub fn rename_query(&mut self, pack: &str, old: &str, q: &PackQuery) -> Result<(), PackError> {
        self.rewrite(pack, |file, text| rename_query(file, text, old, q))
    }

    /// Delete query `id` from pack `pack`'s file.
    pub fn remove_query(&mut self, pack: &str, id: &str) -> Result<(), PackError> {
        self.rewrite(pack, |file, text| remove_query(file, text, id))
    }

    /// Read a pack's file, change its text, write it back and reload it.
    fn rewrite(
        &mut self,
        pack: &str,
        change: impl FnOnce(&str, &str) -> Result<String, PackError>,
    ) -> Result<(), PackError> {
        let Some(i) = self.packs.iter().position(|p| p.name == pack) else {
            return Err(PackError { file: pack.to_owned(), pos: None, msg: format!("pack '{pack}' isn't loaded") });
        };
        let file = self.packs[i].file.clone();
        let io_err = |e: std::io::Error| PackError { file: file.clone(), pos: None, msg: e.to_string() };
        let text = std::fs::read_to_string(&file).map_err(io_err)?;
        let new = change(&file, &text)?;
        std::fs::write(&file, &new).map_err(io_err)?;
        self.packs[i] = parse_pack(&file, &new)?;
        Ok(())
    }

    /// The pack and query for a qualified id (`pack.query`).
    pub fn get(&self, qualified: &str) -> Option<(&Pack, &PackQuery)> {
        let (pack, id) = qualified.split_once('.')?;
        let p = self.packs.iter().find(|p| p.name == pack)?;
        Some((p, p.query(id)?))
    }

    /// Every query (qualified ids, library order) with `label` or a label
    /// below it (`system` matches `system/vision`).
    pub fn with_label(&self, label: &str) -> Vec<String> {
        let label = label.trim_end_matches('/');
        let under = |l: &str| l == label || l.strip_prefix(label).is_some_and(|r| r.starts_with('/'));
        self.packs
            .iter()
            .flat_map(|p| p.queries.iter().map(move |q| (p, q)))
            .filter(|(_, q)| q.labels.iter().any(|l| under(l)))
            .map(|(p, q)| format!("{}.{}", p.name, q.id))
            .collect()
    }

    /// All labels in use, sorted, without duplicates.
    pub fn labels(&self) -> Vec<&str> {
        let mut v: Vec<&str> =
            self.packs.iter().flat_map(|p| &p.queries).flat_map(|q| q.labels.iter().map(String::as_str)).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// Where a piece of query text sits: parameters are escaped for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ctx {
    /// Outside regexes and strings, e.g. `ecu == $ecu`.
    Bare,
    /// Inside `/…/`.
    Regex,
    /// Inside `"…"`.
    Str,
}

/// Walk a query body the way the NFD lexer does, copying text and calling
/// `param(name, ctx, pos)` for every parameter. With `collapse`, runs of
/// whitespace outside regexes and strings become one space (and the
/// result is trimmed), so a multi-line body becomes one line.
pub(crate) fn render<E>(
    text: &str,
    collapse: bool,
    mut param: impl FnMut(&str, Ctx, usize) -> Result<String, E>,
    unterminated: impl Fn(usize) -> E,
) -> Result<String, E> {
    let mut out = String::with_capacity(text.len());
    let mut ctx = Ctx::Bare;
    let mut chars = text.char_indices().peekable();
    let is_name_start = |c: char| c.is_ascii_alphabetic() || c == '_';
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '_';
    while let Some((i, c)) = chars.next() {
        match (c, ctx) {
            ('$', _) => match chars.peek().map(|&(_, n)| n) {
                Some('$') => {
                    chars.next();
                    out.push('$');
                }
                Some('{') => {
                    chars.next();
                    let start = i + 2;
                    let end = text[start..].find('}').map(|e| start + e).ok_or_else(|| unterminated(i))?;
                    while chars.peek().is_some_and(|&(j, _)| j <= end) {
                        chars.next();
                    }
                    out.push_str(&param(&text[start..end], ctx, i)?);
                }
                Some(n) if is_name_start(n) => {
                    let end = text[i + 1..].find(|ch: char| !is_name(ch)).map_or(text.len(), |e| i + 1 + e);
                    while chars.peek().is_some_and(|&(j, _)| j < end) {
                        chars.next();
                    }
                    out.push_str(&param(&text[i + 1..end], ctx, i)?);
                }
                _ => out.push('$'),
            },
            ('\\', Ctx::Regex | Ctx::Str) => {
                out.push('\\');
                if let Some((_, n)) = chars.next() {
                    out.push(n);
                }
            }
            ('/', Ctx::Bare) => {
                ctx = Ctx::Regex;
                out.push(c);
            }
            ('"', Ctx::Bare) => {
                ctx = Ctx::Str;
                out.push(c);
            }
            ('/', Ctx::Regex) | ('"', Ctx::Str) => {
                ctx = Ctx::Bare;
                out.push(c);
            }
            (c, Ctx::Bare) if collapse && c.is_whitespace() => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            (c, _) => out.push(c),
        }
    }
    if collapse && out.ends_with(' ') {
        out.pop();
    }
    Ok(out)
}

/// Can `s` name a query? A NAME, or any text without `.`, `"`, `#`, `:`
/// or control characters that doesn't start or end with a space. Names
/// that aren't a NAME are quoted in pack files: `query "xyz logs"`.
pub fn is_query_name(s: &str) -> bool {
    !s.is_empty() && s.trim() == s && !s.chars().any(|c| matches!(c, '.' | '"' | '#' | ':') || c.is_control())
}

/// Is `s` a valid pack, query or parameter name (`[A-Za-z_][A-Za-z0-9_]*`)?
pub fn is_name(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod expand_tests;
#[cfg(test)]
mod tests;

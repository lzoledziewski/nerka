//! Presets: a saved way to analyse logs. A preset holds a selection of
//! library queries (in order, with their role and settings), copies of the
//! queries that aren't in a pack, parameter values and the typed text of the
//! Filter and Search boxes. It refers to library queries by id, so it is
//! independent of the log file it was made on.
//!
//! File format (`*.nerka-preset`, UTF-8), one item per line:
//!
//! ```text
//! # A comment.
//! preset "cam triage"
//! node include lit          query vision.cam
//! node exclude              query "camera team.noise"
//! node include detached     query vision.drops
//! node signal lit           query vision.speed
//! node include lit          local n1  from vision.timeouts  name "slow ones"
//! param cam = CAM1
//! param re raw = a.b(c)?
//! filter selection
//! search auto timeout AND level >= WARN
//!
//! pack _preset
//! query n1
//!     /timeout/ AND level >= ERROR
//! ```
//!
//! ```text
//! file   := "preset" NAME line* [ "pack" "_preset" query* ]   -- queries as in packs
//! line   := "node" ("include" | "exclude" | "signal") flag* source
//!         | "param" NAME ["raw"] "=" VALUE                    -- VALUE: rest of the line
//!         | ("filter" | "search") ("selection" | TAG TEXT)    -- TAG: auto, text, regex, query
//! flag   := "lit" | "detached"
//! source := "query" ID                                        -- a library query (pack.query)
//!         | "local" ID ["from" ID] ["name" NAME]              -- a query of the _preset pack
//! ```
//!
//! IDs and NAMEs are quoted (`"…"`, with `\"` and `\\`) when they hold
//! spaces or quotes. `filter selection` means the Filter box held the
//! selection's expansion, which is expanded again when the preset is used.

use crate::pack::{Pack, PackError, PackQuery, Role, format_query, parse_pack};
use std::path::{Path, PathBuf};

/// File extension of preset files.
pub const PRESET_EXTENSION: &str = "nerka-preset";
/// Name of the pack inside a preset file that holds its local queries.
pub const LOCAL_PACK: &str = "_preset";

/// The per-user preset directory (`<config dir>/presets`).
pub fn default_dir() -> Option<PathBuf> {
    crate::config_dir().map(|d| d.join("presets"))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Preset {
    pub name: String,
    pub nodes: Vec<PresetNode>,
    pub params: Vec<PresetParam>,
    pub filter: Option<BoxText>,
    pub search: Option<BoxText>,
    /// Copies of the queries that aren't (as they are) in a pack.
    pub locals: Vec<PackQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetNode {
    pub source: NodeSource,
    pub role: Role,
    /// Its lines are coloured.
    pub lit: bool,
    /// Without its parents: only its own text counts.
    pub detached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeSource {
    /// A library query by qualified id.
    Library(String),
    /// A query of [`Preset::locals`] by id; `from` is the library query it
    /// is an edited copy of, `name` its display name (empty: unnamed).
    Local { id: String, from: Option<String>, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetParam {
    pub name: String,
    pub value: String,
    pub raw: bool,
}

/// What a Filter or Search box held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoxText {
    /// The selection's expansion (expanded again when used).
    Selection,
    /// Typed text, with its history tag (`auto`, `text`, `regex`, `query`).
    Typed { tag: String, text: String },
}

impl Preset {
    /// The local query with this id.
    pub fn local(&self, id: &str) -> Option<&PackQuery> {
        self.locals.iter().find(|q| q.id == id)
    }

    /// Library query `old` (qualified id) is now `new` (issue #7): its
    /// nodes, the copies made from it and the queries refining it follow.
    /// Returns whether anything changed.
    pub fn rename_query(&mut self, old: &str, new: &str) -> bool {
        let mut changed = false;
        let mut follow = |id: &mut String| {
            if id == old {
                *id = new.to_owned();
                changed = true;
            }
        };
        for n in &mut self.nodes {
            match &mut n.source {
                NodeSource::Library(id) => follow(id),
                NodeSource::Local { from: Some(id), .. } => follow(id),
                NodeSource::Local { from: None, .. } => {}
            }
        }
        for q in &mut self.locals {
            if let Some(p) = &mut q.parent {
                follow(p);
            }
        }
        changed
    }

    /// Library query `id` was deleted (issue #7): its nodes go; an edited
    /// copy of it stays, as a query of its own. Returns whether anything
    /// changed.
    pub fn remove_query(&mut self, id: &str) -> bool {
        let before = self.nodes.len();
        self.nodes.retain(|n| !matches!(&n.source, NodeSource::Library(l) if l == id));
        let mut changed = self.nodes.len() != before;
        for n in &mut self.nodes {
            if let NodeSource::Local { from, .. } = &mut n.source
                && from.as_deref() == Some(id)
            {
                *from = None;
                changed = true;
            }
        }
        changed
    }

    /// Whether the preset picks library query `id` (or a copy of it).
    pub fn uses_query(&self, id: &str) -> bool {
        self.nodes.iter().any(|n| match &n.source {
            NodeSource::Library(l) => l == id,
            NodeSource::Local { from, .. } => from.as_deref() == Some(id),
        })
    }

    /// The file text.
    pub fn serialize(&self) -> String {
        let mut out = format!("preset {}\n", quote(&self.name));
        for n in &self.nodes {
            out += match n.role {
                Role::Include => "node include",
                Role::Exclude => "node exclude",
                Role::Signal => "node signal",
            };
            if n.lit {
                out += " lit";
            }
            if n.detached {
                out += " detached";
            }
            match &n.source {
                NodeSource::Library(id) => out += &format!(" query {}", quote(id)),
                NodeSource::Local { id, from, name } => {
                    out += &format!(" local {}", quote(id));
                    if let Some(f) = from {
                        out += &format!(" from {}", quote(f));
                    }
                    if !name.is_empty() {
                        out += &format!(" name {}", quote(name));
                    }
                }
            }
            out.push('\n');
        }
        for p in &self.params {
            out += &format!("param {}{} = {}\n", p.name, if p.raw { " raw" } else { "" }, p.value);
        }
        for (key, b) in [("filter", &self.filter), ("search", &self.search)] {
            match b {
                Some(BoxText::Selection) => out += &format!("{key} selection\n"),
                Some(BoxText::Typed { tag, text }) => out += &format!("{key} {tag} {text}\n"),
                None => {}
            }
        }
        if !self.locals.is_empty() {
            out += &format!("\npack {LOCAL_PACK}\n");
            for q in &self.locals {
                out += &format_query(q);
                out.push('\n');
            }
        }
        out
    }

    /// Write to `path` (directories are created). The text is parsed back
    /// first, so a preset that wouldn't load isn't written.
    pub fn save(&self, path: &Path) -> Result<(), PackError> {
        let file = path.display().to_string();
        let text = self.serialize();
        parse_preset(&file, &text)?;
        let err = |e: std::io::Error| PackError { file: file.clone(), pos: None, msg: e.to_string() };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        std::fs::write(path, text).map_err(err)
    }
}

/// `<dir>/<name>.nerka-preset`, if `name` can be a file name.
pub fn preset_path(dir: &Path, name: &str) -> Result<PathBuf, PackError> {
    let path = dir.join(format!("{name}.{PRESET_EXTENSION}"));
    if !valid_name(name) {
        return Err(PackError {
            file: path.display().to_string(),
            pos: None,
            msg: format!("invalid preset name '{name}' (no . \" # : / \\ * ? < > | and no spaces at the ends)"),
        });
    }
    Ok(path)
}

/// A preset name: like a quoted pack name, so it can be a file name.
pub fn valid_name(name: &str) -> bool {
    crate::pack::is_query_name(name) && !name.contains(['/', '\\', '*', '?', '<', '>', '|'])
}

/// Every `*.nerka-preset` in `dir` (sorted by file name), and the files
/// that didn't load. A missing directory has none.
pub fn load_dir(dir: &Path) -> (Vec<(PathBuf, Preset)>, Vec<PackError>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return (vec![], vec![]) };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == PRESET_EXTENSION))
        .collect();
    files.sort();
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for f in files {
        let file = f.display().to_string();
        match std::fs::read_to_string(&f) {
            Ok(t) => match parse_preset(&file, &t) {
                Ok(p) => ok.push((f, p)),
                Err(e) => bad.push(e),
            },
            Err(e) => bad.push(PackError { file, pos: None, msg: e.to_string() }),
        }
    }
    (ok, bad)
}

pub(crate) fn quote(s: &str) -> String {
    let plain = !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '$'));
    if plain {
        return s.to_owned();
    }
    let mut out = String::from('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Split a line into words; `"…"` is one word (`\"`, `\\` escapes).
/// Returns `(word, char column)` pairs, or the column of an unclosed quote.
pub(crate) fn words(line: &str) -> Result<Vec<(String, usize)>, usize> {
    let mut out = Vec::new();
    let mut it = line.char_indices().peekable();
    while let Some(&(i, c)) = it.peek() {
        if c.is_whitespace() {
            it.next();
            continue;
        }
        let col = line[..i].chars().count();
        let mut w = String::new();
        if c == '"' {
            it.next();
            loop {
                match it.next() {
                    None => return Err(col),
                    Some((_, '"')) => break,
                    Some((_, '\\')) => match it.next() {
                        Some((_, n)) => w.push(n),
                        None => return Err(col),
                    },
                    Some((_, ch)) => w.push(ch),
                }
            }
        } else {
            while let Some(&(_, ch)) = it.peek() {
                if ch.is_whitespace() {
                    break;
                }
                w.push(ch);
                it.next();
            }
        }
        out.push((w, col));
    }
    Ok(out)
}

/// Parse a preset file. `file` is used in error messages.
pub fn parse_preset(file: &str, text: &str) -> Result<Preset, PackError> {
    let err = |ln: usize, col: usize, msg: String| PackError { file: file.to_owned(), pos: Some((ln, col)), msg };
    let mut p = Preset::default();
    let mut named = false;
    let lines: Vec<&str> = text.lines().collect();
    let mut pack_at = None;
    for (ln, line) in lines.iter().enumerate() {
        let content = line.trim_end();
        if content.trim_start().is_empty() || content.trim_start().starts_with('#') {
            continue;
        }
        let w = words(content).map_err(|col| err(ln, col, "missing closing '\"'".into()))?;
        let key = w[0].0.as_str();
        if !named && key != "preset" {
            return Err(err(ln, w[0].1, "expected 'preset NAME' as the first line".into()));
        }
        let end_col = content.chars().count();
        match key {
            "preset" if !named => match w.get(1) {
                Some((n, col)) if w.len() == 2 => {
                    if !valid_name(n) {
                        return Err(err(ln, *col, format!("invalid preset name '{n}'")));
                    }
                    p.name = n.clone();
                    named = true;
                }
                Some((_, _)) => return Err(err(ln, w[2].1, "a preset name with spaces is quoted".into())),
                None => return Err(err(ln, end_col, "expected a preset name after 'preset'".into())),
            },
            "preset" => return Err(err(ln, w[0].1, "a file holds one preset: second 'preset' line".into())),
            "node" => p.nodes.push(node(&w).map_err(|(col, m)| err(ln, col.unwrap_or(end_col), m))?),
            "param" => {
                // param NAME [raw] = VALUE (the value is the rest of the line)
                let Some(eq) = content.find(" = ").or_else(|| content.strip_suffix(" =").map(|s| s.len())) else {
                    return Err(err(ln, end_col, "expected 'param NAME = VALUE'".into()));
                };
                let head: Vec<&str> = content[..eq].split_whitespace().collect();
                let (name, raw) = match head.as_slice() {
                    ["param", n] => (*n, false),
                    ["param", n, "raw"] => (*n, true),
                    _ => return Err(err(ln, 6.min(end_col), "expected 'param NAME [raw] = VALUE'".into())),
                };
                if !crate::pack::is_name(name) {
                    return Err(err(ln, 6, format!("invalid parameter name '{name}'")));
                }
                let value = content.get(eq + 3..).unwrap_or("").to_owned();
                p.params.push(PresetParam { name: name.to_owned(), value, raw });
            }
            "filter" | "search" => {
                let slot = if key == "filter" { &mut p.filter } else { &mut p.search };
                if slot.is_some() {
                    return Err(err(ln, w[0].1, format!("'{key}' is given twice")));
                }
                let rest = content.trim_start()[key.len()..].trim_start();
                let (tag, text) = rest.split_once(' ').unwrap_or((rest, ""));
                *slot = Some(match tag {
                    "selection" if text.trim().is_empty() => BoxText::Selection,
                    "auto" | "text" | "regex" | "query" if !text.trim().is_empty() => {
                        BoxText::Typed { tag: tag.to_owned(), text: text.to_owned() }
                    }
                    _ => {
                        let col = w.get(1).map_or(end_col, |x| x.1);
                        return Err(err(
                            ln,
                            col,
                            "expected 'selection', or auto / text / regex / query and the text".into(),
                        ));
                    }
                });
            }
            "pack" => {
                pack_at = Some(ln);
                break;
            }
            w0 => {
                return Err(err(ln, w[0].1, format!("expected node, param, filter, search or pack but found '{w0}'")));
            }
        }
    }
    if !named {
        return Err(PackError { file: file.to_owned(), pos: None, msg: "no 'preset NAME' line".into() });
    }
    if let Some(at) = pack_at {
        // The pack part, with the lines before it blanked so positions stay.
        let mut rest = "\n".repeat(at);
        rest += &lines[at..].join("\n");
        let pack: Pack = parse_pack(file, &rest)?;
        if pack.name != LOCAL_PACK {
            return Err(err(at, 5, format!("the pack in a preset is '{LOCAL_PACK}'")));
        }
        p.locals = pack.queries;
    }
    for (i, n) in p.nodes.iter().enumerate() {
        if let NodeSource::Local { id, .. } = &n.source
            && p.local(id).is_none()
        {
            return Err(PackError {
                file: file.to_owned(),
                pos: None,
                msg: format!("node {} refers to local query '{id}', which isn't in the file", i + 1),
            });
        }
    }
    Ok(p)
}

type NodeErr = (Option<usize>, String);

fn node(w: &[(String, usize)]) -> Result<PresetNode, NodeErr> {
    let mut it = w.iter().skip(1).peekable();
    let what = it.next().ok_or((None, "expected include, exclude or signal".to_owned()))?;
    let role = match what.0.as_str() {
        "include" => Role::Include,
        "exclude" => Role::Exclude,
        "signal" => Role::Signal,
        r => return Err((Some(what.1), format!("expected include, exclude or signal but found '{r}'"))),
    };
    let (mut lit, mut detached) = (false, false);
    while let Some((f, _)) = it.peek() {
        match f.as_str() {
            "lit" => lit = true,
            "detached" => detached = true,
            _ => break,
        }
        it.next();
    }
    let value = |it: &mut std::iter::Peekable<std::iter::Skip<std::slice::Iter<'_, (String, usize)>>>, what: &str| {
        it.next().map(|(v, _)| v.clone()).ok_or((None, format!("expected {what}")))
    };
    let kind = it.next().ok_or((None, "expected 'query ID' or 'local ID'".to_owned()))?;
    let source = match kind.0.as_str() {
        "query" => {
            let id = value(&mut it, "a query id (pack.query)")?;
            if !id.contains('.') {
                return Err((Some(kind.1), format!("'{id}' isn't a qualified id (pack.query)")));
            }
            NodeSource::Library(id)
        }
        "local" => {
            let id = value(&mut it, "a local query id")?;
            let (mut from, mut name) = (None, String::new());
            while let Some((k, col)) = it.next() {
                match k.as_str() {
                    "from" => from = Some(value(&mut it, "a query id after 'from'")?),
                    "name" => name = value(&mut it, "a name after 'name'")?,
                    _ => return Err((Some(*col), format!("expected from or name but found '{k}'"))),
                }
            }
            NodeSource::Local { id, from, name }
        }
        k => return Err((Some(kind.1), format!("expected 'query' or 'local' but found '{k}'"))),
    };
    if let Some((extra, col)) = it.next() {
        return Err((Some(*col), format!("unexpected '{extra}'")));
    }
    Ok(PresetNode { source, role, lit, detached })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::params_in;

    fn local(id: &str, text: &str) -> PackQuery {
        PackQuery {
            id: id.into(),
            parent: Some("vision.cam".into()),
            labels: vec!["x".into()],
            params: params_in(text),
            color: Some("red".into()),
            signals: Vec::new(),
            text: text.into(),
            line: 0,
            end_line: 0,
        }
    }

    fn sample() -> Preset {
        Preset {
            name: "cam triage".into(),
            nodes: vec![
                PresetNode {
                    source: NodeSource::Library("vision.cam".into()),
                    role: Role::Include,
                    lit: true,
                    detached: false,
                },
                PresetNode {
                    source: NodeSource::Library("camera team.noise".into()),
                    role: Role::Exclude,
                    lit: false,
                    detached: false,
                },
                PresetNode {
                    source: NodeSource::Library("vision.drops".into()),
                    role: Role::Include,
                    lit: false,
                    detached: true,
                },
                PresetNode {
                    source: NodeSource::Local {
                        id: "n1".into(),
                        from: Some("vision.timeouts".into()),
                        name: "slow \"ones\"".into(),
                    },
                    role: Role::Include,
                    lit: true,
                    detached: false,
                },
                PresetNode {
                    source: NodeSource::Local { id: "n2".into(), from: None, name: String::new() },
                    role: Role::Include,
                    lit: false,
                    detached: false,
                },
            ],
            params: vec![
                PresetParam { name: "cam".into(), value: "CAM 1".into(), raw: false },
                PresetParam { name: "re".into(), value: "a.b(c)? = x".into(), raw: true },
                PresetParam { name: "empty".into(), value: String::new(), raw: false },
            ],
            filter: Some(BoxText::Selection),
            search: Some(BoxText::Typed { tag: "auto".into(), text: "timeout AND level >= WARN".into() }),
            locals: vec![local("n1", "/timeout/ AND\n  level >= ERROR"), local("n2", "/$cam/")],
        }
    }

    /// Issue #7: presets follow a renamed or deleted library query.
    #[test]
    fn presets_follow_renamed_and_deleted_queries() {
        let mut p = sample();
        assert!(p.uses_query("vision.timeouts") && !p.uses_query("vision.nope"));
        assert!(!p.rename_query("vision.nope", "vision.x"));
        assert!(p.rename_query("vision.cam", "vision.camera"));
        assert_eq!(p.nodes[0].source, NodeSource::Library("vision.camera".into()));
        assert!(p.locals.iter().all(|q| q.parent.as_deref() == Some("vision.camera")));
        assert!(p.rename_query("vision.timeouts", "vision.slow"));
        assert!(matches!(&p.nodes[3].source, NodeSource::Local { from: Some(f), .. } if f == "vision.slow"));
        // The file still reads back the same.
        assert_eq!(parse_preset("t", &p.serialize()).unwrap().serialize(), p.serialize());

        assert!(p.remove_query("vision.camera"));
        assert_eq!(p.nodes.len(), 4);
        assert!(!p.uses_query("vision.camera"));
        assert!(p.remove_query("vision.slow"));
        assert_eq!(p.nodes.len(), 4, "an edited copy stays");
        assert!(matches!(&p.nodes[2].source, NodeSource::Local { from: None, .. }));
        assert!(!p.remove_query("vision.slow"));
        assert_eq!(parse_preset("t", &p.serialize()).unwrap().serialize(), p.serialize());
    }

    #[test]
    fn signal_nodes_round_trip() {
        let text = "preset p\nnode signal lit query v.speed\n";
        let p = parse_preset("t", text).unwrap();
        assert_eq!(p.nodes[0].role, Role::Signal);
        assert!(p.nodes[0].lit);
        assert_eq!(p.serialize(), text);
    }

    #[test]
    fn round_trips() {
        let p = sample();
        let text = p.serialize();
        let back = parse_preset("t", &text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        let strip = |mut p: Preset| {
            for q in &mut p.locals {
                (q.line, q.end_line) = (0, 0);
            }
            p
        };
        assert_eq!(strip(back), p);
        assert!(text.starts_with("preset \"cam triage\"\nnode include lit query vision.cam\n"), "{text}");
        assert!(text.contains("node exclude query \"camera team.noise\"\n"));
        assert!(text.contains("local n1 from vision.timeouts name \"slow \\\"ones\\\"\"\n"));
        assert!(text.contains("param re raw = a.b(c)? = x\n"));
        assert!(text.contains("\npack _preset\nquery n1  parent: vision.cam"));
    }

    #[test]
    fn a_minimal_preset() {
        let p = parse_preset("t", "# mine\npreset p\n").unwrap();
        assert_eq!(p, Preset { name: "p".into(), ..Default::default() });
        let p = parse_preset("t", "preset p\nfilter text a  b \n").unwrap();
        assert_eq!(p.filter, Some(BoxText::Typed { tag: "text".into(), text: "a  b".into() }));
    }

    #[track_caller]
    fn bad(text: &str, line: usize, col: usize, msg: &str) {
        let e = parse_preset("t", text).unwrap_err();
        assert!(e.msg.contains(msg), "{:?} doesn't contain {msg:?}", e.msg);
        assert_eq!(e.pos, Some((line - 1, col - 1)), "position of {:?}", e.msg);
    }

    #[test]
    fn errors_have_positions() {
        bad("node include query a.b\n", 1, 1, "expected 'preset NAME'");
        bad("preset\n", 1, 7, "expected a preset name");
        bad("preset a b\n", 1, 10, "is quoted");
        bad("preset \"a.b\"\n", 1, 8, "invalid preset name");
        bad("preset \"a\n", 1, 8, "missing closing");
        bad("preset p\npreset q\n", 2, 1, "second 'preset'");
        bad("preset p\nnode maybe query a.b\n", 2, 6, "include, exclude or signal");
        bad("preset p\nnode include query ab\n", 2, 14, "isn't a qualified id");
        bad("preset p\nnode include lit thing a.b\n", 2, 18, "'query' or 'local'");
        bad("preset p\nnode include query a.b extra\n", 2, 24, "unexpected 'extra'");
        bad("preset p\nnode include local n1 colour red\n", 2, 23, "from or name");
        bad("preset p\nparam x\n", 2, 8, "param NAME = VALUE");
        bad("preset p\nparam 1x = y\n", 2, 7, "invalid parameter name");
        bad("preset p\nfilter maybe\n", 2, 8, "selection");
        bad("preset p\nfilter selection\nfilter selection\n", 3, 1, "given twice");
        bad("preset p\nhello\n", 2, 1, "found 'hello'");
        bad("preset p\npack other\nquery a\n  /x/\n", 2, 6, "'_preset'");
        bad("preset p\npack _preset\nquery a\n  level >= LOUD\n", 4, 12, "unknown level");
        let e = parse_preset("t", "preset p\nnode include local n9\n").unwrap_err();
        assert!(e.msg.contains("'n9'"), "{e}");
        assert!(parse_preset("t", "# only\n").unwrap_err().msg.contains("no 'preset NAME'"));
    }

    #[test]
    fn files_save_and_load() {
        let dir = std::env::temp_dir().join(format!("nerka-presets-{}", std::process::id()));
        let path = preset_path(&dir, "cam triage").unwrap();
        sample().save(&path).unwrap();
        std::fs::write(dir.join("broken.nerka-preset"), "preset\n").unwrap();
        std::fs::write(dir.join("other.txt"), "x").unwrap();
        let (ok, bad) = load_dir(&dir);
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].1.name, "cam triage");
        assert_eq!(bad.len(), 1);
        assert!(preset_path(&dir, "a/b").is_err());
        assert!(preset_path(&dir, " a").is_err());
        assert_eq!(load_dir(&dir.join("missing")).0.len(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

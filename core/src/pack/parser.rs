//! Pack file parser (format in `pack/mod.rs`).

use super::{Ctx, Pack, PackError, PackQuery, is_name, is_query_name, render};
use std::ops::Range;

/// Parse the text of a pack file. `file` is used in error messages.
pub fn parse_pack(file: &str, text: &str) -> Result<Pack, PackError> {
    let err = |ln: usize, col: usize, msg: String| PackError { file: file.to_owned(), pos: Some((ln, col)), msg };
    let mut name: Option<String> = None;
    let mut queries: Vec<PackQuery> = Vec::new();
    // The query whose body is being read, with its body lines.
    let mut cur: Option<(PackQuery, Vec<(usize, &str)>)> = None;

    for (ln, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            if let Some((_, body)) = &mut cur {
                body.push((ln, ""));
            }
            continue;
        }
        if line.starts_with([' ', '\t']) {
            match &mut cur {
                Some((_, body)) => body.push((ln, line)),
                None => {
                    let col = line.len() - line.trim_start().len();
                    return Err(err(ln, col, "indented text outside a query".into()));
                }
            }
            continue;
        }
        if let Some((q, body)) = cur.take() {
            queries.push(finish(file, q, body)?);
        }
        let content = strip_comment(line);
        let Some(word) = content.split_whitespace().next() else { continue };
        match (word, &name) {
            ("pack", None) => {
                let rest: Vec<&str> = content[4..].split_whitespace().collect();
                match rest.as_slice() {
                    [n] if is_name(n) => name = Some((*n).to_owned()),
                    [] => return Err(err(ln, content.len(), "expected a pack name after 'pack'".into())),
                    [n] => {
                        let col = col_of(line, 4 + content[4..].find(n).unwrap_or(0));
                        return Err(err(ln, col, format!("invalid pack name '{n}' (letters, digits and _)")));
                    }
                    [n, extra, ..] => {
                        let after = 4 + content[4..].find(n).unwrap_or(0) + n.len();
                        let col = col_of(line, after + content[after..].find(extra).unwrap_or(0));
                        return Err(err(ln, col, "a pack name is a single word".into()));
                    }
                }
            }
            ("pack", Some(_)) => return Err(err(ln, 0, "a file holds one pack: second 'pack' line".into())),
            ("query", Some(_)) => {
                let q = header(line, &content[..content.trim_end().len()], ln).map_err(|(col, m)| err(ln, col, m))?;
                if let Some(other) = queries.iter().find(|o| o.id == q.id) {
                    let col = col_of(line, line.find(&q.id).unwrap_or(0));
                    return Err(err(
                        ln,
                        col,
                        format!("query '{}' is already defined on line {}", q.id, other.line + 1),
                    ));
                }
                cur = Some((q, Vec::new()));
            }
            (_, None) => return Err(err(ln, 0, "expected 'pack NAME' as the first line".into())),
            (w, Some(_)) => return Err(err(ln, 0, format!("expected 'query' but found '{w}'"))),
        }
    }
    if let Some((q, body)) = cur.take() {
        queries.push(finish(file, q, body)?);
    }
    let Some(name) = name else {
        return Err(PackError { file: file.to_owned(), pos: None, msg: "no 'pack NAME' line".into() });
    };
    Ok(Pack { name, file: file.to_owned(), queries })
}

/// The line up to a comment: `#` in column 1, or ` #` followed by
/// whitespace or the line end (so `color: #ff8000` is not a comment).
fn strip_comment(line: &str) -> &str {
    if line.starts_with('#') {
        return "";
    }
    let b = line.as_bytes();
    let cut = (1..b.len()).find(|&i| {
        b[i] == b'#' && b[i - 1].is_ascii_whitespace() && b.get(i + 1).is_none_or(|n| n.is_ascii_whitespace())
    });
    &line[..cut.unwrap_or(line.len())]
}

/// Character column of byte offset `byte` in `line`.
fn col_of(line: &str, byte: usize) -> usize {
    line[..byte].chars().count()
}

/// Parse `query NAME(PARAMS) attrs…` (`content` is the line without its
/// comment). Errors are `(column, message)`.
fn header(line: &str, content: &str, ln: usize) -> Result<PackQuery, (usize, String)> {
    let at = |byte: usize, msg: String| Err((col_of(line, byte), msg));
    let mut pos = "query".len();
    let skip_ws = |p: usize| p + content[p..].len() - content[p..].trim_start().len();
    pos = skip_ws(pos);
    let (id, name_end) = if content[pos..].starts_with('"') {
        // A quoted name may hold spaces: query "xyz logs"
        let Some(close) = content[pos + 1..].find('"').map(|c| pos + 1 + c) else {
            return at(pos, "missing closing '\"' after the query name".into());
        };
        let id = &content[pos + 1..close];
        if !is_query_name(id) {
            return at(pos + 1, format!("invalid query name '{id}' (no . \" # : and no spaces at the ends)"));
        }
        (id, close + 1)
    } else {
        let end = content[pos..].find(|c: char| c == '(' || c.is_whitespace()).map_or(content.len(), |e| pos + e);
        let id = &content[pos..end];
        if id.is_empty() {
            return at(pos, "expected a query name after 'query'".into());
        }
        if !is_name(id) {
            return at(pos, format!("invalid query name '{id}' (letters, digits and _, or a \"quoted name\")"));
        }
        (id, end)
    };
    pos = skip_ws(name_end);

    let mut params: Vec<String> = Vec::new();
    if content[pos..].starts_with('(') {
        let Some(close) = content[pos..].find(')').map(|c| pos + c) else {
            return at(pos, "missing ')' after the parameter list".into());
        };
        let mut p = pos + 1;
        for part in content[pos + 1..close].split(',') {
            let start = p + part.len() - part.trim_start().len();
            let name = part.trim();
            p += part.len() + 1;
            if name.is_empty() && params.is_empty() && content[pos + 1..close].trim().is_empty() {
                break; // `()`
            }
            if !is_name(name) {
                return at(start, format!("invalid parameter name '{name}'"));
            }
            if params.iter().any(|x| x == name) {
                return at(start, format!("parameter '{name}' is declared twice"));
            }
            params.push(name.to_owned());
        }
        pos = close + 1;
    }

    let mut q = PackQuery {
        id: id.to_owned(),
        parent: None,
        labels: vec![],
        params,
        color: None,
        text: String::new(),
        line: ln,
        end_line: ln,
    };
    let attrs = attributes(content, pos).map_err(|(b, m)| (col_of(line, b), m))?;
    let mut seen: Vec<&str> = Vec::new();
    for (key, key_at, val) in attrs {
        if seen.contains(&key) {
            return at(key_at, format!("'{key}:' is given twice"));
        }
        seen.push(key);
        let v = content[val.clone()].trim();
        let v_at = val.start + content[val.clone()].len() - content[val.clone()].trim_start().len();
        if v.is_empty() {
            return at(key_at, format!("'{key}:' needs a value"));
        }
        match key {
            "parent" => {
                // `name`, `pack.name`, or quoted: "xyz logs", "pack.xyz logs".
                let quoted = v.len() >= 2 && v.starts_with('"') && v.ends_with('"');
                let inner = if quoted { &v[1..v.len() - 1] } else { v };
                let name_ok = |n: &str| if quoted { is_query_name(n) } else { is_name(n) };
                let ok = match inner.split_once('.') {
                    Some((p, n)) => is_name(p) && name_ok(n),
                    None => name_ok(inner),
                };
                if !ok {
                    return at(v_at, format!("invalid parent '{v}' (a query name, or pack.query)"));
                }
                if inner == q.id {
                    return at(v_at, "a query can't be its own parent".into());
                }
                q.parent = Some(inner.to_owned());
            }
            "labels" => {
                let mut p = val.start;
                for part in content[val.clone()].split(',') {
                    let start = p + part.len() - part.trim_start().len();
                    p += part.len() + 1;
                    let l = part.trim();
                    let seg_ok =
                        |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-');
                    if l.contains(char::is_whitespace) {
                        return at(start, format!("labels are separated by commas: '{l}'"));
                    }
                    if !l.split('/').all(seg_ok) {
                        return at(start, format!("invalid label '{l}' (words separated by '/')"));
                    }
                    if !q.labels.iter().any(|x| x == l) {
                        q.labels.push(l.to_owned());
                    }
                }
            }
            "color" => {
                let named = v.chars().all(|c| c.is_ascii_lowercase());
                let hex = v.len() == 7 && v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit());
                if !named && !hex {
                    return at(v_at, format!("invalid colour '{v}' (a lowercase name or #rrggbb)"));
                }
                q.color = Some(v.to_owned());
            }
            _ => unreachable!(),
        }
    }
    Ok(q)
}

/// `(key, key offset, value byte range)` of a header attribute.
type Attr = (&'static str, usize, Range<usize>);

/// Split `content[from..]` into `key: value` attributes:
/// `(key, key offset, value byte range)`.
fn attributes(content: &str, from: usize) -> Result<Vec<Attr>, (usize, String)> {
    const KEYS: [&str; 3] = ["parent", "labels", "color"];
    let mut out: Vec<Attr> = Vec::new();
    let rest = &content[from..];
    for tok in rest.split_whitespace() {
        let t_at = from + (tok.as_ptr() as usize - rest.as_ptr() as usize);
        let key =
            tok.split_once(':').map(|(k, _)| k).filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic()));
        match key {
            Some(k) => {
                let Some(&known) = KEYS.iter().find(|&&x| x == k) else {
                    return Err((t_at, format!("unknown attribute '{k}:' (parent:, labels:, color:)")));
                };
                if let Some(last) = out.last_mut() {
                    last.2.end = t_at;
                }
                out.push((known, t_at, t_at + k.len() + 1..content.len()));
            }
            None if out.is_empty() => {
                return Err((t_at, format!("expected parent:, labels: or color: but found '{tok}'")));
            }
            None => {}
        }
    }
    Ok(out)
}

/// Dedent the body, check its parameters and NFD syntax.
fn finish(file: &str, mut q: PackQuery, mut body: Vec<(usize, &str)>) -> Result<PackQuery, PackError> {
    while body.last().is_some_and(|(_, l)| l.trim().is_empty()) {
        body.pop();
    }
    let err = |pos: (usize, usize), msg: String| PackError { file: file.to_owned(), pos: Some(pos), msg };
    if body.is_empty() {
        return Err(err((q.line, 0), format!("query '{}' has no text (indent it below the header)", q.id)));
    }
    let indent = |l: &str| l.len() - l.trim_start_matches([' ', '\t']).len();
    let cut = body.iter().filter(|(_, l)| !l.trim().is_empty()).map(|(_, l)| indent(l)).min().unwrap_or(0);
    let lines: Vec<&str> = body.iter().map(|(_, l)| if l.trim().is_empty() { "" } else { &l[cut..] }).collect();
    q.text = lines.join("\n");
    q.end_line = body.last().map_or(q.line, |(ln, _)| *ln);

    // Byte offset in `q.text` -> (file line, char column).
    let file_pos = |off: usize| {
        let mut start = 0;
        for (k, l) in lines.iter().enumerate() {
            if off <= start + l.len() || k + 1 == lines.len() {
                let b = (off - start).min(l.len());
                return (body[k].0, cut + l[..b].chars().count());
            }
            start += l.len() + 1;
        }
        (body[0].0, cut)
    };

    // Same-length placeholders keep DSL error positions valid. A bare
    // parameter becomes a string, as in the real expansion.
    let text = &q.text;
    let checked = render(
        text,
        false,
        |name, ctx, at| {
            if !is_name(name) {
                return Err(err(file_pos(at), format!("invalid parameter name '{name}'")));
            }
            if !q.params.iter().any(|p| p == name) {
                return Err(err(
                    file_pos(at),
                    format!("parameter '${name}' is not declared (list it in the header: query {}({name}))", q.id),
                ));
            }
            let len = if text[at + 1..].starts_with('{') { name.len() + 3 } else { name.len() + 1 };
            Ok(match ctx {
                Ctx::Bare => format!("\"{}\"", "x".repeat(len - 2)),
                Ctx::Regex | Ctx::Str => "x".repeat(len),
            })
        },
        |at| err(file_pos(at), "missing '}' after '${'".into()),
    )?;
    crate::dsl::parse(&checked).map_err(|e| err(file_pos(e.pos), e.msg))?;
    Ok(q)
}

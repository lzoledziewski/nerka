//! Writing queries into pack files, git-friendly: one query's block is
//! replaced or appended, every other line (comments, blank lines, other
//! queries, their order) stays as it was.

use super::{Ctx, PACK_EXTENSION, PackError, PackQuery, is_name, parse_pack, render};

/// A query name as written in a file: quoted unless it is a NAME.
fn quoted(name: &str) -> String {
    if is_name(name) { name.to_owned() } else { format!("\"{name}\"") }
}
use std::path::{Path, PathBuf};

/// A query as pack text: the header line and the body indented by four
/// spaces, without a trailing newline.
pub fn format_query(q: &PackQuery) -> String {
    let mut out = format!("query {}", quoted(&q.id));
    if !q.params.is_empty() {
        out += &format!("({})", q.params.join(", "));
    }
    if let Some(p) = &q.parent {
        let plain = match p.split_once('.') {
            Some((pack, name)) => is_name(pack) && is_name(name),
            None => is_name(p),
        };
        out += &if plain { format!("  parent: {p}") } else { format!("  parent: \"{p}\"") };
    }
    if !q.labels.is_empty() {
        out += &format!("  labels: {}", q.labels.join(", "));
    }
    if let Some(c) = &q.color {
        out += &format!("  color: {c}");
    }
    for line in q.text.lines() {
        out.push('\n');
        if !line.trim().is_empty() {
            out += "    ";
            out += line;
        }
    }
    out
}

/// `text` (a pack file) with `q` in it: the block of the query with the
/// same id (its header through its last body line) is replaced, or the
/// query is appended after a blank line. The file's line ending is kept.
/// The result is parsed before it is returned, so an invalid query (or
/// one that clashes with the file) is an error and nothing changes.
pub fn upsert_query(file: &str, text: &str, q: &PackQuery) -> Result<String, PackError> {
    put(file, text, &q.id, Some(q))
}

/// `text` with query `old` replaced by `q` in place (`q` may have another
/// name). Errors if `old` doesn't exist or another query has `q`'s name.
pub fn rename_query(file: &str, text: &str, old: &str, q: &PackQuery) -> Result<String, PackError> {
    let pack = parse_pack(file, text)?;
    let err = |msg: String| PackError { file: file.to_owned(), pos: None, msg };
    if pack.query(old).is_none() {
        return Err(err(format!("no query '{old}' in pack '{}'", pack.name)));
    }
    if q.id != old && pack.query(&q.id).is_some() {
        return Err(err(format!("pack '{}' already has a query '{}'", pack.name, q.id)));
    }
    put(file, text, old, Some(q))
}

/// `text` without query `id` (and the blank line before it, if any).
pub fn remove_query(file: &str, text: &str, id: &str) -> Result<String, PackError> {
    if parse_pack(file, text)?.query(id).is_none() {
        return Err(PackError { file: file.to_owned(), pos: None, msg: format!("no query '{id}'") });
    }
    put(file, text, id, None)
}

/// Replace the block of query `at` with `q` (or remove it when `q` is
/// `None`); append `q` if there is no such query. Keeps every other line
/// and the line ending, and checks the result parses.
fn put(file: &str, text: &str, at: &str, q: Option<&PackQuery>) -> Result<String, PackError> {
    let pack = parse_pack(file, text)?;
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let block: Vec<String> = q.map(|q| format_query(q).lines().map(str::to_owned).collect()).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    match pack.query(at) {
        Some(old) => {
            let mut start = old.line;
            if q.is_none() && start > 0 && lines[start - 1].trim().is_empty() {
                start -= 1;
            }
            lines.splice(start..=old.end_line, block);
        }
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            lines.push(String::new());
            lines.extend(block);
        }
    }
    let mut out = lines.join(nl);
    out += nl;
    parse_pack(file, &out)?;
    Ok(out)
}

/// Parameter names used in a query text, in first-use order. Text that
/// isn't valid enough to scan (an unterminated `${`) gives what was found
/// before it.
pub fn params_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let _ = render(
        text,
        false,
        |name, _: Ctx, _| {
            if is_name(name) && !out.iter().any(|p| p == name) {
                out.push(name.to_owned());
            }
            Ok::<_, ()>(String::new())
        },
        |_| (),
    );
    out
}

/// Create `<dir>/<name>.nerka-pack` holding just `pack <name>`. Fails if
/// the name is invalid or the file exists.
pub fn create_pack_file(dir: &Path, name: &str) -> Result<PathBuf, PackError> {
    let path = dir.join(format!("{name}.{PACK_EXTENSION}"));
    let err = |msg: String| PackError { file: path.display().to_string(), pos: None, msg };
    if !is_name(name) {
        return Err(err(format!("invalid pack name '{name}' (letters, digits and _)")));
    }
    if path.exists() {
        return Err(err("the file already exists".into()));
    }
    std::fs::create_dir_all(dir).map_err(|e| err(e.to_string()))?;
    std::fs::write(&path, format!("pack {name}\n")).map_err(|e| err(e.to_string()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
# team pack
pack p

query a(x)  labels: l
    /$x/
    OR /y/
# about b
query b
    level >= ERROR
";

    fn q(id: &str, text: &str) -> PackQuery {
        PackQuery {
            id: id.into(),
            parent: None,
            labels: vec![],
            params: params_in(text),
            color: None,
            text: text.into(),
            line: 0,
            end_line: 0,
        }
    }

    #[test]
    fn replaces_a_block_and_keeps_everything_else() {
        let mut new = q("a", "/$x/\n\nAND /z/");
        new.parent = Some("b".into());
        new.color = Some("#ff8000".into());
        let out = upsert_query("t", FILE, &new).unwrap();
        assert_eq!(
            out,
            "# team pack\npack p\n\nquery a(x)  parent: b  color: #ff8000\n    /$x/\n\n    AND /z/\n# about b\nquery b\n    level >= ERROR\n"
        );
        let p = parse_pack("t", &out).unwrap();
        assert_eq!(p.query("a").unwrap().text, "/$x/\n\nAND /z/");
        assert_eq!((p.queries[0].line, p.queries[0].end_line), (3, 6));
    }

    #[test]
    fn appends_new_queries_and_keeps_crlf() {
        let crlf = FILE.replace('\n', "\r\n") + "\r\n\r\n";
        let out = upsert_query("t", &crlf, &q("c", "apid == NAV")).unwrap();
        assert!(out.ends_with("level >= ERROR\r\n\r\nquery c\r\n    apid == NAV\r\n"), "{out:?}");
        assert!(!out.replace("\r\n", "").contains('\n'));
        let ids: Vec<String> = parse_pack("t", &out).unwrap().queries.into_iter().map(|q| q.id).collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }

    #[test]
    fn invalid_queries_are_not_written() {
        let e = upsert_query("t", FILE, &q("c", "level >= LOUD")).unwrap_err();
        assert!(e.msg.contains("unknown level"), "{e}");
        let mut bad = q("c", "/$y/");
        bad.params.clear();
        assert!(upsert_query("t", FILE, &bad).unwrap_err().msg.contains("not declared"));
    }

    #[test]
    fn quoted_names_rename_and_remove() {
        let mut spaced = q("xyz logs", "apid == XYZ");
        spaced.parent = Some("other.cam scope".into());
        let out = upsert_query("t", FILE, &spaced).unwrap();
        assert!(out.ends_with("query \"xyz logs\"  parent: \"other.cam scope\"\n    apid == XYZ\n"), "{out}");
        let p = parse_pack("t", &out).unwrap();
        let x = p.query("xyz logs").unwrap();
        assert_eq!(x.parent.as_deref(), Some("other.cam scope"));

        // Rename in place; a clash is refused.
        let renamed = rename_query("t", &out, "a", &q("a2", "/$x/")).unwrap();
        let ids: Vec<String> = parse_pack("t", &renamed).unwrap().queries.into_iter().map(|q| q.id).collect();
        assert_eq!(ids, ["a2", "b", "xyz logs"]);
        assert!(rename_query("t", &out, "a", &q("b", "/x/")).unwrap_err().msg.contains("already has"));
        assert!(rename_query("t", &out, "nope", &q("c", "/x/")).is_err());

        // Remove takes the blank line before the block too.
        let removed = remove_query("t", &out, "xyz logs").unwrap();
        assert_eq!(removed, FILE);
        assert!(remove_query("t", FILE, "nope").is_err());
    }

    #[test]
    fn params_and_new_files() {
        assert_eq!(params_in("ecu == $ecu AND /${cam} $$x \\$y $ecu/"), ["ecu", "cam"]);
        assert!(params_in("/down$/").is_empty());
        let dir = std::env::temp_dir().join(format!("nerka-newpack-{}", std::process::id()));
        let path = create_pack_file(&dir, "mine").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "pack mine\n");
        assert!(create_pack_file(&dir, "mine").unwrap_err().msg.contains("exists"));
        assert!(create_pack_file(&dir, "my-pack").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

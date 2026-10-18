//! Writing queries into pack files, git-friendly: one query's block is
//! replaced or appended, every other line (comments, blank lines, other
//! queries, their order) stays as it was.

use super::{Ctx, PACK_EXTENSION, PackError, PackQuery, is_name, parse_pack, render};
use std::path::{Path, PathBuf};

/// A query as pack text: the header line and the body indented by four
/// spaces, without a trailing newline.
pub fn format_query(q: &PackQuery) -> String {
    let mut out = format!("query {}", q.id);
    if !q.params.is_empty() {
        out += &format!("({})", q.params.join(", "));
    }
    if let Some(p) = &q.parent {
        out += &format!("  parent: {p}");
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
    let pack = parse_pack(file, text)?;
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let block: Vec<String> = format_query(q).lines().map(str::to_owned).collect();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    match pack.query(&q.id) {
        Some(old) => {
            lines.splice(old.line..=old.end_line, block);
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

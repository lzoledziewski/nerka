//! Auto mode of the search boxes (`doc/search-modes.md`): plain words are
//! an exact phrase, NFD syntax is a query, and they mix. The box is
//! rewritten into plain NFD (the language itself doesn't change); a box that
//! isn't a valid query is literal text.
//!
//! A word is part of a phrase unless it's a field (followed by an
//! operator), a value (after an operator) or an upper-case `AND` / `OR` /
//! `NOT`. Each run of phrase words becomes one `"…"` (or `/…/i` with ignore
//! case), spanning the words exactly as typed.

use super::parser::{OpTok, Tok, tokens};
use super::{DslError, Query};
use crate::event::FieldDef;
use crate::signal::Signals;
use crate::{MatcherOptions, text_to_nfd};

/// How Auto reads a box.
#[derive(Debug, Clone)]
pub enum AutoRead {
    /// Literal text: the box is one phrase (trimmed).
    Text(String),
    /// A query: the box rewritten into NFD, compiled for the source.
    /// `plain` is set when the query is nothing but a text search (one
    /// quoted string or regex, alone or as `raw ~ …`), so a UI can say so.
    Query { nfd: String, query: Query, plain: Option<Plain> },
    /// Not a valid query, so literal text (the whole box, trimmed); `err`
    /// is why it isn't a query, at a byte offset into the box.
    NotQuery { text: String, err: DslError },
}

/// A query that is only a text search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plain {
    /// Lines containing this text (exact case: it was quoted).
    Text(String),
    /// Lines matching this regex.
    Regex { pattern: String, ignore_case: bool },
}

/// Read `src` as Auto does, phrases literal. `ignore_case` applies to the
/// phrases; `schema` is the open source's fields (a query must compile for
/// it).
pub fn read_auto(src: &str, ignore_case: bool, schema: &[FieldDef]) -> AutoRead {
    let literal = MatcherOptions { fixed_string: true, case_insensitive: ignore_case, invert: false };
    read_auto_with(src, &literal, schema)
}

/// Read `src` as Auto does, phrases as `phrases` says: literal, or regexes
/// (`fixed_string: false`, the Regex checkbox), with or without case.
/// [`AutoRead::Text`] and [`AutoRead::NotQuery`] carry the box's text; the
/// caller reads it with the same options.
pub fn read_auto_with(src: &str, phrases: &MatcherOptions, schema: &[FieldDef]) -> AutoRead {
    read_auto_in(src, phrases, schema, &Signals::default())
}

/// [`read_auto_with`] in a tab that defines `signals` (`@speed > 100`).
pub fn read_auto_in(src: &str, phrases: &MatcherOptions, schema: &[FieldDef], signals: &Signals) -> AutoRead {
    let text = src.trim().to_owned();
    let toks = match tokens(src) {
        Ok(t) => t,
        Err(err) => return AutoRead::NotQuery { text, err },
    };
    // In Auto only upper-case keywords are operators: "error and timeout"
    // is a phrase.
    let word = |i: usize| match &toks[i].0 {
        Tok::Word(_) => true,
        Tok::And | Tok::Or | Tok::Not => {
            let s = &src[toks[i].1.clone()];
            s.chars().all(char::is_alphabetic) && s != s.to_ascii_uppercase()
        }
        _ => false,
    };
    let is_op = |i: usize| matches!(toks.get(i), Some((Tok::Op(_), _)));
    let phrase_word = |i: usize| word(i) && !is_op(i + 1) && !(i > 0 && is_op(i - 1));

    let options = MatcherOptions { invert: false, ..phrases.clone() };
    let mut nfd = String::with_capacity(src.len() + 8);
    let mut copied = 0; // `src` is copied up to here
    let mut phrases = 0;
    let mut i = 0;
    while i < toks.len() {
        if !phrase_word(i) {
            i += 1;
            continue;
        }
        let start = toks[i].1.start;
        let mut end = toks[i].1.end;
        while i + 1 < toks.len() && phrase_word(i + 1) {
            i += 1;
            end = toks[i].1.end;
        }
        nfd.push_str(&src[copied..start]);
        nfd.push_str(&text_to_nfd(&src[start..end], &options));
        copied = end;
        phrases += 1;
        i += 1;
    }
    nfd.push_str(&src[copied..]);
    let nfd = nfd.trim().to_owned();

    let whole_phrase = phrases == 1 && toks.iter().enumerate().all(|(i, _)| phrase_word(i));
    if toks.is_empty() || whole_phrase {
        return AutoRead::Text(text);
    }
    match Query::compile_in(&nfd, schema, signals) {
        Ok(query) => {
            let term = match toks.as_slice() {
                [(t, _)] => Some(t),
                [(Tok::Word(w), _), (Tok::Op(OpTok::Match), _), (t, _)] if w.eq_ignore_ascii_case("raw") => Some(t),
                _ => None,
            };
            let plain = match term {
                Some(Tok::Str(s)) => Some(Plain::Text(s.clone())),
                Some(Tok::Regex { pat, ci }) => Some(Plain::Regex { pattern: pat.clone(), ignore_case: *ci }),
                _ => None,
            };
            AutoRead::Query { nfd, query, plain }
        }
        Err(err) => {
            // Positions are in the rewritten text; map them back to the box
            // only where nothing before them was rewritten.
            let pos = if src.trim_start().get(..err.pos) == nfd.get(..err.pos) {
                err.pos + (src.len() - src.trim_start().len())
            } else {
                0
            };
            AutoRead::NotQuery { text, err: DslError { pos, ..err } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::search::tests::run_on;
    use crate::event::FieldKind;
    use crate::{Filter, Matcher, TextFile};

    /// `Ok(nfd)` for a query, `Err(text)` for text (with the error, if any).
    fn read(src: &str) -> Result<String, (String, Option<String>)> {
        read_with(src, false, &[])
    }

    fn read_with(src: &str, ci: bool, schema: &[FieldDef]) -> Result<String, (String, Option<String>)> {
        match read_auto(src, ci, schema) {
            AutoRead::Text(t) => Err((t, None)),
            AutoRead::Query { nfd, .. } => Ok(nfd),
            AutoRead::NotQuery { text, err } => Err((text, Some(err.msg))),
        }
    }

    fn text(t: &str) -> Result<String, (String, Option<String>)> {
        Err((t.to_owned(), None))
    }

    #[test]
    fn plain_words_are_one_exact_phrase() {
        assert_eq!(read("connection reset"), text("connection reset"));
        assert_eq!(read("  connection   reset  "), text("connection   reset"));
        assert_eq!(read("error and timeout"), text("error and timeout"));
        assert_eq!(read("Not found"), text("Not found"));
        assert_eq!(read("ERROR"), text("ERROR"));
        assert_eq!(read(""), text(""));
    }

    #[test]
    fn nfd_stays_a_query_and_mixes_with_phrases() {
        assert_eq!(read("level >= ERROR").unwrap(), "level >= ERROR");
        assert_eq!(read("/SSL|CERT/i AND NOT \"healthcheck\"").unwrap(), "/SSL|CERT/i AND NOT \"healthcheck\"");
        assert_eq!(read("timeout AND level >= WARN").unwrap(), "\"timeout\" AND level >= WARN");
        assert_eq!(read("NOT healthcheck").unwrap(), "NOT \"healthcheck\"");
        assert_eq!(
            read("connection  reset OR (disk full && !ok)").unwrap(),
            "\"connection  reset\" OR (\"disk full\" && !\"ok\")"
        );
        // A phrase in double quotes or a regex is written out: kept as is.
        assert_eq!(read("\"a b\" OR c").unwrap(), "\"a b\" OR \"c\"");
    }

    #[test]
    fn queries_that_are_only_text_say_so() {
        let plain = |src: &str| match read_auto(src, false, &[]) {
            AutoRead::Query { plain, .. } => plain,
            r => panic!("{src}: {r:?}"),
        };
        assert_eq!(plain("\"ctid = BOOT\""), Some(Plain::Text("ctid = BOOT".into())));
        assert_eq!(plain("raw ~ \"x y\""), Some(Plain::Text("x y".into())));
        assert_eq!(plain("/SSL|CERT/i"), Some(Plain::Regex { pattern: "SSL|CERT".into(), ignore_case: true }));
        assert_eq!(plain("RAW ~ /a b/"), Some(Plain::Regex { pattern: "a b".into(), ignore_case: false }));
        assert_eq!(plain("NOT \"x\""), None);
        assert_eq!(plain("\"a\" OR \"b\""), None);
        assert_eq!(plain("level >= ERROR"), None);
    }

    #[test]
    fn ignore_case_applies_to_phrases_only() {
        assert_eq!(read_with("connection reset", true, &[]), text("connection reset"));
        assert_eq!(read_with("a.b AND /X/", true, &[]).unwrap(), "/a\\.b/i AND /X/");
        assert_eq!(read_with("x AND \"Y\"", true, &[]).unwrap(), "/x/i AND \"Y\"");
    }

    #[test]
    fn phrases_can_be_regexes() {
        let re = |ci| MatcherOptions { fixed_string: false, case_insensitive: ci, invert: false };
        let read_re = |src: &str, ci| match read_auto_with(src, &re(ci), &[]) {
            AutoRead::Query { nfd, .. } => Ok(nfd),
            AutoRead::Text(t) | AutoRead::NotQuery { text: t, .. } => Err(t),
        };
        assert_eq!(read_re("time.out AND level >= ERROR", false).unwrap(), "/time.out/ AND level >= ERROR");
        assert_eq!(read_re("time.out AND level >= ERROR", true).unwrap(), "/time.out/i AND level >= ERROR");
        // Regex syntax NFD doesn't know makes the whole box one regex.
        assert_eq!(read_re("time(out)? AND level >= ERROR", false), Err("time(out)? AND level >= ERROR".into()));
        assert_eq!(read_re("a|b", false), Err("a|b".into()));
        // Literal by default.
        assert_eq!(read("time.out AND level >= ERROR").unwrap(), "\"time.out\" AND level >= ERROR");
    }

    #[test]
    fn anything_else_is_literal_text() {
        // Characters NFD doesn't know.
        let (t, e) = read("[ERROR] disk full").unwrap_err();
        assert_eq!((t.as_str(), e.unwrap().contains("unexpected character '['")), ("[ERROR] disk full", true));
        // Valid tokens, not a valid query.
        let (t, e) = read(" foo (bar) ").unwrap_err();
        assert_eq!(t, "foo (bar)");
        assert!(e.is_some());
        assert!(read("a AND").unwrap_err().1.is_some());
        // Fields the source doesn't have.
        let (t, e) = read("apid == NAV").unwrap_err();
        assert_eq!(t, "apid == NAV");
        assert!(e.unwrap().contains("apid"));
        let schema = [FieldDef { name: "apid", kind: FieldKind::Str, costly: false }];
        assert_eq!(read_with("apid == NAV AND timeout", false, &schema).unwrap(), "apid == NAV AND \"timeout\"");
    }

    #[test]
    fn signals_mix_with_phrases() {
        let lit = MatcherOptions { fixed_string: true, ..Default::default() };
        let mut sig = Signals::default();
        sig.set(crate::SignalDef::parse_spec(r"v=/v=(?<v>\d+)/").unwrap());
        match read_auto_in("@v > 1 AND disk full", &lit, &[], &sig) {
            AutoRead::Query { nfd, .. } => assert_eq!(nfd, "@v > 1 AND \"disk full\""),
            r => panic!("{r:?}"),
        }
        // Without the signal it isn't a query: literal text.
        assert!(matches!(read("@v > 1"), Err((_, Some(e))) if e.contains("unknown signal '@v'")));
    }

    #[test]
    fn error_positions_point_into_the_box() {
        let AutoRead::NotQuery { err, .. } = read_auto("  level >= LOUD", false, &[]) else { panic!() };
        assert_eq!(err.pos, 11);
        // After a rewritten phrase the position can't be mapped back.
        let AutoRead::NotQuery { err, .. } = read_auto("x AND level >= LOUD", false, &[]) else { panic!() };
        assert_eq!(err.pos, 0);
    }

    /// What Auto runs finds the same lines as the hand-written filter,
    /// fast path against naive (`run_on`).
    #[test]
    fn auto_finds_the_hand_written_lines() {
        let log = "[INFO] connection reset by peer\n[ERROR] connection  reset\n[ERROR] disk full\n[WARN] Timeout\n[ERROR] timeout\n";
        let f = TextFile::from_bytes("t", log.as_bytes().to_vec());
        let run = |src: &str, ci: bool| {
            let filter = match read_auto(src, ci, &[]) {
                AutoRead::Query { query, .. } => Filter::Query(query),
                AutoRead::Text(t) | AutoRead::NotQuery { text: t, .. } => {
                    let o = MatcherOptions { fixed_string: true, case_insensitive: ci, invert: false };
                    Filter::Regex(Matcher::new(&t, o).unwrap())
                }
            };
            run_on(&f, &filter)
        };
        let q = |s: &str| run_on(&f, &Filter::Query(Query::compile(s).unwrap()));
        assert_eq!(run("connection reset", false), [0]);
        assert_eq!(run("timeout AND level >= WARN", false), q("raw ~ \"timeout\" AND level >= WARN"));
        assert_eq!(run("timeout AND level >= WARN", true), [3, 4]);
        assert_eq!(run("[ERROR] disk", false), [2]);
    }
}

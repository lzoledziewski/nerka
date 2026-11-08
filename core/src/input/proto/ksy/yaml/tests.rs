use super::*;

/// The value as a compact string: maps `{k: v}`, sequences `[a, b]`,
/// quoted scalars `'x'`, nulls `~`.
fn show(n: &Node) -> String {
    match &n.value {
        Value::Null => "~".into(),
        Value::Scalar { text, quoted: true } => format!("'{text}'"),
        Value::Scalar { text, quoted: false } => text.clone(),
        Value::Seq(items) => format!("[{}]", items.iter().map(show).collect::<Vec<_>>().join(", ")),
        Value::Map(entries) => {
            format!(
                "{{{}}}",
                entries.iter().map(|(k, v)| format!("{}: {}", k.text, show(v))).collect::<Vec<_>>().join(", ")
            )
        }
    }
}

fn yaml(text: &str) -> String {
    show(&parse(text).unwrap_or_else(|e| panic!("{e}")))
}

fn error(text: &str) -> (usize, usize, String) {
    let e = parse(text).expect_err("should fail");
    (e.line, e.col, e.message)
}

#[test]
fn block_mappings_and_sequences() {
    let text = "meta:\n  id: dlt # comment\n  endian: be\nseq:\n  - id: htyp\n    type: u1\n  - id: len\n    valid:\n      min: 4\n";
    assert_eq!(yaml(text), "{meta: {id: dlt, endian: be}, seq: [{id: htyp, type: u1}, {id: len, valid: {min: 4}}]}");
}

#[test]
fn a_sequence_at_its_keys_indent() {
    assert_eq!(yaml("a:\n- 1\n- 2\nb: 3\n"), "{a: [1, 2], b: 3}");
}

#[test]
fn flow_collections() {
    assert_eq!(yaml("x: { id: a, type: u1, if: (h & 0x04) != 0 }"), "{x: {id: a, type: u1, if: (h & 0x04) != 0}}");
    assert_eq!(yaml("x: [1, 'a, b', \"c\"]"), "{x: [1, 'a, b', 'c']}");
    assert_eq!(yaml("- { a: [1, 2], b: {} }\n- []"), "[{a: [1, 2], b: {}}, []]");
    // Over several lines.
    assert_eq!(yaml("x: { a: 1,\n     b: 2 }\ny: 3"), "{x: {a: 1, b: 2}, y: 3}");
}

#[test]
fn quoted_and_plain_scalars() {
    assert_eq!(
        yaml("a: 'it''s'\nb: \"x\\ty\\x41\"\nc: ~\nd:\ne: 'a ? b : c'"),
        "{a: 'it's', b: 'x\tyA', c: ~, d: ~, e: 'a ? b : c'}"
    );
    assert_eq!(yaml("'[0x49, 0x49]': le\ntrue: be"), "{[0x49, 0x49]: le, true: be}");
    assert_eq!(yaml("a: x # not this\nb: 'y # but this'"), "{a: x, b: 'y # but this'}");
    assert_eq!(yaml("a: x#y"), "{a: x#y}");
}

#[test]
fn keys_starting_with_a_dash_are_keys() {
    assert_eq!(yaml("-nerka:\n  name: X\n"), "{-nerka: {name: X}}");
}

#[test]
fn block_scalars() {
    assert_eq!(yaml("doc: |\n  one\n  two\n\nx: 1"), "{doc: 'one\ntwo\n', x: 1}");
    assert_eq!(yaml("doc: >-\n  one\n  two\nx: 1"), "{doc: 'one two', x: 1}");
}

#[test]
fn document_start_and_empty_documents() {
    assert_eq!(yaml("---\na: 1"), "{a: 1}");
    assert_eq!(yaml(""), "~");
    assert_eq!(yaml("# only a comment\n"), "~");
}

#[test]
fn errors_with_positions() {
    assert_eq!(error("a: 1\na: 2"), (2, 1, "`a` is given twice".into()));
    assert_eq!(error("a:\n\tb: 1"), (2, 1, "tabs can't indent YAML".into()));
    assert_eq!(error("a: { b: 1"), (1, 4, "not closed".into()));
    assert_eq!(error("a: &x 1").2, "anchors aren't read");
    assert_eq!(error("a: 'open").2, "the quote isn't closed");
    assert_eq!(
        error("a:\n  b: 1\n    c: 2"),
        (3, 5, "a plain value can't continue on the next line here; quote it".into())
    );
    assert_eq!(error("a: [1, 2] x"), (1, 11, "unexpected text after the closing bracket".into()));
    assert_eq!(error("a: 1\n b: 2").0, 2);
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let deep = "[".repeat(200) + &"]".repeat(200);
    assert_eq!(error(&format!("a: {deep}")).2, "nested too deeply");
    let mut block = String::new();
    for i in 0..200 {
        block.push_str(&" ".repeat(i));
        block.push_str("k:\n");
    }
    assert_eq!(error(&block).2, "nested too deeply");
}

#[test]
fn garbage_never_panics() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let alphabet = b"ab:- {}[],'\"#|>\n  \t0x&*!~\\";
    for _ in 0..3000 {
        let len = (seed % 60) as usize;
        let text: String = (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                alphabet[(seed % alphabet.len() as u64) as usize] as char
            })
            .collect();
        let _ = parse(&text);
    }
}

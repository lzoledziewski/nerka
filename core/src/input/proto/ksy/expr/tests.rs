use super::*;

/// The tree with every operation in brackets.
fn show(e: &Expr) -> String {
    match e {
        Expr::Int(v) => v.to_string(),
        Expr::Float(v) => format!("{v:?}"),
        Expr::Bool(b) => b.to_string(),
        Expr::Str(s) => format!("'{}'", String::from_utf8_lossy(s)),
        Expr::Name(n) => n.clone(),
        Expr::Attr(a, n) => format!("{}.{n}", show(a)),
        Expr::Index(a, i) => format!("{}[{}]", show(a), show(i)),
        Expr::Unary(op, a) => format!("({op:?} {})", show(a)),
        Expr::Binary(op, a, b) => format!("({} {op:?} {})", show(a), show(b)),
        Expr::Cond(c, a, b) => format!("({} ? {} : {})", show(c), show(a), show(b)),
        Expr::EnumLit(e, l) => format!("{e}::{l}"),
    }
}

fn p(text: &str) -> String {
    show(&parse(text).unwrap_or_else(|e| panic!("{text}: {e}")))
}

fn err(text: &str) -> (usize, String) {
    let e = parse(text).expect_err("should fail");
    (e.at, e.message)
}

#[test]
fn comparisons_bind_more_loosely_than_bit_operations() {
    assert_eq!(p("htyp & 0x04 != 0"), "((htyp BitAnd 4) Ne 0)");
    assert_eq!(p("(htyp & 0xe0) == 0x20"), "((htyp BitAnd 224) Eq 32)");
    assert_eq!(p("a | b ^ c & d << 1 + 2 * 3"), "(a BitOr (b BitXor (c BitAnd (d Shl (1 Add (2 Mul 3))))))");
}

#[test]
fn logic_and_conditions() {
    assert_eq!(p("not a and b or c"), "(((Not a) And b) Or c)");
    assert_eq!(p("not a == b"), "(Not (a Eq b))");
    assert_eq!(p("a ? b : c ? d : e"), "(a ? b : (c ? d : e))");
    assert_eq!(p("x.verbose ? 'verbose' : 'non-verbose'"), "(x.verbose ? 'verbose' : 'non-verbose')");
}

#[test]
fn names_paths_and_indexes() {
    assert_eq!(p("_root.body.ext.apid"), "_root.body.ext.apid");
    assert_eq!(p("payload[0] + _io.size"), "(payload[0] Add _io.size)");
    assert_eq!(p("a.length - 4"), "(a.length Sub 4)");
}

#[test]
fn numbers() {
    assert_eq!(p("0x1F + 0b101 + 0o17 + 1_000"), "(((31 Add 5) Add 15) Add 1000)");
    assert_eq!(p("tmsp / 10000.0"), "(tmsp Div 10000.0)");
    assert_eq!(p("-1"), "-1");
    assert_eq!(p("-x"), "(Neg x)");
    assert_eq!(p("~0"), "(BitNot 0)");
    assert_eq!(p("0xffffffffffffffff"), "-1");
    assert_eq!(p("-9223372036854775808"), "-9223372036854775808");
}

#[test]
fn strings() {
    assert_eq!(p("\"a\\tb\""), "'a\tb'");
    assert_eq!(p("'a\\tb'"), "'a\\tb'");
}

#[test]
fn errors_at_their_place() {
    assert_eq!(err("a +"), (3, "expected a value".into()));
    assert_eq!(err("(a"), (2, "expected `)`, found the end".into()));
    assert_eq!(err("a b"), (2, "unexpected `b`".into()));
    assert_eq!(err("a == b == c"), (7, "comparisons don't chain; use `and`".into()));
    assert_eq!(p("sid == service::marker"), "(sid Eq service::marker)");
    assert_eq!(err("a::b::c"), (4, "enums of other types (`a::b::c`) aren't read yet".into()));
    assert_eq!(err("a:: 1"), (4, "expected an enum label after `::`".into()));
    assert_eq!(err("0xZZ"), (0, "`0xZZ` isn't a number".into()));
    assert_eq!(err("'open"), (0, "the string isn't closed".into()));
    assert_eq!(err("a $ b"), (2, "unexpected `$`".into()));
    assert_eq!(err("a and"), (5, "expected a value".into()));
    assert_eq!(err("a.f(1)"), (3, "methods with arguments aren't read yet".into()));
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let deep = "(".repeat(500) + "1" + &")".repeat(500);
    assert_eq!(err(&deep).1, "nested too deeply");
    assert_eq!(err(&"-".repeat(500)).1, "nested too deeply");
    assert_eq!(err(&"not ".repeat(500)).1, "nested too deeply");
    assert!(err(&"a+".repeat(3000)).1.starts_with("longer than"));
}

#[test]
fn garbage_never_panics() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let alphabet = b"ab_0x1.()[]?:<>=!&|^~+-*/% '\"notandor";
    for _ in 0..5000 {
        let len = (seed % 30) as usize;
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

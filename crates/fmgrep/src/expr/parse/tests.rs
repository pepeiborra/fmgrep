use super::*;

/// Deterministic xorshift, so failures replay.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n) as usize
    }
}

fn t(s: &str) -> Expr {
    Expr::Term(s.to_owned())
}

fn re(s: &str) -> Expr {
    Expr::Regex(s.to_owned())
}

fn and(v: Vec<Expr>) -> Expr {
    Expr::And(v)
}

fn or(v: Vec<Expr>) -> Expr {
    Expr::Or(v)
}

fn parsed(src: &str) -> Expr {
    src.parse()
        .unwrap_or_else(|e: SyntaxError| panic!("{src:?}: {}", e.render(src)))
}

#[test]
fn text_and_binds_tighter_than_or() {
    for (src, want) in [
        ("a", t("a")),
        ("a AND b OR c", or(vec![and(vec![t("a"), t("b")]), t("c")])),
        ("a OR b AND c", or(vec![t("a"), and(vec![t("b"), t("c")])])),
        (
            "a OR b AND c OR d",
            or(vec![t("a"), and(vec![t("b"), t("c")]), t("d")]),
        ),
        ("a AND b AND c", and(vec![t("a"), t("b"), t("c")])),
        ("a OR b OR c", or(vec![t("a"), t("b"), t("c")])),
        (
            "a AND b AND c OR d OR e AND f",
            or(vec![
                and(vec![t("a"), t("b"), t("c")]),
                t("d"),
                and(vec![t("e"), t("f")]),
            ]),
        ),
    ] {
        assert_eq!(parsed(src), want, "{src}");
    }
}

#[test]
fn text_parentheses_group_and_nest() {
    for (src, want) in [
        (
            "(a OR b) AND c",
            and(vec![or(vec![t("a"), t("b")]), t("c")]),
        ),
        (
            "a AND (b OR (c AND d))",
            and(vec![t("a"), or(vec![t("b"), and(vec![t("c"), t("d")])])]),
        ),
        ("((a))", t("a")),
        ("(a OR b) OR c", or(vec![or(vec![t("a"), t("b")]), t("c")])),
        ("a AND(b)", and(vec![t("a"), t("b")])),
        ("(a)AND(b)", and(vec![t("a"), t("b")])),
        (" \t( a\nOR b )\n", or(vec![t("a"), t("b")])),
    ] {
        assert_eq!(parsed(src), want, "{src}");
    }
    let deep = format!("{}a{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
    assert_eq!(parsed(&deep), t("a"));
    let want = (0..MAX_DEPTH / 2).fold(t("a"), |e, _| or(vec![and(vec![e, t("b")]), t("c")]));
    assert_eq!(parsed(&worst_case(MAX_DEPTH / 2)), want);
}

/// `n` groups of two nodes each: `Or[And[inner, b], c]`.
fn worst_case(n: usize) -> String {
    (0..n).fold("a".to_owned(), |e, _| format!("({e} AND b OR c)"))
}

#[test]
fn text_quoting_is_shell_like() {
    for (src, want) in [
        (r#""a b""#, t("a b")),
        ("'it''s'", t("its")),
        (r#"it"'"s"#, t("it's")),
        (r#""q\"\\""#, t(r#"q"\"#)),
        (r#""a\nb""#, t(r"a\nb")),
        (r"a\ b", t("a b")),
        (r"\(p\)", t("(p)")),
        ("'a\\b'", t("a\\b")),
        (r#""""#, t("")),
        (r#""AND""#, t("AND")),
        ("'OR'", t("OR")),
        (r"\AND", t("AND")),
        ("and", t("and")),
        ("Or", t("Or")),
        ("a/b", t("a/b")),
        ("StgClosure*", t("StgClosure*")),
        ("-", t("-")),
        ("\"é\u{a0}ü\" AND x", and(vec![t("é\u{a0}ü"), t("x")])),
    ] {
        assert_eq!(parsed(src), want, "{src}");
    }
    // NBSP separates words, as any whitespace does.
    assert_eq!("é\u{a0}ü".parse::<Expr>().unwrap_err().at, "é\u{a0}".len());
}

#[test]
fn text_regex_leaves() {
    for (src, want) in [
        (r"/a\/b/", re("a/b")),
        (r"/\w+ one/", re(r"\w+ one")),
        (r"/a\\/", re(r"a\\")),
        ("/[)(]/", re("[)(]")),
        ("(/x/)", re("x")),
        (r#"/pprPanic "[^"\n]*"/"#, re(r#"pprPanic "[^"\n]*""#)),
        ("/a/ AND(/b/)", and(vec![re("a"), re("b")])),
        (r#""/x/""#, t("/x/")),
    ] {
        assert_eq!(parsed(src), want, "{src}");
    }
}

#[test]
fn text_errors_point_at_the_fault() {
    const OPERAND: &str = "expected a term, /regex/ or '('";
    const NEXT: &str = "expected AND, OR or ')'";
    let quote = format!("{NEXT} (quote a term that holds spaces or parentheses)");
    let caps = format!("{NEXT} (operators are AND / OR in capitals)");
    let deep = format!(
        "{}a{}",
        "(".repeat(MAX_DEPTH + 1),
        ")".repeat(MAX_DEPTH + 1)
    );
    let too_many_nodes = format!("{} AND d", worst_case(MAX_DEPTH / 2));
    let cases: Vec<(&str, usize, &str)> = vec![
        ("", 0, "empty expression"),
        ("   ", 0, "empty expression"),
        ("(a", 0, "unclosed '('"),
        ("a AND ((b)", 6, "unclosed '('"),
        ("a)", 1, "unmatched ')'"),
        (")", 0, OPERAND),
        ("()", 1, "empty ()"),
        ("a AND ()", 7, "empty ()"),
        ("a AND", 5, OPERAND),
        ("AND a", 0, OPERAND),
        ("a AND OR b", 6, OPERAND),
        ("a OR", 4, OPERAND),
        ("(a AND)", 6, OPERAND),
        ("a b", 2, &quote),
        ("f(x)", 1, &quote),
        ("a /b/", 2, NEXT),
        ("a and b", 2, &caps),
        ("a Or b", 2, &caps),
        (r#"x "abc"#, 2, "unterminated \""),
        ("'abc", 0, "unterminated '"),
        (r#""ab\""#, 0, "unterminated \""),
        ("/abc", 0, "unterminated /regex/"),
        (r"/ab\/", 0, "unterminated /regex/"),
        ("//", 0, "empty /regex/"),
        ("/x/y", 3, "expected a space or parenthesis after /regex/"),
        ("/x/i", 3, "expected a space or parenthesis after /regex/"),
        ("a\\", 1, "trailing backslash"),
        (&deep, MAX_DEPTH, "nesting deeper than 60"),
        (&too_many_nodes, 0, "nesting deeper than 60"),
    ];
    for (src, at, msg) in cases {
        let err = src.parse::<Expr>().unwrap_err();
        assert_eq!(
            err,
            SyntaxError {
                at,
                msg: msg.to_owned()
            },
            "{src:?}"
        );
    }
}

#[test]
fn text_error_renders_a_caret() {
    let render = |src: &str| src.parse::<Expr>().unwrap_err().render(src);
    assert_eq!(
        render("a b"),
        "expected AND, OR or ')' (quote a term that holds spaces or parentheses)\n  a b\n    ^"
    );
    assert_eq!(
        render("a AND"),
        "expected a term, /regex/ or '('\n  a AND\n       ^"
    );
    assert_eq!(render("é\tAND\n(x"), "unclosed '('\n  é AND (x\n        ^");
}

/// `e` up to what the text cannot tell apart: one-operand nodes, and `\/`
/// against `/` in a regex.
fn norm(e: &Expr) -> Expr {
    match e {
        Expr::Term(_) => e.clone(),
        Expr::Regex(r) => {
            let mut out = String::new();
            let mut chars = r.chars();
            while let Some(c) = chars.next() {
                match (c, chars.clone().next()) {
                    ('\\', Some('/')) => {
                        chars.next();
                        out.push('/');
                    }
                    ('\\', Some(e)) => {
                        chars.next();
                        out.extend(['\\', e]);
                    }
                    _ => out.push(c),
                }
            }
            Expr::Regex(out)
        }
        Expr::And(v) | Expr::Or(v) if v.len() == 1 => norm(&v[0]),
        Expr::And(v) => Expr::And(v.iter().map(norm).collect()),
        Expr::Or(v) => Expr::Or(v.iter().map(norm).collect()),
    }
}

const PIECES: [&str; 20] = [
    " ", "\t", "\n", "\u{a0}", "\"", "'", "\\", "/", "\\/", "(", ")", "AND", "OR", "and", "é", "*",
    "-", "a", "x", "\\\\",
];

fn leaf_text(rng: &mut Rng) -> String {
    (0..rng.below(4))
        .map(|_| PIECES[rng.below(PIECES.len() as u64)])
        .collect()
}

fn tree(rng: &mut Rng, depth: u32) -> Expr {
    if depth == 0 || rng.below(3) == 0 {
        let s = leaf_text(rng);
        let odd_tail = s.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1;
        // A regex ending in a lone backslash is no regex.
        return if rng.below(2) == 0 || s.is_empty() || odd_tail {
            Expr::Term(s)
        } else {
            Expr::Regex(s)
        };
    }
    let kids = (0..1 + rng.below(3))
        .map(|_| tree(rng, depth - 1))
        .collect();
    if rng.below(2) == 0 {
        Expr::And(kids)
    } else {
        Expr::Or(kids)
    }
}

#[test]
fn display_reads_back_as_the_same_tree() {
    let fixed = [
        and(vec![or(vec![t("a"), t("b")]), re("c+")]),
        or(vec![t("a"), or(vec![t("b"), t("c")])]),
        and(vec![t("x"), or(vec![or(vec![t("a"), t("b")])])]),
        or(vec![
            t(""),
            t("AND"),
            t("OR"),
            t("/x/"),
            t("a b"),
            t("q\"\\"),
            t("(p)"),
        ]),
        and(vec![t("and"), t("a/b"), t("StgClosure*")]),
        or(vec![t("it's"), re("a/b"), re(r"a\/b"), re(r"a\\/b")]),
    ];
    for e in fixed {
        assert_eq!(parsed(&e.to_string()), norm(&e), "{e}");
    }
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for _ in 0..20_000 {
        let e = tree(&mut rng, 4);
        assert_eq!(parsed(&e.to_string()), norm(&e), "{e:?} as {e}");
    }
}

#[test]
fn text_tree_equals_json_tree() {
    for (text, json) in [
        ("a", r#"{"term":"a"}"#),
        ("/b+/", r#"{"regex":"b+"}"#),
        ("a AND /b+/", r#"{"and":[{"term":"a"},{"regex":"b+"}]}"#),
        (
            "a OR b AND c",
            r#"{"or":[{"term":"a"},{"and":[{"term":"b"},{"term":"c"}]}]}"#,
        ),
        (
            "(a OR b) AND (c OR d)",
            r#"{"and":[{"or":[{"term":"a"},{"term":"b"}]},{"or":[{"term":"c"},{"term":"d"}]}]}"#,
        ),
        (
            "(a OR b) OR c",
            r#"{"or":[{"or":[{"term":"a"},{"term":"b"}]},{"term":"c"}]}"#,
        ),
        (
            r#""StgClosure *" AND 'x"y'"#,
            r#"{"and":[{"term":"StgClosure *"},{"term":"x\"y"}]}"#,
        ),
        (
            r#"/pprPanic "[^"\n]*"/ OR "AND""#,
            r#"{"or":[{"regex":"pprPanic \"[^\"\\n]*\""},{"term":"AND"}]}"#,
        ),
    ] {
        assert_eq!(
            parsed(text),
            serde_json::from_str::<Expr>(json).unwrap(),
            "{text}"
        );
    }
}

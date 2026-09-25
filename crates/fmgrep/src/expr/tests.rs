use super::*;

fn term(t: &str) -> Expr {
    Expr::Term(t.to_owned())
}

fn re(r: &str) -> Expr {
    Expr::Regex(r.to_owned())
}

#[test]
fn expr_json_shape_round_trips() {
    let e = Expr::Or(vec![
        Expr::And(vec![term("HscEnv"), re("pr[i1]ntk")]),
        term("a b"),
        Expr::Or(vec![Expr::And(vec![term("x")])]),
    ]);
    let json = serde_json::to_string(&e).unwrap();
    assert_eq!(
        json,
        r#"{"or":[{"and":[{"term":"HscEnv"},{"regex":"pr[i1]ntk"}]},{"term":"a b"},{"or":[{"and":[{"term":"x"}]}]}]}"#
    );
    assert_eq!(serde_json::from_str::<Expr>(&json).unwrap(), e);
}

#[test]
fn expr_json_refuses_unknown_keys_and_empty_operands() {
    for bad in [
        r#"{"not":{"term":"a"}}"#,
        r#"{"term":"a","regex":"b"}"#,
        r#"{"term":1}"#,
        r#"{"and":{"term":"a"}}"#,
        r#""a""#,
        r#"{}"#,
    ] {
        assert!(serde_json::from_str::<Expr>(bad).is_err(), "{bad}");
    }
    for empty in [r#"{"and":[]}"#, r#"{"or":[{"term":"a"},{"or":[]}]}"#] {
        let e: Expr = serde_json::from_str(empty).unwrap();
        assert!(
            e.check().unwrap_err().contains("at least one operand"),
            "{empty}"
        );
    }
    assert!(Expr::And(vec![term("a")]).check().is_ok());
}

#[test]
fn expr_display_quotes_and_parenthesizes() {
    let cases = [
        (Expr::And(vec![term("a"), term("b")]), "a AND b"),
        (
            Expr::Or(vec![Expr::And(vec![term("a"), term("b")]), term("c")]),
            "a AND b OR c",
        ),
        (
            Expr::And(vec![Expr::Or(vec![term("a"), term("b")]), re("c+")]),
            "(a OR b) AND /c+/",
        ),
        (
            Expr::Or(vec![term("a"), Expr::Or(vec![term("b"), term("c")])]),
            "a OR (b OR c)",
        ),
        (
            Expr::And(vec![term("a"), Expr::And(vec![term("b"), term("c")])]),
            "a AND (b AND c)",
        ),
        (
            Expr::And(vec![
                term("x"),
                Expr::Or(vec![Expr::Or(vec![term("a"), term("b")])]),
            ]),
            "x AND (a OR b)",
        ),
        (Expr::Or(vec![term("only")]), "only"),
        (
            Expr::Or(vec![
                term(""),
                term("AND"),
                term("OR"),
                term("/x/"),
                term("a b"),
                term("q\"\\"),
                term("(p)"),
            ]),
            r#""" OR "AND" OR "OR" OR "/x/" OR "a b" OR "q\"\\" OR "(p)""#,
        ),
        (
            Expr::And(vec![term("and"), term("a/b"), term("StgClosure*")]),
            "and AND a/b AND StgClosure*",
        ),
        (
            Expr::Or(vec![term("it's"), re("a/b"), re(r"a\/b"), re(r"a\\/b")]),
            r#""it's" OR /a\/b/ OR /a\/b/ OR /a\\\/b/"#,
        ),
    ];
    for (e, want) in cases {
        assert_eq!(e.to_string(), want, "{e:?}");
    }
}

#[test]
fn expr_eval_follows_and_or() {
    let e = Expr::Or(vec![
        Expr::And(vec![term("a"), term("b")]),
        Expr::And(vec![term("c"), Expr::Or(vec![term("d")])]),
    ]);
    assert_eq!(e.leaves().len(), 4);
    assert!(e.has_and());
    assert!(!Expr::Or(vec![term("a"), Expr::Or(vec![re("b")])]).has_and());
    for bits in 0u32..16 {
        let mut asked = Vec::new();
        let got = e.eval(&mut |i| {
            asked.push(i);
            bits >> i & 1 == 1
        });
        let b = |i: u32| bits >> i & 1 == 1;
        assert_eq!(got, (b(0) && b(1)) || (b(2) && b(3)), "bits={bits:04b}");
        assert_eq!(asked, [0, 1, 2, 3], "every leaf is asked, in order");
    }
}

use super::*;
use crate::tests::support::{re, t};

#[test]
fn expr_json_shape_round_trips() {
    let e = Expr::Or(vec![
        Expr::And(vec![t("HscEnv"), re("pr[i1]ntk")]),
        t("a b"),
        Expr::Or(vec![Expr::And(vec![t("x")])]),
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
    assert!(Expr::And(vec![t("a")]).check().is_ok());
}

#[test]
fn expr_display_quotes_and_parenthesizes() {
    let cases = [
        (Expr::And(vec![t("a"), t("b")]), "a AND b"),
        (
            Expr::Or(vec![Expr::And(vec![t("a"), t("b")]), t("c")]),
            "a AND b OR c",
        ),
        (
            Expr::And(vec![Expr::Or(vec![t("a"), t("b")]), re("c+")]),
            "(a OR b) AND /c+/",
        ),
        (
            Expr::Or(vec![t("a"), Expr::Or(vec![t("b"), t("c")])]),
            "a OR (b OR c)",
        ),
        (
            Expr::And(vec![t("a"), Expr::And(vec![t("b"), t("c")])]),
            "a AND (b AND c)",
        ),
        (
            Expr::And(vec![t("x"), Expr::Or(vec![Expr::Or(vec![t("a"), t("b")])])]),
            "x AND (a OR b)",
        ),
        (Expr::Or(vec![t("only")]), "only"),
        (
            Expr::Or(vec![
                t(""),
                t("AND"),
                t("OR"),
                t("/x/"),
                t("a b"),
                t("q\"\\"),
                t("(p)"),
            ]),
            r#""" OR "AND" OR "OR" OR "/x/" OR "a b" OR "q\"\\" OR "(p)""#,
        ),
        (
            Expr::And(vec![t("and"), t("a/b"), t("StgClosure*")]),
            "and AND a/b AND StgClosure*",
        ),
        (
            Expr::Or(vec![t("it's"), re("a/b"), re(r"a\/b"), re(r"a\\/b")]),
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
        Expr::And(vec![t("a"), t("b")]),
        Expr::And(vec![t("c"), Expr::Or(vec![t("d")])]),
    ]);
    assert_eq!(e.leaves().len(), 4);
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

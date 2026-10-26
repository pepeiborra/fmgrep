//! `--expr-json` / `--expr` over both layouts against a per-document oracle.

mod demote;

use super::*;

use crate::expr::Expr;
use crate::matching::{LineMatcher, MatchMode, QueryOutput, strip_bom};
use crate::wire::QueryReport;

const FILES: [(&str, &[u8]); 7] = [
    ("a.c", b"printk one\nnothing\nprintk two printk\n"),
    ("b.c", b"aaaa\ntail printk"),
    ("c.c", b"alpha beta\nPRINTK alpha\n"),
    ("d.c", b"\xef\xbb\xbfbom alpha\nbeeta\n"),
    ("e.c", b""),
    ("f.c", b"beta only\nBETA\nprint k\n"),
    ("g.c", b"alpha\r\nprintk\r\n"),
];

fn trees() -> Vec<Expr> {
    vec![
        and(vec![t("printk"), t("alpha")]),
        or(vec![t("printk"), t("alpha")]),
        or(vec![and(vec![t("printk"), re("a{3}")]), t("alpha")]),
        and(vec![or(vec![t("printk"), t("alpha")]), re("be+ta")]),
        and(vec![
            or(vec![and(vec![t("printk"), t("alpha")]), re("tail")]),
            or(vec![re("p.intk"), t("beta")]),
        ]),
        and(vec![t("printk"), t("")]),
        or(vec![t(""), re("^beta")]),
        or(vec![t("printk"), t("printk")]),
        and(vec![re("\\bone$"), re("^printk\\b")]),
        and(vec![t("PRINTK"), re("alpha\\s*$")]),
        or(vec![and(vec![t("alpha")])]),
    ]
}

fn stage(tmp: &Path, store_docs: bool) -> PathBuf {
    let folder = stage_corpus(tmp, &FILES);
    let index = tmp.join(format!("idx-{store_docs}"));
    index_it(&folder, &index, store_docs);
    index
}

fn expr_words(index: &Path, flags: &[&str], expr: &Expr) -> Vec<String> {
    let json = serde_json::to_string(expr).unwrap();
    let flags = [flags, &["--expr-json", &json]].concat();
    words(&["--index", &index.display().to_string()], &flags, &[])
}

fn printed_expr(index: &Path, flags: &[&str], expr: &Expr) -> Result<String, String> {
    output(&expr_words(index, flags, expr), false).map(|o| String::from_utf8(o).unwrap())
}

fn report(index: &Path, flags: &[&str], expr: &Expr) -> Result<QueryReport, String> {
    match dispatch(expr_words(index, flags, expr))? {
        Outcome::Query(mut r) => Ok(r.queries.remove(0)),
        _ => panic!("expected a query report"),
    }
}

fn leaf_pattern(leaf: &Expr, mode: MatchMode) -> Option<String> {
    match leaf {
        Expr::Term(s) if mode.ignore_case && !s.is_empty() => Some(regex::escape(s)),
        Expr::Term(_) => None,
        Expr::Regex(r) => Some(r.clone()),
        _ => unreachable!(),
    }
}

fn leaf_matcher(leaf: &Expr, mode: MatchMode) -> LineMatcher {
    match (leaf, leaf_pattern(leaf, mode)) {
        (_, Some(p)) => LineMatcher::regex(&p, None, mode).unwrap(),
        (Expr::Term(s), None) => LineMatcher::literal(s.as_bytes(), mode).unwrap(),
        _ => unreachable!(),
    }
}

/// The member paths: each leaf's line matcher per document, then the tree.
fn members(files: &[(&str, &[u8])], expr: &Expr, mode: MatchMode) -> Vec<String> {
    let matchers: Vec<LineMatcher> = expr
        .leaves()
        .into_iter()
        .map(|l| leaf_matcher(l, mode))
        .collect();
    let mut out: Vec<String> = files
        .iter()
        .filter(|(_, raw)| {
            expr.eval(&mut |i| {
                matchers[i]
                    .scan(strip_bom(raw), QueryOutput::FilesWithMatches)
                    .matched_lines
                    > 0
            })
        })
        .map(|(p, _)| p.to_string())
        .collect();
    out.sort();
    out
}

/// Counts mode's per-leaf totals within `members`, as the per-term counts
/// drive computes them.
fn leaf_totals(
    files: &[(&str, &[u8])],
    expr: &Expr,
    mode: MatchMode,
    members: &[String],
) -> Vec<u64> {
    expr.leaves()
        .into_iter()
        .map(|leaf| {
            files
                .iter()
                .filter(|(p, _)| members.iter().any(|m| m == p))
                .map(|(_, raw)| match (leaf, leaf_pattern(leaf, mode)) {
                    (_, Some(p)) => PreparedPattern::prepare_with(&p, mode)
                        .unwrap()
                        .count(strip_bom(raw)),
                    (Expr::Term(s), None) => {
                        count_overlapping(raw, &memchr::memmem::Finder::new(s.as_bytes()))
                    }
                    _ => unreachable!(),
                })
                .sum()
        })
        .collect()
}

/// Lines of `out` whose leading path is a member.
fn only(out: &str, members: &[String]) -> String {
    out.split_inclusive('\n')
        .filter(|l| {
            members
                .iter()
                .any(|m| l.split(':').next().map(str::trim_end) == Some(m.as_str()))
        })
        .collect()
}

/// rg's `-e a -e b`: every leaf joined, as one per-term pattern.
fn alternation(expr: &Expr, mode: MatchMode) -> String {
    expr.leaves()
        .into_iter()
        .filter(|l| !matches!(l, Expr::Term(s) if s.is_empty()))
        .map(|l| match (l, leaf_pattern(l, mode)) {
            (_, Some(p)) => format!("(?:{p})"),
            (Expr::Term(s), None) => regex::escape(s),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>()
        .join("|")
}

#[test]
fn expr_matches_naive_oracle_on_both_layouts() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = stage(tmp.path(), store_docs);
        for ci in [false, true] {
            let mode = MatchMode {
                ignore_case: ci,
                ..MatchMode::default()
            };
            let flag: &[&str] = if ci { &["-i"] } else { &[] };
            for expr in trees() {
                let ctx = format!("store_docs={store_docs} -i={ci} {expr}");
                let m = members(&FILES, &expr, mode);
                let listed: String = m.iter().map(|p| format!("{p}\n")).collect();
                assert_eq!(
                    printed_expr(&index, &[flag, &["-l"]].concat(), &expr).unwrap(),
                    listed,
                    "{ctx}"
                );
                let r = report(&index, flag, &expr).unwrap();
                assert_eq!(r.term, expr.to_string(), "{ctx}");
                assert_eq!(r.docs_matched, m.len(), "{ctx}");
                let totals: Vec<u64> = r.leaves.iter().map(|l| l.total_matches).collect();
                let want = leaf_totals(&FILES, &expr, mode, &m);
                assert_eq!(totals, want, "{ctx}");
                assert_eq!(r.total_matches, want.iter().sum::<u64>(), "{ctx}");
                let l = report(&index, &[flag, &["-l"]].concat(), &expr).unwrap();
                assert_eq!(
                    (
                        l.docs_matched,
                        l.total_matches,
                        l.counts_exact,
                        l.leaves.len()
                    ),
                    (m.len(), m.len() as u64, false, 0),
                    "{ctx}"
                );
                let alt = alternation(&expr, mode);
                if alt.is_empty() {
                    assert!(m.is_empty(), "{ctx}");
                    continue;
                }
                for mode_flags in [
                    &["-c"][..],
                    &["--count-matches"],
                    &["--no-heading"],
                    &["-o", "--no-heading"],
                    &["--no-heading", "-C", "1"],
                ] {
                    let flags = [flag, mode_flags].concat();
                    let want = only(
                        &printed(&index, &[&flags[..], &["--regex"]].concat(), &[&alt]),
                        &m,
                    );
                    let got = printed_expr(&index, &flags, &expr).unwrap();
                    if mode_flags.contains(&"-C") {
                        // Context lines of non-members interleave; compare members' lines only.
                        assert_eq!(only(&got, &m), want, "{ctx} {mode_flags:?}");
                    } else {
                        assert_eq!(got, want, "{ctx} {mode_flags:?}");
                    }
                }
                let c = report(&index, &[flag, &["--count-matches"]].concat(), &expr).unwrap();
                let per_file: u64 = only(
                    &printed(
                        &index,
                        &[flag, &["--count-matches", "--regex"]].concat(),
                        &[&alt],
                    ),
                    &m,
                )
                .lines()
                .map(|l| l.rsplit(':').next().unwrap().parse::<u64>().unwrap())
                .sum();
                assert_eq!(
                    (c.total_matches, c.docs_matched, c.counts_exact),
                    (per_file, m.len(), true),
                    "{ctx}"
                );
                assert_eq!(c.leaves.len(), expr.leaves().len(), "{ctx}");
                // One rg event stream, whose summary counts only members.
                let json = printed_expr(&index, &[flag, &["--matches", "--json"]].concat(), &expr)
                    .unwrap();
                let begins = json
                    .lines()
                    .filter(|l| l.contains(r#""type":"begin""#))
                    .count();
                assert_eq!(begins, m.len(), "{ctx}");
                assert_eq!(
                    json.lines()
                        .filter(|l| l.contains(r#""type":"summary""#))
                        .count(),
                    1,
                    "{ctx}"
                );
            }
        }
    }
}

#[test]
fn expr_single_leaf_equals_its_term() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = stage(tmp.path(), store_docs);
        for (leaf, term) in [
            (t("printk"), "printk"),
            (re("pr[i1]ntk"), "/pr[i1]ntk/"),
            (t("alpha"), "alpha"),
        ] {
            for flags in [
                &["-l"][..],
                &["-c"],
                &["--count-matches"],
                &["--matches"],
                &["-o", "-n"],
                &["-i", "-c"],
            ] {
                assert_eq!(
                    printed_expr(&index, flags, &leaf).unwrap(),
                    printed(&index, flags, &[term]),
                    "store_docs={store_docs} {term} {flags:?}"
                );
            }
            let one = report(&index, &[], &leaf).unwrap();
            let per_term = query_it(&index, false, &[term]).queries.remove(0);
            assert_eq!(
                (one.total_matches, one.docs_matched),
                (per_term.total_matches, per_term.docs_matched),
                "store_docs={store_docs} {term}"
            );
            assert_eq!(
                one.counts_exact, per_term.counts_exact,
                "store_docs={store_docs} {term}"
            );
        }
    }
}

/// A shard index whose docs.shard is hidden: any fetch fails loudly.
fn blind_index(tmp: &Path, files: &[(&str, &[u8])]) -> PathBuf {
    let folder = stage_corpus(tmp, files);
    let index = tmp.join("idx");
    index_it(&folder, &index, true);
    std::fs::rename(index.join(DOCS_SHARD), index.join("hidden")).unwrap();
    index
}

#[test]
fn expr_literal_only_fetches_nothing() {
    // Every AND samples; the unreadable sample locates.
    crate::tests::BAND.set(Some((0.0, f64::INFINITY)));
    let tmp = tempfile::tempdir().unwrap();
    let index = blind_index(
        tmp.path(),
        &[
            ("a.c", b"alpha x printk"),
            ("b.c", b"alpha"),
            ("c.c", b"printk"),
        ],
    );
    for expr in [
        and(vec![t("alpha"), t("printk")]),
        or(vec![t("alpha"), and(vec![t("printk"), t("x")])]),
    ] {
        let r = report(&index, &[], &expr).unwrap();
        assert!(r.docs_matched > 0, "{expr}");
        assert!(
            !printed_expr(&index, &["-l"], &expr).unwrap().is_empty(),
            "{expr}"
        );
        // Lines need the bytes.
        assert!(
            printed_expr(&index, &["-c"], &expr)
                .unwrap_err()
                .contains("cannot read"),
            "{expr}"
        );
    }
    // A regex that is a literal probes as one.
    assert_eq!(
        report(&index, &[], &and(vec![re("printk"), t("x")]))
            .unwrap()
            .docs_matched,
        1
    );
    crate::tests::BAND.set(None);
}

#[test]
fn expr_and_under_an_or_keeps_the_ratio_test() {
    // The cost rule would demote printk, and approximate counts would read it.
    crate::tests::BAND.set(Some((0.0, 0.0)));
    let tmp = tempfile::tempdir().unwrap();
    let index = blind_index(
        tmp.path(),
        &[
            ("a.c", b"alpha x printk"),
            ("b.c", b"alpha"),
            ("c.c", b"printk"),
        ],
    );
    let both = and(vec![t("alpha"), t("printk")]);
    let under_or = report(
        &index,
        &["--approx-counts"],
        &or(vec![both.clone(), t("zzz")]),
    );
    let at_root = report(&index, &["--approx-counts"], &both);
    crate::tests::BAND.set(None);
    assert_eq!(under_or.unwrap().docs_matched, 1);
    assert!(at_root.unwrap_err().contains("cannot read"));
}

#[test]
fn expr_verifies_only_the_intersection() {
    let tmp = tempfile::tempdir().unwrap();
    let index = blind_index(tmp.path(), &[("a.c", b"alpha x"), ("b.c", b"beta y")]);
    // No document holds both the literal and the regex's anchor.
    let disjoint = and(vec![t("alpha"), re("be+ta")]);
    assert_eq!(report(&index, &[], &disjoint).unwrap().docs_matched, 0);
    assert_eq!(printed_expr(&index, &["-l"], &disjoint).unwrap(), "");
    // a.c is in both, so it must be read.
    let err = report(&index, &[], &and(vec![t("alpha"), re("al+pha")])).unwrap_err();
    assert!(err.contains("cannot read"), "{err}");
    let err = report(&index, &[], &or(vec![t("alpha"), re("be+ta")])).unwrap_err();
    assert!(err.contains("cannot read"), "{err}");
}

#[test]
fn expr_stale_docs_are_scanned_for_every_leaf() {
    let tmp = tempfile::tempdir().unwrap();
    let files: [(&str, &[u8]); 4] = [
        ("a.c", b"printk only\n"),
        ("b.c", b"printk alpha\nbeta\n"),
        ("c.c", b"alpha printk\n"),
        ("d.c", b"alpha beta\n"),
    ];
    let folder = stage_corpus(tmp.path(), &files);
    let stale = tmp.path().join("idx");
    index_it(&folder, &stale, false);
    // a.c now satisfies the tree, b.c does not, c.c is gone.
    std::fs::write(folder.join("a.c"), b"printk only\nalpha, beeta\n").unwrap();
    std::fs::write(folder.join("b.c"), b"printk\n").unwrap();
    std::fs::remove_file(folder.join("c.c")).unwrap();
    let fresh = tmp.path().join("fresh");
    index_it(&folder, &fresh, false);
    for expr in [
        and(vec![t("printk"), t("alpha")]),
        and(vec![t("printk"), re("be+ta")]),
        or(vec![and(vec![t("alpha"), re("be+ta")]), t("printk")]),
    ] {
        for flags in [&[][..], &["-i"]] {
            let (s, f) = (
                report(&stale, flags, &expr).unwrap(),
                report(&fresh, flags, &expr).unwrap(),
            );
            assert_eq!(
                (s.docs_matched, s.total_matches, &s.leaves),
                (f.docs_matched, f.total_matches, &f.leaves),
                "{expr} {flags:?}"
            );
            for mode in [&["-l"][..], &["-c"], &["--count-matches"], &["--matches"]] {
                let flags = [flags, mode].concat();
                assert_eq!(
                    printed_expr(&stale, &flags, &expr).unwrap(),
                    printed_expr(&fresh, &flags, &expr).unwrap(),
                    "{expr} {flags:?}"
                );
            }
        }
    }
    assert_eq!(
        printed_expr(&stale, &["-l"], &and(vec![t("printk"), t("alpha")])).unwrap(),
        "a.c\n"
    );
}

#[test]
fn expr_added_docs_are_scanned_for_every_leaf() {
    let tmp = tempfile::tempdir().unwrap();
    let files: [(&str, &[u8]); 3] = [
        ("a.c", b"printk only\n"),
        ("c.c", b"alpha printk\n"),
        ("e.c", b"alpha beta\n"),
    ];
    let folder = stage_corpus(tmp.path(), &files);
    let stale = tmp.path().join("idx");
    index_it(&folder, &stale, false);
    std::fs::write(folder.join("b.c"), b"printk alpha, beeta\n").unwrap();
    std::fs::create_dir_all(folder.join("new")).unwrap();
    std::fs::write(folder.join("new/n.c"), b"alpha beta printk\n").unwrap();
    std::fs::write(folder.join("d.c"), b"beta\n").unwrap();
    let fresh = tmp.path().join("fresh");
    index_it(&folder, &fresh, false);
    for expr in [
        and(vec![t("printk"), t("alpha")]),
        and(vec![t("printk"), re("be+ta")]),
        or(vec![and(vec![t("alpha"), re("be+ta")]), t("printk")]),
        or(vec![t("beta"), t("only")]),
    ] {
        for flags in [&[][..], &["-i"]] {
            let (s, f) = (
                report(&stale, flags, &expr).unwrap(),
                report(&fresh, flags, &expr).unwrap(),
            );
            assert_eq!(
                (s.docs_matched, s.total_matches, &s.leaves),
                (f.docs_matched, f.total_matches, &f.leaves),
                "{expr} {flags:?}"
            );
            for mode in [&["-l"][..], &["-c"], &["--count-matches"], &["--matches"]] {
                let flags = [flags, mode].concat();
                assert_eq!(
                    printed_expr(&stale, &flags, &expr).unwrap(),
                    printed_expr(&fresh, &flags, &expr).unwrap(),
                    "{expr} {flags:?}"
                );
            }
        }
    }
    assert_eq!(
        printed_expr(&stale, &["-l"], &and(vec![t("printk"), t("alpha")])).unwrap(),
        "b.c\nc.c\nnew/n.c\n"
    );
}

#[test]
fn expr_counts_exact_only_for_disjoint_exact_literals() {
    let tmp = tempfile::tempdir().unwrap();
    let index = stage(tmp.path(), true);
    let exact = |flags: &[&str], e: &Expr| report(&index, flags, e).unwrap().counts_exact;
    let pair = and(vec![t("printk"), t("tail")]);
    assert!(exact(&[], &pair));
    assert_eq!(report(&index, &[], &pair).unwrap().docs_matched, 1);
    // The exact sum is rg's alternation count within the members.
    assert_eq!(
        report(&index, &[], &pair).unwrap().total_matches,
        report(&index, &["--count-matches"], &pair)
            .unwrap()
            .total_matches
    );
    for inexact in [
        or(vec![t("printk"), t("print")]),
        or(vec![t("tail"), t("ilk")]),
        or(vec![t("printk"), t("printk")]),
        or(vec![t("printk"), re("alpha")]),
        or(vec![t("aa"), t("printk")]),
        or(vec![t(""), t("printk")]),
        and(vec![t("printk"), t("alpha")]),
    ] {
        assert!(!exact(&[], &inexact), "{inexact}");
    }
    assert!(!exact(&["-i"], &pair));
}

#[test]
fn expr_refusals_are_loud_and_before_io() {
    let tmp = tempfile::tempdir().unwrap();
    let index = blind_index(tmp.path(), &[("a.c", b"alpha\nbeta")]);
    for (flags, expr, want) in [
        (&[][..], and(vec![t("alpha"), re("(")]), "regex parse error"),
        (&["-l"], or(vec![re(""), t("alpha")]), "empty regex pattern"),
        (
            &["-c"],
            and(vec![t("alpha\nbeta"), t("alpha")]),
            "is not allowed",
        ),
        (&["--matches"], or(vec![re("a\\nb")]), "is not allowed"),
    ] {
        let err = printed_expr(&index, flags, &expr).unwrap_err();
        assert!(
            err.contains(want) && !err.contains("cannot read"),
            "{flags:?} {expr}: {err}"
        );
    }
    let err = printed_expr(&index, &[], &and(vec![])).unwrap_err();
    assert!(err.contains("at least one operand"), "{err}");
    // -U lifts the terminator refusal, as for a term.
    assert!(
        printed_expr(&index, &["-U", "-c"], &and(vec![t("alpha\nbeta")]))
            .unwrap_err()
            .contains("cannot read")
    );
}

#[test]
fn expr_counts_summary_lists_leaves() {
    let tmp = tempfile::tempdir().unwrap();
    let index = stage(tmp.path(), true);
    let and_flags = printed(&index, &["--approx-counts", "--and"], &["printk", "alpha"]);
    assert_eq!(
        and_flags,
        "printk AND alpha: 2 matches in 1 file\n  printk: 1 match\n  alpha: 1 match\n"
    );
    assert_eq!(
        printed(&index, &["--approx-counts"], &["tail", "alpha"]),
        "tail: 1 match in 1 file\nalpha: 4 matches in 3 files\n"
    );
}

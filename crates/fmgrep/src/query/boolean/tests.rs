use std::ops::Range;

use super::*;
use crate::tests::support::{Rng, re, t};

fn tree(rng: &mut Rng, depth: u32) -> Expr {
    if depth == 0 || rng.below(3) == 0 {
        return Expr::Term(String::new());
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

/// Kleene truth: 0 false, 1 unknown, 2 true.
fn kleene(expr: &Expr, leaf: &[u8], next: &mut usize) -> u8 {
    match expr {
        Expr::Term(_) | Expr::Regex(_) => {
            *next += 1;
            leaf[*next - 1]
        }
        Expr::And(v) => v.iter().map(|e| kleene(e, leaf, next)).fold(2, u8::min),
        Expr::Or(v) => v.iter().map(|e| kleene(e, leaf, next)).fold(0, u8::max),
    }
}

#[test]
fn tri_algebra_matches_per_doc_kleene_eval() {
    const DOCS: usize = 12;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..500 {
        let expr = tree(&mut rng, 4);
        let n = expr.leaves().len();
        // Per leaf and doc: 0 absent, 1 maybe, 2 yes.
        let truth: Vec<Vec<u8>> = (0..n)
            .map(|_| (0..DOCS).map(|_| rng.below(3) as u8).collect())
            .collect();
        let tris: Vec<Tri> = truth
            .iter()
            .map(|t| Tri {
                yes: (0..DOCS).filter(|&d| t[d] == 2).collect(),
                maybe: (0..DOCS).filter(|&d| t[d] == 1).collect(),
            })
            .collect();
        let parts: Vec<Part<'_>> = tris.iter().map(Part::Tri).collect();
        let got = prune(&expr, &parts, &[], &mut 0);
        let per_doc: Vec<u8> = (0..DOCS)
            .map(|d| {
                kleene(
                    &expr,
                    &truth.iter().map(|t| t[d]).collect::<Vec<_>>(),
                    &mut 0,
                )
            })
            .collect();
        let want = Tri {
            yes: (0..DOCS).filter(|&d| per_doc[d] == 2).collect(),
            maybe: (0..DOCS).filter(|&d| per_doc[d] == 1).collect(),
        };
        assert_eq!(got, want, "{expr:?}");
    }
}

#[test]
fn overlap_is_containment_or_a_suffix_prefix_border() {
    assert!(can_overlap(b"abc", b"b"));
    assert!(can_overlap(b"abc", b"cde"));
    assert!(can_overlap(b"cde", b"abc"));
    assert!(can_overlap(b"x", b"x"));
    assert!(!can_overlap(b"abc", b"def"));
    assert!(!can_overlap(b"HscEnv", b"DynFlags"));
}

/// Leaves that may be demoted: direct leaf operands of an AND of two or
/// more operands.
fn demotable_slots(expr: &Expr) -> Vec<bool> {
    fn walk(e: &Expr, out: &mut Vec<bool>, slot: bool) {
        match e {
            Expr::Term(_) | Expr::Regex(_) => out.push(slot),
            Expr::Or(v) => v.iter().for_each(|c| walk(c, out, false)),
            Expr::And(v) => v.iter().for_each(|c| walk(c, out, v.len() > 1)),
        }
    }
    let mut out = Vec::new();
    walk(expr, &mut out, false);
    out
}

#[test]
fn unknown_leaves_prune_as_kleene_unknown_within_their_ranges() {
    const DOCS: usize = 12;
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut seen = 0;
    for _ in 0..800 {
        let expr = tree(&mut rng, 4);
        let slots = demotable_slots(&expr);
        let n = slots.len();
        let stale: Vec<usize> = (0..DOCS).filter(|_| rng.below(5) == 0).collect();
        let is_stale = |d: usize| stale.contains(&d);
        let truth: Vec<Vec<u8>> = (0..n)
            .map(|_| (0..DOCS).map(|_| rng.below(3) as u8).collect())
            .collect();
        // A random subset demoted, one operand of each AND kept a Tri.
        let mut unknown = vec![false; n];
        for g in demote::and_groups(&expr) {
            let cand: Vec<usize> = g
                .iter()
                .copied()
                .filter(|&i| slots[i] && rng.below(2) == 0)
                .collect();
            let keep = cand.len() == g.len() && !cand.is_empty();
            for (k, &i) in cand.iter().enumerate() {
                unknown[i] = !(keep && k == 0);
            }
        }
        let ranges: Vec<Vec<Range<usize>>> = (0..n)
            .map(|_| {
                (0..DOCS)
                    .step_by(3)
                    .filter(|_| rng.below(2) == 0)
                    .map(|d| d..d + 3)
                    .collect()
            })
            .collect();
        seen += unknown.iter().filter(|&&u| u).count();
        let tris: Vec<Tri> = truth
            .iter()
            .map(|t| Tri {
                yes: (0..DOCS).filter(|&d| t[d] == 2 && !is_stale(d)).collect(),
                maybe: (0..DOCS).filter(|&d| t[d] == 1 || is_stale(d)).collect(),
            })
            .collect();
        let parts: Vec<Part<'_>> = (0..n)
            .map(|i| match unknown[i] {
                true => Part::Unknown(&ranges[i]),
                false => Part::Tri(&tris[i]),
            })
            .collect();
        let got = prune(&expr, &parts, &stale, &mut 0);
        let per_doc: Vec<u8> = (0..DOCS)
            .map(|d| {
                let leaf: Vec<u8> = (0..n)
                    .map(|i| match (unknown[i], is_stale(d)) {
                        (_, true) => 1,
                        (true, false) => u8::from(demote::contains(&ranges[i], d)),
                        (false, false) => truth[i][d],
                    })
                    .collect();
                kleene(&expr, &leaf, &mut 0)
            })
            .collect();
        let want = Tri {
            yes: (0..DOCS).filter(|&d| per_doc[d] == 2).collect(),
            maybe: (0..DOCS).filter(|&d| per_doc[d] == 1).collect(),
        };
        assert_eq!(got, want, "{expr:?} unknown={unknown:?} stale={stale:?}");
    }
    assert!(seen > 100, "{seen}");
}

/// `(counted, demoted)` for `expr` whose leaves count `totals`: every leaf
/// is countable, a term demotable unless [`demotable`] refuses it.
fn choose(expr: &Expr, totals: &[u64], k: u64) -> (Vec<bool>, Vec<bool>) {
    let leaves = expr.leaves();
    let demotable: Vec<bool> = leaves
        .iter()
        .map(|l| matches!(l, Expr::Term(s) if demotable(s.as_bytes())))
        .collect();
    let groups = demote::and_groups(expr);
    let counted = demote::to_count(&groups, &vec![true; leaves.len()], &demotable, k);
    let totals: Vec<Option<u64>> = totals
        .iter()
        .zip(&counted)
        .map(|(&t, &c)| c.then_some(t))
        .collect();
    (counted, demote::demoted(&groups, &totals, &demotable, k))
}

#[test]
fn demotion_locates_the_rarest_and_demotes_beyond_k() {
    let e = Expr::And(vec![t("a"), t("b"), t("c"), t("d")]);
    assert_eq!(
        choose(&e, &[80, 10, 81, 800], 8).1,
        [false, false, true, true]
    );
    assert_eq!(choose(&e, &[80, 10, 81, 800], 100).1, [false; 4]);
}

#[test]
fn demotion_ties_locate_the_first_and_demote_neither() {
    let ab = Expr::And(vec![t("a"), t("b")]);
    assert_eq!(choose(&ab, &[10, 10], 8).1, [false, false]);
    assert_eq!(
        choose(&Expr::And(vec![t("a"), t("b"), t("c")]), &[10, 10, 500], 8).1,
        [false, false, true]
    );
    assert_eq!(choose(&ab, &[0, 0], 1).1, [false, false]);
}

#[test]
fn demotion_is_off_at_k0() {
    let e = Expr::And(vec![
        t("a"),
        t("b"),
        Expr::Or(vec![t("c"), Expr::And(vec![t("d"), t("e")])]),
    ]);
    assert_eq!(
        choose(&e, &[1, 1_000_000, 1, 1, 1_000_000], 0),
        (vec![false; 5], vec![false; 5])
    );
}

#[test]
fn demotion_all_common_within_k_demotes_nothing() {
    let e = Expr::And(vec![t("a"), t("b"), t("c")]);
    assert_eq!(
        choose(&e, &[900, 1000, 1200], 8),
        (vec![true; 3], vec![false; 3])
    );
}

#[test]
fn demotion_never_touches_or_members() {
    let e = Expr::Or(vec![
        Expr::And(vec![t("a"), t("b")]),
        t("c"),
        Expr::And(vec![t("d"), Expr::Or(vec![t("e"), t("f")])]),
    ]);
    let (counted, demoted) = choose(&e, &[1, 1000, 1, 1000, 1, 1], 8);
    assert_eq!(counted, [true, true, false, false, false, false]);
    assert_eq!(demoted, [false, true, false, false, false, false]);
    // A lone leaf and a one-operand AND count nothing.
    assert_eq!(choose(&t("a"), &[5], 8), (vec![false], vec![false]));
    assert_eq!(
        choose(&Expr::And(vec![t("a")]), &[5], 8),
        (vec![false], vec![false])
    );
}

#[test]
fn demotion_min_zero_demotes_every_present_sibling() {
    let e = Expr::And(vec![t("a"), t("b"), t("c")]);
    assert_eq!(choose(&e, &[3, 0, 1], 8).1, [true, false, true]);
    assert_eq!(choose(&e, &[0, 0, 1], 8).1, [false, false, true]);
}

#[test]
fn demotion_skips_bom_and_separator_literals_but_they_may_be_located() {
    for odd in ["\u{feff}x", "\u{f000}", "a\0b"] {
        assert!(!demotable(odd.as_bytes()), "{odd:?}");
        let e = Expr::And(vec![t(odd), t("x")]);
        assert_eq!(choose(&e, &[1000, 1], 8).1, [false, false], "{odd:?}");
        assert_eq!(choose(&e, &[1, 1000], 8).1, [false, true], "{odd:?}");
    }
    // Nothing demotable, so nothing is counted.
    assert_eq!(
        choose(&Expr::And(vec![t("\u{feff}"), t("\0")]), &[1, 1000], 8).0,
        [false, false]
    );
    assert!(!demotable(b"\xbbx") && !demotable(b"\xbf") && !demotable(b""));
    assert!(demotable(b"x\xef"));
}

#[test]
fn regex_anchor_is_a_reference_and_never_demoted() {
    let e = Expr::And(vec![t("import"), re("zonkTc\\w+")]);
    assert_eq!(
        choose(&e, &[5000, 3], 8),
        (vec![true, true], vec![true, false])
    );
    assert_eq!(
        choose(&e, &[3, 5000], 8),
        (vec![true, true], vec![false, false])
    );
    let regexes = Expr::And(vec![re("a+"), re("b+")]);
    assert_eq!(choose(&regexes, &[1, 9], 8).0, [false, false]);
}

#[test]
fn within_intersects_demoted_ranges_and_unsettled_subtracts_it() {
    let a = [0..4, 6..9, 12..20];
    let b = [2..7, 8..13, 19..30];
    assert_eq!(
        demote::intersect(&a, &b),
        [2..4, 6..7, 8..9, 12..13, 19..20]
    );
    let none: Vec<Range<usize>> = Vec::new();
    assert_eq!(demote::intersect(&a, &[]), none);
    assert_eq!(demote::difference(&a, &b), [0..2, 7..8, 13..19]);
    assert_eq!(demote::difference(&a, &[]), a);
    assert_eq!(demote::difference(&[], &a), none);
    let all = [0..10, 10..12];
    assert_eq!(demote::difference(&all, &all), none);
    for d in 0..32 {
        let want = a.iter().any(|r| r.contains(&d));
        assert_eq!(demote::contains(&a, d), want, "{d}");
    }
}

use std::ops::Range;

use super::demote::ReadBand;
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
        for g in demote::and_groups(&expr).into_iter().map(|g| g.leaves) {
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

/// The plan for `expr` whose leaves count `counts`: every leaf is
/// countable, a term literal and demotable unless [`demotable`] refuses it.
fn arranged(expr: &Expr, counts: Vec<Vec<(Range<usize>, u64)>>) -> (Vec<bool>, demote::Plan) {
    let leaves = expr.leaves();
    let demotable: Vec<bool> = leaves
        .iter()
        .map(|l| matches!(l, Expr::Term(s) if demotable(s.as_bytes())))
        .collect();
    let literal: Vec<bool> = leaves.iter().map(|l| matches!(l, Expr::Term(_))).collect();
    let groups = demote::and_groups(expr);
    let counted = demote::to_count(&groups, &vec![true; leaves.len()], &demotable);
    let plan = demote::arrange(&groups, counts, &counted, &demotable, &literal);
    (counted, plan)
}

/// Every leaf's `total` counted on doc 0.
fn on_doc0(totals: &[u64]) -> Vec<Vec<(Range<usize>, u64)>> {
    totals
        .iter()
        .map(|&t| if t > 0 { vec![(0..1, t)] } else { Vec::new() })
        .collect()
}

/// `(counted, demoted)` under the fallback ratio: `expr` sits under an OR.
fn choose(expr: &Expr, totals: &[u64]) -> (Vec<bool>, Vec<bool>) {
    let (counted, plan) = arranged(&Expr::Or(vec![expr.clone()]), on_doc0(totals));
    assert!(plan.groups.is_empty() && !plan.candidate.contains(&true));
    (counted, plan.demoted)
}

#[test]
fn demotion_locates_the_rarest_and_demotes_beyond_the_fallback_ratio() {
    let e = Expr::And(vec![t("a"), t("b"), t("c"), t("d")]);
    assert_eq!(choose(&e, &[80, 10, 81, 800]).1, [false, false, true, true]);
}

#[test]
fn demotion_ties_locate_the_first_and_demote_neither() {
    let ab = Expr::And(vec![t("a"), t("b")]);
    assert_eq!(choose(&ab, &[10, 10]).1, [false, false]);
    assert_eq!(
        choose(&Expr::And(vec![t("a"), t("b"), t("c")]), &[10, 10, 500]).1,
        [false, false, true]
    );
}

#[test]
fn demotion_all_common_within_the_fallback_ratio_demotes_nothing() {
    let e = Expr::And(vec![t("a"), t("b"), t("c")]);
    assert_eq!(
        choose(&e, &[900, 1000, 1200]),
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
    let (counted, demoted) = choose(&e, &[1, 1000, 1, 1000, 1, 1]);
    assert_eq!(counted, [true, true, false, false, false, false]);
    assert_eq!(demoted, [false, true, false, false, false, false]);
    // A lone leaf and a one-operand AND count nothing.
    assert_eq!(choose(&t("a"), &[5]), (vec![false], vec![false]));
    assert_eq!(
        choose(&Expr::And(vec![t("a")]), &[5]),
        (vec![false], vec![false])
    );
}

#[test]
fn demotion_min_zero_demotes_every_present_sibling() {
    let e = Expr::And(vec![t("a"), t("b"), t("c")]);
    assert_eq!(choose(&e, &[3, 0, 1]).1, [true, false, true]);
    assert_eq!(choose(&e, &[0, 0, 1]).1, [false, false, true]);
}

#[test]
fn demotion_skips_bom_and_separator_literals_but_they_may_be_located() {
    for odd in ["\u{feff}x", "\u{f000}", "a\0b"] {
        assert!(!demotable(odd.as_bytes()), "{odd:?}");
        let e = Expr::And(vec![t(odd), t("x")]);
        assert_eq!(choose(&e, &[1000, 1]).1, [false, false], "{odd:?}");
        assert_eq!(choose(&e, &[1, 1000]).1, [false, true], "{odd:?}");
    }
    // Nothing demotable, so nothing is counted.
    assert_eq!(
        choose(&Expr::And(vec![t("\u{feff}"), t("\0")]), &[1, 1000]).0,
        [false, false]
    );
    assert!(!demotable(b"\xbbx") && !demotable(b"\xbf") && !demotable(b""));
    assert!(demotable(b"x\xef"));
}

#[test]
fn regex_anchor_is_a_reference_and_never_demoted() {
    let e = Expr::And(vec![t("import"), re("zonkTc\\w+")]);
    assert_eq!(
        choose(&e, &[5000, 3]),
        (vec![true, true], vec![true, false])
    );
    assert_eq!(
        choose(&e, &[3, 5000]),
        (vec![true, true], vec![false, false])
    );
    let regexes = Expr::And(vec![re("a+"), re("b+")]);
    assert_eq!(choose(&regexes, &[1, 9]).0, [false, false]);
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

fn answer(exact: Vec<(usize, u64)>, probe_us: u64) -> Answer {
    Answer {
        exact,
        candidates: Some(Vec::new()),
        probe_us,
        counts: Vec::new(),
    }
}

fn span(r: Range<usize>) -> Vec<Range<usize>> {
    vec![r]
}

const fn band(min_us: f64, max_us: f64) -> ReadBand {
    ReadBand { min_us, max_us }
}

/// [`demote::decide`] with the reference answered by `exact` in `probe_us`;
/// every read costs `read`. Returns the demoted leaves and each sample.
fn decided(
    plan: &mut demote::Plan,
    band: ReadBand,
    exact: Vec<(usize, u64)>,
    probe_us: u64,
    read: f64,
) -> (Vec<bool>, Vec<Vec<usize>>) {
    let reference = plan.groups[0].reference;
    let a = answer(exact, probe_us);
    let stage1: Vec<Option<&Answer>> = (0..plan.demoted.len())
        .map(|i| (i == reference).then_some(&a))
        .collect();
    let mut samples = Vec::new();
    demote::decide(plan, band, &stage1, &mut |ids| {
        samples.push(ids.to_vec());
        Ok(read)
    });
    (plan.demoted.clone(), samples)
}

#[test]
fn cost_rule_demotes_when_locating_costs_more_than_reading() {
    // 16x the occurrences locate in 8x the time.
    assert!(demote::demote_by_cost(1000, 100, 1, 1600, 7999.0));
    assert!(!demote::demote_by_cost(1000, 100, 1, 1600, 8001.0));
    assert!(demote::demote_by_cost(1000, 100, 10, 1600, 100.0));
    assert!(!demote::demote_by_cost(1000, 100, 100, 1600, 100.0));
    // An untimed locate counts 1 us.
    assert!(demote::demote_by_cost(0, 1, 1, 1, 0.5) && !demote::demote_by_cost(0, 1, 1, 1, 1.0));
    assert!(!demote::demote_by_cost(1000, 100, 1, 0, 0.0));
}

#[test]
fn cost_rule_samples_once_and_only_inside_the_band() {
    let e = Expr::And(vec![t("a"), t("b"), t("c"), t("d"), t("e")]);
    let totals = [100, 200, 1000, 2000, 100_000];
    let counts = || totals.iter().map(|&t| vec![(0..40, t)]).collect();
    let exact: Vec<(usize, u64)> = (0..20).map(|d| (d, 5)).collect();
    // 20 docs: the band spans 200..2000 us; b, c, d, e estimate 168, 562, 946
    // and 17783 us.
    let (_, mut plan) = arranged(&e, counts());
    assert_eq!(plan.candidate, [false, true, true, true, true]);
    let (demoted, samples) = decided(&mut plan, band(10.0, 100.0), exact.clone(), 100, 40.0);
    assert_eq!(demoted, [false, false, false, true, true]);
    let spread: Vec<usize> = (0..16).map(|k| k * 20 / 16).collect();
    assert_eq!(samples, [spread]);
    assert_eq!(plan.within[3], None);
    assert_eq!(plan.within[2], Some(span(0..40)));
    // Every estimate outside the band: nothing is read.
    let (_, mut plan) = arranged(&e, counts());
    let (demoted, samples) = decided(&mut plan, band(1.0, 5.0), exact, 100, 40.0);
    assert_eq!(
        (demoted, samples.len()),
        (vec![false, true, true, true, true], 0)
    );
}

#[test]
fn cost_rule_zero_rare_docs_demotes_every_candidate() {
    let e = Expr::And(vec![t("a"), t("b"), t("c")]);
    let (_, mut plan) = arranged(&e, on_doc0(&[5, 0, 7]));
    assert_eq!(
        (plan.groups[0].reference, plan.candidate.clone()),
        (1, vec![true, false, true])
    );
    let (demoted, samples) = decided(&mut plan, band(0.0, f64::INFINITY), Vec::new(), 100, 1.0);
    assert_eq!((demoted, samples.len()), (vec![true, false, true], 0));
}

#[test]
fn cost_rule_regex_reference_falls_back_to_ratio_8() {
    let e = Expr::And(vec![t("import"), re("zonkTc\\w+"), t("x")]);
    for (totals, want) in [
        ([5000, 3, 24], [true, false, false]),
        ([24, 3, 25], [false, false, true]),
    ] {
        let (_, plan) = arranged(&e, on_doc0(&totals));
        assert!(
            plan.groups.is_empty() && !plan.candidate.contains(&true),
            "{totals:?}"
        );
        assert_eq!(plan.demoted, want, "{totals:?}");
    }
}

#[test]
fn cost_rule_counts_only_occurrences_inside_w() {
    let e = Expr::And(vec![t("a"), t("b")]);
    let counts = || {
        vec![
            vec![(0..10, 10), (10..20, 10)],
            vec![(10..20, 10), (20..30, 1_000_000)],
        ]
    };
    let exact = vec![(3, 4), (12, 5), (15, 5)];
    // Inside W both count 10: the estimate is the reference's 50 us, under
    // 2 docs x 30 us.
    let (_, mut plan) = arranged(&e, counts());
    assert_eq!(plan.groups[0].within, span(10..20));
    let (demoted, samples) = decided(&mut plan, band(30.0, 1000.0), exact.clone(), 50, 1.0);
    assert_eq!((demoted, samples.len()), (vec![false, false], 0));
    // Only the reference's docs inside W are sampled.
    let (_, mut plan) = arranged(&e, counts());
    let (demoted, samples) = decided(&mut plan, band(0.0, f64::INFINITY), exact, 50, 24.0);
    assert_eq!((demoted, samples), (vec![false, true], vec![vec![12, 15]]));
}

#[test]
fn within_spans_every_demotable_counted_leaf_even_when_none_is_demoted() {
    let e = Expr::And(vec![t("a"), t("b"), t("\u{feff}c"), re("d+"), t("e")]);
    let counts = vec![
        vec![(0..10, 1), (10..20, 1), (20..30, 1)],
        vec![(10..20, 1), (20..30, 1)],
        vec![(0..10, 1)],
        vec![(0..10, 1)],
        vec![(0..10, 1), (20..30, 1)],
    ];
    let (counted, plan) = arranged(&e, counts);
    assert_eq!((counted, plan.demoted), (vec![true; 5], vec![false; 5]));
    let w = Some(span(20..30));
    assert_eq!(plan.within, [w.clone(), w.clone(), w.clone(), None, w]);
    assert_eq!(plan.unsettled[0], [0..10, 10..20]);
    assert_eq!(plan.unsettled[2], span(0..10));
    assert!(plan.unsettled[3].is_empty());
}

#[test]
fn cost_rule_under_an_or_keeps_the_ratio_test_and_the_demoted_w() {
    let e = Expr::And(vec![
        t("x"),
        t("y"),
        Expr::Or(vec![t("z"), Expr::And(vec![t("a"), t("b")])]),
    ]);
    let x = vec![(0..10, 1)];
    let y = vec![(0..10, 1), (10..20, 1)];
    let (_, plan) = arranged(
        &e,
        vec![x.clone(), y.clone(), vec![], y.clone(), vec![(10..20, 100)]],
    );
    assert_eq!(plan.candidate, [false, true, false, false, false]);
    assert_eq!(plan.demoted, [false, false, false, false, true]);
    let w = |r| Some(span(r));
    assert_eq!(plan.within, [w(0..10), w(0..10), None, w(10..20), None]);
    assert_eq!(plan.unsettled[3], span(0..10));
    // Nothing demoted under the OR: no skip, nothing unsettled.
    let (_, plan) = arranged(&e, vec![x, y.clone(), vec![], y, vec![(10..20, 10)]]);
    assert_eq!(plan.demoted, [false; 5]);
    assert_eq!(
        (plan.within[3].clone(), plan.within[4].clone()),
        (None, None)
    );
    assert!(plan.unsettled[3].is_empty() && plan.unsettled[4].is_empty());
}

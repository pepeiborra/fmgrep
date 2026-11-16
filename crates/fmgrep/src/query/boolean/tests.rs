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
    arranged_with(expr, counts, &[])
}

/// [`arranged`] with the terms `folds` case-folded: exact, never narrowed.
fn arranged_with(
    expr: &Expr,
    counts: Vec<Vec<(Range<usize>, u64)>>,
    folds: &[usize],
) -> (Vec<bool>, demote::Plan) {
    let leaves = expr.leaves();
    let demotable: Vec<bool> = leaves
        .iter()
        .map(|l| matches!(l, Expr::Term(s) if demotable(s.as_bytes())))
        .collect();
    let exact: Vec<bool> = leaves.iter().map(|l| matches!(l, Expr::Term(_))).collect();
    let narrowed: Vec<bool> = (0..leaves.len())
        .map(|i| exact[i] && !folds.contains(&i))
        .collect();
    let groups = demote::and_groups(expr);
    let counted = demote::to_count(&groups, &vec![true; leaves.len()], &demotable);
    let plan = demote::arrange(&groups, counts, &counted, &demotable, &exact, &narrowed);
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
        saved_us: 0,
        counts: Vec::new(),
    }
}

fn span(r: Range<usize>) -> Vec<Range<usize>> {
    vec![r]
}

fn span_count(r: Range<usize>, n: u64) -> Vec<(Range<usize>, u64)> {
    vec![(r, n)]
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
    let walked = unwalked(plan.demoted.len());
    let mut samples = Vec::new();
    demote::decide(plan, band, &stage1, &walked.of(), &mut |ids| {
        samples.push(ids.to_vec());
        Ok(sample(read, 0.0))
    });
    (plan.demoted.clone(), samples)
}

fn sample(us: f64, held: f64) -> demote::Sample {
    demote::Sample { us, held }
}

/// [`demote::Walked`]'s inputs, owned.
struct Walks {
    leaves: Vec<bool>,
    located_us: Vec<Option<f64>>,
    row_us: f64,
    doc_lens: Vec<u64>,
}

impl Walks {
    fn of(&self) -> demote::Walked<'_> {
        demote::Walked {
            leaves: &self.leaves,
            located_us: &self.located_us,
            row_us: self.row_us,
            doc_lens: &self.doc_lens,
        }
    }
}

/// No leaf of `n` walked.
fn unwalked(n: usize) -> Walks {
    Walks {
        leaves: vec![false; n],
        located_us: vec![None; n],
        row_us: 0.7,
        doc_lens: Vec::new(),
    }
}

#[test]
fn cost_rule_demotes_when_locating_costs_more_than_reading() {
    // 16x the occurrences locate in 16x the time.
    assert!(demote::demote_by_cost(1000, 100, 1, 1600, 15999.0));
    assert!(!demote::demote_by_cost(1000, 100, 1, 1600, 16001.0));
    assert!(demote::demote_by_cost(1000, 100, 10, 1600, 100.0));
    assert!(!demote::demote_by_cost(1000, 100, 200, 1600, 100.0));
    // An untimed locate counts 1 us.
    assert!(demote::demote_by_cost(0, 1, 1, 1, 0.5) && !demote::demote_by_cost(0, 1, 1, 1, 1.0));
    assert!(!demote::demote_by_cost(1000, 100, 1, 0, 0.0));
}

#[test]
fn cost_rule_samples_once_and_only_inside_the_band() {
    let e = Expr::And(vec![t("a"), t("b"), t("c"), t("d"), t("e")]);
    let totals = [100, 150, 600, 1200, 100_000];
    let counts = || totals.iter().map(|&t| vec![(0..40, t)]).collect();
    let exact: Vec<(usize, u64)> = (0..20).map(|d| (d, 5)).collect();
    // 20 docs: the band spans 200..2000 us; b is below it, c and d inside
    // (read 800 us) and e above.
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

/// A reference whose locate walked in parallel prices its candidates by its
/// work: its probe time plus what the parallel walks saved, as if it had
/// walked on one thread.
#[test]
fn cost_rule_prices_a_parallel_reference_by_its_work() {
    let e = Expr::And(vec![t("a"), t("b"), t("c"), t("d"), t("e")]);
    let totals = [100, 150, 600, 1200, 100_000];
    let counts = || totals.iter().map(|&t| vec![(0..40, t)]).collect();
    let exact: Vec<(usize, u64)> = (0..20).map(|d| (d, 5)).collect();
    let run = |probe_us, saved_us| {
        let (_, mut plan) = arranged(&e, counts());
        let a = Answer {
            saved_us,
            ..answer(exact.clone(), probe_us)
        };
        let stage1: Vec<Option<&Answer>> = (0..5).map(|i| (i == 0).then_some(&a)).collect();
        let walked = unwalked(5);
        demote::decide(
            &mut plan,
            band(10.0, 100.0),
            &stage1,
            &walked.of(),
            &mut |_| Ok(sample(40.0, 0.0)),
        );
        plan.demoted
    };
    let serial = run(100, 0);
    assert_eq!(serial, [false, false, false, true, true]);
    assert_eq!(run(25, 75), serial);
    assert_eq!(run(25, 0), [false, false, false, false, true]);
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

#[test]
fn arrange_records_the_reference_and_the_ratio() {
    use demote::Why;
    let e = Expr::And(vec![t("import"), re("zonkTc\\w+"), t("x")]);
    let (counted, plan) = arranged(&e, on_doc0(&[5000, 3, 24]));
    assert_eq!(plan.counted, counted);
    let ratio = |total| Why::Ratio {
        reference: 1,
        total,
        min: 3,
    };
    assert_eq!(plan.why, [ratio(5000), Why::Reference, ratio(24)]);
    let (_, plan) = arranged(&Expr::And(vec![t("a"), t("b")]), on_doc0(&[5, 1]));
    assert_eq!(plan.why, [Why::Located, Why::Reference]);
}

#[test]
fn decide_records_the_edge_or_sample_that_decided() {
    use demote::{Read, Why};
    let e = Expr::And(vec![t("a"), t("b"), t("c"), t("d"), t("e")]);
    let totals = [100, 150, 600, 1200, 100_000];
    let counts = || totals.iter().map(|&t| vec![(0..40, t)]).collect();
    let exact: Vec<(usize, u64)> = (0..20).map(|d| (d, 5)).collect();
    let cost = |locate_us, read_us, read| Why::Cost {
        reference: 0,
        locate_us,
        rows: None,
        docs: 20,
        read_us,
        read_all_us: 20.0 * read_us,
        held: None,
        large: None,
        read,
    };
    let (_, mut plan) = arranged(&e, counts());
    decided(&mut plan, band(10.0, 100.0), exact.clone(), 100, 40.0);
    assert_eq!(
        plan.why,
        [
            Why::Reference,
            cost(150.0, 10.0, Read::Floor),
            cost(600.0, 40.0, Read::Sampled(16)),
            cost(1200.0, 40.0, Read::Sampled(16)),
            cost(100_000.0, 100.0, Read::Ceiling),
        ]
    );
    let (_, mut plan) = arranged(&e, counts());
    let a = answer(exact, 100);
    let stage1: Vec<Option<&Answer>> = (0..5).map(|i| (i == 0).then_some(&a)).collect();
    let walked = unwalked(5);
    demote::decide(
        &mut plan,
        band(10.0, 100.0),
        &stage1,
        &walked.of(),
        &mut |_| Err("gone".into()),
    );
    assert_eq!(plan.why[2], cost(600.0, 100.0, Read::Failed));
    assert_eq!(plan.demoted, [false, false, false, false, true]);
}

#[test]
fn a_folded_leaf_is_counted_and_the_plan_names_it() {
    use crate::tests::support::{index_it, request, stage_corpus};
    let files: [(&str, &[u8]); 3] = [
        ("a.c", b"printk PRINTK 123\n"),
        ("b.c", b"Print\xe2\x84\xaa pr1ntk 123 123\n"),
        ("c.c", b"nothing\n"),
    ];
    let tmp = tempfile::tempdir().unwrap();
    let index = tmp.path().join("idx");
    index_it(&stage_corpus(tmp.path(), &files), &index, false);
    let manifest = crate::manifest::read_manifest(&index).unwrap();
    let (fm, _) =
        super::super::load(&index, &manifest, None, crate::fetch::Ask::Load, None).unwrap();
    let expr = Expr::And(vec![t("PrintK"), t("123"), re("pr.ntk")]);
    let req = QueryRequest {
        mode: crate::matching::MatchMode {
            ignore_case: true,
            ..Default::default()
        },
        ..request(&index, &manifest, &[], QueryOutput::Counts)
    };
    let leaves = leaves(&expr, &req).unwrap();
    assert!(matches!(leaves[0].probe, Probe::Fold));
    assert!(matches!(&leaves[1].probe, Probe::Literal(l) if l == b"123"));
    assert!(matches!(leaves[2].probe, Probe::Regex));
    let (plan, walks) = demote::plan(&fm, &expr, &leaves, None);
    let walked: Vec<bool> = walks.iter().map(Option::is_some).collect();
    assert_eq!(walked, [true, false, false]);
    assert_eq!(plan.counted, [true, true, false]);
    assert_eq!(plan.counts[0], [(0..3, 3)]);
    assert_eq!(plan.why[0], demote::Why::Reference);
    assert_eq!(plan.candidate, [false, true, false]);
    let hits: Vec<Hits> = vec![(Vec::new(), Vec::new()); 3];
    let text = explain::Explain {
        expr: &expr,
        leaves: &leaves,
        plan: &plan,
        leaf_us: &[],
        hits: &hits,
        root: &Tri::default(),
        stale: 0,
        output: QueryOutput::Counts,
        reads: 0,
    }
    .to_string();
    assert!(
        text.contains("PrintK    folded literal  counted 3 in 1 shard [0..3)\n"),
        "{text}"
    );
}

#[test]
fn a_folded_leaf_may_be_reference_candidate_or_demoted() {
    use demote::Why;
    let fb = Expr::And(vec![t("f"), t("b")]);
    let (_, plan) = arranged_with(
        &fb,
        vec![span_count(0..10, 5), span_count(0..10, 500)],
        &[0],
    );
    assert_eq!(plan.why[0], Why::Reference);
    assert_eq!(
        (plan.groups[0].reference, plan.candidate),
        (0, vec![false, true])
    );
    assert_eq!(plan.within, [None, Some(span(0..10))]);

    let af = Expr::And(vec![t("a"), t("f")]);
    let counts = || vec![span_count(0..10, 5), vec![(0..10, 500), (10..20, 7)]];
    let (_, mut plan) = arranged_with(&af, counts(), &[1]);
    assert_eq!(plan.candidate, [false, true]);
    let (demoted, _) = decided(&mut plan, band(0.0, 0.0), vec![(3, 5)], 50, 1.0);
    assert_eq!(demoted, [false, true]);
    assert_eq!(
        (&plan.within[1], &plan.unsettled[1][..]),
        (&None, &[0..10, 10..20][..])
    );
    let (_, mut plan) = arranged_with(&af, counts(), &[1]);
    let inf = f64::INFINITY;
    let (demoted, _) = decided(&mut plan, band(inf, inf), vec![(3, 5)], 50, 1.0);
    assert_eq!(demoted, [false, false]);
    assert_eq!(plan.within, [Some(span(0..10)), None]);
    assert!(plan.unsettled[1].is_empty());

    let or = Expr::Or(vec![af, t("z")]);
    let counts = vec![
        span_count(0..10, 1),
        vec![(0..10, 100), (10..20, 1)],
        vec![],
    ];
    let (_, plan) = arranged_with(&or, counts, &[1]);
    assert_eq!(plan.demoted, [false, true, false]);
    assert_eq!(plan.unsettled[1], [0..10, 10..20]);
    assert_eq!(plan.within[0], Some(vec![0..10, 10..20]));
}

#[test]
fn decide_reads_a_folded_reference_from_its_candidates() {
    let e = Expr::And(vec![t("f"), t("b")]);
    let counts = vec![vec![(0..10, 4), (10..20, 4)], span_count(0..10, 100)];
    let (_, mut plan) = arranged_with(&e, counts, &[0]);
    assert_eq!(plan.groups[0].within, span(0..10));
    let a = Answer {
        exact: Vec::new(),
        candidates: Some(vec![2, 5, 12]),
        probe_us: 40,
        saved_us: 0,
        counts: Vec::new(),
    };
    let stage1 = [Some(&a), None];
    let mut samples = Vec::new();
    let walked = unwalked(2);
    demote::decide(
        &mut plan,
        band(0.0, f64::INFINITY),
        &stage1,
        &walked.of(),
        &mut |ids| {
            samples.push(ids.to_vec());
            Ok(sample(1.0, 0.0))
        },
    );
    assert_eq!(samples, [vec![2, 5]]);
    assert!(matches!(plan.why[1], demote::Why::Cost { docs: 2, .. }));
}

#[test]
fn decide_prices_a_walked_candidate_by_its_rows_bytes_and_sample() {
    use demote::{LARGE_BYTE_NS, Read, Why};
    // The reference counts 4 in its busiest shard and probes in 40 µs: 10
    // µs a row, capped at `row_us`. The walked candidate's busiest shard
    // holds 3000 rows. The reference's 40 documents of 1000 bytes, but for
    // those `big` sizes, are sampled 16 at a time.
    let e = Expr::And(vec![t("a"), t("f")]);
    let counts = || {
        vec![
            vec![(0..20, 4), (20..40, 2)],
            vec![(0..20, 3000), (20..40, 1000)],
        ]
    };
    let exact: Vec<(usize, u64)> = (0..40).map(|d| (d, 2)).collect();
    let run_located = |band: ReadBand,
                       row_us: f64,
                       big: &[(usize, u64)],
                       (us, held): (f64, f64),
                       located: Option<f64>| {
        let (_, mut plan) = arranged_with(&e, counts(), &[1]);
        let mut doc_lens = vec![1000; 40];
        big.iter().for_each(|&(d, len)| doc_lens[d] = len);
        let walks = Walks {
            leaves: vec![false, true],
            located_us: vec![located, None],
            row_us,
            doc_lens,
        };
        let a = answer(exact.clone(), 40);
        let stage1 = [Some(&a), None];
        let mut samples = Vec::new();
        demote::decide(&mut plan, band, &stage1, &walks.of(), &mut |ids| {
            samples.push(ids.to_vec());
            Ok(sample(us, held))
        });
        (plan.demoted[1], plan.why[1], samples)
    };
    let run = |band, row_us, big, sampled| {
        let (demoted, why, samples) = run_located(band, row_us, big, sampled, None);
        (demoted, why, samples.len())
    };
    let cost = |locate_us, row_us, read_us, read_all_us, held, large, read| Why::Cost {
        reference: 0,
        locate_us,
        rows: Some((3000, row_us)),
        docs: 40,
        read_us,
        read_all_us,
        held: Some(held),
        large: Some(large),
        read,
    };
    // 3000 rows x 0.5 µs = 1.5 ms: more than reading 40 docs at the ceiling.
    let ceiling = cost(1500.0, 0.5, 30.0, 1200.0, 0.0, (0, 0.0), Read::Ceiling);
    assert_eq!(
        run(band(10.0, 30.0), 0.5, &[], (0.0, 0.0)),
        (true, ceiling, 0)
    );
    // Under a larger `row_us` the reference's own 10 µs a row stands, and
    // locating costs no more than 16 docs may cost to sample: no sample.
    let unsampled = |docs, us| Read::Unsampled { docs, us };
    let cheap = cost(
        30_000.0,
        10.0,
        1e4,
        4e5,
        0.0,
        (0, 0.0),
        unsampled(16, 1.6e5),
    );
    assert_eq!(
        run(band(1e3, 1e4), 50.0, &[], (0.0, 0.0)),
        (false, cheap, 0)
    );
    // A document of 300 KB reads at 0.15 ns a byte in more than the 30 µs
    // ceiling: it is large, and costs 300 µs at 1 ns a byte. Locating still
    // costs more than the whole read.
    assert_eq!(LARGE_BYTE_NS, 1.0);
    let some = cost(
        1500.0,
        0.5,
        30.0,
        39.0 * 30.0 + 300.0,
        0.0,
        (1, 300.0),
        Read::Ceiling,
    );
    assert_eq!(
        run(band(10.0, 30.0), 0.5, &[(12, 300_000)], (0.0, 0.0)),
        (true, some, 0)
    );
    // Ten such documents: at a plain read they would cost 450 µs, and
    // locating more than the whole read. Checked at 1 ns a byte they cost
    // 3 ms, more than demoting can save: no sample.
    let ten: Vec<(usize, u64)> = (0..10).map(|d| (d, 300_000)).collect();
    let dear = cost(
        1500.0,
        0.5,
        30.0,
        30.0 * 30.0 + 3000.0,
        0.0,
        (10, 3000.0),
        unsampled(16, 16.0 * 30.0),
    );
    assert_eq!(
        run(band(10.0, 30.0), 0.5, &ten, (0.0, 0.0)),
        (false, dear, 0)
    );
    // A document of 100 MB costs 100 ms: more than locating, so demoting
    // saves nothing whatever a sample finds. No sample.
    let big = cost(
        1500.0,
        0.5,
        30.0,
        39.0 * 30.0 + 1e5,
        0.0,
        (1, 1e5),
        unsampled(16, 16.0 * 30.0),
    );
    assert_eq!(
        run(band(10.0, 30.0), 0.5, &[(12, 100_000_000)], (0.0, 0.0)),
        (false, big, 0)
    );
    // Every document large: none to sample, and their bytes keep the
    // candidate located.
    let every: Vec<(usize, u64)> = (0..40).map(|d| (d, 1_000_000)).collect();
    let large = cost(1500.0, 0.5, 30.0, 4e4, 0.0, (40, 4e4), unsampled(0, 0.0));
    assert_eq!(
        run(band(10.0, 30.0), 0.5, &every, (0.0, 0.0)),
        (false, large, 0)
    );
    // Inside the band the sample decides: locating saves nothing where every
    // sampled document holds the candidate, and the whole read where none
    // does.
    let all = cost(1500.0, 0.5, 40.0, 1600.0, 1.0, (0, 0.0), Read::Sampled(16));
    assert_eq!(run(band(10.0, 40.0), 0.5, &[], (40.0, 1.0)), (true, all, 1));
    let none = cost(1500.0, 0.5, 40.0, 1600.0, 0.0, (0, 0.0), Read::Sampled(16));
    assert_eq!(
        run(band(10.0, 40.0), 0.5, &[], (40.0, 0.0)),
        (false, none, 1)
    );
    // A large document of 400 KB is not sampled and counts as lacking the
    // candidate, whatever the sample of the others finds.
    let (demoted, why, samples) =
        run_located(band(10.0, 40.0), 0.5, &[(12, 400_000)], (40.0, 1.0), None);
    let held = cost(
        1500.0,
        0.5,
        40.0,
        39.0 * 40.0 + 400.0,
        1.0,
        (1, 400.0),
        Read::Sampled(16),
    );
    assert_eq!((demoted, why), (true, held));
    let usual: Vec<usize> = (0..40).filter(|&d| d != 12).collect();
    assert_eq!(samples, [demote::spread(&usual, demote::SAMPLE_DOCS)]);
    // A located walked reference's slowest shard, 1 µs for its 4 rows,
    // prices a row at 0.25 µs.
    let slowest = cost(750.0, 0.25, 30.0, 1200.0, 1.0, (0, 0.0), Read::Sampled(16));
    let (demoted, why, samples) = run_located(band(10.0, 40.0), 0.5, &[], (30.0, 1.0), Some(1.0));
    assert_eq!((demoted, why, samples.len()), (true, slowest, 1));
    // 0.04 µs for 4 rows is 0.01 µs a row, below the 0.02 µs a measured row
    // cost is clamped to.
    let clamped = cost(
        60.0,
        0.02,
        40.0,
        1600.0,
        0.0,
        (0, 0.0),
        unsampled(16, 640.0),
    );
    let (demoted, why, samples) = run_located(band(10.0, 40.0), 0.5, &[], (30.0, 1.0), Some(0.04));
    assert_eq!((demoted, why, samples.len()), (false, clamped, 0));
}

#[test]
fn a_sample_discounts_only_its_own_group() {
    // Two ANDs, each with a walked candidate. The inner one is decided
    // first, by a sample of its documents; that sample says nothing of the
    // outer one's documents.
    let e = Expr::And(vec![t("a"), t("f"), Expr::And(vec![t("b"), t("g")])]);
    let counts = vec![
        vec![(0..20, 4)],
        vec![(0..40, 3000)],
        vec![(20..40, 4)],
        vec![(0..40, 3000)],
    ];
    let (_, mut plan) = arranged_with(&e, counts, &[1, 3]);
    let refs: Vec<usize> = plan.groups.iter().map(|g| g.reference).collect();
    assert_eq!(refs, [2, 0]);
    let walks = Walks {
        leaves: vec![false, true, false, true],
        located_us: vec![None; 4],
        row_us: 0.5,
        doc_lens: vec![1000; 40],
    };
    let a = answer((0..20).map(|d| (d, 2)).collect(), 40);
    let b = answer((20..40).map(|d| (d, 2)).collect(), 40);
    let stage1 = [Some(&a), None, Some(&b), None];
    let mut samples = Vec::new();
    demote::decide(
        &mut plan,
        band(10.0, 90.0),
        &stage1,
        &walks.of(),
        &mut |ids| {
            samples.push(ids.to_vec());
            Ok(sample(90.0, 1.0))
        },
    );
    let inner: Vec<usize> = (20..40).collect();
    assert_eq!(samples, [demote::spread(&inner, demote::SAMPLE_DOCS)]);
    assert_eq!(plan.demoted, [false, false, false, true]);
}

#[test]
fn the_plan_states_each_decision_and_its_numbers() {
    use demote::{Read, Why};
    use explain::{Reason, Runs};
    let said = |why, demoted, counted, regex_reference| {
        Reason {
            why,
            demoted,
            counted,
            regex_reference,
        }
        .to_string()
    };
    let ratio = Why::Ratio {
        reference: 1,
        total: 27166,
        min: 2004,
    };
    let cost = |docs, read| Why::Cost {
        reference: 1,
        locate_us: 82_000.0,
        rows: None,
        docs,
        read_us: 9.8,
        read_all_us: docs as f64 * 9.8,
        held: None,
        large: None,
        read,
    };
    for (why, demoted, counted, regex, want) in [
        (Why::Located, false, false, false, "located"),
        (Why::Located, false, true, false, "located: not demotable"),
        (
            Why::Reference,
            false,
            true,
            false,
            "reference: rarest counted leaf of its AND",
        ),
        (
            ratio,
            true,
            true,
            true,
            "demoted by the 8x fallback ratio: 27166 > 8 x 2004 of leaf 1 (a regex reference)",
        ),
        (
            ratio,
            false,
            true,
            false,
            "located: 27166 <= 8 x 2004 of leaf 1, within the 8x fallback ratio (an AND under an OR)",
        ),
        (
            cost(85, Read::Ceiling),
            true,
            true,
            false,
            "demoted: locate est 82.0 ms > read 85 docs of leaf 1 x 9.80 µs = 833 µs at the band ceiling",
        ),
        (
            cost(10_000, Read::Floor),
            false,
            true,
            false,
            "located: locate est 82.0 ms <= read 10000 docs of leaf 1 x 9.80 µs = 98.0 ms at the band floor",
        ),
        (
            cost(1, Read::Sampled(1)),
            true,
            true,
            false,
            "demoted: locate est 82.0 ms > read 1 doc of leaf 1 x 9.80 µs = 9.80 µs, the read cost sampled over 1 doc",
        ),
        (
            cost(85, Read::Failed),
            true,
            true,
            false,
            "demoted: locate est 82.0 ms > read 85 docs of leaf 1 x 9.80 µs = 833 µs at the band ceiling, the read sample failed",
        ),
        (
            cost(0, Read::Ceiling),
            true,
            true,
            false,
            "demoted: leaf 1 has no document to read",
        ),
    ] {
        assert_eq!(said(why, demoted, counted, regex), want);
    }
    let walked = |read_all_us, held, large, read| Why::Cost {
        reference: 0,
        locate_us: 1500.0,
        rows: Some((3000, 0.5)),
        docs: 3,
        read_us: 10.0,
        read_all_us,
        held: Some(held),
        large: Some(large),
        read,
    };
    let unsampled = |docs, us| Read::Unsampled { docs, us };
    for (why, demoted, want) in [
        (
            walked(30.0, 0.0, (0, 0.0), Read::Ceiling),
            false,
            "located: locate est 1.5 ms (busiest shard 3000 x 0.500 µs) <= read 3 docs of leaf 0 x 10 µs = 30 µs at the band ceiling",
        ),
        (
            walked(15_020.0, 0.0, (1, 15_000.0), Read::Ceiling),
            false,
            "located: locate est 1.5 ms (busiest shard 3000 x 0.500 µs) <= read 3 docs of leaf 0: 2 x 10 µs and 1 large at 1.00 ns a byte = 15.0 ms at the band ceiling",
        ),
        (
            walked(30.0, 0.0, (0, 0.0), unsampled(3, 30.0)),
            false,
            "located: locate est 1.5 ms (busiest shard 3000 x 0.500 µs) <= 30 µs, a read sample of 3 of the 3 docs of leaf 0 at the band ceiling",
        ),
        (
            walked(15_020.0, 0.0, (1, 15_000.0), unsampled(2, 20.0)),
            false,
            "located: locate est 1.5 ms (busiest shard 3000 x 0.500 µs) <= 15.0 ms, a read sample of 2 of the 3 docs of leaf 0 at the band ceiling, and its 1 large doc at 1.00 ns a byte",
        ),
        (
            walked(15_000.0, 0.0, (3, 15_000.0), unsampled(0, 0.0)),
            false,
            "located: locate est 1.5 ms (busiest shard 3000 x 0.500 µs) <= 15.0 ms, the 3 large docs of leaf 0 at 1.00 ns a byte, none to sample",
        ),
        (
            walked(30.0, 1.0, (0, 0.0), Read::Sampled(3)),
            true,
            "demoted: locate est 1.5 ms (busiest shard 3000 x 0.500 µs) > read 3 docs of leaf 0 x 10 µs = 30 µs, less the 3 of 3 sampled that hold the candidates: 0 µs, the read cost sampled over 3 docs",
        ),
        (
            walked(420.0, 1.0, (1, 400.0), Read::Sampled(2)),
            true,
            "demoted: locate est 1.5 ms (busiest shard 3000 x 0.500 µs) > read 3 docs of leaf 0: 2 x 10 µs and 1 large at 1.00 ns a byte = 420 µs, less the 2 of 2 sampled that hold the candidates: 400 µs, the read cost sampled over 2 docs",
        ),
    ] {
        assert_eq!(said(why, demoted, true, false), want);
    }
    let row = |us| explain::Row(us).to_string();
    assert_eq!(
        [row(0.7), row(0.0067), row(12.345), row(250.0)],
        ["0.700 µs", "0.00670 µs", "12.3 µs", "250 µs"]
    );
    let runs = |r: &[Range<usize>]| Runs(r.iter().cloned()).to_string();
    assert_eq!(runs(&[]), "[]");
    assert_eq!(runs(&[0..10, 10..12, 20..30]), "[0..12) [20..30)");
    let many: Vec<Range<usize>> = (0..7).map(|k| 2 * k..2 * k + 1).collect();
    assert_eq!(runs(&many), "[0..1) [2..3) [4..5) [6..7) +3 more");
    let us = |x| crate::verbose::Us(x).to_string();
    assert_eq!(
        [us(1.76), us(8.0), us(989.4), us(6800.0), us(2.5e6)],
        ["1.76 µs", "8 µs", "989 µs", "6.8 ms", "2.50 s"]
    );
}

#[test]
fn a_measured_band_keeps_the_default_shape_within_2_to_200_us() {
    assert_eq!(ReadBand::around(12.0), ReadBand::DEFAULT);
    let band = ReadBand {
        min_us: 30.0,
        max_us: 200.0,
    };
    assert_eq!(ReadBand::around(30.0), band);
    assert_eq!(ReadBand::around(0.0).min_us, 2.0);
    assert_eq!(ReadBand::around(1e9).min_us, 200.0);
    for w in [0.0, 7.5, 12.0, 30.0, 1e9] {
        let b = ReadBand::around(w);
        assert!((b.max_us / b.min_us - 80.0 / 12.0).abs() < 1e-12, "{w}");
    }
}

/// Answers to counts, -l and -c.
type Answers = [Result<serde_json::Value, String>; 3];

/// Pinned bands that make the plan whatever a probe's wall time: read the
/// documents of every candidate, or locate every candidate. Between a
/// band's edges, a probe's wall time picks the plan.
const PLANS: [(f64, f64); 2] = [(0.0, 0.0), (f64::INFINITY, f64::INFINITY)];

/// The band of a fresh load of `index`, measured when `measure`, and its
/// answers to `expr` under the band `pin`, or its own band.
fn answers(
    index: &std::path::Path,
    expr: &Expr,
    measure: bool,
    pin: Option<(f64, f64)>,
) -> (ReadBand, Answers) {
    let manifest = crate::manifest::read_manifest(index).unwrap();
    let (fm, mut loaded) =
        super::super::load(index, &manifest, None, crate::fetch::Ask::Load, None).unwrap();
    if measure {
        loaded.measure_band(&manifest);
    }
    crate::tests::BAND.set(pin);
    let outs = [
        QueryOutput::Counts,
        QueryOutput::FilesWithMatches,
        QueryOutput::CountLines,
    ]
    .map(|output| {
        let req = QueryRequest {
            expr: Some(expr),
            ..crate::tests::support::request(index, &manifest, &[], output)
        };
        answer_expr(&fm, &mut loaded, &req, expr, &mut crate::sink::Discard)
            .map(crate::tests::support::timeless)
    });
    crate::tests::BAND.set(None);
    (loaded.band, outs)
}

/// The band measured over `index`, and the answers to `expr` under each of
/// [`PLANS`], once a measured load is checked against a default one under
/// each, and each answer under its own band against those of the plans.
fn measured_band(index: &std::path::Path, expr: &Expr) -> (ReadBand, [Answers; 2]) {
    let plans = PLANS.map(|pin| {
        let (_, got) = answers(index, expr, true, Some(pin));
        let (default, want) = answers(index, expr, false, Some(pin));
        assert_eq!(default, ReadBand::DEFAULT);
        assert_eq!(got, want, "{pin:?}");
        got
    });
    // Each search plans on its own: its answer is one plan's.
    let (band, got) = answers(index, expr, true, None);
    for (i, got) in got.iter().enumerate() {
        assert!(plans.iter().any(|p| p[i] == *got), "{i}: {got:?} {plans:?}");
    }
    (band, plans)
}

#[test]
fn serve_measures_the_band_through_the_store() {
    use crate::tests::support::{LAYOUTS, index_it, stage_corpus};
    let files: [(&str, &[u8]); 4] = [
        ("a.c", b"alpha one printk\n"),
        ("b.c", b"printk printk printk alpha\n"),
        ("c.c", b"one\n"),
        ("d.c", b""),
    ];
    let expr = Expr::And(vec![t("one"), t("printk"), t("alpha")]);
    let indexed = |tmp: &std::path::Path, files: &[(&str, &[u8])], store_docs| {
        let index = tmp.join("idx");
        index_it(&stage_corpus(tmp, files), &index, store_docs);
        index
    };
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let (band, [read, located]) =
            measured_band(&indexed(tmp.path(), &files, store_docs), &expr);
        assert!((2.0..=200.0).contains(&band.min_us), "{band:?}");
        assert_eq!(band, ReadBand::around(band.min_us));
        assert_eq!(read, located);
    }
    // Only stale or empty files: nothing fresh to read.
    let tmp = tempfile::tempdir().unwrap();
    let stale = indexed(
        tmp.path(),
        &[("a.c", b"one printk alpha\n"), ("d.c", b"")],
        false,
    );
    std::fs::write(
        tmp.path().join("corpus/a.c"),
        b"one printk alpha, changed\n",
    )
    .unwrap();
    let (band, [read, located]) = measured_band(&stale, &expr);
    assert_eq!(band, ReadBand::DEFAULT);
    assert_eq!(read, located);
    // No docs.shard: the measure fails, and only a search whose plan reads
    // documents fails: -c always, the counts and -l when they read.
    let tmp = tempfile::tempdir().unwrap();
    let blind = indexed(tmp.path(), &files, true);
    std::fs::rename(
        blind.join(crate::manifest::DOCS_SHARD),
        blind.join("hidden"),
    )
    .unwrap();
    let (band, [read, located]) = measured_band(&blind, &expr);
    assert_eq!(band, ReadBand::DEFAULT);
    let unread = |r: &Result<_, String>| r.as_ref().is_err_and(|e| e.contains("cannot read"));
    assert!(read.iter().all(unread), "{read:?}");
    assert!(located[..2].iter().all(Result::is_ok), "{located:?}");
    assert!(unread(&located[2]), "{located:?}");
}

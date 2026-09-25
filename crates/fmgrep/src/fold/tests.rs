//! Tests over [`ScanCursor`], no index: the naive scan is the oracle for both
//! the plan and the walk.

use super::*;
use crate::cursor::ScanCursor;

/// The case-folded HIR `-i` compiles, i.e. what [`fold_plan`] reads.
fn folded(pattern: &str) -> Hir {
    regex_syntax::ParserBuilder::new()
        .case_insensitive(true)
        .build()
        .parse(pattern)
        .expect("valid pattern")
}

fn plan_of(pattern: &str) -> Option<FoldPlan> {
    fold_plan(&folded(pattern))
}

/// One step's alternatives as strings, in plan order.
fn step_strings(plan: &FoldPlan, step: usize) -> Vec<String> {
    plan.steps()[step]
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

#[test]
fn plan_folds_every_variant_the_regex_crate_does() {
    // Unicode simple folding, not ASCII: `regex -i` matches all three, so a
    // plan that branched only over {K,k} would answer a silent zero.
    let plan = plan_of("k").expect("plan");
    assert_eq!(step_strings(&plan, 0), ["K", "k", "\u{212A}"]);
    let plan = plan_of("s").expect("plan");
    assert_eq!(step_strings(&plan, 0), ["S", "s", "\u{17F}"]);
    // The Kelvin sign is three UTF-8 bytes; a step alternative is whole.
    assert_eq!(plan_of("k").unwrap().steps()[0][2].len(), 3);
    let plan = plan_of("Hsc").expect("plan");
    assert_eq!(plan.steps().len(), 3);
    assert_eq!(step_strings(&plan, 1), ["S", "s", "\u{17F}"]);
}

#[test]
fn plan_refuses_non_concat_shapes_and_wide_classes() {
    for pattern in [
        "a+", "a*", "[a-z]", "foo|bar", "^abc", "abc$", "a.c", "(a)?",
    ] {
        assert!(plan_of(pattern).is_none(), "pattern {pattern:?}");
    }
    // A term longer than the step cap is refused, not truncated.
    assert!(plan_of(&"a1".repeat(MAX_STEPS)).is_none());
}

#[test]
fn plan_refuses_separator_bearing_alternatives() {
    assert!(plan_of("a\\x00b").is_none());
    assert!(plan_of("\\x00").is_none());
}

#[test]
fn plan_merges_singleton_runs_into_one_extension() {
    // `[1]` and `[2]` are separate single-member classes in the HIR; the
    // plan carries one extension for the pair.
    let plan = plan_of("a[1][2]b").expect("plan");
    assert_eq!(plan.steps().len(), 3);
    assert_eq!(step_strings(&plan, 1), ["12"]);
}

/// Every document position a `(?i)` regex matches at.
fn oracle(text: &[u8], pattern: &str) -> bool {
    regex::bytes::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .expect("valid pattern")
        .is_match(text)
}

#[test]
fn walk_agrees_with_a_naive_case_insensitive_scan() {
    let texts: &[&[u8]] = &[
        b"HscEnv is here",
        b"hscenv",
        b"HSCENV",
        b"hSCeNV and more",
        b"nothing to see",
        b"print\xe2\x84\xaa", // printK with the Kelvin sign
        b"cla\xc5\xbfs",      // class with a long s
        b"PRINTK",
        b"",
        b"printk printk",
    ];
    for pattern in ["hscenv", "printk", "class", "HSCENV", "Printk"] {
        let plan = plan_of(pattern).expect("plan");
        for text in texts {
            assert_eq!(
                fold_any_capped(ScanCursor::from(*text), &plan, max_live()),
                Some(oracle(text, pattern)),
                "pattern {pattern:?} text {:?}",
                String::from_utf8_lossy(text),
            );
        }
    }
}

#[test]
fn surviving_offsets_are_exactly_the_folded_match_starts() {
    let plan = plan_of("printk").expect("plan");
    let text = b"printk PRINTK print\xe2\x84\xaa Printk".as_slice();
    let mut offsets = fold_locate(ScanCursor::from(text), &plan).expect("under the cap");
    offsets.sort_unstable();
    assert_eq!(offsets, [0, 7, 14, 23]);
    // Nothing matches: an empty offset list, not a give-up.
    assert_eq!(
        fold_locate(ScanCursor::from(b"nothing".as_slice()), &plan).map(|o| o.len()),
        Some(0)
    );
}

#[test]
fn live_cap_gives_up_instead_of_answering_no() {
    let plan = plan_of("printk").expect("plan");
    // Two case variants of the last position survive, so a cap of 1 is hit.
    let text = b"printk PRINTK".as_slice();
    assert_eq!(fold_any_capped(ScanCursor::from(text), &plan, 1), None);
    assert_eq!(fold_locate_capped(ScanCursor::from(text), &plan, 1), None);
    // A doc that matches nowhere still answers `false` under the same cap:
    // giving up is about branch count, not about the answer.
    assert_eq!(
        fold_any_capped(ScanCursor::from(b"nothing".as_slice()), &plan, 1),
        Some(false)
    );
}

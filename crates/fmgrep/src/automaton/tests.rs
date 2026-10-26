//! Tests over [`ScanCursor`], no index: the regex crate is the oracle for both
//! the plan and the walk.

use super::*;
use crate::cursor::ScanCursor;

fn ci() -> MatchMode {
    MatchMode {
        ignore_case: true,
        ..MatchMode::default()
    }
}

fn dotall() -> MatchMode {
    MatchMode {
        multiline: true,
        dotall: true,
        ..MatchMode::default()
    }
}

/// Plan `pattern` exactly as [`crate::PreparedPattern`] does under `mode`.
fn plan_of(pattern: &str, mode: MatchMode) -> Option<RegexPlan> {
    let hir = regex_syntax::ParserBuilder::new()
        .multi_line(true)
        .dot_matches_new_line(mode.multiline && mode.dotall)
        .case_insensitive(mode.ignore_case)
        .build()
        .parse(pattern)
        .expect("valid pattern");
    regex_plan(pattern, &hir, mode)
}

fn oracle(pattern: &str, mode: MatchMode) -> regex::bytes::Regex {
    regex::bytes::RegexBuilder::new(pattern)
        .multi_line(true)
        .dot_matches_new_line(mode.multiline && mode.dotall)
        .case_insensitive(mode.ignore_case)
        .build()
        .expect("valid pattern")
}

/// Bounded-class shapes: every walk below finishes inside the budgets, so a
/// `None` anywhere in this battery is a failure, not a widen.
const BATTERY: &[&str] = &[
    "abc",
    "a[bc]d",
    "(foo|bar)baz",
    "Int(32|64)#",
    "[A-Z][a-z]+Env",
    "fo+bar",
    "x?yz",
    "a{2,3}b",
    "[0-9]{4,}",
    "gr[ae]y",
];

const TEXTS: &[&[u8]] = &[
    b"abc",
    b"xabcx",
    b"abd",
    b"gray and grey",
    b"foobaz barbaz",
    b"Int32# Int64#",
    b"HscEnv DynFlags",
    b"fooobar fobar",
    b"yz xyz",
    b"aab aaab ab",
    b"12345 and 999",
    b"nothing to see here",
    b"",
];

#[test]
fn plan_refuses_lookaround_and_unsupported_shapes() {
    // The walk sees no surrounding context, so an assertion is unknowable.
    for pattern in ["^foo", "foo$", "\\bfoo\\b", "\\Afoo", "foo\\z", "(?m)^foo"] {
        assert!(
            plan_of(pattern, MatchMode::default()).is_none(),
            "{pattern:?}"
        );
    }
    // A pattern matching the empty string prunes nothing.
    for pattern in ["a*", "x?", "(|a)"] {
        assert!(
            plan_of(pattern, MatchMode::default()).is_none(),
            "{pattern:?}"
        );
    }
    // An over-large DFA is refused, not built.
    assert!(plan_capped("[a-z]{300}", MatchMode::default(), 1024).is_none());
    assert!(plan_capped("[a-z]{300}", MatchMode::default(), DFA_BYTES).is_some());
}

#[test]
fn walk_agrees_with_the_regex_crate() {
    for pattern in BATTERY {
        let plan = plan_of(pattern, MatchMode::default()).expect("plan");
        let from_hir = plan_hir(&hir_of(pattern), DFA_BYTES).expect("plan");
        let re = oracle(pattern, MatchMode::default());
        for text in TEXTS {
            assert_eq!(
                any_capped(ScanCursor::from(*text), &from_hir, &CAPS),
                any_capped(ScanCursor::from(*text), &plan, &CAPS),
                "pattern {pattern:?}",
            );
            assert_eq!(
                any_capped(ScanCursor::from(*text), &plan, &CAPS),
                Some(re.is_match(text)),
                "pattern {pattern:?} text {:?}",
                String::from_utf8_lossy(text),
            );
        }
    }
}

#[test]
fn accepted_offsets_start_matches_and_meet_every_match() {
    for pattern in BATTERY {
        let plan = plan_of(pattern, MatchMode::default()).expect("plan");
        let re = oracle(pattern, MatchMode::default());
        // `^` here is the start of the slice handed to it: a test that the
        // offset really begins a whole match.
        let at_offset = oracle(&format!("^(?:{pattern})"), MatchMode::default());
        for text in TEXTS {
            let mut got = locate_capped(ScanCursor::from(*text), &plan, &CAPS).expect("no widen");
            got.sort_unstable();
            got.dedup();
            for &off in &got {
                assert!(
                    at_offset.find(&text[off..]).is_some_and(|m| m.start() == 0),
                    "pattern {pattern:?} offset {off} starts no match",
                );
            }
            // Every true match holds an offset — the walk stops at the
            // shortest accepting suffix, which still lands in the match's
            // document. This is the candidate-set guarantee, and the one
            // the one-byte match delay would break.
            for m in re.find_iter(text) {
                assert!(
                    got.iter().any(|&o| (m.start()..m.end()).contains(&o)),
                    "pattern {pattern:?} text {:?} misses match {}..{}",
                    String::from_utf8_lossy(text),
                    m.start(),
                    m.end(),
                );
            }
        }
    }
}

#[test]
fn depth_cap_widens_instead_of_answering_no() {
    let plan = plan_of("[A-Z][a-z]+Env", MatchMode::default()).expect("plan");
    let caps = Caps { depth: 3, ..CAPS };
    assert_eq!(
        any_capped(ScanCursor::from(b"HscEnv".as_slice()), &plan, &caps),
        None
    );
    // Giving up is about the walk, not the answer: a text the first step
    // already kills still answers `false`.
    assert_eq!(
        any_capped(ScanCursor::from(b"nothing".as_slice()), &plan, &caps),
        Some(false)
    );
}

#[test]
fn live_cap_widens() {
    let plan = plan_of("gr[ae]y", MatchMode::default()).expect("plan");
    let caps = Caps { live: 1, ..CAPS };
    assert_eq!(
        any_capped(ScanCursor::from(b"gray grey".as_slice()), &plan, &caps),
        None
    );
    assert_eq!(
        any_capped(ScanCursor::from(b"gray only".as_slice()), &plan, &caps),
        Some(true)
    );
}

#[test]
fn node_cap_widens() {
    let plan = plan_of("abc", MatchMode::default()).expect("plan");
    let caps = Caps { nodes: 1, ..CAPS };
    assert_eq!(
        any_capped(ScanCursor::from(b"abc".as_slice()), &plan, &caps),
        None
    );
    assert_eq!(
        any_capped(ScanCursor::from(b"xyz".as_slice()), &plan, &caps),
        Some(false)
    );
}

#[test]
fn class_width_cap_widens() {
    let plan = plan_of("a[bc]d", MatchMode::default()).expect("plan");
    let caps = Caps {
        class_bytes: 1,
        ..CAPS
    };
    assert_eq!(
        any_capped(ScanCursor::from(b"abd".as_slice()), &plan, &caps),
        None
    );
}

#[test]
fn nul_matching_patterns_never_plan() {
    // The FM-index cannot answer for NUL, so a pattern that can hold
    // one must not prune — `.` and negated classes admit NUL, in every mode.
    for pattern in ["a\\x00b", "\\x00", "a.b", "a[^x]b"] {
        for mode in [MatchMode::default(), dotall()] {
            assert!(plan_of(pattern, mode).is_none(), "{pattern:?}");
        }
    }
    // `\\d` and `\\w` hold no NUL and still plan.
    assert!(plan_of("a\\d\\wb", MatchMode::default()).is_some());
}

#[test]
fn an_over_wide_first_step_never_plans() {
    // The walk reads a match back to front, and Unicode `\\d` ends on any of
    // 74 bytes — over the shipped 64-byte cap. A pattern ending there would
    // widen on every document, so the anchor keeps the job.
    assert!(plan_of("\\d{4,}", MatchMode::default()).is_none());
    assert!(plan_of("\\d{4,}x", MatchMode::default()).is_some());
}

#[test]
fn ci_plan_covers_the_unicode_folds() {
    let plan = plan_of("printk", ci()).expect("plan");
    for text in [b"printk".as_slice(), b"PRINTK", b"print\xe2\x84\xaa"] {
        assert_eq!(
            any_capped(ScanCursor::from(text), &plan, &CAPS),
            Some(true),
            "text {:?}",
            String::from_utf8_lossy(text),
        );
    }
    assert_eq!(
        any_capped(ScanCursor::from(b"nothing".as_slice()), &plan, &CAPS),
        Some(false)
    );
}

fn hir_of(pattern: &str) -> Hir {
    regex_syntax::ParserBuilder::new()
        .multi_line(true)
        .build()
        .parse(pattern)
        .expect("valid pattern")
}

#[test]
fn wide_classes_count_their_last_bytes() {
    for pattern in ["\\w", "[\\w.]", "\\d", "(?-u:[\\x00-\\x40])"] {
        assert!(wide(&hir_of(pattern), 64), "{pattern:?}");
    }
    for pattern in [
        "[a-z]",
        "[ \\t]",
        "[Kk\u{212A}]",
        "(?-u:\\d)",
        "(?-u:[\\x00-\\x3F])",
        "union",
    ] {
        assert!(!wide(&hir_of(pattern), 64), "{pattern:?}");
    }
}

/// Patterns whose core is planned; each text holds matches.
const CORE_BATTERY: &[&str] = &[
    "union[ \\t]+\\{",
    "typedef[ \\t]+struct",
    "(foo|bar)[0-9]+baz",
    "import GHC\\.(Core|Stg|Cmm)\\.[\\w.]+",
    "DCHECK_(EQ|NE)\\([^;\\n]*\\)",
    "^foo(bar|baz)\\w+",
];

const CORE_TEXTS: &[&[u8]] = &[
    b"union {} union\t\t{ union x",
    b"typedef struct s; typedef  struct",
    b"foo12baz bar9baz foobaz",
    b"import GHC.Core.Type\nimport GHC.Stg.Syntax (x)\nimport GHC.Types",
    b"DCHECK_EQ(a, b); DCHECK_NE(x) DCHECK_EQ(;)",
    b"foobarx\nfoobaz_1 foobar",
];

#[test]
fn core_plan_meets_every_match() {
    for pattern in CORE_BATTERY {
        let hir = hir_of(pattern);
        let anchor = required_literal(&hir).expect("anchored");
        let core = core(&hir, &anchor, 64).unwrap_or_else(|| panic!("{pattern:?} has a core"));
        let plan = plan_hir(&core, DFA_BYTES).expect("plan");
        let re = oracle(pattern, MatchMode::default());
        for text in CORE_TEXTS {
            let got = locate_capped(ScanCursor::from(*text), &plan, &CAPS).expect("no widen");
            for m in re.find_iter(text) {
                assert!(
                    got.iter().any(|&o| (m.start()..m.end()).contains(&o)),
                    "pattern {pattern:?} misses match {}..{}",
                    m.start(),
                    m.end(),
                );
            }
        }
    }
    for pattern in [
        "printk",
        "stg_\\w+_info",
        "a.b",
        "^printk$",
        "import qualified \\w+ [a-z]",
    ] {
        let hir = hir_of(pattern);
        let anchor = required_literal(&hir).expect("anchored");
        assert!(core(&hir, &anchor, 64).is_none(), "{pattern:?}");
    }
}

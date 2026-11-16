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
            let mut got =
                locate_capped(ScanCursor::from(*text), &plan, &CAPS, None).expect("no widen");
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
            let got = locate_capped(ScanCursor::from(*text), &plan, &CAPS, None).expect("no widen");
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

/// Every offset of `needle` in `text`, overlaps included, ascending.
fn offsets_of(text: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut at = Vec::new();
    ScanCursor::from(text)
        .extend_front(needle)
        .locate(&mut |o| at.push(o));
    at
}

/// The located offsets, ascending, of `pattern` over `text` with `anchor`.
fn located(pattern: &str, text: &[u8], caps: &Caps, anchor: &[u8]) -> Option<Vec<usize>> {
    let plan = plan_of(pattern, MatchMode::default()).expect("plan");
    let mut got = locate_capped(ScanCursor::from(text), &plan, caps, Some(anchor))?;
    got.sort_unstable();
    Some(got)
}

#[test]
fn a_walk_that_gave_up_after_its_anchor_locates_its_frontier() {
    // The walk steps `ULL`, then `FULL`, then four strings: over a cap of
    // two. Its last whole frontier is `FULL`, four of the seven `ULL`s.
    let text = b"0x1FULL 0x2FULL 3FULL 4FULL zzULL zzULL zzULL".as_slice();
    let caps = Caps { live: 2, ..CAPS };
    let plan = plan_of("0x[0-9A-F]{2}ULL", MatchMode::default()).expect("plan");
    assert_eq!(
        locate_capped(ScanCursor::from(text), &plan, &caps, None),
        None
    );
    let got = located("0x[0-9A-F]{2}ULL", text, &caps, b"ULL");
    assert_eq!(got, Some(offsets_of(text, b"FULL")));
    assert_eq!(offsets_of(text, b"ULL").len(), 7);
    // The other budgets stop a walk inside a frontier: it widens as before.
    let nodes = Caps { nodes: 2, ..CAPS };
    assert_eq!(located("0x[0-9A-F]{2}ULL", text, &nodes, b"ULL"), None);
    let depth = Caps { depth: 4, ..CAPS };
    assert_eq!(located("0x[0-9A-F]{2}ULL", text, &depth, b"ULL"), None);
    let narrow = Caps {
        class_bytes: 2,
        ..CAPS
    };
    let text = b"caULL abULL xULL".as_slice();
    assert_eq!(located("[a-c][ab]ULL", text, &narrow, b"ULL"), None);
}

#[test]
fn a_frontier_owes_each_string_not_accepted_once() {
    // The last whole frontier is `2ULL`, a match located when it was
    // accepted, and `3ULL`, whose three ways back are over a cap of two:
    // `2ULL` is not located again.
    let caps = Caps { live: 2, ..CAPS };
    let text = b"13ULL 2ULL 53ULL 63ULL".as_slice();
    assert_eq!(
        located("(2|[1-9]3)ULL", text, &caps, b"ULL"),
        Some(vec![1, 6, 12, 18])
    );
}

#[test]
fn a_walk_that_branched_before_its_anchor_widens() {
    let caps = Caps { live: 2, ..CAPS };
    // The anchor first: the walk steps `L`, never `0x`.
    let text = b"0x1FL 0x2FL 3FL 4FL".as_slice();
    assert_eq!(located("0x[0-9A-F]{2}L", text, &caps, b"0x"), None);
    // The anchor inside the first step but not at its end.
    let text = b"12ULLx 34ULLx 5ULLx 6ULLx".as_slice();
    assert_eq!(located("[0-9]{2}ULLx", text, &caps, b"ULL"), None);
    // The anchor in the middle: the first step already branches.
    let text = b"12ULLA 34ULLB 5ULLC 6ULLD".as_slice();
    assert_eq!(located("[0-9]{2}ULL[A-D]", text, &caps, b"ULL"), None);
    // A walk that gives up inside its first step has no frontier past it.
    let narrow = Caps {
        class_bytes: 2,
        ..CAPS
    };
    let text = b"aULL bULL cULL".as_slice();
    assert_eq!(located("[a-c]ULL", text, &narrow, b"ULL"), None);
}

/// Seeded corpora of NUL-separated documents built from `CHUNKS`.
const CHUNKS: &[&str] = &[
    "0x", "ULL", "UL", "L", "1F", "A", "0", "1", " ", "z", "x", "FULL", "\n",
];

/// Pattern pieces: literals, classes, repetitions and alternations.
const PIECES: &[&str] = &[
    "0x",
    "ULL",
    "L",
    "UL",
    "z",
    "[0-9A-F]",
    "[01AF]",
    "[AF]{2}",
    "[0-9A-F]{2}",
    "[01]{1,2}",
    "(A|1F)",
    "(UL|ULL)",
    "(0|x)",
    "F+",
];

#[test]
fn a_frontier_never_locates_more_than_its_anchor() {
    use crate::pattern::{PreparedPattern, anchor};
    let mut rng = crate::tests::support::Rng(0x9E37_79B9_7F4A_7C15);
    let (mut fired, mut kept) = (0, 0);
    for round in 0..5000 {
        let pieces = 2 + rng.below(3);
        let pattern: String = (0..pieces)
            .map(|_| PIECES[rng.below(PIECES.len())])
            .collect();
        let mode = match rng.below(4) {
            0 => ci(),
            _ => MatchMode::default(),
        };
        let prepared = PreparedPattern::prepare_with(&pattern, mode).expect("valid");
        let (Some(plan), Some(anchor)) = (prepared.automaton(), anchor(&prepared)) else {
            continue;
        };
        let docs: Vec<Vec<u8>> = (0..3 + rng.below(6))
            .map(|_| {
                (0..rng.below(24))
                    .flat_map(|_| CHUNKS[rng.below(CHUNKS.len())].bytes())
                    .collect()
            })
            .collect();
        let text = docs.join(&[crate::UNIFIED_SEPARATOR][..]);
        let mut starts = vec![0];
        for d in &docs {
            starts.push(starts.last().unwrap() + d.len() + 1);
        }
        let doc_of = |off: usize| starts.partition_point(|&s| s <= off) - 1;
        let caps = Caps {
            live: 1 + rng.below(4),
            depth: [5, 64, 64, 64][rng.below(4)],
            ..CAPS
        };
        let main = locate_capped(ScanCursor::from(text.as_slice()), plan, &caps, None);
        let tip = locate_capped(ScanCursor::from(text.as_slice()), plan, &caps, Some(anchor));
        let what = format!(
            "round {round} {pattern:?} {mode:?} {:?}",
            text.escape_ascii()
        );
        let hits = match (main, tip) {
            (Some(main), tip) => {
                // A walk that did not give up answers as it did.
                assert_eq!(tip, Some(main), "{what}");
                continue;
            }
            (None, None) => {
                kept += 1;
                continue;
            }
            (None, Some(hits)) => hits,
        };
        fired += 1;
        // Main located the anchor: never fewer rows, never fewer documents.
        // Rows count with repeats: a located string and a longer one it
        // begins share their start rows, but each located row ends at its
        // own occurrence of the anchor.
        let rows = offsets_of(&text, anchor);
        let mut sorted = hits.clone();
        sorted.sort_unstable();
        assert!(hits.len() <= rows.len(), "{what}");
        let docs_of = |at: &[usize]| {
            let mut d: Vec<usize> = at.iter().map(|&o| doc_of(o)).collect();
            d.dedup();
            d
        };
        let (got, wide) = (docs_of(&sorted), docs_of(&rows));
        assert!(got.iter().all(|d| wide.contains(d)), "{what}");
        // Every match holds a located offset, so its document is kept.
        let re = oracle(&pattern, mode);
        for (d, doc) in docs.iter().enumerate() {
            for m in re.find_iter(doc) {
                let (a, b) = (starts[d] + m.start(), starts[d] + m.end());
                assert!(sorted.iter().any(|o| (a..b).contains(o)), "{what}");
            }
        }
    }
    // The battery reaches both sides of the rule.
    assert!(fired > 100 && kept > 100, "fired {fired} kept {kept}");
}

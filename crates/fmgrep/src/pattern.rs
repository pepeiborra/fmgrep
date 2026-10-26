use crate::{RegexError, UNIFIED_SEPARATOR};

use crate::automaton::{DeferredPlan, MAX_ANCHOR, RegexPlan, regex_plan};
use crate::fold::{FoldPlan, fold_plan};
use crate::matching::MatchMode;

/// Overlapping occurrences of `finder`'s needle; an empty needle counts 0.
pub fn count_overlapping(haystack: &[u8], finder: &memchr::memmem::Finder<'_>) -> u64 {
    if finder.needle().is_empty() {
        return 0;
    }
    let mut count = 0u64;
    let mut start = 0usize;
    while let Some(rel) = finder.find(&haystack[start..]) {
        count += 1;
        start += rel + 1;
    }
    count
}

/// Inputs longer than this never seed the quadratic
/// [`longest_common_substring`].
const ANCHOR_LEN_CAP: usize = 1024;

/// Longest common byte substring of `a` and `b` (quadratic DP over the shorter
/// outer loop). Over-long inputs degrade to empty (no anchor) rather than
/// paying quadratic cost on adversarial patterns.
fn longest_common_substring(a: &[u8], b: &[u8]) -> Vec<u8> {
    if a.len() > ANCHOR_LEN_CAP || b.len() > ANCHOR_LEN_CAP {
        return Vec::new();
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut prev = vec![0usize; long.len() + 1];
    let (mut best_len, mut best_end) = (0usize, 0usize);
    for (i, &sb) in short.iter().enumerate() {
        let mut cur = vec![0usize; long.len() + 1];
        for (j, &lb) in long.iter().enumerate() {
            if sb == lb {
                cur[j + 1] = prev[j] + 1;
                if cur[j + 1] > best_len {
                    best_len = cur[j + 1];
                    best_end = i + 1;
                }
            }
        }
        prev = cur;
    }
    short[best_end - best_len..best_end].to_vec()
}

/// Longest byte string every match of `hir` must contain, or `None` when no
/// non-empty such string is known.
///
/// Soundness rule: the result is contained in every match, so absence proved
/// by FM backward search means zero matches. Literals propagate;
/// concatenations take the longest child literal; min > 0 repetitions and
/// captures propagate their inner literal; alternations take the longest
/// common substring of the branches' literals; classes, look-around, nullable
/// repetitions and empty subexpressions contribute nothing.
pub fn required_literal(hir: &regex_syntax::hir::Hir) -> Option<Vec<u8>> {
    use regex_syntax::hir::HirKind;
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) | HirKind::Class(_) => None,
        HirKind::Literal(lit) => {
            if lit.0.is_empty() {
                None
            } else {
                Some(lit.0.to_vec())
            }
        }
        HirKind::Capture(cap) => required_literal(&cap.sub),
        HirKind::Repetition(rep) => {
            if rep.min == 0 {
                None
            } else {
                required_literal(&rep.sub)
            }
        }
        HirKind::Concat(hirs) => hirs
            .iter()
            .filter_map(required_literal)
            .max_by_key(Vec::len),
        HirKind::Alternation(hirs) => {
            let mut acc: Option<Vec<u8>> = None;
            for h in hirs {
                let lit = required_literal(h)?;
                acc = Some(match acc {
                    None => lit,
                    Some(a) => longest_common_substring(&a, &lit),
                });
                if acc.as_ref().is_some_and(Vec::is_empty) {
                    return None;
                }
            }
            acc.filter(|s| !s.is_empty())
        }
    }
}

/// One regex compiled once, plus the literals the FM-index prunes it with.
pub struct PreparedPattern {
    re: regex::bytes::Regex,
    anchor: Option<Vec<u8>>,
    literal: Option<Vec<u8>>,
    fold: Option<FoldPlan>,
    automaton: Option<RegexPlan>,
    deferred: Option<DeferredPlan>,
}

/// The anchor of a parsed pattern: its required literal, unless that literal
/// holds the separator byte.
fn anchor_of(hir: &regex_syntax::hir::Hir) -> Option<Vec<u8>> {
    required_literal(hir).filter(|lit| !lit.is_empty() && !lit.contains(&UNIFIED_SEPARATOR))
}

/// `hir` as a literal whose occurrences cannot overlap, span a line or a
/// separator, or touch a BOM byte.
fn counted_literal(hir: &regex_syntax::hir::Hir) -> Option<Vec<u8>> {
    let regex_syntax::hir::HirKind::Literal(lit) = hir.kind() else {
        return None;
    };
    let bytes = &lit.0;
    (bytes.is_ascii()
        && !bytes.contains(&UNIFIED_SEPARATOR)
        && crate::matching::literal_counts_are_exact(bytes))
    .then(|| bytes.to_vec())
}

impl PreparedPattern {
    /// Compile `pattern` under rg's default mode.
    #[cfg(test)]
    pub fn prepare(pattern: &str) -> Result<Self, RegexError> {
        Self::prepare_with(pattern, MatchMode::default())
    }

    /// Compile `pattern` under `mode`; empty and invalid patterns are a
    /// [`RegexError`] naming it. Every prune comes from one HIR parsed with
    /// the matcher's flags; [`Self::count`]'s regex carries only `-i`.
    pub fn prepare_with(pattern: &str, mode: MatchMode) -> Result<Self, RegexError> {
        if pattern.is_empty() {
            return Err(RegexError::new(
                pattern,
                "empty regex pattern: rejected by the native-regex contract (not match-everywhere)",
            ));
        }
        let re = regex::bytes::RegexBuilder::new(pattern)
            .case_insensitive(mode.ignore_case)
            .build()
            .map_err(|e| RegexError::new(pattern, e.to_string()))?;
        // A pattern the HIR parser refuses (it rejects matching invalid
        // UTF-8, which `regex::bytes` allows) simply prunes with nothing.
        let hir = regex_syntax::ParserBuilder::new()
            .multi_line(true)
            .dot_matches_new_line(mode.multiline && mode.dotall)
            .case_insensitive(mode.ignore_case)
            .build()
            .parse(pattern)
            .ok();
        let anchor = hir.as_ref().and_then(anchor_of);
        let literal = hir.as_ref().and_then(counted_literal);
        let fold = hir
            .as_ref()
            .filter(|_| mode.ignore_case)
            .and_then(fold_plan);
        let short = anchor.as_ref().map_or(0, Vec::len) <= MAX_ANCHOR;
        let automaton = hir
            .as_ref()
            .filter(|_| fold.is_none() && short)
            .and_then(|hir| regex_plan(pattern, hir, mode));
        let deferred = hir
            .as_ref()
            .zip(anchor.as_deref())
            .filter(|_| fold.is_none() && !short)
            .and_then(|(hir, anchor)| DeferredPlan::new(hir, anchor));
        Ok(Self {
            re,
            anchor,
            literal,
            fold,
            automaton,
            deferred,
        })
    }

    /// The branching fold plan, or `None` when this pattern has no usable one.
    pub fn fold(&self) -> Option<&FoldPlan> {
        self.fold.as_ref()
    }

    /// The plan every shard walks: built at prepare time for an anchor of at
    /// most [`MAX_ANCHOR`] bytes.
    pub fn automaton(&self) -> Option<&RegexPlan> {
        self.automaton.as_ref()
    }

    /// Exact non-overlapping count over `raw`: `find_iter` is the pinned
    /// `Matches` semantics (`/aa/` on `"aaaa"` is 2).
    pub fn count(&self, raw: &[u8]) -> u64 {
        self.re.find_iter(raw).count() as u64
    }
}

/// The literal `p` is, when its FM count per document equals
/// [`PreparedPattern::count`] on the BOM-stripped document.
pub fn literal(p: &PreparedPattern) -> Option<&[u8]> {
    p.literal.as_deref()
}

/// Whether the FM-index can narrow `p` at all; with `false` every document
/// is a candidate.
pub fn narrows(p: &PreparedPattern) -> bool {
    p.anchor.is_some() || p.fold.is_some() || p.automaton.is_some()
}

/// The literal every match of `p` holds, never holding the separator.
pub fn anchor(p: &PreparedPattern) -> Option<&[u8]> {
    p.anchor.as_deref()
}

/// The plan a shard walks where `p`'s longer anchor is common.
pub fn deferred(p: &PreparedPattern) -> Option<&DeferredPlan> {
    p.deferred.as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rg's `-i` mode, the one every fold plan is built under.
    fn ci() -> MatchMode {
        MatchMode {
            ignore_case: true,
            ..MatchMode::default()
        }
    }

    fn anchor_bytes(pattern: &str) -> Option<Vec<u8>> {
        regex_syntax::Parser::new()
            .parse(pattern)
            .ok()
            .and_then(|hir| required_literal(&hir))
    }

    #[test]
    fn required_literal_extraction() {
        // Pure literals anchor on themselves.
        assert_eq!(anchor_bytes("printk"), Some(b"printk".to_vec()));
        // Concatenation: longest child wins (ties go to the last max).
        assert_eq!(anchor_bytes("pre.*post"), Some(b"post".to_vec()));
        assert_eq!(anchor_bytes("\\d+_lock"), Some(b"_lock".to_vec()));
        assert_eq!(anchor_bytes("[ab]+c"), Some(b"c".to_vec()));
        // Captures and min>0 repetitions propagate (the anchor is the repeated
        // unit, not its expansion).
        assert_eq!(anchor_bytes("(kmalloc)"), Some(b"kmalloc".to_vec()));
        assert_eq!(anchor_bytes("a{2,3}b"), Some(b"b".to_vec()));
        // Alternation: longest common substring of branch literals.
        assert_eq!(anchor_bytes("printk|printf"), Some(b"print".to_vec()));
        // No byte common to all branches: no anchor (correct, just unpruned).
        assert_eq!(anchor_bytes("foo|bar"), None);
        assert_eq!(anchor_bytes("a|"), None);
        // Nullable / zero-width classes: no anchor.
        assert_eq!(anchor_bytes("a*"), None);
        assert_eq!(anchor_bytes("x?"), None);
        assert_eq!(anchor_bytes("[ab]+"), None);
        assert_eq!(anchor_bytes("a+|b+"), None);
    }

    #[test]
    fn longest_common_substring_caps_long_inputs() {
        assert_eq!(longest_common_substring(b"printk", b"printf"), b"print");
        assert_eq!(longest_common_substring(b"foo", b"bar"), b"");
        let long = vec![b'a'; ANCHOR_LEN_CAP + 1];
        assert!(longest_common_substring(&long, &long).is_empty());
    }

    #[test]
    fn prepare_rejects_empty_and_invalid_loudly() {
        for bad in ["", "(["] {
            match PreparedPattern::prepare(bad) {
                Err(e) => assert_eq!(e.pattern, bad, "must name the pattern"),
                Ok(_) => panic!("pattern {bad:?} must be rejected"),
            }
        }
        // NUL-containing anchors never prune.
        assert_eq!(anchor(&PreparedPattern::prepare("a\\x00b").unwrap()), None);
    }

    #[test]
    fn ci_anchor_comes_from_the_folded_hir() {
        // A case-sensitive anchor would prune away real `-i` matches. Only
        // the fold-invariant bytes may anchor.
        let prepared = PreparedPattern::prepare_with("foo_bar", ci()).unwrap();
        assert_eq!(anchor(&prepared), Some(b"_".as_slice()));
        assert_eq!(
            anchor(&PreparedPattern::prepare_with("printk", ci()).unwrap()),
            None
        );
        // The case-sensitive compile keeps the whole literal.
        assert_eq!(
            anchor(&PreparedPattern::prepare("printk").unwrap()),
            Some(b"printk".as_slice())
        );
    }

    #[test]
    fn ci_fold_plan_exists_only_for_concat_shapes() {
        assert!(
            PreparedPattern::prepare_with("HscEnv", ci())
                .unwrap()
                .fold()
                .is_some()
        );
        assert!(
            PreparedPattern::prepare_with("foo|bar", ci())
                .unwrap()
                .fold()
                .is_none()
        );
        assert!(
            PreparedPattern::prepare_with("a+b", ci())
                .unwrap()
                .fold()
                .is_none()
        );
        // A case-sensitive compile never plans one.
        assert!(PreparedPattern::prepare("HscEnv").unwrap().fold().is_none());
        // Empty and invalid stay loud on both paths.
        for bad in ["", "(["] {
            assert!(
                PreparedPattern::prepare_with(bad, ci()).is_err(),
                "pattern {bad:?}"
            );
        }
    }

    #[test]
    fn an_automaton_plan_replaces_only_a_short_anchor() {
        // Anchorless and short-anchor shapes plan a walk.
        for pattern in ["Int(32|64)#", "[A-Z][a-z]+Env", "(foo|bar)Baz", "\\d{4,}px"] {
            let prepared = PreparedPattern::prepare(pattern).unwrap();
            assert!(prepared.automaton().is_some(), "{pattern:?} must plan");
        }
        // A longer anchor defers its plan; a nullable pattern prunes nothing;
        // `\\d{4,}` ends on any of 74 bytes.
        for pattern in [
            "stg_[a-z]+_info",
            "printk",
            "GHC\\.[A-Z][a-z]+\\.Types",
            "a*",
            "\\d{4,}",
        ] {
            let prepared = PreparedPattern::prepare(pattern).unwrap();
            assert!(prepared.automaton().is_none(), "{pattern:?} must not plan");
        }
        // The fold walk is cheaper where it exists.
        let folded = PreparedPattern::prepare_with("HscEnv", ci()).unwrap();
        assert!(folded.fold().is_some() && folded.automaton().is_none());
        // Nothing narrows a nullable anchorless pattern.
        assert!(!narrows(&PreparedPattern::prepare("a*").unwrap()));
        assert!(narrows(
            &PreparedPattern::prepare("[A-Z][a-z]+Env").unwrap()
        ));
    }

    #[test]
    fn a_long_anchor_defers_its_plan() {
        for pattern in [
            "union[ \\t]+\\{",
            "typedef[ \\t]+struct",
            "import GHC\\.(Core|Stg|Cmm)\\.[\\w.]+",
        ] {
            let prepared = PreparedPattern::prepare(pattern).unwrap();
            assert!(prepared.automaton().is_none(), "{pattern:?}");
            let plan = deferred(&prepared).unwrap_or_else(|| panic!("{pattern:?} must defer"));
            assert!(!plan.built());
            let first = plan.plan().expect("plans") as *const RegexPlan;
            assert!(std::ptr::eq(first, plan.plan().unwrap()) && plan.built());
        }
        // Literals, patterns whose only runs holding the anchor are lone
        // literals, and a fold plan get none.
        // A run without the anchor would locate more than the anchor does.
        for pattern in [
            "printk",
            "module_platform_driver\\(",
            "stg_\\w+_info",
            "\\{-# INLINE \\w+ #-\\}",
            "import qualified \\w+ [a-z]",
            "instance Outputable \\w+ where[ \\t]",
            "import qualified \\w+\\.\\w+ as [A-Z]",
        ] {
            assert!(
                deferred(&PreparedPattern::prepare(pattern).unwrap()).is_none(),
                "{pattern:?}"
            );
        }
        let folded = PreparedPattern::prepare_with("x____y", ci()).unwrap();
        assert!(folded.fold().is_some() && anchor(&folded) == Some(b"____".as_slice()));
        assert!(deferred(&folded).is_none());
    }

    #[test]
    fn literal_only_for_plain_ascii_literals() {
        let lit = |p: &str, mode| {
            literal(&PreparedPattern::prepare_with(p, mode).unwrap()).map(<[u8]>::to_vec)
        };
        let plain = MatchMode::default();
        assert_eq!(
            lit("module_platform_driver\\(", plain),
            Some(b"module_platform_driver(".to_vec())
        );
        for p in [
            "aa",
            "abab",
            "caf\u{e9}",
            "a\nb",
            "a\x00b",
            "printk+",
            "(printk)",
            "^printk",
            "pr[i1]ntk",
        ] {
            assert_eq!(lit(p, plain), None, "{p:?}");
        }
        assert_eq!(lit("printk", ci()), None);
    }

    #[test]
    fn count_is_non_overlapping_while_occurrences_overlap() {
        let prepared = PreparedPattern::prepare("aa").unwrap();
        assert_eq!(prepared.count(b"aaaa"), 2);
        let n = |hay: &[u8], needle: &[u8]| {
            count_overlapping(hay, &memchr::memmem::Finder::new(needle))
        };
        assert_eq!(n(b"aaaa", b"aa"), 3);
        assert_eq!(n(b"aaaa", b"a"), 4);
        assert_eq!(n(b"", b"a"), 0);
        assert_eq!(n(b"a", b""), 0);
        assert_eq!(n(b"ab", b"abc"), 0);
        assert_eq!(n(b"printk\nprintkprintk\0a", b"kp"), 1);
        assert_eq!(n(b"aba\0a\0a", b"\0a"), 2);
    }
}

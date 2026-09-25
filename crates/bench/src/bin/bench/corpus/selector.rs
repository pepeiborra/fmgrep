//! `--corpus` grammar: `<base>[-N]`, base in {linux, chromium, ghc, all},
//! N >= 1 replicas. `all` is one corpus merged from every registered corpus,
//! not a per-corpus loop.

use bench::corpora::{CorpusSpec, discover_corpora};

/// Which registered corpora a selection is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusBase {
    Linux,
    Chromium,
    Ghc,
    /// Every registered corpus, merged into one.
    All,
}

impl CorpusBase {
    fn id(self) -> &'static str {
        match self {
            CorpusBase::Linux => "linux",
            CorpusBase::Chromium => "chromium",
            CorpusBase::Ghc => "ghc",
            CorpusBase::All => "all",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "linux" => Some(CorpusBase::Linux),
            "chromium" => Some(CorpusBase::Chromium),
            "ghc" => Some(CorpusBase::Ghc),
            "all" => Some(CorpusBase::All),
            _ => None,
        }
    }
}

/// A parsed `--corpus` value: `base` replicated `replicas` times.
/// `replicas == 1` is the bare base (`ghc-1` is `ghc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorpusSelector {
    pub base: CorpusBase,
    pub replicas: usize,
}

fn bad_value(raw: &str, why: &str) -> String {
    format!(
        "--corpus {raw:?} is not a valid selection ({why}): expected \
         <base>[-N] with base one of \"linux\", \"chromium\", \"ghc\", \
         \"all\" and N >= 1 replicas (default: \"ghc\"; \"all\" is ONE corpus \
         merged from every registered corpus; e.g. \"linux-5\", \"all-4\")"
    )
}

impl CorpusSelector {
    /// `None` (flag absent) is `ghc`. Anything unparseable is a loud error —
    /// a bad selection must never silently narrow or widen the run.
    pub fn resolve(raw: Option<&str>) -> Result<Self, String> {
        let Some(raw) = raw else {
            return Ok(Self {
                base: CorpusBase::Ghc,
                replicas: 1,
            });
        };
        let (base_str, replicas) = match raw.rsplit_once('-') {
            Some((prefix, suffix))
                if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) =>
            {
                let n: usize = suffix
                    .parse()
                    .map_err(|_| bad_value(raw, "replica count out of range"))?;
                (prefix, n)
            }
            _ => (raw, 1),
        };
        if replicas == 0 {
            return Err(bad_value(raw, "0 replicas"));
        }
        let base = CorpusBase::parse(base_str).ok_or_else(|| bad_value(raw, "unknown base"))?;
        Ok(Self { base, replicas })
    }

    /// Whether the flag was absent: marks the header line `(default)`.
    pub fn is_default(raw: Option<&str>) -> bool {
        raw.is_none()
    }

    /// Registered corpora this selection is made of, in registry order.
    pub fn members(self) -> Vec<CorpusSpec> {
        discover_corpora()
            .into_iter()
            .filter(|spec| match self.base {
                CorpusBase::All => true,
                other => spec.id == other.id(),
            })
            .collect()
    }

    /// Trace stamp and table label: `ghc`, `ghc-2`, `all`, `all-4`.
    pub fn corpus_id(self) -> String {
        if self.replicas == 1 {
            self.base.id().to_string()
        } else {
            format!("{}-{}", self.base.id(), self.replicas)
        }
    }

    /// Header spelling of the effective selection; never silent.
    pub fn header_label(self, default: bool) -> String {
        let members: Vec<&str> = self.members().iter().map(|s| s.id).collect();
        let mut label = self.corpus_id();
        match (self.base, self.replicas) {
            (CorpusBase::All, 1) => label.push_str(&format!(" ({})", members.join(", "))),
            (_, 1) => {}
            (_, n) => label.push_str(&format!(" ({n} replicas of {})", members.join(", "))),
        }
        if default {
            label.push_str(" (default)");
        }
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(raw: &str) -> CorpusSelector {
        CorpusSelector::resolve(Some(raw)).unwrap_or_else(|e| panic!("--corpus {raw:?}: {e}"))
    }

    #[test]
    fn selector_parses_base_and_replicas() {
        assert_eq!(
            sel("ghc"),
            CorpusSelector {
                base: CorpusBase::Ghc,
                replicas: 1
            }
        );
        assert_eq!(
            sel("linux-5"),
            CorpusSelector {
                base: CorpusBase::Linux,
                replicas: 5
            }
        );
        assert_eq!(
            sel("chromium-3"),
            CorpusSelector {
                base: CorpusBase::Chromium,
                replicas: 3
            }
        );
        assert_eq!(
            sel("all"),
            CorpusSelector {
                base: CorpusBase::All,
                replicas: 1
            }
        );
        assert_eq!(
            sel("all-4"),
            CorpusSelector {
                base: CorpusBase::All,
                replicas: 4
            }
        );
        let default = CorpusSelector::resolve(None).unwrap();
        assert_eq!(
            default,
            CorpusSelector {
                base: CorpusBase::Ghc,
                replicas: 1
            }
        );
        assert!(CorpusSelector::is_default(None));
        assert!(!CorpusSelector::is_default(Some("ghc")));
    }

    #[test]
    fn selector_replica_one_is_the_bare_base() {
        assert_eq!(sel("ghc-1"), sel("ghc"));
        assert_eq!(sel("ghc-1").corpus_id(), "ghc");
        assert_eq!(sel("all-1").corpus_id(), "all");
        assert_eq!(sel("ghc-2").corpus_id(), "ghc-2");
        assert_eq!(sel("all-4").corpus_id(), "all-4");
    }

    #[test]
    fn selector_rejects_bad_replicas() {
        for bogus in [
            "linux-0",
            "linux-x",
            "linux-",
            "-5",
            "all-0",
            "all-x",
            "",
            "GHC",
            "Linux",
            "gh",
            "chrome",
            "both",
            "none",
            "linux,chromium",
            "linux-2-3",
            "ghc-99999999999999999999999999",
        ] {
            let err = CorpusSelector::resolve(Some(bogus)).unwrap_err();
            assert!(err.contains("--corpus"), "bogus {bogus:?}: {err}");
            for expected in [
                "\"linux\"",
                "\"chromium\"",
                "\"ghc\"",
                "\"all\"",
                "<base>[-N]",
            ] {
                assert!(err.contains(expected), "bogus {bogus:?}: {err}");
            }
        }
    }

    #[test]
    fn selector_members_follow_the_registry() {
        let registry: Vec<&str> = bench::corpora::discover_corpora()
            .iter()
            .map(|s| s.id)
            .collect();
        let all: Vec<&str> = sel("all").members().iter().map(|s| s.id).collect();
        assert_eq!(all, registry, "all is every registered corpus, in order");
        assert_eq!(sel("all-4").members().len(), registry.len());
        for id in &registry {
            let single: Vec<&str> = sel(id).members().iter().map(|s| s.id).collect();
            assert_eq!(single, vec![*id]);
        }
    }

    #[test]
    fn selector_header_label_names_members_and_replicas() {
        assert_eq!(sel("ghc").header_label(true), "ghc (default)");
        assert_eq!(sel("ghc").header_label(false), "ghc");
        assert_eq!(sel("linux").header_label(false), "linux");
        let two = sel("ghc-2").header_label(false);
        assert!(two.starts_with("ghc-2"), "{two}");
        assert!(two.contains("2 replicas of ghc"), "{two}");
        let all = sel("all").header_label(false);
        let all4 = sel("all-4").header_label(false);
        assert!(all4.contains("4 replicas of"), "{all4}");
        for spec in bench::corpora::discover_corpora() {
            assert!(all.contains(spec.id), "label {all:?} misses {:?}", spec.id);
            assert!(
                all4.contains(spec.id),
                "label {all4:?} misses {:?}",
                spec.id
            );
        }
    }
}

//! Per-corpus benchmark query sets, read from `queries/<corpus>.json`.

use serde::Deserialize;

use crate::regex_pattern;

/// The queries one corpus is benchmarked with: `/.../` entries are regexes,
/// the rest literals, literals first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermSet {
    /// Header spelling of the set.
    pub name: String,
    /// The benchmark term list, in the spelling every row parses.
    pub queries: Vec<String>,
}

impl TermSet {
    pub fn literals(&self) -> impl Iterator<Item = &str> {
        self.queries
            .iter()
            .map(String::as_str)
            .filter(|q| regex_pattern(q).is_none())
    }

    /// Pattern bodies, without the slashes.
    pub fn regexes(&self) -> impl Iterator<Item = &str> {
        self.queries.iter().filter_map(|q| regex_pattern(q))
    }

    /// Query set of a merged corpus: every literal, then every regex, each
    /// deduped with the first occurrence winning, sets in the order given.
    pub fn union(sets: &[TermSet]) -> TermSet {
        let names: Vec<&str> = sets.iter().map(|s| s.name.as_str()).collect();
        let mut queries: Vec<String> = Vec::new();
        let literals = sets.iter().flat_map(|s| s.literals().map(str::to_string));
        let regexes = sets
            .iter()
            .flat_map(|s| s.regexes().map(|p| format!("/{p}/")));
        for q in literals.chain(regexes) {
            if !queries.contains(&q) {
                queries.push(q);
            }
        }
        TermSet {
            name: format!("union: {}", names.join(" + ")),
            queries,
        }
    }
}

/// Where a corpus's pinned checkout comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub repo_url: String,
    pub commit: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryFile {
    repo_url: String,
    commit: String,
    queries: Vec<String>,
}

/// A malformed in-tree file is a data bug, so it panics.
fn query_file(corpus_id: &str) -> Option<QueryFile> {
    let json = match corpus_id {
        "ghc" => include_str!("../queries/ghc.json"),
        "linux" => include_str!("../queries/linux.json"),
        "chromium" => include_str!("../queries/chromium.json"),
        _ => return None,
    };
    Some(
        serde_json::from_str(json)
            .unwrap_or_else(|e| panic!("queries/{corpus_id}.json is not a query file: {e}")),
    )
}

/// Query set of the corpus with registry id `corpus_id`; `None` for an
/// unknown id.
pub fn term_set(corpus_id: &str) -> Option<TermSet> {
    query_file(corpus_id).map(|f| TermSet {
        name: corpus_id.to_string(),
        queries: f.queries,
    })
}

/// The repository and commit the corpus's query set is pinned to.
pub fn source(corpus_id: &str) -> Option<Source> {
    query_file(corpus_id).map(|f| Source {
        repo_url: f.repo_url,
        commit: f.commit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const IDS: [&str; 3] = ["ghc", "linux", "chromium"];

    fn set(id: &str) -> TermSet {
        term_set(id).unwrap()
    }

    fn strings<'a>(it: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
        it.collect()
    }

    #[test]
    fn union_dedups_and_keeps_registry_order() {
        let a = TermSet {
            name: "a".into(),
            queries: vec![
                "printk".into(),
                "shared".into(),
                r"/a\w+/".into(),
                r"/both\d/".into(),
            ],
        };
        let b = TermSet {
            name: "b".into(),
            queries: vec![
                "shared".into(),
                "DCHECK".into(),
                r"/both\d/".into(),
                r"/b\w+/".into(),
            ],
        };
        let u = TermSet::union(&[a, b]);
        assert_eq!(
            u.queries,
            vec![
                "printk",
                "shared",
                "DCHECK",
                r"/a\w+/",
                r"/both\d/",
                r"/b\w+/"
            ]
        );
    }

    #[test]
    fn union_name_lists_the_members() {
        let u = TermSet::union(&[set("linux"), set("ghc")]);
        assert!(u.name.contains("linux"), "{}", u.name);
        assert!(u.name.contains("ghc"), "{}", u.name);
        assert!(u.name.starts_with("union:"), "{}", u.name);
    }

    #[test]
    fn query_files_are_the_benchmark_term_lists() {
        for id in IDS {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("queries")
                .join(format!("{id}.json"));
            let doc: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let queries: Vec<&str> = doc["queries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|q| q.as_str().unwrap())
                .collect();
            assert_eq!(set(id).queries, queries, "{id}");
            let url = doc["repo_url"].as_str().unwrap();
            assert!(
                url.starts_with("https://") && url.ends_with(".git"),
                "{id}: {url}"
            );
            let commit = doc["commit"].as_str().unwrap();
            assert!(
                commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()),
                "{id}: {commit}"
            );
            assert_eq!(source(id).unwrap().commit, commit);
        }
    }

    #[test]
    fn query_sets_have_pinned_literal_and_regex_counts() {
        for (id, literals, regexes) in [("ghc", 30, 5), ("linux", 92, 10), ("chromium", 30, 5)] {
            let s = set(id);
            assert_eq!(s.literals().count(), literals, "{id}");
            assert_eq!(s.regexes().count(), regexes, "{id}");
            // Literals first: the term order every trace and table follows.
            assert!(
                s.queries[..literals]
                    .iter()
                    .all(|q| regex_pattern(q).is_none()),
                "{id}"
            );
        }
        assert_eq!(
            strings(set("linux").regexes()),
            vec![
                r"typedef[ \t]+struct",
                r"union[ \t]+\{",
                r"__attribute__[ \t]*\(\(",
                r"module_platform_driver\(",
                r"MODULE_DEVICE_TABLE\(of,",
                r"DEFINE_SPINLOCK\(",
                r"DECLARE_WAIT_QUEUE_HEAD\(",
                r"subsys_initcall\(",
                r"WARN_ONCE\(",
                r"EXPORT_SYMBOL_GPL\(devm_\w+\)",
            ]
        );
        assert!(set("chromium").literals().any(|q| q == "LOG(ERROR)"));
    }

    #[test]
    fn every_query_set_is_distinct() {
        for id in IDS {
            let s = set(id);
            let mut sorted = s.queries.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), s.queries.len(), "{id}: duplicate query");
            assert!(s.queries.iter().all(|q| !q.trim().is_empty()), "{id}");
        }
        // The default set survives a `--terms` round trip, which splits on
        // commas.
        let ghc = set("ghc");
        assert!(ghc.queries.iter().all(|q| !q.contains(',')));
        assert!(ghc.literals().filter(|q| q.contains(' ')).count() >= 5);
    }

    #[test]
    fn unknown_corpus_has_no_query_set() {
        assert_eq!(term_set("gecko"), None);
        assert_eq!(source("gecko"), None);
    }

    #[test]
    fn source_names_the_pinned_clone() {
        assert_eq!(
            source("ghc"),
            Some(Source {
                repo_url: "https://github.com/ghc/ghc.git".into(),
                commit: "4cf7628aa66f56000c29aef93924b6b8515db8f7".into(),
            })
        );
    }

    /// Whether a match of `hir` can contain a `\n` byte.
    fn can_match_newline(hir: &regex_syntax::hir::Hir) -> bool {
        use regex_syntax::hir::{Class, HirKind};
        match hir.kind() {
            HirKind::Empty | HirKind::Look(_) => false,
            HirKind::Literal(lit) => lit.0.contains(&b'\n'),
            HirKind::Class(Class::Unicode(c)) => c
                .ranges()
                .iter()
                .any(|r| r.start() <= '\n' && '\n' <= r.end()),
            HirKind::Class(Class::Bytes(c)) => c
                .ranges()
                .iter()
                .any(|r| r.start() <= b'\n' && b'\n' <= r.end()),
            HirKind::Capture(c) => can_match_newline(&c.sub),
            HirKind::Repetition(r) => can_match_newline(&r.sub),
            HirKind::Concat(v) | HirKind::Alternation(v) => v.iter().any(can_match_newline),
        }
    }

    #[test]
    fn line_safety_check_rejects_newline_capable_patterns() {
        for p in [
            r"a\s+b",
            r"a[^;]*b",
            r"a\p{Any}",
            r"(?s)a.b",
            r"a\nb",
            r"a[\x00-\x7f]",
        ] {
            assert!(can_match_newline(&regex_syntax::parse(p).unwrap()), "{p}");
        }
        for p in [
            r"a[ \t]+b",
            r"a[^;\n]*b",
            r"a.b",
            r"^a\w+$",
            r"\bstg_\w+_info",
        ] {
            assert!(!can_match_newline(&regex_syntax::parse(p).unwrap()), "{p}");
        }
    }

    #[test]
    fn no_query_can_match_across_a_newline() {
        // The harness searches whole documents; a pattern spanning `\n`
        // would count differently from a line-oriented rg.
        for id in IDS {
            let s = set(id);
            for p in s.regexes() {
                let hir = regex_syntax::parse(p).unwrap();
                assert!(
                    !can_match_newline(&hir),
                    "{id}: pattern {p:?} can match a newline"
                );
            }
            for l in s.literals() {
                assert!(!l.contains('\n'), "{id}: literal {l:?} holds a newline");
            }
        }
    }

    #[test]
    fn every_regex_set_compiles_and_prefilters() {
        // Each pattern compiles and has a non-empty required literal, so the
        // FM prefilter has something to search for.
        use regex_syntax::hir::literal::Extractor;
        for id in IDS {
            for p in set(id).regexes() {
                regex::bytes::Regex::new(p)
                    .unwrap_or_else(|e| panic!("{id} pattern {p:?} must compile: {e}"));
                let hir = regex_syntax::parse(p)
                    .unwrap_or_else(|e| panic!("{id} pattern {p:?} must parse: {e}"));
                let seq = Extractor::new().extract(&hir);
                assert!(
                    matches!(seq.min_literal_len(), Some(n) if n > 0),
                    "{id} pattern {p:?} has no non-empty required literal"
                );
            }
        }
    }
}

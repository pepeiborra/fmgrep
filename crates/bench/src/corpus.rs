//! The corpora, their pinned query sets and their checkouts.

use std::path::{Path, PathBuf};

use crate::report;

pub const CORPORA: [&str; 4] = ["ghc", "linux", "chromium", "aosp"];

pub struct QuerySet {
    pub repo_url: String,
    pub commit: String,
    pub queries: Vec<String>,
}

pub fn query_set(corpus: &str) -> QuerySet {
    let json = match corpus {
        "ghc" => include_str!("../queries/ghc.json"),
        "linux" => include_str!("../queries/linux.json"),
        "aosp" => include_str!("../queries/aosp.json"),
        _ => include_str!("../queries/chromium.json"),
    };
    let v: serde_json::Value = serde_json::from_str(json).expect("query file is JSON");
    let text = |v: &serde_json::Value| v.as_str().expect("a string").to_string();
    let queries = v["queries"].as_array().expect("a query list");
    QuerySet {
        repo_url: text(&v["repo_url"]),
        commit: text(&v["commit"]),
        queries: queries.iter().map(text).collect(),
    }
}

pub fn corpus_root(id: &str) -> Result<PathBuf, String> {
    let var = format!("FMGREP_CORPUS_{}", id.to_uppercase());
    let root = match std::env::var_os(&var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/bench")).join(id),
    };
    if root.exists() {
        return Ok(root);
    }
    let at = root.display();
    Err(if id == "aosp" {
        format!(
            "no aosp corpus at {at}: AOSP is not one git clone; sync it (docs/benchmark.md#aosp), \
             then run scripts/bench.sh --corpus aosp --repo-path DIR, or set {var}=DIR"
        )
    } else {
        format!(
            "no {id} corpus at {at}: run scripts/bench.sh --corpus {id} [--repo-path DIR] \
             to clone the pinned commit, or set {var}=DIR"
        )
    })
}

/// The checkout's revision: a repo-tool tree's manifest branch or tag, else
/// HEAD of the git tree rooted at `root`.
pub fn revision(root: &Path) -> Option<String> {
    let git = |dir: &Path, args: &[&str]| {
        let dir = dir.to_str()?;
        report::run("git", &[&["-C", dir], args].concat())
    };
    let manifests = root.join(".repo/manifests");
    if manifests.is_dir() {
        let merge = git(&manifests, &["config", "branch.default.merge"])?;
        let name = merge.strip_prefix("refs/tags/");
        return Some(
            name.or(merge.strip_prefix("refs/heads/"))
                .unwrap_or(&merge)
                .to_string(),
        );
    }
    let top = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    (top.canonicalize().ok()? == root.canonicalize().ok()?)
        .then(|| git(root, &["rev-parse", "HEAD"]))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, leaves};
    use fmgrep::regex_pattern;
    use std::process::Command;

    fn regexes(set: &QuerySet) -> impl Iterator<Item = &str> {
        set.queries
            .iter()
            .flat_map(|q| leaves(q))
            .filter_map(regex_pattern)
    }

    #[test]
    fn query_files_pin_a_commit_and_list_literals_first() {
        let counts = [
            ("ghc", 30, 5, 5),
            ("linux", 92, 13, 8),
            ("chromium", 30, 8, 8),
            ("aosp", 30, 8, 8),
        ];
        let release_tag = |c: &str| {
            let rest = c.strip_prefix("android-").unwrap_or("");
            let (version, r) = rest.split_once("_r").unwrap_or(("", ""));
            let numbers = |s: &str, n| {
                let parts: Vec<&str> = s.split('.').collect();
                let digits = |p: &&str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
                parts.len() == n && parts.iter().all(digits)
            };
            numbers(version, 3) && numbers(r, 1)
        };
        for (id, literals, regexes, intersections) in counts {
            let QuerySet {
                repo_url,
                commit,
                queries,
            } = query_set(id);
            assert!(repo_url.starts_with("https://") && repo_url.ends_with(".git"));
            let sha = commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit());
            assert!(
                sha || (id == "aosp" && release_tag(&commit)),
                "{id} {commit}"
            );
            let kinds: Vec<Kind> = queries.iter().map(|q| Kind::of(q)).collect();
            let want = [
                vec![Kind::Literal; literals],
                vec![Kind::Regex; regexes],
                vec![Kind::Intersection; intersections],
            ];
            assert_eq!(kinds, want.concat(), "{id}");
            let mut sorted = queries.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), queries.len(), "{id}: duplicate query");
            assert!(queries.iter().all(|q| !q.trim().is_empty()), "{id}");
        }
        let ghc = query_set("ghc").commit;
        assert_eq!(ghc, "4cf7628aa66f56000c29aef93924b6b8515db8f7");
        assert_eq!(query_set("aosp").commit, "android-17.0.0_r1");
    }

    /// The rg, tgrep and elgrep rows' last stage reads literal leaves as regexes.
    #[test]
    fn intersection_literals_need_no_escaping() {
        for id in CORPORA {
            let set = query_set(id);
            let ands = set
                .queries
                .iter()
                .filter(|q| Kind::of(q) == Kind::Intersection);
            for q in ands {
                assert_eq!(leaves(q).len(), 2, "{id} {q:?}");
                for l in leaves(q).into_iter().filter(|l| regex_pattern(l).is_none()) {
                    let plain = !l.contains(char::is_whitespace) && regex::escape(l) == l;
                    assert!(plain, "{id} {q:?}");
                }
            }
        }
    }

    #[test]
    fn every_regex_set_compiles_and_prefilters() {
        use regex_syntax::hir::literal::Extractor;
        for id in CORPORA {
            let set = query_set(id);
            for p in regexes(&set) {
                regex::bytes::Regex::new(p).unwrap_or_else(|e| panic!("{id} {p:?}: {e}"));
                let seq = Extractor::new().extract(&regex_syntax::parse(p).unwrap());
                assert!(
                    matches!(seq.min_literal_len(), Some(n) if n > 0),
                    "{id} {p:?}"
                );
            }
        }
    }

    #[test]
    fn revision_reads_repo_trees_and_ignores_enclosing_git() {
        let git = |dir: &Path, args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .args(args)
                .output();
            matches!(out, Ok(o) if o.status.success())
        };
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer");
        let (plain, repo) = (outer.join("plain"), outer.join("repo"));
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(repo.join(".repo/manifests")).unwrap();
        if !git(&outer, &["init", "-q"]) {
            eprintln!("skipped: git is not on PATH");
            return;
        }
        assert!(git(&outer, &["commit", "-q", "--allow-empty", "-m", "x"]));
        let head = report::run("git", &["-C", outer.to_str().unwrap(), "rev-parse", "HEAD"]);
        assert_eq!(revision(&outer), head);
        assert_eq!(revision(&plain), None);
        let manifests = repo.join(".repo/manifests");
        assert!(git(&manifests, &["init", "-q"]));
        let tag = "refs/heads/android-17.0.0_r1";
        assert!(git(&manifests, &["config", "branch.default.merge", tag]));
        assert_eq!(revision(&repo).as_deref(), Some("android-17.0.0_r1"));
        let tag = "refs/tags/android-17.0.0_r1";
        assert!(git(&manifests, &["config", "branch.default.merge", tag]));
        assert_eq!(revision(&repo).as_deref(), Some("android-17.0.0_r1"));
    }
}

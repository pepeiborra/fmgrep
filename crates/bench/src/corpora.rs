//! The benchmark corpora: [`discover_corpora`] is the registry (Linux
//! kernel, Chromium, GHC). The indexable-doc set under a root is
//! [`fmgrep::discover_index_docs`], shared with `fmgrep index`.

use std::env;
use std::path::{Path, PathBuf};

/// Registry ids in run order, with the env var overriding each root.
const CORPORA: [(&str, &str); 3] = [
    ("linux", "FMGREP_CORPUS_LINUX"),
    ("chromium", "FMGREP_CORPUS_CHROMIUM"),
    ("ghc", "FMGREP_CORPUS_GHC"),
];

/// One benchmark corpus: stable id (trace stamps, table labels, `--verify`
/// attribution) and on-disk root, which may not exist.
#[derive(Debug, Clone)]
pub struct CorpusSpec {
    pub id: &'static str,
    pub root: PathBuf,
    /// The root came from `FMGREP_CORPUS_<ID>`, so a missing one is an error
    /// rather than a fixture run.
    pub explicit: bool,
}

/// The registry in run order. A root is `$FMGREP_CORPUS_<ID>` when set and
/// non-empty, else `<workspace>/target/bench/<id>`, where `scripts/bench.sh`
/// clones.
pub fn discover_corpora() -> Vec<CorpusSpec> {
    CORPORA
        .iter()
        .map(|&(id, var)| match env::var(var) {
            Ok(val) if !val.is_empty() => CorpusSpec {
                id,
                root: PathBuf::from(val),
                explicit: true,
            },
            _ => CorpusSpec {
                id,
                root: default_root(id),
                explicit: false,
            },
        })
        .collect()
}

/// The env var overriding the root of corpus `id`.
pub fn corpus_env_var(id: &str) -> Option<&'static str> {
    CORPORA.iter().find(|(i, _)| *i == id).map(|(_, var)| *var)
}

fn default_root(id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/bench")
        .join(id)
}

/// `HEAD` of the checkout at `root`; `None` when it is not a git checkout
/// or `git` fails.
pub fn corpus_git_revision(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?;
    let sha = sha.trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_corpora_lists_linux_then_chromium_then_ghc() {
        let specs = discover_corpora();
        let ids: Vec<_> = specs.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["linux", "chromium", "ghc"]);
        assert!(specs[0].root.ends_with("linux"));
        assert!(specs[1].root.ends_with("chromium"));
        assert!(specs[2].root.ends_with("ghc"));
    }

    #[test]
    fn default_roots_live_under_target_bench() {
        for (id, _) in CORPORA {
            let root = default_root(id);
            assert!(
                root.ends_with(Path::new("target/bench").join(id)),
                "{root:?}"
            );
        }
        assert_eq!(corpus_env_var("ghc"), Some("FMGREP_CORPUS_GHC"));
        assert_eq!(corpus_env_var("linux"), Some("FMGREP_CORPUS_LINUX"));
        assert_eq!(corpus_env_var("gecko"), None);
    }

    #[test]
    fn git_revision_none_for_non_checkout() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(corpus_git_revision(dir.path()), None);
    }

    #[test]
    fn git_revision_reads_head_of_checkout() {
        // Positive provenance path via a throwaway repo (skipped where no
        // git binary exists so the suite stays green on minimal toolchains).
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .expect("git run");
            assert!(st.success(), "git {args:?} failed");
        };
        git(&["init"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ]);
        let expect = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(expect.status.success());
        let expect = String::from_utf8(expect.stdout).unwrap();
        assert_eq!(
            corpus_git_revision(dir.path()),
            Some(expect.trim().to_string())
        );
    }
}

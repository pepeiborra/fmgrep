//! Corpus composition: a [`CorpusSelector`] becomes one [`bench::Corpus`].
//! Merged doc paths are prefixed `<member-id>/`, replicas `r<k>/`; the root is
//! a temp dir of symlinks to the member roots. Doc ids are replica-major, so
//! replica 0 is a prefix of the doc list. One member at one replica keeps its
//! own root and paths.

use std::path::{Path, PathBuf};

use bench::corpora::{CorpusSpec, corpus_env_var, corpus_git_revision};
use bench::{Corpus, DocMeta, discover_index_meta};

use crate::corpus::fixtures::{
    ensure_chromium_fixture, ensure_ghc_fixture, ensure_synthetic_fixture,
};
use crate::corpus::selector::{CorpusBase, CorpusSelector};

/// One member of a composed corpus, as the header lines and the trace
/// provenance report it.
pub struct CorpusMember {
    pub id: &'static str,
    pub root: PathBuf,
    pub revision: Option<String>,
    /// True when the docs are a fixture/synthetic stand-in, not a checkout.
    pub fixture: bool,
    pub num_docs: usize,
    pub num_bytes: u64,
}

/// The corpus of one bench run: members merged, replicated `replicas` times.
pub struct LoadedCorpus {
    pub root: PathBuf,
    pub docs: Vec<DocMeta>,
    pub members: Vec<CorpusMember>,
    pub replicas: usize,
    /// True when any member is a fixture/synthetic stand-in.
    pub fixture: bool,
    /// Docs in one replica; `docs[..base_docs]` is replica 0.
    base_docs: usize,
    /// Symlink tree keeping the composed root alive; `None` when the root is
    /// the member's own.
    _links: Option<tempfile::TempDir>,
}

impl LoadedCorpus {
    /// The whole corpus: every member, every replica.
    pub fn corpus(&self) -> Corpus<'_> {
        Corpus {
            root: &self.root,
            docs: &self.docs,
        }
    }

    /// Replica 0 alone: what the staging copies and the pre-flight sizes.
    pub fn base_view(&self) -> Corpus<'_> {
        Corpus {
            root: &self.root,
            docs: &self.docs[..self.base_docs],
        }
    }

    /// Comma-joined member ids, for the trace stamp; `None` for one member.
    pub fn member_ids(&self) -> Option<String> {
        if self.members.len() < 2 {
            return None;
        }
        Some(
            self.members
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
                .join(","),
        )
    }

    /// `corpus revision:` value: one SHA for one member, `id=<sha>` pairs for
    /// a merge. `None` when no member has one.
    pub fn revision(&self) -> Option<String> {
        if let [only] = &self.members[..] {
            return only.revision.clone();
        }
        let pairs: Vec<String> = self
            .members
            .iter()
            .filter_map(|m| m.revision.as_ref().map(|sha| format!("{}={sha}", m.id)))
            .collect();
        (!pairs.is_empty()).then(|| pairs.join(","))
    }
}

/// A root set through `FMGREP_CORPUS_<ID>` must exist: a typo must not turn
/// into fixture numbers.
fn check_explicit(spec: &CorpusSpec) -> Result<(), String> {
    if spec.explicit && !spec.root.exists() {
        return Err(format!(
            "{}={} does not exist",
            corpus_env_var(spec.id).unwrap_or("the corpus override"),
            spec.root.display()
        ));
    }
    Ok(())
}

/// Resolve one registered corpus to a readable root: the checkout, else the
/// corpus's tiny fixture. Both warn loudly about what the numbers are.
fn member_root(spec: &CorpusSpec) -> (PathBuf, bool, Option<String>) {
    if spec.root.exists() {
        let revision = corpus_git_revision(&spec.root);
        return (spec.root.clone(), false, revision);
    }
    let hint = format!(
        "run scripts/bench.sh --corpus {} or set {}",
        spec.id,
        corpus_env_var(spec.id).unwrap_or("its FMGREP_CORPUS_<ID>")
    );
    let (fixture, display) = match spec.id {
        "chromium" => (ensure_chromium_fixture(), "Chromium"),
        "ghc" => (ensure_ghc_fixture(), "GHC"),
        _ => {
            eprintln!(
                "warning: {} corpus dir {} missing ({hint}); using synthetic docs.",
                spec.id,
                spec.root.display(),
            );
            return (spec.root.clone(), false, None);
        }
    };
    eprintln!(
        "warning: {} corpus dir {} missing ({hint}). Using tiny FIXTURE corpus at {} — NOT real {} sources; its numbers must not be compared with corpus numbers.",
        spec.id,
        spec.root.display(),
        fixture.display(),
        display
    );
    (fixture, true, None)
}

/// Descriptors of one member, with the synthetic last resort for a root that
/// yields none.
fn member_docs(id: &str, root: &Path) -> (PathBuf, bool, Vec<DocMeta>) {
    let docs = if root.exists() {
        discover_index_meta(root).unwrap_or_else(|e| {
            eprintln!("warning: {e}");
            Vec::new()
        })
    } else {
        Vec::new()
    };
    if !docs.is_empty() {
        return (root.to_path_buf(), false, docs);
    }
    eprintln!(
        "warning: no docs loaded for corpus '{id}'; falling back to synthetic docs for validation."
    );
    match ensure_synthetic_fixture().and_then(|dir| Ok((discover_index_meta(&dir)?, dir))) {
        Ok((docs, dir)) => (dir, true, docs),
        // A read-only TMPDIR must not abort the run.
        Err(e) => {
            eprintln!("warning: cannot write the synthetic fallback corpus: {e}");
            (root.to_path_buf(), true, Vec::new())
        }
    }
}

/// Load the corpus `sel` selects: walk every member once, then merge and
/// replicate. `all` drops members that are not checked out (or yield no
/// docs) instead of substituting fixtures; single bases fall back to one.
pub fn load(sel: CorpusSelector) -> Result<LoadedCorpus, String> {
    let mut members = Vec::new();
    let mut per_member = Vec::new();
    let mut skipped = Vec::new();
    for spec in sel.members() {
        check_explicit(&spec)?;
        if sel.base == CorpusBase::All {
            let docs = if spec.root.exists() {
                discover_index_meta(&spec.root).unwrap_or_else(|e| {
                    eprintln!("warning: {e}");
                    Vec::new()
                })
            } else {
                Vec::new()
            };
            if docs.is_empty() {
                skipped.push(format!("{} ({})", spec.id, spec.root.display()));
                continue;
            }
            members.push(CorpusMember {
                id: spec.id,
                revision: corpus_git_revision(&spec.root),
                root: spec.root,
                fixture: false,
                num_docs: docs.len(),
                num_bytes: docs.iter().map(|d| d.len).sum(),
            });
            per_member.push(docs);
            continue;
        }
        let (root, fixture, revision) = member_root(&spec);
        let (root, synthetic, docs) = member_docs(spec.id, &root);
        members.push(CorpusMember {
            id: spec.id,
            root,
            revision,
            fixture: fixture || synthetic,
            num_docs: docs.len(),
            num_bytes: docs.iter().map(|d| d.len).sum(),
        });
        per_member.push(docs);
    }
    if !skipped.is_empty() {
        eprintln!(
            "warning: --corpus all skips members with no checked-out docs: {}",
            skipped.join(", ")
        );
    }
    if members.is_empty() {
        return Err(
            "--corpus all: no registered corpus is checked out (run scripts/bench.sh or set FMGREP_CORPUS_<ID>)"
                .to_string(),
        );
    }
    compose(members, per_member, sel.replicas)
}

fn prefixed(dir: &str, path: &str) -> String {
    if dir.is_empty() {
        path.to_string()
    } else {
        format!("{dir}/{path}")
    }
}

/// Merge `per_member` docs under one root and replicate them `replicas`
/// times. One member at one replica keeps the member root; anything else
/// gets a symlink root.
fn compose(
    members: Vec<CorpusMember>,
    per_member: Vec<Vec<DocMeta>>,
    replicas: usize,
) -> Result<LoadedCorpus, String> {
    let fixture = members.iter().any(|m| m.fixture);
    if let ([only], [docs], 1) = (&members[..], &per_member[..], replicas) {
        let root = only.root.clone();
        return Ok(LoadedCorpus {
            root,
            base_docs: docs.len(),
            docs: docs.clone(),
            members,
            replicas,
            fixture,
            _links: None,
        });
    }
    let links = tempfile::tempdir()
        .map_err(|e| format!("failed to create the composed corpus root: {e}"))?;
    let named_members = members.len() > 1;
    let mut docs = Vec::new();
    let mut base_docs = 0;
    for r in 0..replicas {
        let replica_dir = if replicas > 1 {
            format!("r{r}")
        } else {
            String::new()
        };
        for (member, member_docs) in members.iter().zip(&per_member) {
            let dir = if named_members {
                prefixed(&replica_dir, member.id)
            } else {
                replica_dir.clone()
            };
            link_member(links.path(), &dir, &member.root)?;
            docs.extend(member_docs.iter().map(|d| DocMeta {
                path: prefixed(&dir, &d.path),
                len: d.len,
            }));
        }
        if r == 0 {
            base_docs = docs.len();
        }
    }
    Ok(LoadedCorpus {
        root: links.path().to_path_buf(),
        docs,
        members,
        replicas,
        fixture,
        base_docs,
        _links: Some(links),
    })
}

/// Symlink `<links>/<dir>` at the member root, creating `<dir>`'s parent.
fn link_member(links: &Path, dir: &str, root: &Path) -> Result<(), String> {
    let link = links.join(dir);
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    std::os::unix::fs::symlink(root, &link).map_err(|e| {
        format!(
            "failed to link {} -> {}: {e}",
            link.display(),
            root.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A member corpus on disk with `docs` written into it.
    fn member(
        id: &'static str,
        docs: &[(&str, &[u8])],
    ) -> (tempfile::TempDir, CorpusMember, Vec<DocMeta>) {
        let dir = tempfile::tempdir().unwrap();
        for (rel, bytes) in docs {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let metas = discover_index_meta(dir.path()).unwrap();
        let meta = CorpusMember {
            id,
            root: dir.path().to_path_buf(),
            revision: None,
            fixture: false,
            num_docs: metas.len(),
            num_bytes: metas.iter().map(|d| d.len).sum(),
        };
        (dir, meta, metas)
    }

    /// Two members: 2 docs / 8 bytes and 1 doc / 5 bytes.
    fn two_members() -> (Vec<tempfile::TempDir>, Vec<CorpusMember>, Vec<Vec<DocMeta>>) {
        let (d0, m0, docs0) = member("linux", &[("kernel/sched.c", b"printk"), ("a.c", b"hi")]);
        let (d1, m1, docs1) = member("ghc", &[("compiler/Main.hs", b"panic")]);
        (vec![d0, d1], vec![m0, m1], vec![docs0, docs1])
    }

    fn paths(loaded: &LoadedCorpus) -> Vec<&str> {
        loaded.docs.iter().map(|d| d.path.as_str()).collect()
    }

    #[test]
    fn explicit_missing_root_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let spec = |root: PathBuf, explicit| CorpusSpec {
            id: "ghc",
            root,
            explicit,
        };
        let missing = dir.path().join("nope");
        let err = check_explicit(&spec(missing.clone(), true)).unwrap_err();
        assert!(err.contains("FMGREP_CORPUS_GHC"), "{err}");
        assert!(err.contains("nope"), "{err}");
        // A missing default root falls back to the fixture instead.
        assert!(check_explicit(&spec(missing, false)).is_ok());
        assert!(check_explicit(&spec(dir.path().to_path_buf(), true)).is_ok());
    }

    #[test]
    fn loaded_docs_are_the_shared_indexable_set() {
        // No extension whitelist and no size cap: the harness describes
        // exactly what an engine `index <root>` run indexes, binaries and
        // hidden entries excluded.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.c"), "printk\n").unwrap();
        std::fs::write(dir.path().join("tool.py"), "printk\n").unwrap();
        std::fs::write(dir.path().join("big.txt"), vec![b'x'; 2 * 1024 * 1024]).unwrap();
        std::fs::write(dir.path().join("blob.bin"), b"a\x00b").unwrap();
        std::fs::write(dir.path().join(".hidden.c"), "printk\n").unwrap();

        let (root, synthetic, docs) = member_docs("ghc", dir.path());
        assert_eq!(root, dir.path());
        assert!(!synthetic);
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["a.c", "big.txt", "tool.py"]);
        // The over-1-MiB doc is described whole, not truncated or dropped.
        assert_eq!(docs[1].len, 2 * 1024 * 1024);
        assert_eq!(
            docs.iter().map(|d| d.len).sum::<u64>(),
            2 * 1024 * 1024 + 14
        );
    }

    #[test]
    fn merged_paths_are_member_prefixed_and_unique() {
        let (_dirs, members, docs) = two_members();
        let loaded = compose(members, docs, 1).unwrap();
        assert_eq!(
            paths(&loaded),
            vec!["linux/a.c", "linux/kernel/sched.c", "ghc/compiler/Main.hs"]
        );
        let unique: HashSet<&str> = paths(&loaded).into_iter().collect();
        assert_eq!(unique.len(), loaded.docs.len());
    }

    #[test]
    fn merged_docs_are_member_major_in_registry_order() {
        let (_dirs, members, docs) = two_members();
        let loaded = compose(members, docs, 1).unwrap();
        assert!(paths(&loaded)[..2].iter().all(|p| p.starts_with("linux/")));
        assert!(paths(&loaded)[2].starts_with("ghc/"));
        assert_eq!(loaded.base_view().num_docs(), loaded.corpus().num_docs());
    }

    #[test]
    fn merged_totals_are_the_sum_of_members() {
        let (_dirs, members, docs) = two_members();
        let expected: u64 = members.iter().map(|m| m.num_bytes).sum();
        let count: usize = members.iter().map(|m| m.num_docs).sum();
        let loaded = compose(members, docs, 1).unwrap();
        assert_eq!(loaded.corpus().total_bytes(), expected);
        assert_eq!(loaded.corpus().num_docs(), count);
    }

    #[test]
    fn replica_paths_are_r_prefixed_and_unique() {
        let (_dirs, members, docs) = two_members();
        let loaded = compose(members, docs, 3).unwrap();
        assert!(
            paths(&loaded)
                .iter()
                .all(|p| p.starts_with("r0/") || p.starts_with("r1/") || p.starts_with("r2/"))
        );
        let unique: HashSet<&str> = paths(&loaded).into_iter().collect();
        assert_eq!(unique.len(), loaded.docs.len());
    }

    #[test]
    fn replica_docs_are_exactly_n_times_the_base() {
        let (_dirs, members, docs) = two_members();
        let single = compose(members, docs, 1).unwrap();
        let (_dirs, members, docs) = two_members();
        let triple = compose(members, docs, 3).unwrap();
        assert_eq!(triple.corpus().num_docs(), 3 * single.corpus().num_docs());
        assert_eq!(
            triple.corpus().total_bytes(),
            3 * single.corpus().total_bytes()
        );
        assert_eq!(triple.base_view().num_docs(), single.corpus().num_docs());
        assert_eq!(
            triple.base_view().total_bytes(),
            single.corpus().total_bytes()
        );
        // Replica 0 leads the list, so the staging replicates a prefix.
        assert!(
            paths(&triple)[..single.docs.len()]
                .iter()
                .all(|p| p.starts_with("r0/"))
        );
    }

    #[test]
    fn single_member_single_replica_keeps_the_bare_root() {
        let (dir, meta, docs) = member("ghc", &[("a.c", b"printk")]);
        let loaded = compose(vec![meta], vec![docs], 1).unwrap();
        assert_eq!(loaded.root, dir.path());
        assert_eq!(paths(&loaded), vec!["a.c"]);
        assert!(loaded.member_ids().is_none());
        // One member, several replicas: r-prefixed under a link root.
        let (_dir2, meta, docs) = member("ghc", &[("a.c", b"printk")]);
        let two = compose(vec![meta], vec![docs], 2).unwrap();
        assert_eq!(paths(&two), vec!["r0/a.c", "r1/a.c"]);
        assert_ne!(two.root, dir.path());
    }

    #[test]
    fn link_root_resolves_every_doc_path() {
        for replicas in [1, 2] {
            let (_dirs, members, docs) = two_members();
            let loaded = compose(members, docs, replicas).unwrap();
            for id in 0..loaded.corpus().num_docs() {
                let path = loaded.corpus().doc_path(id).unwrap();
                let bytes =
                    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                assert_eq!(bytes.len() as u64, loaded.docs[id].len);
            }
        }
    }
}

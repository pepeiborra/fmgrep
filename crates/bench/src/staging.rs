//! Read-only staging of a corpus as one flat temp dir of `{id:08}.<ext>`
//! entries, for consumers that must be handed a real tree.

use std::path::Path;

use crate::Corpus;

/// A corpus staged into a fresh temp dir (deleted on drop), one entry per
/// document named by its positional id. Entries are copies, not hardlinks,
/// and no document is held in memory.
pub struct StagedDocs {
    dir: tempfile::TempDir,
    num_docs: usize,
}

impl StagedDocs {
    /// Stage every document of `corpus` as `{id:08}.{ext}`.
    pub fn stage(corpus: Corpus<'_>, ext: &str) -> Result<Self, String> {
        Self::stage_replicated(corpus, ext, 1)
    }

    /// Stage `replicas` copies of `corpus`: replica 0 is copied, replica
    /// `r > 0` hard-links `{r * base + i:08}.<ext>` to replica 0's entry, so
    /// the disk cost is one copy of the corpus whatever `replicas` is. Ids
    /// are replica-major, matching a replicated corpus's doc order. Reading
    /// a linked entry pays the APFS indirection noted above.
    pub fn stage_replicated(
        corpus: Corpus<'_>,
        ext: &str,
        replicas: usize,
    ) -> Result<Self, String> {
        if replicas == 0 {
            return Err("cannot stage 0 replicas of a corpus".to_string());
        }
        let dir =
            tempfile::tempdir().map_err(|e| format!("failed to create doc staging dir: {e}"))?;
        let base = corpus.num_docs();
        for id in 0..base {
            let src = corpus
                .doc_path(id)
                .ok_or_else(|| format!("corpus has no document {id}"))?;
            let dst = dir.path().join(Self::file_name(id, ext));
            // `copy` follows symlinked documents, so every entry is a real
            // file rather than a dangling relative link.
            std::fs::copy(&src, &dst)
                .map_err(|e| format!("failed to stage {}: {e}", dst.display()))?;
            for r in 1..replicas {
                let link = dir.path().join(Self::file_name(r * base + id, ext));
                std::fs::hard_link(&dst, &link).map_err(|e| {
                    format!(
                        "failed to hard-link replica {r} of {} to {}: {e}",
                        dst.display(),
                        link.display()
                    )
                })?;
            }
        }
        Ok(Self {
            dir,
            num_docs: base * replicas,
        })
    }

    /// The staging dir, for the consumer to walk.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    pub fn num_docs(&self) -> usize {
        self.num_docs
    }

    /// Staged name of document `id`: zero-padded, collision-free, parseable.
    pub fn file_name(id: usize, ext: &str) -> String {
        format!("{id:08}.{ext}")
    }

    /// Inverse of [`StagedDocs::file_name`]; `None` for a name we did not
    /// stage (the caller must fail rather than attribute it).
    pub fn file_index(name: &str, ext: &str) -> Option<usize> {
        name.strip_suffix(ext)?.strip_suffix('.')?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover_index_meta;

    /// Corpus dir with `docs` written into it, plus its descriptors.
    fn fixture(docs: &[(&str, &[u8])]) -> (tempfile::TempDir, Vec<crate::DocMeta>) {
        let dir = tempfile::tempdir().unwrap();
        for (rel, bytes) in docs {
            std::fs::write(dir.path().join(rel), bytes).unwrap();
        }
        let metas = discover_index_meta(dir.path()).unwrap();
        (dir, metas)
    }

    #[test]
    fn staged_names_round_trip() {
        assert_eq!(StagedDocs::file_name(0, "c"), "00000000.c");
        assert_eq!(StagedDocs::file_name(42, "c"), "00000042.c");
        assert_eq!(StagedDocs::file_name(42, "bin"), "00000042.bin");
        assert_eq!(StagedDocs::file_index("00000042.c", "c"), Some(42));
        assert_eq!(StagedDocs::file_index("00000042.bin", "bin"), Some(42));
        assert_eq!(StagedDocs::file_index("a.c", "c"), None);
        assert_eq!(StagedDocs::file_index("00000042.h", "c"), None);
        assert_eq!(StagedDocs::file_index("evil.c:5", "c"), None);
    }

    #[test]
    fn stage_writes_one_entry_per_doc_named_by_id() {
        let (dir, metas) = fixture(&[("a.c", b"one"), ("b.c", b""), ("c.c", b"three")]);
        let staged = StagedDocs::stage(
            Corpus {
                root: dir.path(),
                docs: &metas,
            },
            "bin",
        )
        .unwrap();
        assert_eq!(staged.num_docs(), 3);
        let mut names: Vec<String> = std::fs::read_dir(staged.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["00000000.bin", "00000001.bin", "00000002.bin"]);
        assert_eq!(
            std::fs::read(staged.dir().join("00000002.bin")).unwrap(),
            b"three"
        );
    }

    #[cfg(unix)]
    #[test]
    fn staged_files_are_independent_of_the_corpus_inodes() {
        use std::os::unix::fs::MetadataExt;
        let (dir, metas) = fixture(&[("a.c", b"one")]);
        let staged = StagedDocs::stage(
            Corpus {
                root: dir.path(),
                docs: &metas,
            },
            "bin",
        )
        .unwrap();
        let src = std::fs::metadata(dir.path().join("a.c")).unwrap();
        let dst = std::fs::metadata(staged.dir().join("00000000.bin")).unwrap();
        assert_ne!(src.ino(), dst.ino(), "staging must not hardlink");
        assert_eq!(
            std::fs::read(staged.dir().join("00000000.bin")).unwrap(),
            b"one"
        );
    }

    #[cfg(unix)]
    #[test]
    fn staged_symlinked_doc_is_materialized_not_linked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.c"), b"one").unwrap();
        std::os::unix::fs::symlink("real.c", dir.path().join("alias.c")).unwrap();
        let metas = discover_index_meta(dir.path()).unwrap();
        assert_eq!(metas.len(), 2);
        let staged = StagedDocs::stage(
            Corpus {
                root: dir.path(),
                docs: &metas,
            },
            "bin",
        )
        .unwrap();
        for id in 0..2 {
            let staged_path = staged.dir().join(StagedDocs::file_name(id, "bin"));
            assert!(
                !std::fs::symlink_metadata(&staged_path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(std::fs::read(&staged_path).unwrap(), b"one");
        }
    }

    #[cfg(unix)]
    #[test]
    fn stage_replicated_links_replicas_to_replica_zero() {
        use std::os::unix::fs::MetadataExt;
        let (dir, metas) = fixture(&[("a.c", b"one"), ("b.c", b"two")]);
        let staged = StagedDocs::stage_replicated(
            Corpus {
                root: dir.path(),
                docs: &metas,
            },
            "bin",
            3,
        )
        .unwrap();
        assert_eq!(staged.num_docs(), 6);
        for i in 0..2 {
            let zero =
                std::fs::metadata(staged.dir().join(StagedDocs::file_name(i, "bin"))).unwrap();
            assert_eq!(zero.nlink(), 3, "doc {i} link count");
            for r in 1..3 {
                let path = staged.dir().join(StagedDocs::file_name(r * 2 + i, "bin"));
                assert_eq!(std::fs::metadata(&path).unwrap().ino(), zero.ino());
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    std::fs::read(dir.path().join(if i == 0 { "a.c" } else { "b.c" })).unwrap()
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn stage_replicated_costs_one_copy_of_bytes() {
        use std::collections::HashSet;
        use std::os::unix::fs::MetadataExt;
        let (dir, metas) = fixture(&[("a.c", b"one"), ("b.c", b"three")]);
        let staged = StagedDocs::stage_replicated(
            Corpus {
                root: dir.path(),
                docs: &metas,
            },
            "bin",
            4,
        )
        .unwrap();
        let mut seen = HashSet::new();
        let mut unique_bytes = 0u64;
        for entry in std::fs::read_dir(staged.dir()).unwrap() {
            let meta = entry.unwrap().metadata().unwrap();
            if seen.insert(meta.ino()) {
                unique_bytes += meta.len();
            }
        }
        assert_eq!(
            unique_bytes, 8,
            "one copy of the base bytes, links for the rest"
        );
    }

    #[test]
    fn stage_replicated_once_equals_stage() {
        let (dir, metas) = fixture(&[("a.c", b"one"), ("b.c", b"two")]);
        let staged = StagedDocs::stage_replicated(
            Corpus {
                root: dir.path(),
                docs: &metas,
            },
            "bin",
            1,
        )
        .unwrap();
        assert_eq!(staged.num_docs(), 2);
        assert_eq!(std::fs::read_dir(staged.dir()).unwrap().count(), 2);
        assert!(
            StagedDocs::stage_replicated(
                Corpus {
                    root: dir.path(),
                    docs: &metas,
                },
                "bin",
                0
            )
            .is_err()
        );
    }

    #[test]
    fn stage_rejects_foreign_names_on_file_index() {
        let (dir, metas) = fixture(&[("a.c", b"one")]);
        let staged = StagedDocs::stage(
            Corpus {
                root: dir.path(),
                docs: &metas,
            },
            "bin",
        )
        .unwrap();
        let name = StagedDocs::file_name(0, "bin");
        assert_eq!(StagedDocs::file_index(&name, "bin"), Some(0));
        assert_eq!(StagedDocs::file_index("lookup.bin", "bin"), None);
        assert_eq!(StagedDocs::file_index(&name, "c"), None);
        assert_eq!(staged.num_docs(), 1);
    }
}

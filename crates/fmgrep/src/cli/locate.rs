//! Which index a search or `serve` reads.

use std::path::{Path, PathBuf};

use crate::index::INDEX_DIR;
use crate::manifest::{BLOBS_SHARD, MANIFEST_BIN, MANIFEST_JSON};

pub struct Located {
    pub index_dir: PathBuf,
    /// The tree a files-source index reads: the parent of a [`INDEX_DIR`].
    pub root: Option<PathBuf>,
}

/// `start` itself when it holds an index, else the [`INDEX_DIR`] of the
/// nearest directory at or above it.
pub fn locate(start: &Path) -> Result<Located, String> {
    let abs = std::fs::canonicalize(start)
        .map_err(|e| format!("error: cannot resolve {}: {e}", start.display()))?;
    if is_index(&abs) {
        let root = (abs.file_name() == Some(INDEX_DIR.as_ref()))
            .then(|| abs.parent().map(Path::to_path_buf))
            .flatten();
        return Ok(Located {
            index_dir: start.to_path_buf(),
            root,
        });
    }
    abs.ancestors()
        .find(|dir| dir.join(INDEX_DIR).is_dir())
        .map(|dir| Located {
            index_dir: dir.join(INDEX_DIR),
            root: Some(dir.to_path_buf()),
        })
        .ok_or_else(|| {
            if has_manifest(&abs) {
                return format!("error: {} has no {BLOBS_SHARD}", abs.display());
            }
            format!(
                "error: no {INDEX_DIR} index in {} or any parent; run `fmgrep index` at the corpus root",
                abs.display()
            )
        })
}

/// A manifest alone could be a corpus file; the blob shard makes it an index.
fn is_index(dir: &Path) -> bool {
    has_manifest(dir) && dir.join(BLOBS_SHARD).is_file()
}

/// A manifest, or the `manifest.json` of an older fmgrep: found, then
/// refused by its format.
fn has_manifest(dir: &Path) -> bool {
    dir.join(MANIFEST_BIN).is_file() || dir.join(MANIFEST_JSON).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_at(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(MANIFEST_BIN), "").unwrap();
        std::fs::write(dir.join(BLOBS_SHARD), "").unwrap();
    }

    fn canon(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap()
    }

    #[test]
    fn finds_the_dotdir_from_a_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        index_at(&tmp.path().join(INDEX_DIR));
        let sub = tmp.path().join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        let got = locate(&sub).unwrap();
        assert_eq!(got.index_dir, canon(tmp.path()).join(INDEX_DIR));
        assert_eq!(got.root, Some(canon(tmp.path())));
    }

    #[test]
    fn an_index_dir_is_taken_as_given() {
        let tmp = tempfile::tempdir().unwrap();
        let idx = tmp.path().join("idx");
        index_at(&idx);
        // A dotdir above does not win over an index given directly.
        index_at(&tmp.path().join(INDEX_DIR));
        let got = locate(&idx).unwrap();
        assert_eq!((got.index_dir, got.root), (idx, None));
    }

    #[test]
    fn nearest_dotdir_wins() {
        let tmp = tempfile::tempdir().unwrap();
        index_at(&tmp.path().join(INDEX_DIR));
        let inner = tmp.path().join("inner");
        index_at(&inner.join(INDEX_DIR));
        let got = locate(&inner.join(INDEX_DIR)).unwrap();
        assert_eq!(got.root, Some(canon(&inner)));
        let got = locate(&inner).unwrap();
        assert_eq!(got.index_dir, canon(&inner).join(INDEX_DIR));
    }

    /// So that a search refuses it by its format.
    #[test]
    fn an_index_of_an_older_fmgrep_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        let idx = tmp.path().join("idx");
        std::fs::create_dir_all(&idx).unwrap();
        std::fs::write(idx.join(MANIFEST_JSON), "{}").unwrap();
        std::fs::write(idx.join(BLOBS_SHARD), "").unwrap();
        let got = locate(&idx).unwrap();
        assert_eq!((got.index_dir, got.root), (idx, None));
    }

    #[test]
    fn a_manifest_alone_is_corpus_not_index() {
        for name in [MANIFEST_BIN, MANIFEST_JSON] {
            let tmp = tempfile::tempdir().unwrap();
            index_at(&tmp.path().join(INDEX_DIR));
            std::fs::write(tmp.path().join(name), "{}").unwrap();
            let got = locate(tmp.path()).unwrap();
            assert_eq!(got.index_dir, canon(tmp.path()).join(INDEX_DIR));
        }
    }

    #[test]
    fn a_manifest_without_blobs_is_named_when_nothing_is_found() {
        for name in [MANIFEST_BIN, MANIFEST_JSON] {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(tmp.path().join(name), "{}").unwrap();
            match locate(tmp.path()) {
                Err(e) => assert!(e.contains(BLOBS_SHARD), "{e}"),
                // Only possible when some ancestor of the temp dir holds one.
                Ok(got) => assert!(!got.index_dir.starts_with(canon(tmp.path()))),
            }
        }
    }

    #[test]
    fn no_dotdir_above_is_refused_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let err = match locate(tmp.path()) {
            Err(e) => e,
            // Only possible when some ancestor of the temp dir holds one.
            Ok(got) => return assert!(!got.index_dir.starts_with(canon(tmp.path()))),
        };
        assert!(
            err.contains(INDEX_DIR) && err.contains("fmgrep index"),
            "{err}"
        );
        let err = locate(&tmp.path().join("missing")).err().unwrap();
        assert!(err.starts_with("error: cannot resolve"), "{err}");
    }
}

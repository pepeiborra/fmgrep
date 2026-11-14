use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod bin;
mod files;

pub use files::{FileTable, partition_ids};

/// On-disk manifest format written by `index`; every other format is
/// refused (re-run `index`).
pub const MANIFEST_FORMAT: u32 = 8;

/// The manifest, in the layout of [`bin`].
pub const MANIFEST_BIN: &str = "manifest.bin";

/// The manifest of an index of format 7 or older: refused by its number.
pub const MANIFEST_JSON: &str = "manifest.json";

/// Concatenated document shard, at the index root next to `manifest.bin`.
pub const DOCS_SHARD: &str = "docs.shard";

/// Concatenated blob shard, at the index root next to `manifest.bin`.
pub const BLOBS_SHARD: &str = "blobs.shard";

/// Write buffer for both shards: the encoders drive a few bytes per write
/// through `dyn Write`.
pub const SHARD_WRITE_BUFFER: usize = 1 << 20;

/// On-disk manifest (`<index>/manifest.bin`). Field order is the order of
/// `fmgrep status --json`; do not reorder.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Manifest {
    pub format: u32,
    /// Documents are read from `docs.shard`, not from the files under
    /// `corpus_root`.
    pub store_docs: bool,
    pub num_docs: usize,
    pub num_bytes: u64,
    /// Leading UTF-8 BOM bytes over all docs. rg's searcher strips a BOM, so
    /// `num_bytes - bom_bytes` is the corpus's `bytes_searched`; the stored
    /// bytes themselves keep it.
    pub bom_bytes: u64,
    pub index_size_bytes: u64,
    /// The index wall: document stream, construction and blob flush, all of
    /// it. Not a sum of per-shard spans, so it keeps its meaning whatever the
    /// shard concurrency.
    pub build_time_us: u64,
    /// When the index began, in ns since the Unix epoch: before it listed or
    /// read anything. A search's record names the build it was written for
    /// by it.
    pub index_started_ns: i64,
    /// Corpus-relative path per doc, ascending by id: what a search reports as
    /// a match's file.
    pub files: FileTable,
    /// Blob inventory: one `shard-<start>-<end>.bin` label per shard.
    pub blobs: Vec<String>,
    /// Raw byte length per doc, parallel to `files`: sizes docs without
    /// reading them, slices [`DOCS_SHARD`], and is the files layout's size
    /// stamp.
    pub doc_lens: Vec<u64>,
    /// Raw byte length per blob, parallel to `blobs`: slices [`BLOBS_SHARD`],
    /// and its sum is the shard's expected byte count.
    pub blob_lens: Vec<u64>,
    /// Absolute UTF-8 path; present iff not `store_docs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corpus_root: Option<String>,
}

impl Manifest {
    /// The files layout's tree: `corpus_root` if given, else the recorded one.
    pub fn tree(&self, corpus_root: Option<&Path>) -> PathBuf {
        corpus_root.map_or_else(
            || PathBuf::from(self.corpus_root.as_deref().unwrap_or_default()),
            Path::to_path_buf,
        )
    }
}

/// Read from a `manifest.json` that an older fmgrep wrote, so that its
/// format is refused by number.
#[derive(Deserialize)]
struct Header {
    format: u32,
}

/// Byte offsets of each entry inside a separator-free concatenation: prefix
/// sums of `lens`, so `offsets.len == lens.len + 1` and the last entry is the
/// total. Shards need no separators because the length tables recover every
/// boundary exactly.
///
/// The sums are [`u64::checked_add`]: a tampered length table summing past
/// [`u64::MAX`] is a loud refusal, never a wrapped offset that would pass the
/// shard byte-count check and then underflow into a gigantic allocation.
pub fn shard_offsets(lens: &[u64]) -> Result<Vec<u64>, String> {
    let mut offsets = Vec::with_capacity(lens.len() + 1);
    let mut cur: u64 = 0;
    for &len in lens {
        offsets.push(cur);
        cur = cur.checked_add(len).ok_or_else(|| {
            "error: shard length table sums past u64::MAX; refusing a skewed index".to_string()
        })?;
    }
    offsets.push(cur);
    Ok(offsets)
}

/// Create `output`, refusing rather than merging into an existing index.
pub fn create_output_dir(output: &Path) -> Result<(), String> {
    if output.exists() {
        return Err(format!(
            "error: {} already exists; remove it to re-index, or pass another --output",
            output.display()
        ));
    }
    std::fs::create_dir_all(output)
        .map_err(|e| format!("error: cannot create {}: {e}", output.display()))
}

/// Write `manifest.bin`. Written last: a partial index dir is never
/// queryable.
pub fn write_manifest(output: &Path, manifest: &Manifest) -> Result<(), String> {
    let path = output.join(MANIFEST_BIN);
    let cannot = |e: std::io::Error| format!("error: cannot write {}: {e}", path.display());
    let mut out =
        BufWriter::with_capacity(SHARD_WRITE_BUFFER, File::create(&path).map_err(cannot)?);
    bin::encode(manifest, &mut out).map_err(cannot)?;
    out.flush().map_err(cannot)
}

/// Read + validate the manifest at `dir` without touching the shards.
///
/// An index of another format is refused by number, whatever else its
/// manifest holds; so is the `manifest.json` of an older fmgrep. Table
/// values stay unchecked against the shards — the query serves blobs
/// verbatim — but every table length must be lockstep.
///
/// The file is read front to back into owned buffers, not mapped: on a
/// cold cache a map read slower (measured on macOS). A map would hand
/// [`bin::decode`] a reader over its bytes; the layout would not change.
pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join(MANIFEST_BIN);
    let cannot = |e: std::io::Error| format!("error: cannot read {}: {e}", path.display());
    let mut file = match File::open(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let json = dir.join(MANIFEST_JSON);
            return Err(match json.is_file() {
                true => refuse_json(&json),
                false => cannot(e),
            });
        }
        opened => opened.map_err(cannot)?,
    };
    let len = file.metadata().map_err(cannot)?.len();
    let manifest = bin::decode(&mut file, len, &path)?;
    validate(&manifest, &path)?;
    Ok(manifest)
}

/// The refusal of an index that an older fmgrep wrote: by the format its
/// `manifest.json` records, if it records one.
fn refuse_json(path: &Path) -> String {
    let format = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Header>(&raw).ok())
        .map(|h| h.format)
        .filter(|&f| f != MANIFEST_FORMAT);
    foreign(path, format)
}

/// The refusal of manifest format `format`, when it is known.
fn foreign(path: &Path, format: Option<u32>) -> String {
    let format = format.map(|f| format!(" {f}")).unwrap_or_default();
    format!(
        "error: unsupported manifest format{format} in {} (want {MANIFEST_FORMAT}); re-run `index` with this binary",
        path.display()
    )
}

/// The checks of a manifest's values that its encoding does not hold:
/// `corpus_root` present iff not `store_docs`, and absolute; `bom_bytes`
/// within `num_bytes`; each table as long as the list it describes.
fn validate(manifest: &Manifest, manifest_path: &Path) -> Result<(), String> {
    match (manifest.store_docs, &manifest.corpus_root) {
        (false, Some(root)) if Path::new(root).is_absolute() => {}
        (false, _) => {
            return Err(format!(
                "error: {} reads documents from files but records no absolute corpus_root; refusing a tampered index",
                manifest_path.display()
            ));
        }
        (true, Some(_)) => {
            return Err(format!(
                "error: {} reads documents from docs.shard but records a corpus_root; refusing a tampered index",
                manifest_path.display()
            ));
        }
        (true, None) => {}
    }
    if manifest.bom_bytes > manifest.num_bytes {
        return Err(format!(
            "error: manifest records {} BOM byte(s) over {} corpus byte(s); refusing a tampered index",
            manifest.bom_bytes, manifest.num_bytes,
        ));
    }
    if manifest.doc_lens.len() != manifest.files.len() {
        return Err(format!(
            "error: manifest per-doc tables skew (files={} doc_lens={}); refusing a tampered index",
            manifest.files.len(),
            manifest.doc_lens.len(),
        ));
    }
    if manifest.blob_lens.len() != manifest.blobs.len() {
        return Err(format!(
            "error: manifest blob tables skew (blobs={} blob_lens={}); refusing a tampered index",
            manifest.blobs.len(),
            manifest.blob_lens.len(),
        ));
    }
    debug_assert!(manifest.files.is_sorted());
    Ok(())
}

fn refuse_foreign(path: &Path, format: u32) -> Result<(), String> {
    match format {
        MANIFEST_FORMAT => Ok(()),
        _ => Err(foreign(path, Some(format))),
    }
}

/// Rewrite the manifest at `dir` with `edit` applied, past the checks of
/// its values: for tests of what a load refuses.
#[cfg(test)]
pub(crate) fn rewrite(dir: &Path, edit: impl FnOnce(&mut Manifest)) {
    let path = dir.join(MANIFEST_BIN);
    let raw = std::fs::read(&path).unwrap();
    let mut manifest = bin::decode(&mut &raw[..], raw.len() as u64, &path).unwrap();
    edit(&mut manifest);
    write_manifest(dir, &manifest).unwrap();
}

/// Overwrite the bytes of the manifest at `dir` from offset `at` on.
#[cfg(test)]
pub(crate) fn patch(dir: &Path, at: usize, bytes: &[u8]) {
    let path = dir.join(MANIFEST_BIN);
    let mut raw = std::fs::read(&path).unwrap();
    raw[at..at + bytes.len()].copy_from_slice(bytes);
    std::fs::write(&path, raw).unwrap();
}

/// Slice the docs shard back into per-doc bytes in order. Test-only:
/// production queries fetch the same ranges through
/// [`crate::fetch::DocStore`] instead of materializing the corpus.
#[cfg(test)]
fn read_docs(dir: &Path, manifest: &Manifest) -> Result<Vec<crate::Document>, String> {
    let path = dir.join(DOCS_SHARD);
    let bytes =
        std::fs::read(&path).map_err(|e| format!("error: cannot read {}: {e}", path.display()))?;
    if bytes.len() as u64 != manifest.num_bytes {
        return Err(format!(
            "error: {} holds {} byte(s) but the manifest num_bytes is {}; refusing a skewed index",
            path.display(),
            bytes.len(),
            manifest.num_bytes,
        ));
    }
    let offsets = shard_offsets(&manifest.doc_lens)?;
    let mut docs = Vec::with_capacity(manifest.files.len());
    for (pos, name) in manifest.files.iter().enumerate() {
        let (lo, hi) = (offsets[pos] as usize, offsets[pos + 1] as usize);
        let bytes = bytes.get(lo..hi).ok_or_else(|| {
            format!(
                "error: doc {pos} range [{lo}..{hi}] exceeds {} ({} byte(s)); refusing a skewed index",
                path.display(),
                bytes.len(),
            )
        })?;
        docs.push(crate::Document {
            id: pos,
            path: name.to_owned(),
            bytes: bytes.to_vec(),
        });
    }
    Ok(docs)
}

/// Manifest plus both shards sliced back, in slot order.
#[cfg(test)]
type LoadedIndex = (Manifest, Vec<crate::Document>, Vec<Vec<u8>>);

/// Full-load route, test-only: validate, then slice both shards.
#[cfg(test)]
fn load_index(dir: &Path) -> Result<LoadedIndex, String> {
    let manifest = read_manifest(dir)?;
    let docs = read_docs(dir, &manifest)?;
    let slots = crate::blobs::BlobSlots::open(dir, &manifest.blobs, &manifest.blob_lens)?;
    let blobs = crate::blobs::map_slots(&slots)?
        .into_iter()
        .map(|b| b.to_vec())
        .collect();
    Ok((manifest, docs, blobs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_of(docs: &[&[u8]], blobs: &[&[u8]]) -> Manifest {
        Manifest {
            format: MANIFEST_FORMAT,
            store_docs: true,
            num_docs: docs.len(),
            num_bytes: docs.iter().map(|d| d.len() as u64).sum(),
            bom_bytes: 0,
            index_size_bytes: 1,
            build_time_us: 1,
            index_started_ns: 0,
            files: (0..docs.len()).map(|i| format!("doc{i}.txt")).collect(),
            blobs: (0..blobs.len()).map(|i| format!("{i:08}.bin")).collect(),
            doc_lens: docs.iter().map(|d| d.len() as u64).collect(),
            blob_lens: blobs.iter().map(|b| b.len() as u64).collect(),
            corpus_root: None,
        }
    }

    /// Stage a hand-built index dir (no build involved).
    fn stage(dir: &Path, docs: &[&[u8]], blobs: &[&[u8]]) -> Manifest {
        let manifest = manifest_of(docs, blobs);
        create_output_dir(dir).unwrap();
        std::fs::write(dir.join(DOCS_SHARD), docs.concat()).unwrap();
        let mut sink = crate::blobs::create_sink(dir).unwrap();
        for (i, blob) in blobs.iter().enumerate() {
            sink.write_slot(|out| {
                out.write_all(blob).unwrap();
                Ok(format!("{i:08}.bin"))
            })
            .unwrap();
        }
        crate::blobs::finish_sink(sink).unwrap();
        write_manifest(dir, &manifest).unwrap();
        manifest
    }

    #[test]
    fn status_json_keeps_the_field_order() {
        let m = Manifest {
            format: MANIFEST_FORMAT,
            store_docs: false,
            num_docs: 2,
            num_bytes: 10,
            bom_bytes: 3,
            index_size_bytes: 99,
            build_time_us: 7,
            index_started_ns: 9,
            files: ["a/one.hs", "b/two.hs"].into_iter().collect(),
            blobs: vec!["shard-00000000-00000002.bin".into()],
            doc_lens: vec![6, 4],
            blob_lens: vec![11],
            corpus_root: Some("/corpus".into()),
        };
        let json = serde_json::to_string(&m).unwrap();
        // The order of the JSON that an older fmgrep wrote as manifest.json;
        // a reader keying on it must not see it drift.
        let mut at = 0usize;
        for key in [
            "\"format\":",
            "\"store_docs\":false",
            "\"num_docs\":",
            "\"num_bytes\":",
            "\"bom_bytes\":",
            "\"index_size_bytes\":",
            "\"build_time_us\":",
            "\"index_started_ns\":",
            "\"files\":",
            "\"blobs\":",
            "\"doc_lens\":",
            "\"blob_lens\":",
            "\"corpus_root\":",
        ] {
            let pos = json[at..]
                .find(key)
                .unwrap_or_else(|| panic!("{key} missing or out of order in {json}"));
            at += pos + key.len();
        }
        assert!(json.starts_with("{\"format\":8,"), "{json}");
        for gone in ["engine", "index_name", "num_files_indexed"] {
            assert!(!json.contains(&format!("\"{gone}\"")), "{gone}: {json}");
        }
        assert!(
            json.ends_with("\"corpus_root\":\"/corpus\"}"),
            "corpus_root is the last key: {json}"
        );
        let stored = Manifest {
            store_docs: true,
            corpus_root: None,
            ..m
        };
        let json = serde_json::to_string(&stored).unwrap();
        assert!(json.ends_with("\"blob_lens\":[11]}"), "{json}");
    }

    #[test]
    fn an_index_reads_back_its_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("idx");
        let manifest = stage(&dir, &[b"hello", b"bye"], &[b"blob"]);
        assert_eq!(read_manifest(&dir), Ok(manifest));
        assert!(!dir.join(MANIFEST_JSON).exists());
    }

    #[test]
    fn other_manifest_formats_are_refused() {
        // Every other layout fails at the explicit format check — never a
        // flag or size error, never a silent rebuild.
        let tmp = tempfile::tempdir().unwrap();
        for format in [1u32, 2, 3, 4, 5, 7, 9, 12] {
            let dir = tmp.path().join(format!("idx{format}"));
            stage(&dir, &[b"hello"], &[b"blob"]);
            patch(&dir, 8, &format.to_le_bytes());
            patch(&dir, 12, &3u32.to_le_bytes());
            let err = read_manifest(&dir).unwrap_err();
            assert_eq!(
                err,
                format!(
                    "error: unsupported manifest format {format} in {} (want 8); re-run `index` with this binary",
                    dir.join(MANIFEST_BIN).display()
                )
            );
        }
    }

    /// The manifest.json of an older fmgrep, and no manifest.bin.
    #[test]
    fn an_index_of_an_older_fmgrep_is_refused_by_its_format() {
        let tmp = tempfile::tempdir().unwrap();
        let json = tmp.path().join(MANIFEST_JSON);
        let want = |format: &str| {
            format!(
                "error: unsupported manifest format{format} in {} (want 8); re-run `index` with this binary",
                json.display()
            )
        };
        for (raw, format) in [
            (r#"{"format":7,"store_docs":true,"files":[]}"#, " 7"),
            (r#"{"format":1,"engine":"fmgrep"}"#, " 1"),
            (r#"{"format":8}"#, ""),
            (r#"{"files":[]}"#, ""),
            ("not json", ""),
        ] {
            std::fs::write(&json, raw).unwrap();
            assert_eq!(read_manifest(tmp.path()), Err(want(format)), "{raw}");
        }
        std::fs::remove_file(&json).unwrap();
        let err = read_manifest(tmp.path()).unwrap_err();
        assert!(
            err.starts_with("error: cannot read") && err.contains(MANIFEST_BIN),
            "{err}"
        );
    }

    #[test]
    fn unknown_flags_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("idx");
        stage(&dir, &[b"hello"], &[b"blob"]);
        patch(&dir, 12, &(1u32 << 31 | 1).to_le_bytes());
        let err = read_manifest(&dir).unwrap_err();
        assert!(err.contains("sets unknown flag bits 0x80000000"), "{err}");
        assert!(err.ends_with("; re-run `index`"), "{err}");
    }

    #[test]
    fn files_layout_needs_an_absolute_root() {
        let tmp = tempfile::tempdir().unwrap();
        for (store_docs, root) in [
            (false, None),
            (false, Some("relative/corpus")),
            (true, Some("/corpus")),
        ] {
            let dir = tmp.path().join(format!("{store_docs}-{}", root.is_none()));
            stage(&dir, &[b"hello"], &[b"blob"]);
            rewrite(&dir, |m| {
                m.store_docs = store_docs;
                m.corpus_root = root.map(str::to_owned);
            });
            let err = read_manifest(&dir).unwrap_err();
            assert!(
                err.contains("corpus_root") && err.contains("tampered"),
                "{store_docs} {root:?}: {err}"
            );
        }
        let dir = tmp.path().join("ok");
        stage(&dir, &[b"hello"], &[b"blob"]);
        rewrite(&dir, |m| {
            m.store_docs = false;
            m.corpus_root = Some("/corpus".into());
        });
        let manifest = read_manifest(&dir).unwrap();
        assert_eq!(manifest.corpus_root.as_deref(), Some("/corpus"));
    }

    #[test]
    fn a_changed_count_is_refused_at_open() {
        // One count sizes each group of tables, so a changed count no longer
        // matches the file's size.
        let tmp = tempfile::tempdir().unwrap();
        for (at, count, want) in [
            (16, 2u64, "is truncated"),
            (16, 0, "past its last section"),
            (64, 2, "is truncated"),
            (64, 0, "past its last section"),
        ] {
            let dir = tmp.path().join(format!("{at}-{count}"));
            stage(&dir, &[b"hello"], &[b"blob"]);
            patch(&dir, at, &count.to_le_bytes());
            let err = read_manifest(&dir).unwrap_err();
            assert!(
                err.contains(want) && err.ends_with("re-run `index`"),
                "{at}: {err}"
            );
        }
    }

    #[test]
    fn missing_blob_shard_rejected_loudly() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("idx");
        stage(&dir, &[b"hello world"], &[b"blob"]);
        std::fs::remove_file(dir.join(BLOBS_SHARD)).unwrap();
        let err = load_index(&dir).unwrap_err();
        assert!(err.contains("cannot read"), "got: {err}");
        assert!(err.contains(BLOBS_SHARD), "must name the shard: {err}");
    }

    #[test]
    fn skewed_blob_shard_rejected_loudly() {
        // Truncated or padded: a byte count differing from the `blob_lens` sum
        // is refused before any decode, never a silent short read.
        let tmp = tempfile::tempdir().unwrap();
        for (name, delta) in [("short", -1i64), ("long", 1)] {
            let dir = tmp.path().join(name);
            stage(&dir, &[b"hello world"], &[b"blobbytes"]);
            let shard = std::fs::read(dir.join(BLOBS_SHARD)).unwrap();
            let mut tampered = shard.clone();
            if delta < 0 {
                tampered.pop();
            } else {
                tampered.push(0);
            }
            std::fs::write(dir.join(BLOBS_SHARD), &tampered).unwrap();
            let err = load_index(&dir).unwrap_err();
            assert!(err.contains("tampered"), "{name} got: {err}");
            assert!(err.contains(BLOBS_SHARD), "{name} got: {err}");
        }
    }

    #[test]
    fn shard_offsets_rejects_u64_overflow() {
        assert_eq!(shard_offsets(&[]).unwrap(), vec![0]);
        assert_eq!(shard_offsets(&[4, 0, 7]).unwrap(), vec![0, 4, 4, 11]);
        assert_eq!(shard_offsets(&[u64::MAX]).unwrap(), vec![0, u64::MAX]);
        assert!(shard_offsets(&[u64::MAX, u64::MAX]).is_err());
        let err = shard_offsets(&[u64::MAX, 1]).unwrap_err();
        assert!(err.contains("skewed"), "got: {err}");
    }

    #[test]
    fn wrapped_length_table_sum_refused_loudly() {
        // `blob_lens = [u64::MAX, S+1]` wraps to the real shard size S: a
        // wrapping check would pass, then the offset subtraction would
        // underflow into a gigantic allocation. Refuse loudly instead — and
        // the same holds for `doc_lens` through the shared prefix sums.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("idx");
        stage(&dir, &[b"hello world", b"bye"], &[b"aa", b"bb"]);
        let shard_len = std::fs::metadata(dir.join(BLOBS_SHARD)).unwrap().len();
        rewrite(&dir, |m| {
            m.blob_lens = vec![u64::MAX, shard_len.wrapping_add(1)]
        });
        let err = load_index(&dir).unwrap_err();
        assert!(err.contains("skewed"), "wrapped blob sum got: {err}");

        let dir = tmp.path().join("idx2");
        stage(&dir, &[b"hello world", b"bye"], &[b"aa", b"bb"]);
        rewrite(&dir, |m| m.doc_lens = vec![u64::MAX, 1]);
        let err = load_index(&dir).unwrap_err();
        assert!(err.contains("skewed"), "overflowing doc_lens got: {err}");
    }

    #[test]
    fn output_dir_refuses_an_existing_index() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("idx");
        stage(&dir, &[b"hello"], &[b"blob"]);
        let err = create_output_dir(&dir).unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");
    }
}

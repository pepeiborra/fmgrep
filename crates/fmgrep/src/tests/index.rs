use super::*;

use crate::manifest::MANIFEST_FORMAT;

#[test]
fn index_of_an_unindexable_folder_errors_and_leaves_no_output() {
    // Discovery runs inside the output dir the indexer just created: an
    // empty or missing folder is still a loud error with nothing persisted.
    let tmp = tempfile::tempdir().unwrap();
    let empty = stage_corpus(tmp.path(), &[]);
    let output = tmp.path().join("idx");
    let err = match dispatch(&cli_spec(), &index_words(&empty, Some(&output), true)) {
        Err(e) => e,
        Ok(_) => panic!("an unindexable folder must refuse"),
    };
    assert!(err.contains("no indexable files"), "got: {err}");
    assert!(!output.exists(), "a failed index must persist nothing");
}

#[test]
fn index_of_a_missing_folder_creates_nothing() {
    // The default --output lives under the corpus folder, so the folder must
    // be refused before any directory is created.
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope");
    let err = match dispatch(&cli_spec(), &index_words(&missing, None, true)) {
        Err(e) => e,
        Ok(_) => panic!("a missing folder must refuse"),
    };
    assert!(err.contains("cannot list folder"), "got: {err}");
    assert!(!missing.exists(), "a failed index must create nothing");
}

#[test]
fn load_index_ignores_stray_root_files() {
    // Both shards are single named files: a stray root file is junk, never
    // a phantom doc or blob.
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), &[("a.c", b"hello")]);
    let output = tmp.path().join("idx");
    assert_eq!(index_it(&folder, &output, true).num_files, 1);
    std::fs::write(output.join("extra.bin"), b"junk").unwrap();
    let (manifest, docs, blobs) = read_back(&output);
    assert_eq!(manifest.files, vec!["a.c".to_string()]);
    assert_eq!(manifest.blobs, vec!["shard-00000000-00000001.bin"]);
    assert_eq!(docs, vec![b"hello".to_vec()]);
    assert_eq!(
        blobs,
        vec![std::fs::read(output.join(BLOBS_SHARD)).unwrap()]
    );
}

#[test]
fn stored_docs_index_writes_both_shards_and_tables() {
    // Doc lengths and blob lengths round-trip; the docs shard is the staged
    // bytes concatenated, and each length table's prefix sums slice its
    // shard back exactly.
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(
        tmp.path(),
        &[
            ("a.c", b"printk hello".as_slice()),
            ("b.c", b"zz".as_slice()),
            ("e.c", b"".as_slice()),
        ],
    );
    let output = tmp.path().join("idx");
    index_it(&folder, &output, true);
    let (manifest, docs, blobs) = read_back(&output);
    assert_eq!(manifest.format, MANIFEST_FORMAT);
    assert_eq!(manifest.doc_lens, vec![12, 2, 0]);
    assert_eq!(manifest.doc_lens.iter().sum::<u64>(), manifest.num_bytes);

    let doc_shard = std::fs::read(output.join(DOCS_SHARD)).unwrap();
    assert_eq!(doc_shard, b"printk hellozz");
    let offsets = shard_offsets(&manifest.doc_lens).unwrap();
    assert_eq!(*offsets.last().unwrap(), doc_shard.len() as u64);
    for (i, doc) in docs.iter().enumerate() {
        let (lo, hi) = (offsets[i] as usize, offsets[i + 1] as usize);
        assert_eq!(&doc_shard[lo..hi], &doc[..], "doc={i}");
    }

    let blob_shard = std::fs::read(output.join(BLOBS_SHARD)).unwrap();
    assert_eq!(manifest.blobs, vec!["shard-00000000-00000003.bin"]);
    assert_eq!(manifest.blob_lens, vec![blob_shard.len() as u64]);
    assert_eq!(blobs, vec![blob_shard]);
}

#[test]
fn files_index_writes_blobs_stamps_manifest_only() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), &[("a.c", b"printk hello"), ("e.c", b"")]);
    let output = tmp.path().join("idx");
    let summary = index_it(&folder, &output, false);
    let mut root: Vec<String> = std::fs::read_dir(&output)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    root.sort();
    assert_eq!(
        root,
        [BLOBS_SHARD, crate::source::DOCS_STAMPS, "manifest.json"]
    );
    let manifest = read_manifest(&output).unwrap();
    assert!(!manifest.store_docs);
    let canonical = std::fs::canonicalize(&folder).unwrap();
    assert_eq!(manifest.corpus_root.as_deref(), canonical.to_str());
    assert_eq!(manifest.num_bytes, 12);
    // The document copy is the only size difference: 12 bytes plus one
    // separator.
    let stored = index_it(&folder, &tmp.path().join("stored"), true);
    assert_eq!(stored.index_size_bytes, summary.index_size_bytes + 13);
    let stamps = std::fs::read(output.join(crate::source::DOCS_STAMPS)).unwrap();
    assert_eq!(stamps.len(), 16);
    let mtime = crate::Stamp::of(&std::fs::metadata(folder.join("a.c")).unwrap()).mtime_ns;
    assert_eq!(stamps[..8], mtime.to_le_bytes());
}

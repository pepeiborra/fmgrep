//! End-to-end roundtrip through `fmgrep`: the shared scenarios for both
//! layouts, the files-layout ones, the stored-docs ones, plus the refusals.

use crate::scenarios::roundtrip::{BLOBS_SHARD, do_index, index_dir_of, query_err, stage};
use crate::{Fmgrep, index_argv};

const FILES: Fmgrep = Fmgrep {
    store_docs: &[false],
};

const STORED: Fmgrep = Fmgrep {
    store_docs: &[true],
};

crate::tests!(FILES;
    roundtrip::roundtrip_literal_matches_oracle,
    roundtrip::roundtrip_regex_marker_matches_oracle,
    roundtrip::roundtrip_mixed_slash_terms_match_oracle,
    roundtrip::invalid_pattern_fails_loudly,
    roundtrip::human_output_is_readable,
    roundtrip::index_writes_manifest_and_shards,
    roundtrip::index_summary_and_query_report_carry_every_wire_field,
    roundtrip::index_refuses_to_overwrite_and_reports_argv_errors,
    roundtrip::sharded_matches_unsharded,
    roundtrip::shards_in_flight_do_not_change_the_index,
    roundtrip::stale_file_is_scanned_in_full,
    roundtrip::missing_file_is_warned_once_and_excluded,
    roundtrip::moved_tree_answers_through_its_dotdir,
    roundtrip::index_defaults_to_a_dotdir_found_from_subdirectories,
    roundtrip::no_index_above_the_cwd_is_refused,
    roundtrip::dotdir_output_outside_the_folder_is_refused,
    roundtrip::a_common_anchor_reads_only_matching_docs,
);

mod stored {
    use super::*;

    crate::tests!(STORED;
        roundtrip::roundtrip_literal_matches_oracle,
        roundtrip::roundtrip_regex_marker_matches_oracle,
        roundtrip::roundtrip_mixed_slash_terms_match_oracle,
        roundtrip::invalid_pattern_fails_loudly,
        roundtrip::human_output_is_readable,
        roundtrip::index_writes_manifest_and_shards,
        roundtrip::literal_query_answers_without_docs_shard,
        roundtrip::index_summary_and_query_report_carry_every_wire_field,
        roundtrip::index_refuses_to_overwrite_and_reports_argv_errors,
        roundtrip::sharded_matches_unsharded,
        roundtrip::shards_in_flight_do_not_change_the_index,
        roundtrip::index_defaults_to_a_dotdir_found_from_subdirectories,
    );
}

#[test]
fn tampered_blob_is_refused_loudly() {
    let (docs_dir, _docs) = stage();
    let index_dir = index_dir_of(docs_dir.path());
    do_index(&FILES, docs_dir.path(), false);
    let blobs = index_dir.join(BLOBS_SHARD);
    let mut bytes = std::fs::read(&blobs).unwrap();
    bytes[..5].copy_from_slice(b"XXXXX");
    std::fs::write(&blobs, &bytes).unwrap();
    let err = query_err(&FILES, &index_dir, false, &["printk"]);
    assert!(err.contains("corrupt docfm blob"), "got: {err}");
}

/// A build that fails after the output dir exists removes it: nothing
/// half-written survives, and a retry does not hit "already exists".
#[test]
fn failed_build_removes_the_output() {
    let (docs_dir, _docs) = stage();
    let index_dir = index_dir_of(docs_dir.path());
    let out = FILES
        .command()
        .args(index_argv(docs_dir.path(), &index_dir, false, &[]))
        .env("FMGREP_SHARD_BYTES", "nope")
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("FMGREP_SHARD_BYTES"), "got: {err}");
    assert!(!index_dir.exists(), "a failed index must persist nothing");
}

/// An index of another manifest format is refused by a search and `serve`,
/// naming the format, before any blob is read.
#[test]
fn other_manifest_format_is_refused() {
    let (docs_dir, _docs) = stage();
    let index_dir = index_dir_of(docs_dir.path());
    do_index(&FILES, docs_dir.path(), false);
    let path = index_dir.join("manifest.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["format"] = 12.into();
    std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
    std::fs::write(index_dir.join(BLOBS_SHARD), b"").unwrap();
    let index = index_dir.display().to_string();
    for argv in [
        &["--index", &index, "printk"][..],
        &["serve", "--stdio", &index],
    ] {
        let out = FILES.run(argv);
        assert_eq!(out.status.code(), Some(2), "{argv:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("unsupported manifest format 12") && err.contains("re-run"),
            "{argv:?}: {err}"
        );
    }
}

/// `--store-docs` adds the document copy and nothing else: the same blobs,
/// `docs.shard` instead of `docs.stamps`, the same answers.
#[test]
fn store_docs_changes_only_the_document_copy() {
    let (docs_dir, _docs) = stage();
    let out_root = tempfile::tempdir().unwrap();
    let mut cells = Vec::new();
    for store_docs in [false, true] {
        let index_dir = out_root.path().join(format!("idx-{store_docs}"));
        let out = FILES.run(&index_argv(docs_dir.path(), &index_dir, store_docs, &[]));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let has = |f: &str| index_dir.join(f).exists();
        assert_eq!(
            (has("docs.shard"), has("docs.stamps")),
            (store_docs, !store_docs)
        );
        let index = index_dir.display().to_string();
        let query = FILES.run(&["--index", &index, "-n", "printk", "/a+/"]);
        assert!(query.status.success());
        cells.push((
            std::fs::read(index_dir.join(BLOBS_SHARD)).unwrap(),
            query.stdout,
        ));
    }
    assert_eq!(cells[0].0, cells[1].0, "blobs.shard");
    assert_eq!(cells[0].1, cells[1].1, "query output");
}

//! End-to-end roundtrip: `index` a temp folder, search it back through the
//! persisted blobs, and check the counts against a naive oracle (overlapping
//! scan for literals, `find_iter` for patterns).
//! Non-UTF8 docs ride along raw: the on-disk layout has no encoding step.

use std::path::{Path, PathBuf};

use crate::{Fmgrep, index_argv};

const DOCS_SHARD: &str = "docs.shard";
const DOCS_STAMPS: &str = "docs.stamps";
pub const BLOBS_SHARD: &str = "blobs.shard";

/// (file name, bytes) staged docs. `b.bin` is non-UTF8 on purpose; its high
/// bytes stay control-free so the index-time binary filter keeps it.
fn staged() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a.bin", b"printk hello printk\n".to_vec()),
        ("b.bin", b"\x80\x81 non-utf8 \xe9 blob printk".to_vec()),
        ("c.bin", b"aaaa nothing here".to_vec()),
    ]
}

/// Stage `staged()` into a fresh temp dir; the returned byte vectors are in
/// sorted-name order, exactly the binary's folder load.
pub fn stage() -> (tempfile::TempDir, Vec<Vec<u8>>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut staged = staged();
    staged.sort_by_key(|(n, _)| *n);
    for (name, bytes) in &staged {
        std::fs::write(dir.path().join(name), bytes).expect("stage doc");
    }
    let docs = staged.into_iter().map(|(_, b)| b).collect();
    (dir, docs)
}

/// Overlapping literal oracle: the CLI's literal semantics.
fn naive(hay: &[u8], needle: &[u8]) -> u64 {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    (0..=hay.len() - needle.len())
        .filter(|&w| &hay[w..w + needle.len()] == needle)
        .count() as u64
}

/// `(total_matches, docs_matched)` a term must produce: `/.../`-wrapped and
/// `regex`-forced terms count non-overlapping, bare terms overlapping.
fn oracle(docs: &[Vec<u8>], term: &str, regex: bool) -> (u64, u64) {
    let pattern = if regex {
        Some(term.to_string())
    } else {
        let b = term.as_bytes();
        (b.len() >= 2 && b[0] == b'/' && b[b.len() - 1] == b'/')
            .then(|| term[1..term.len() - 1].to_string())
    };
    let counts: Vec<u64> = match pattern {
        Some(p) => {
            let re = regex::bytes::Regex::new(&p).expect("oracle pattern");
            docs.iter()
                .map(|d| re.find_iter(d).count() as u64)
                .collect()
        }
        None => docs.iter().map(|d| naive(d, term.as_bytes())).collect(),
    };
    (
        counts.iter().sum(),
        counts.iter().filter(|&&c| c > 0).count() as u64,
    )
}

/// `index [--store-docs] <extra...>` of the cwd.
fn index_here<'a>(store_docs: bool, extra: &[&'a str]) -> Vec<&'a str> {
    let mut argv = vec!["index"];
    if store_docs {
        argv.push("--store-docs");
    }
    argv.extend(extra);
    argv
}

pub fn index_dir_of(dir: &Path) -> PathBuf {
    dir.join("idx")
}

/// `index --json` of `docs_dir` into [`index_dir_of`] it; the summary.
pub fn do_index(e: &Fmgrep, docs_dir: &Path, store_docs: bool) -> serde_json::Value {
    let index_dir = index_dir_of(docs_dir);
    let out = e.run(&index_argv(docs_dir, &index_dir, store_docs, &["--json"]));
    assert!(
        out.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("summary JSON")
}

fn query_argv(index_dir: &Path, regex: bool, terms: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "--index".to_string(),
        index_dir.display().to_string(),
        "--json".into(),
    ];
    if regex {
        argv.push("--regex".into());
    }
    argv.push("--".into());
    argv.extend(terms.iter().map(|t| t.to_string()));
    argv
}

fn do_query(e: &Fmgrep, index_dir: &Path, regex: bool, terms: &[&str]) -> serde_json::Value {
    do_query_argv(e, &query_argv(index_dir, regex, terms))
}

fn do_query_argv(e: &Fmgrep, argv: &[String]) -> serde_json::Value {
    let out = e.run(argv);
    assert!(
        out.status.success(),
        "query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("report JSON")
}

/// A `--json` search that must exit 2 with no report; its stderr.
pub fn query_err(e: &Fmgrep, index_dir: &Path, regex: bool, terms: &[&str]) -> String {
    let out = e.run(&query_argv(index_dir, regex, terms));
    assert_eq!(out.status.code(), Some(2), "query must fail for {terms:?}");
    assert!(out.stdout.is_empty(), "no report on failure");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn check_query(e: &Fmgrep, store_docs: bool, regex: bool, terms: &[&str]) {
    let (docs_dir, docs) = stage();
    let summary = do_index(e, docs_dir.path(), store_docs);
    assert_eq!(summary["index_name"], "fmgrep");
    assert_eq!(summary["num_files"], docs.len() as u64);
    let report = do_query(e, &index_dir_of(docs_dir.path()), regex, terms);
    assert_eq!(report["index_name"], "fmgrep");
    assert_eq!(report["num_files_indexed"], docs.len() as u64);
    let queries = report["queries"].as_array().expect("queries");
    assert_eq!(queries.len(), terms.len());
    for (got, term) in queries.iter().zip(terms) {
        let (total, matched) = oracle(&docs, term, regex);
        assert_eq!(got["term"], *term);
        assert_eq!(got["total_matches"], total, "{store_docs} term {term:?}");
        assert_eq!(got["docs_matched"], matched, "{store_docs} term {term:?}");
    }
}

pub fn roundtrip_literal_matches_oracle(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        check_query(e, store_docs, false, &["printk", "aa", "missing"]);
    }
}

pub fn roundtrip_regex_marker_matches_oracle(e: &Fmgrep) {
    // Verbatim patterns incl. a `/.../`-wrapped one (the marker wins over the
    // slashes) and a non-overlapping `aa` on `aaaa`.
    for &store_docs in e.store_docs {
        check_query(e, store_docs, true, &["/printk/", "pr[i1]ntk", "aa"]);
    }
}

pub fn roundtrip_mixed_slash_terms_match_oracle(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        check_query(e, store_docs, false, &["/pr[i1]ntk/", "printk", "aa"]);
    }
}

pub fn invalid_pattern_fails_loudly(e: &Fmgrep) {
    let (docs_dir, _docs) = stage();
    do_index(e, docs_dir.path(), e.store_docs());
    let err = query_err(e, &index_dir_of(docs_dir.path()), true, &["printk", "(["]);
    assert!(err.contains("error:"), "stderr: {err}");
    assert!(err.contains("(["), "must name the pattern: {err}");
}

pub fn human_output_is_readable(e: &Fmgrep) {
    let (docs_dir, _docs) = stage();
    let index_dir = index_dir_of(docs_dir.path());
    let out = e.run(&index_argv(
        docs_dir.path(),
        &index_dir,
        e.store_docs(),
        &[],
    ));
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("indexed 3 files"));
    // Human output is rg's, per term: grouped matches, and exit 0 with none.
    let out = e.run(&[
        "--index",
        &index_dir.display().to_string(),
        "printk",
        "missing",
    ]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("== printk ==\na.bin\n1:printk hello printk\n"),
        "{stdout}"
    );
    assert!(stdout.ends_with("\n== missing ==\n"), "{stdout}");
    // -c prints rg's line counts per file instead.
    let out = e.run(&["--index", &index_dir.display().to_string(), "-c", "printk"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.bin:1\nb.bin:1\n");
}

fn manifest_of(index_dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(index_dir.join("manifest.json")).unwrap()).unwrap()
}

/// The index root's file names, sorted.
fn root_files(index_dir: &Path) -> Vec<String> {
    let mut root: Vec<String> = std::fs::read_dir(index_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    root.sort();
    root
}

pub fn index_writes_manifest_and_shards(e: &Fmgrep) {
    // blobs.shard carries the index, the documents are docs.shard or the
    // stamped files, and nothing else.
    for &store_docs in e.store_docs {
        let (docs_dir, docs) = stage();
        let index_dir = index_dir_of(docs_dir.path());
        do_index(e, docs_dir.path(), store_docs);
        let manifest = manifest_of(&index_dir);
        let root = root_files(&index_dir);
        assert_eq!(manifest["store_docs"], store_docs);
        let docs_file = if store_docs {
            assert!(manifest.get("corpus_root").is_none());
            DOCS_SHARD
        } else {
            let canonical = std::fs::canonicalize(docs_dir.path()).unwrap();
            assert_eq!(manifest["corpus_root"], canonical.display().to_string());
            DOCS_STAMPS
        };
        let mut want = vec![BLOBS_SHARD, docs_file, "manifest.json"];
        want.sort();
        assert_eq!(root, want, "{store_docs}");
        assert_eq!(manifest["format"], 1, "{store_docs}");
        assert_eq!(manifest["engine"], "fmgrep", "{store_docs}");
        assert_eq!(manifest["index_name"], "fmgrep", "{store_docs}");
        let keys: Vec<&String> = manifest.as_object().unwrap().keys().collect();
        let mut want_keys = vec![
            "blob_lens",
            "blobs",
            "bom_bytes",
            "build_time_us",
            "doc_lens",
            "engine",
            "files",
            "format",
            "index_name",
            "index_size_bytes",
            "num_bytes",
            "num_docs",
            "num_files_indexed",
            "store_docs",
        ];
        if !store_docs {
            want_keys.push("corpus_root");
            want_keys.sort();
        }
        assert_eq!(keys, want_keys, "{store_docs}");
        let blob_lens: Vec<u64> = serde_json::from_value(manifest["blob_lens"].clone()).unwrap();
        assert_eq!(blob_lens.len(), 1, "{store_docs}");
        assert!(
            blob_lens.iter().all(|&l| l > 0),
            "{store_docs}: structures persisted"
        );
        let blobs_len = std::fs::metadata(index_dir.join(BLOBS_SHARD))
            .unwrap()
            .len();
        assert_eq!(blob_lens.iter().sum::<u64>(), blobs_len, "{store_docs}");
        let docs_len = std::fs::metadata(index_dir.join(docs_file)).unwrap().len();
        let want = match docs_file {
            DOCS_SHARD => docs.iter().map(|d| d.len() as u64).sum::<u64>(),
            _ => 8 * docs.len() as u64,
        };
        assert_eq!(docs_len, want, "{store_docs}");
    }
}

pub fn literal_query_answers_without_docs_shard(e: &Fmgrep) {
    // Every literal is answered from the structure alone, so the document
    // shard is not even opened; a raw-needing regex then fails loudly naming
    // it rather than rebuilding.
    for &store_docs in e.store_docs {
        let (docs_dir, docs) = stage();
        let index_dir = index_dir_of(docs_dir.path());
        do_index(e, docs_dir.path(), store_docs);
        std::fs::rename(index_dir.join(DOCS_SHARD), index_dir.join("hidden")).unwrap();
        let report = do_query(e, &index_dir, false, &["printk", "missing"]);
        let (total, matched) = oracle(&docs, "printk", false);
        assert_eq!(report["queries"][0]["total_matches"], total, "{store_docs}");
        assert_eq!(
            report["queries"][0]["docs_matched"], matched,
            "{store_docs}"
        );
        assert_eq!(report["queries"][1]["total_matches"], 0, "{store_docs}");
        let err = query_err(e, &index_dir, true, &["pr[i1]ntk"]);
        assert!(
            err.contains(DOCS_SHARD),
            "{store_docs} must name the shard: {err}"
        );
        std::fs::rename(index_dir.join("hidden"), index_dir.join(DOCS_SHARD)).unwrap();
        assert_eq!(
            do_query(e, &index_dir, true, &["pr[i1]ntk"])["queries"][0]["total_matches"],
            oracle(&docs, "pr[i1]ntk", true).0,
            "{store_docs}"
        );
    }
}

pub fn index_summary_and_query_report_carry_every_wire_field(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        let (docs_dir, docs) = stage();
        let index_dir = index_dir_of(docs_dir.path());
        let summary = do_index(e, docs_dir.path(), store_docs);
        let num_bytes: u64 = docs.iter().map(|d| d.len() as u64).sum();
        assert_eq!(summary["index_name"], "fmgrep", "{store_docs}");
        assert_eq!(summary["store_docs"], store_docs, "{store_docs}");
        assert_eq!(summary.as_object().unwrap().len(), 7, "{summary}");
        assert_eq!(
            summary["path"],
            index_dir.display().to_string(),
            "{store_docs}"
        );
        assert_eq!(summary["num_files"], 3, "{store_docs}");
        assert_eq!(summary["num_bytes"], num_bytes, "{store_docs}");
        assert!(
            summary["index_size_bytes"].as_u64().unwrap() > 0,
            "{store_docs}"
        );
        assert!(
            summary["build_time_us"].as_u64().unwrap() > 0,
            "{store_docs}"
        );

        let report = do_query(e, &index_dir, false, &["printk"]);
        // Load and build must agree or the harness refuses the run.
        assert_eq!(
            report["index_size_bytes"], summary["index_size_bytes"],
            "{store_docs}"
        );
        assert_eq!(report["num_files_indexed"], 3, "{store_docs}");
        assert!(report["index_time_us"].as_u64().is_some(), "{store_docs}");
        assert!(
            report["queries"][0]["latency_us"].as_u64().is_some(),
            "{store_docs}"
        );
    }
}

pub fn index_refuses_to_overwrite_and_reports_argv_errors(e: &Fmgrep) {
    let (docs_dir, _docs) = stage();
    let index_dir = index_dir_of(docs_dir.path());
    do_index(e, docs_dir.path(), e.store_docs());
    let out = e.run(&index_argv(
        docs_dir.path(),
        &index_dir,
        e.store_docs(),
        &[],
    ));
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already exists"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // An unknown index flag is a usage error.
    let out = e.run(&["index", &docs_dir.path().display().to_string(), "--bogus"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("usage:"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn sharded_matches_unsharded(e: &Fmgrep) {
    // The shard cap shapes `index` only, and never a count: the same corpus
    // indexed at one shard and at several answers identically.
    const TERMS: [&str; 4] = ["printk", "aaaa", "nothing", "missing"];
    const PATTERNS: [&str; 3] = ["pr[i1]ntk", "a+", "missing"];
    for &store_docs in e.store_docs {
        let (docs_dir, _docs) = stage();
        // Index dirs live outside the corpus: discovery is recursive.
        let out_root = tempfile::tempdir().unwrap();
        let mut reports = Vec::new();
        for cap in ["", "64"] {
            let index_dir = out_root.path().join(format!("idx-{cap}"));
            let out = e
                .command()
                .args(index_argv(
                    docs_dir.path(),
                    &index_dir,
                    store_docs,
                    &["--json"],
                ))
                .env(
                    "FMGREP_SHARD_BYTES",
                    if cap.is_empty() { "1024000" } else { cap },
                )
                .output()
                .expect("spawn");
            assert!(
                out.status.success(),
                "index: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let slots = manifest_of(&index_dir)["blobs"].as_array().unwrap().len();
            assert_eq!(
                slots,
                if cap.is_empty() { 1 } else { 2 },
                "{store_docs} cap={cap:?}"
            );
            reports.push((
                do_query(e, &index_dir, false, &TERMS),
                do_query(e, &index_dir, true, &PATTERNS),
            ));
        }
        let counts = |r: &serde_json::Value| {
            r["queries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|q| {
                    (
                        q["term"].clone(),
                        q["total_matches"].clone(),
                        q["docs_matched"].clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            counts(&reports[0].0),
            counts(&reports[1].0),
            "{store_docs} literals"
        );
        assert_eq!(
            counts(&reports[0].1),
            counts(&reports[1].1),
            "{store_docs} regex"
        );
    }
}

/// The thread counts are a schedule, not a format: the persisted bytes and
/// the counts must not move with `FMGREP_BUILD_THREADS` /
/// `FMGREP_BUILD_SHARD_THREADS` / `FMGREP_READ_THREADS`.
pub fn shards_in_flight_do_not_change_the_index(e: &Fmgrep) {
    const TERMS: [&str; 3] = ["printk", "aaaa", "missing"];
    for &store_docs in e.store_docs {
        let (docs_dir, _docs) = stage();
        let out_root = tempfile::tempdir().unwrap();
        let mut cells = Vec::new();
        for (threads, shard_threads) in [("1", "3"), ("4", "1")] {
            let index_dir = out_root.path().join(format!("idx-{threads}"));
            let out = e
                .command()
                .args(index_argv(
                    docs_dir.path(),
                    &index_dir,
                    store_docs,
                    &["--json"],
                ))
                .env("FMGREP_SHARD_BYTES", "20")
                .env("FMGREP_BUILD_THREADS", threads)
                .env("FMGREP_BUILD_SHARD_THREADS", shard_threads)
                .env("FMGREP_READ_THREADS", threads)
                .output()
                .expect("spawn");
            assert!(
                out.status.success(),
                "index: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let manifest = manifest_of(&index_dir);
            assert!(
                manifest["blobs"].as_array().unwrap().len() > 2,
                "{store_docs} threads={threads}"
            );
            let report = do_query(e, &index_dir, false, &TERMS);
            let hits: Vec<_> = report["queries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|q| {
                    (
                        q["term"].clone(),
                        q["total_matches"].clone(),
                        q["docs_matched"].clone(),
                    )
                })
                .collect();
            let files: Vec<(String, Vec<u8>)> = root_files(&index_dir)
                .into_iter()
                .filter(|f| f != "manifest.json")
                .map(|f| (f.clone(), std::fs::read(index_dir.join(&f)).unwrap()))
                .collect();
            cells.push((
                files,
                manifest["blobs"].clone(),
                manifest["blob_lens"].clone(),
                manifest["index_size_bytes"].clone(),
                hits,
            ));
        }
        assert_eq!(cells[0].0, cells[1].0, "{store_docs} index files");
        assert_eq!(cells[0].1, cells[1].1, "{store_docs} blobs");
        assert_eq!(cells[0].2, cells[1].2, "{store_docs} blob_lens");
        assert_eq!(cells[0].3, cells[1].3, "{store_docs} index_size_bytes");
        assert_eq!(cells[0].4, cells[1].4, "{store_docs} counts");
    }
}

/// Files source: a file changed after `index` is counted from its current
/// bytes by every term, under `--occurrences` too.
pub fn stale_file_is_scanned_in_full(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        let (docs_dir, mut docs) = stage();
        let index_dir = index_dir_of(docs_dir.path());
        do_index(e, docs_dir.path(), store_docs);
        docs[0] = b"printk changed\nprintk aaaa printk\n".to_vec();
        std::fs::write(docs_dir.path().join("a.bin"), &docs[0]).unwrap();
        docs[2] = b"no more".to_vec();
        std::fs::write(docs_dir.path().join("c.bin"), &docs[2]).unwrap();
        for (regex, terms) in [
            (false, &["printk", "aa", "missing"][..]),
            (true, &["pr[i1]ntk", "a+"]),
        ] {
            for occurrences in [false, true] {
                let mut argv = query_argv(&index_dir, regex, terms);
                if occurrences {
                    argv.insert(0, "--occurrences".into());
                }
                let out = e.command().args(&argv).output().unwrap();
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(out.status.success(), "{store_docs}: {stderr}");
                assert!(!stderr.contains("warning"), "{store_docs}: {stderr}");
                let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                for (q, term) in report["queries"].as_array().unwrap().iter().zip(terms) {
                    let (total, matched) = oracle(&docs, term, regex);
                    assert_eq!(
                        q["total_matches"], total,
                        "{store_docs} {term} occurrences={occurrences}"
                    );
                    let want = if occurrences {
                        serde_json::Value::Null
                    } else {
                        matched.into()
                    };
                    assert_eq!(
                        q["docs_matched"], want,
                        "{store_docs} {term} occurrences={occurrences}"
                    );
                }
            }
        }
    }
}

/// Files source: a deleted file is excluded from every term, with one
/// warning per process.
pub fn missing_file_is_warned_once_and_excluded(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        let (docs_dir, docs) = stage();
        let index_dir = index_dir_of(docs_dir.path());
        do_index(e, docs_dir.path(), store_docs);
        std::fs::remove_file(docs_dir.path().join("a.bin")).unwrap();
        for (regex, terms) in [(false, ["printk", "hello"]), (true, ["pr[i1]ntk", "h.llo"])] {
            let out = e
                .command()
                .args(query_argv(&index_dir, regex, &terms))
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{store_docs}: {stderr}");
            assert_eq!(
                stderr.matches("warning:").count(),
                1,
                "{store_docs}: {stderr}"
            );
            assert!(
                stderr.contains("warning: excluding") && stderr.contains("a.bin"),
                "{store_docs}: {stderr}"
            );
            let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            for (q, term) in report["queries"].as_array().unwrap().iter().zip(terms) {
                let (total, matched) = oracle(&docs[1..], term, regex);
                assert_eq!(q["total_matches"], total, "{store_docs} {term}");
                assert_eq!(q["docs_matched"], matched, "{store_docs} {term}");
            }
        }
    }
}

/// Files source: a `.fmgrep` index follows its tree when the tree moves; an
/// `--output` index names its recorded root once the tree is gone.
pub fn moved_tree_answers_through_its_dotdir(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        let (docs_dir, docs) = stage();
        let out_root = tempfile::tempdir().unwrap();
        let index_dir = out_root.path().join("idx");
        for out in [
            e.run(&index_argv(docs_dir.path(), &index_dir, store_docs, &[])),
            e.run_in(docs_dir.path(), &index_here(store_docs, &[])),
        ] {
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let moved = out_root.path().join("moved");
        std::fs::rename(docs_dir.path(), &moved).unwrap();
        let err = query_err(e, &index_dir, true, &["pr[i1]ntk"]);
        assert!(err.contains("is not a directory"), "{store_docs}: {err}");
        let out = e.run_in(&moved, &["--json", "--regex", "--", "pr[i1]ntk"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            report["queries"][0]["total_matches"],
            oracle(&docs, "pr[i1]ntk", true).0,
            "{store_docs}"
        );
        // Moved back so the temp dir cleans up where it was created.
        std::fs::rename(&moved, docs_dir.path()).unwrap();
    }
}

/// `index` with no `--output` writes `.fmgrep`, which a search finds from
/// any directory below, printing paths relative to the indexed folder.
pub fn index_defaults_to_a_dotdir_found_from_subdirectories(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        let (docs_dir, docs) = stage();
        let root = docs_dir.path();
        let sub = root.join("sub/deeper");
        std::fs::create_dir_all(&sub).unwrap();
        let out = e.run_in(root, &index_here(store_docs, &["--json"]));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let summary: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(
            summary["path"].as_str().unwrap().ends_with(".fmgrep"),
            "{summary}"
        );
        assert_eq!(summary["num_files"], docs.len() as u64, "{store_docs}");
        let dotdir = root.join(".fmgrep").display().to_string();
        let want = e.run(&["--index", &dotdir, "-l", "printk"]);
        assert!(
            want.status.success() && !want.stdout.is_empty(),
            "{store_docs}"
        );
        let root_arg = root.display().to_string();
        for (dir, args) in [
            (sub.as_path(), &["-l", "printk"][..]),
            (root, &["-l", "printk"]),
            (Path::new("/"), &["--index", &root_arg, "-l", "printk"]),
        ] {
            let got = e.run_in(dir, args);
            assert_eq!(
                (got.status.code(), got.stdout),
                (Some(0), want.stdout.clone()),
                "{store_docs} from {}: {}",
                dir.display(),
                String::from_utf8_lossy(&got.stderr)
            );
        }
        let report = do_query(e, &sub, false, &["printk"]);
        assert_eq!(
            report["queries"][0]["total_matches"],
            oracle(&docs, "printk", false).0,
            "{store_docs}"
        );
    }
}

/// With no index at or above the cwd a search is refused, even for the word
/// `index` after `--`.
pub fn no_index_above_the_cwd_is_refused(e: &Fmgrep) {
    let dir = tempfile::tempdir().unwrap();
    let canon = std::fs::canonicalize(dir.path()).unwrap();
    if canon.ancestors().any(|a| a.join(".fmgrep").is_dir()) {
        return;
    }
    for args in [&["printk"][..], &["--", "index"]] {
        let out = e.run_in(dir.path(), args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("no .fmgrep index in"), "{args:?}: {stderr}");
        assert!(!dir.path().join(".fmgrep").exists());
    }
}

/// A `.fmgrep` anywhere but directly under the indexed folder is refused:
/// a search would read the wrong tree through it.
pub fn dotdir_output_outside_the_folder_is_refused(e: &Fmgrep) {
    let (docs_dir, _docs) = stage();
    let other = tempfile::tempdir().unwrap();
    let foreign = other.path().join(".fmgrep");
    let index = |output: &Path| e.run(&index_argv(docs_dir.path(), output, false, &[]));
    let out = index(&foreign);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("outside"), "{stderr}");
    assert!(!foreign.exists());
    let out = index(&docs_dir.path().join(".fmgrep"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Files source: where a pattern's anchor is common, a document holding the
/// anchor but no match is never read.
pub fn a_common_anchor_reads_only_matching_docs(e: &Fmgrep) {
    use std::os::unix::fs::PermissionsExt;
    const PATTERN: &str = "union[ \\t]+\\{";
    for &store_docs in e.store_docs {
        let docs_dir = tempfile::tempdir().unwrap();
        let docs = [
            b"union x\n".repeat(8),
            b"union {\n".to_vec(),
            b"none\n".to_vec(),
        ];
        for (name, bytes) in ["a.c", "b.c", "c.c"].iter().zip(&docs) {
            std::fs::write(docs_dir.path().join(name), bytes).unwrap();
        }
        let index_dir = index_dir_of(docs_dir.path());
        do_index(e, docs_dir.path(), store_docs);
        let unread = docs_dir.path().join("a.c");
        std::fs::set_permissions(&unread, std::fs::Permissions::from_mode(0o000)).unwrap();
        let run = |min: &str, argv: &[String]| {
            let var = "FMGREP_AUTOMATON_MIN_ANCHOR_COUNT";
            e.command().args(argv).env(var, min).output().unwrap()
        };
        let json = query_argv(&index_dir, true, &[PATTERN]);
        let out = run("4", &json);
        assert!(
            out.status.success(),
            "{store_docs}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let (total, matched) = oracle(&docs, PATTERN, true);
        assert_eq!(report["queries"][0]["total_matches"], total, "{store_docs}");
        assert_eq!(
            report["queries"][0]["docs_matched"], matched,
            "{store_docs}"
        );
        let index = index_dir.display().to_string();
        let count = ["--index", &index, "-c", &format!("/{PATTERN}/")].map(String::from);
        let out = run("4", &count);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "b.c:1\n",
            "{store_docs}"
        );
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("excluding"),
            "{store_docs}"
        );
        // Control arm: the anchor prune reads `a.c`, which is excluded.
        let out = run(&u64::MAX.to_string(), &json);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("warning: excluding") && stderr.contains("a.c"),
            "{store_docs}: {stderr}"
        );
        std::fs::set_permissions(&unread, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
}

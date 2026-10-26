use super::*;

#[test]
fn regex_marker_runs_every_term_as_pattern_verbatim() {
    // Under --regex even a `/.../`-wrapped term is a pattern verbatim: the
    // marker wins over the slash convention.
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let folder = stage_corpus(
            tmp.path(),
            &[("a.c", b"printk hello printk"), ("b.c", b"aaaa")],
        );
        let output = tmp.path().join("idx");
        index_it(&folder, &output, store_docs);
        let out = query_it(&output, true, &["/printk/", "pr[i1]ntk", "aa"]);
        assert_eq!(out.queries[0].term, "/printk/", "store_docs={store_docs}");
        assert_eq!(out.queries[0].total_matches, 0, "store_docs={store_docs}");
        assert_eq!(out.queries[1].total_matches, 2, "store_docs={store_docs}");
        // Non-overlapping: `aa` on `aaaa` is 2.
        assert_eq!(out.queries[2].total_matches, 2, "store_docs={store_docs}");
        assert_eq!(out.queries[2].docs_matched, 1, "store_docs={store_docs}");
    }
}

#[test]
fn human_driven_slash_terms_take_the_regex_path() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let folder = stage_corpus(tmp.path(), &[("a.c", b"printk hello printk")]);
        let output = tmp.path().join("idx");
        index_it(&folder, &output, store_docs);
        let out = query_it(&output, false, &["/pr[i1]ntk/", "printk"]);
        assert_eq!(
            out.queries[0].term, "/pr[i1]ntk/",
            "store_docs={store_docs}"
        );
        assert_eq!(out.queries[0].total_matches, 2, "store_docs={store_docs}");
        assert_eq!(out.queries[1].total_matches, 2, "store_docs={store_docs}");
    }
}

#[test]
fn invalid_patterns_fail_loudly_never_silent() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let folder = stage_corpus(tmp.path(), &[("a.c", b"printk hello")]);
        let output = tmp.path().join("idx");
        index_it(&folder, &output, store_docs);
        let err = query_it_err(&output, true, &["printk", "(["]);
        assert!(
            err.starts_with("error: invalid regex pattern"),
            "got: {err}"
        );
        assert!(err.contains("(["), "must name the pattern: {err}");
        // `//` strips to the empty pattern: loud rejection.
        let err = query_it_err(&output, false, &["//"]);
        assert!(err.contains("empty"), "must reject empty: {err}");
    }
}

#[test]
fn literal_query_needs_no_raw_bytes() {
    // Literals answer from the blobs alone, NUL terms included, so every
    // literal still answers with the docs shard renamed away.
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(
        tmp.path(),
        &[("a.c", b"printk hello printk"), ("b.c", b"nothing here")],
    );
    let output = tmp.path().join("idx");
    index_it(&folder, &output, true);
    let out = query_it(&output, false, &["printk", "missing", "hello"]);
    assert_eq!(
        [
            out.queries[0].total_matches,
            out.queries[1].total_matches,
            out.queries[2].total_matches
        ],
        [2, 0, 1]
    );
    assert_eq!(out.queries[0].docs_matched, 1);
    assert_eq!(out.num_files_indexed, 2);
    std::fs::rename(output.join(DOCS_SHARD), output.join("hidden")).unwrap();
    assert_eq!(
        query_it(&output, false, &["printk"]).queries[0].total_matches,
        2
    );
    assert_eq!(
        query_it(&output, false, &["a\0b"]).queries[0].total_matches,
        0
    );
}

#[test]
fn regex_fetches_only_unpruned_candidates() {
    // `printk` anchors doc 0 and prunes doc 1; a fully-pruned pattern needs
    // no shard at all.
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(
        tmp.path(),
        &[("a.c", b"printk hello printk"), ("b.c", b"nothing here")],
    );
    let output = tmp.path().join("idx");
    index_it(&folder, &output, true);
    let out = query_it(&output, true, &["printk", "pr[i1]ntk", "missing"]);
    assert_eq!(out.queries[0].total_matches, 2);
    assert_eq!(out.queries[0].docs_matched, 1);
    assert_eq!(out.queries[1].total_matches, 2);
    assert_eq!(out.queries[2].total_matches, 0);
    std::fs::rename(output.join(DOCS_SHARD), output.join("hidden")).unwrap();
    // A fully-pruned pattern is exact with no shard; a candidate-bearing one
    // is loud.
    assert_eq!(
        query_it(&output, true, &["missing"]).queries[0].total_matches,
        0
    );
    let err = query_it_err(&output, true, &["printk"]);
    assert!(
        err.contains("cannot read") && err.contains(DOCS_SHARD),
        "got: {err}"
    );
    std::fs::rename(output.join("hidden"), output.join(DOCS_SHARD)).unwrap();
    // Regex answers follow the live shard bytes: candidate reads, not blob
    // replays.
    let shard = std::fs::read(output.join(DOCS_SHARD)).unwrap();
    let mut swapped = b"zzzz zzzz zzzz zzzz".to_vec();
    swapped.extend_from_slice(b"nothing here");
    assert_eq!(swapped.len(), shard.len());
    std::fs::write(output.join(DOCS_SHARD), &swapped).unwrap();
    assert_eq!(
        query_it(&output, true, &["printk"]).queries[0].total_matches,
        0,
        "regex must follow the swapped bytes"
    );
    // ...while literals stay blob-pinned.
    assert_eq!(
        query_it(&output, false, &["printk"]).queries[0].total_matches,
        2,
        "literal must stay blob-pinned"
    );
}

/// Files-layout and stored-docs indexes of `files`, then `a.c` made unopenable:
/// it still stats as indexed, but a fetch excludes it.
fn unreadable_files_index(tmp: &Path, files: &[(&str, &[u8])]) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let folder = stage_corpus(tmp, files);
    let (output, shard) = (tmp.join("idx"), tmp.join("idx-shard"));
    index_it(&folder, &output, false);
    index_it(&folder, &shard, true);
    std::fs::set_permissions(folder.join("a.c"), std::fs::Permissions::from_mode(0o000)).unwrap();
    (output, shard)
}

#[test]
fn literal_pattern_counts_without_fetch() {
    let files: [(&str, &[u8]); 4] = [
        ("a.c", b"\xef\xbb\xbfprintk( x printk("),
        ("b.c", b"printk(\nprintk"),
        ("c.c", b""),
        ("d.c", b"nothing here"),
    ];
    let prepared = PreparedPattern::prepare("printk\\(").unwrap();
    let per_doc: Vec<u64> = files
        .iter()
        .map(|(_, b)| prepared.count(crate::matching::strip_bom(b)))
        .collect();
    let tmp = tempfile::tempdir().unwrap();
    let (output, shard) = unreadable_files_index(tmp.path(), &files);
    let out = query_it(&output, false, &["/printk\\(/"]);
    let q = &out.queries[0];
    assert_eq!(q.total_matches, per_doc.iter().sum::<u64>());
    assert_eq!(q.docs_matched, per_doc.iter().filter(|&&c| c > 0).count());
    assert!(!q.counts_exact);
    // A stored-docs index keeps reading its live docs.shard.
    std::fs::rename(shard.join(DOCS_SHARD), shard.join("hidden")).unwrap();
    assert!(query_it_err(&shard, false, &["/printk\\(/"]).contains("cannot read"));
}

#[test]
fn non_literal_patterns_still_fetch() {
    let files: [(&str, &[u8]); 1] = [("a.c", "aaaa caf\u{e9} a\nb x".as_bytes())];
    let tmp = tempfile::tempdir().unwrap();
    let (output, _) = unreadable_files_index(tmp.path(), &files);
    // The only doc matches every term: a zero proves the fetch.
    for term in ["/aa/", "/caf\u{e9}/", "/a\nb/"] {
        assert_eq!(
            query_it(&output, false, &[term]).queries[0].total_matches,
            0,
            "{term:?}"
        );
    }
    let out = query(&output, &["-i"], &["caf"]).unwrap();
    assert_eq!(out.queries[0].total_matches, 0, "-i must fetch");
}

#[test]
fn empty_docs_index_and_query_exactly() {
    // Zero-byte docs index and answer exact zeros, and coverage still
    // reports every input file.
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let folder = stage_corpus(
            tmp.path(),
            &[
                ("e-lead.c", b"".as_slice()),
                ("a.c", b"printk hello printk".as_slice()),
                ("e-mid.c", b"".as_slice()),
                ("b.c", b"nothing here".as_slice()),
            ],
        );
        let output = tmp.path().join("idx");
        assert_eq!(
            index_it(&folder, &output, store_docs).num_files,
            4,
            "store_docs={store_docs}"
        );
        let manifest = read_manifest(&output).unwrap();
        assert_eq!(manifest.num_docs, 4, "store_docs={store_docs}");
        // Discovery sorts by relative path: the empties land at slots 2-3.
        assert_eq!(
            manifest.doc_lens,
            vec![19, 12, 0, 0],
            "store_docs={store_docs}"
        );
        assert_eq!(
            manifest.blobs,
            ["shard-00000000-00000004.bin"],
            "store_docs={store_docs}"
        );
        let out = query_it(&output, false, &["printk", "missing"]);
        assert_eq!(out.queries[0].total_matches, 2, "store_docs={store_docs}");
        assert_eq!(out.queries[0].docs_matched, 1, "store_docs={store_docs}");
        assert_eq!(out.queries[1].total_matches, 0, "store_docs={store_docs}");
        assert_eq!(out.num_files_indexed, 4, "store_docs={store_docs}");
        let out = query_it(&output, true, &["pr[i1]ntk"]);
        assert_eq!(out.queries[0].total_matches, 2, "store_docs={store_docs}");
    }
}

#[test]
fn blob_arity_skew_rejected_before_any_blob_byte() {
    // The labels must partition the docs; an inventory covering fewer is a
    // tampered index, refused before the blob shard is read.
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), &[("a.c", b"hello"), ("b.c", b"bye")]);
    let output = tmp.path().join("idx");
    index_it(&folder, &output, true);
    let path = output.join("manifest.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    v["blobs"] = serde_json::json!(["shard-00000000-00000001.bin"]);
    v["blob_lens"] = serde_json::json!([5]);
    std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
    // Lengths kept lockstep with the shard, so the coverage is what refuses.
    std::fs::write(output.join(BLOBS_SHARD), b"hello").unwrap();
    let err = query_it_err(&output, false, &["hello"]);
    assert!(err.contains("tampered"), "got: {err}");
    assert!(err.contains("1 blob(s)"), "got: {err}");
}

#[test]
fn unsupported_format_rejected_at_query() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), &[("a.c", b"printk hello")]);
    let output = tmp.path().join("idx");
    index_it(&folder, &output, true);
    let path = output.join("manifest.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    v["format"] = serde_json::json!(12);
    std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
    let err = query_it_err(&output, false, &["printk"]);
    assert!(err.contains("unsupported manifest format 12"), "got: {err}");
}

#[test]
fn coverage_skew_refuses_to_report() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), &[("a.c", b"hello")]);
    let output = tmp.path().join("idx");
    index_it(&folder, &output, true);
    let path = output.join("manifest.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    v["num_docs"] = serde_json::json!(7);
    std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
    let err = query_it_err(&output, false, &["hello"]);
    assert!(err.contains("refusing to report skew"), "got: {err}");
}

#[test]
fn old_format_manifest_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("manifest.json"),
        serde_json::to_string(&serde_json::json!({
            "format": 1,
            "engine": "fmgrep", "store_docs": true,
            "index_name": "fmgrep", "num_docs": 0, "num_files_indexed": 0,
            "num_bytes": 0, "bom_bytes": 0, "index_size_bytes": 0,
            "build_time_us": 0, "files": [], "blobs": [], "doc_lens": [],
            "blob_lens": [],
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(tmp.path().join(BLOBS_SHARD), b"").unwrap();
    let err = query_it_err(tmp.path(), false, &["printk"]);
    assert!(
        err.contains("unsupported manifest format 1") && err.contains("re-run `index`"),
        "got: {err}"
    );
}

#[test]
fn counts_exact_marks_only_rg_exact_terms() {
    // Counts mode reports FM document-wide numbers; the flag says when they
    // are also rg's.
    let want = [
        ("printk", true),
        ("aa", false),
        ("/pr[i1]ntk/", false),
        ("a\nb", false),
    ];
    let terms: Vec<&str> = want.iter().map(|&(t, _)| t).collect();
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let folder = stage_corpus(tmp.path(), &[("a.c", b"printk\naaaa")]);
        let output = tmp.path().join("idx");
        index_it(&folder, &output, store_docs);
        let out = query_it(&output, false, &terms);
        for (i, &(term, exact)) in want.iter().enumerate() {
            assert_eq!(out.queries[i].term, term, "store_docs={store_docs}");
            assert_eq!(
                out.queries[i].counts_exact, exact,
                "store_docs={store_docs} term={term:?}"
            );
        }
    }
}

#[test]
fn batch_drive_matches_per_term_answers() {
    // One probe pass must answer what a per-term scan of the corpus gives,
    // for both blob arities and a mixed literal/regex run.
    let files: [(&str, &[u8]); 3] = [
        ("a.c", b"printk hello printk"),
        ("b.c", b"aaaa"),
        ("c.c", b"tail printk"),
    ];
    let want = [
        ("printk", 3u64, 2usize),
        ("/pr[i1]ntk/", 3, 2),
        // Literals count overlapping, regexes do not.
        ("aa", 3, 1),
        ("/aa/", 2, 1),
        ("/no|tail/", 1, 1),
        ("missing", 0, 0),
    ];
    let terms: Vec<&str> = want.iter().map(|&(t, _, _)| t).collect();
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let folder = stage_corpus(tmp.path(), &files);
        let output = tmp.path().join("idx");
        index_it(&folder, &output, store_docs);
        let out = query_it(&output, false, &terms);
        for (i, &(term, total, docs)) in want.iter().enumerate() {
            assert_eq!(out.queries[i].term, term, "store_docs={store_docs}");
            assert_eq!(
                out.queries[i].total_matches, total,
                "store_docs={store_docs} {term}"
            );
            assert_eq!(
                out.queries[i].docs_matched, docs,
                "store_docs={store_docs} {term}"
            );
        }
    }
}

#[test]
fn ignore_case_runs_every_term_as_a_pattern() {
    // `-i` turns a bare term into the escaped pattern of itself, so counts
    // come from the regex over fetched candidates, never from a literal
    // probe — and the metachars of a bare term stay literal.
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let folder = stage_corpus(
            tmp.path(),
            &[("a.c", b"HscEnv and hscenv\n"), ("b.c", b"HSCENVX h.c\n")],
        );
        let output = tmp.path().join("idx");
        index_it(&folder, &output, store_docs);
        let out = query(&output, &["-i"], &["hscenv", "h.c"]).unwrap();
        assert_eq!(out.queries[0].total_matches, 3, "store_docs={store_docs}");
        assert_eq!(out.queries[0].docs_matched, 2, "store_docs={store_docs}");
        // `.` stays literal, so `h.c` matches the text `h.c` only.
        assert_eq!(out.queries[1].total_matches, 1, "store_docs={store_docs}");
        // Counts under `-i` are the regex's, never the FM-index's literal
        // ones.
        assert!(!out.queries[0].counts_exact, "store_docs={store_docs}");
    }
}

/// `-i` must not change which argv is accepted: the empty bare term still
/// matches nothing, as it does without `-i`.
#[test]
fn ignore_case_keeps_the_empty_term_matching_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), &[("a.c", b"HscEnv\n")]);
    let output = tmp.path().join("idx");
    index_it(&folder, &output, true);
    for flags in [&[][..], &["-i"]] {
        let out = query(&output, flags, &[""]).unwrap();
        assert_eq!(out.queries[0].total_matches, 0, "flags={flags:?}");
    }
}

/// A files-layout index over two docs; the corpus folder and index dir.
fn files_index(tmp: &Path) -> (PathBuf, PathBuf) {
    let folder = stage_corpus(
        tmp,
        &[("a.c", b"printk hello printk"), ("b.c", b"nothing here")],
    );
    let output = tmp.join("idx");
    index_it(&folder, &output, false);
    (folder, output)
}

fn totals(r: &OutputReport) -> Vec<(u64, usize, bool)> {
    r.queries
        .iter()
        .map(|q| (q.total_matches, q.docs_matched, q.counts_exact))
        .collect()
}

#[test]
fn stale_doc_counts_its_current_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let (folder, output) = files_index(tmp.path());
    std::fs::write(folder.join("a.c"), b"printk changed\nprintk printk\n").unwrap();
    for (regex, term) in [(false, "printk"), (true, "pr[i1]ntk")] {
        assert_eq!(
            totals(&query_it(&output, regex, &[term])),
            [(3, 1, !regex)],
            "{term}"
        );
    }
    assert_eq!(printed(&output, &["-c"], &["printk"]), "a.c:2\n");
    assert_eq!(
        printed(&output, &["-c", "--regex"], &["pr[i1]ntk"]),
        "a.c:2\n"
    );
}

#[test]
fn stale_doc_outside_the_indexed_answer_is_found() {
    let tmp = tempfile::tempdir().unwrap();
    let (folder, output) = files_index(tmp.path());
    std::fs::write(folder.join("b.c"), b"nothing here, printk now").unwrap();
    assert_eq!(
        totals(&query_it(&output, false, &["printk", "missing"])),
        [(3, 2, true), (0, 0, true)]
    );
    assert_eq!(
        totals(&query_it(&output, true, &["pr[i1]ntk"])),
        [(3, 2, false)]
    );
}

#[test]
fn missing_doc_is_excluded_from_every_term() {
    let tmp = tempfile::tempdir().unwrap();
    let (folder, output) = files_index(tmp.path());
    std::fs::remove_file(folder.join("a.c")).unwrap();
    let out = query_it(&output, false, &["printk", "/pr[i1]ntk/", "here"]);
    assert_eq!(totals(&out), [(0, 0, true), (0, 0, false), (1, 1, true)]);
    assert_eq!(printed(&output, &["-l"], &["/[a-z]+/"]), "b.c\n");
}

#[test]
fn stale_tree_answers_like_a_fresh_index() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(
        tmp.path(),
        &[
            ("a.c", b"printk one\nprintk two\n"),
            ("b.c", b"nothing\n"),
            ("c.c", b"aaaa printk\n"),
            ("d.c", b"printk gone\n"),
            ("e.c", b"printk kept\naaa\n"),
        ],
    );
    let stale = tmp.path().join("idx");
    index_it(&folder, &stale, false);
    std::fs::write(folder.join("a.c"), b"printk one\nprintk printk\nprintk\n").unwrap();
    std::fs::write(folder.join("b.c"), b"nothing\nnow printk, aa\n").unwrap();
    std::fs::write(folder.join("c.c"), b"").unwrap();
    std::fs::remove_file(folder.join("d.c")).unwrap();
    let fresh = tmp.path().join("fresh");
    index_it(&folder, &fresh, false);
    let terms = ["printk", "/pr[i1]ntk/", "aa", "/a+/"];
    assert_eq!(
        totals(&query_it(&stale, false, &terms)),
        totals(&query_it(&fresh, false, &terms))
    );
    for flags in [
        &["-c"][..],
        &["--count-matches"],
        &["-l"],
        &["--matches"],
        &["-o", "-C", "1"],
    ] {
        for term in ["printk", "/pr[i1]ntk/", "aa"] {
            assert_eq!(
                printed(&stale, flags, &[term]),
                printed(&fresh, flags, &[term]),
                "{flags:?} {term}"
            );
        }
    }
}

#[test]
fn added_tree_answers_like_a_fresh_index() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(
        tmp.path(),
        &[
            ("a.c", b"printk one\n"),
            ("c.c", b"aaaa printk\n"),
            ("d.c", b"printk gone\n"),
            ("e.c", b"printk kept\naaa\n"),
        ],
    );
    let stale = tmp.path().join("idx");
    index_it(&folder, &stale, false);
    std::fs::write(folder.join("b.c"), b"printk added\nprintk printk aa\n").unwrap();
    std::fs::create_dir_all(folder.join("new/deep")).unwrap();
    std::fs::write(folder.join("new/deep/n.c"), b"deep printk\n").unwrap();
    std::fs::write(folder.join("bin.dat"), b"printk\0").unwrap();
    std::fs::write(folder.join("gone.c"), b"printk\n").unwrap();
    std::fs::remove_file(folder.join("gone.c")).unwrap();
    std::fs::remove_file(folder.join("d.c")).unwrap();
    std::fs::write(folder.join("d.c"), b"back printk printk\n").unwrap();
    let fresh = tmp.path().join("fresh");
    index_it(&folder, &fresh, false);
    let terms = ["printk", "/pr[i1]ntk/", "aa", "/a+/", "deep"];
    assert_eq!(
        totals(&query_it(&stale, false, &terms)),
        totals(&query_it(&fresh, false, &terms))
    );
    for flags in [
        &["-c"][..],
        &["--count-matches"],
        &["-l"],
        &["--matches"],
        &["-o", "-C", "1"],
    ] {
        for term in ["printk", "/pr[i1]ntk/", "aa"] {
            assert_eq!(
                printed(&stale, flags, &[term]),
                printed(&fresh, flags, &[term]),
                "{flags:?} {term}"
            );
        }
    }
    assert_eq!(
        printed(&stale, &["-l"], &["printk"]),
        "a.c\nb.c\nc.c\nd.c\ne.c\nnew/deep/n.c\n"
    );
}

#[cfg(unix)]
#[test]
fn pruned_added_tree_answers_like_a_fresh_index() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    let put = |root: &Path, rel: &str, bytes: &[u8]| {
        std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
        std::fs::write(root.join(rel), bytes).unwrap();
    };
    for (rel, bytes) in [
        ("a.c", &b"printk one\n"[..]),
        ("d.c", b"printk gone\n"),
        ("kb/k.dat", b"printk\0binary"),
        ("mw/m.c", b"printk moved\n"),
        ("sub/c.c", b"aaaa printk\n"),
        ("tb.c", b"printk text\n"),
    ] {
        put(&folder, rel, bytes);
    }
    let tree = tmp.path().join("tree");
    put(&tree, "t/deep/x.c", b"deep printk\n");
    put(&tree, "t/y.c", b"printk aa\n");
    backdate(&folder);
    backdate(&tree);
    let stale = tmp.path().join("idx");
    index_it(&folder, &stale, false);
    let recorded = crate::source::Recorded::open(&stale).unwrap();
    assert!(recorded.is_some_and(|r| r.is_dir("kb")));
    put(&folder, "sub/new.c", b"printk new\n");
    put(&folder, "b.c", b"printk added\nprintk printk aa\n");
    put(&folder, "new/deep/n.c", b"deep printk\n");
    std::fs::rename(tree.join("t"), folder.join("sub/t")).unwrap();
    std::fs::write(folder.join("kb/k.dat"), b"printk text now\n").unwrap();
    set_mtime(&folder.join("kb"), T0 + std::time::Duration::from_secs(1));
    std::fs::write(folder.join("tb.c"), b"printk\0now binary").unwrap();
    std::fs::rename(folder.join("mw"), folder.join("mv2")).unwrap();
    put(&folder, "gone.c", b"printk\n");
    std::fs::remove_file(folder.join("gone.c")).unwrap();
    std::fs::remove_file(folder.join("d.c")).unwrap();
    let want = "a.c\nb.c\nkb/k.dat\nmv2/m.c\nnew/deep/n.c\nsub/c.c\nsub/new.c\nsub/t/deep/x.c\nsub/t/y.c\n";
    assert_eq!(
        printed(&stale, &["-l"], &["printk"]),
        want,
        "kb/k.dat, turned text, is found"
    );
    std::fs::write(folder.join("kb/k.dat"), b"printk\0binary").unwrap();
    let fresh = tmp.path().join("fresh");
    index_it(&folder, &fresh, false);
    let terms = ["printk", "/pr[i1]ntk/", "aa", "/a+/", "deep"];
    assert_eq!(
        totals(&query_it(&stale, false, &terms)),
        totals(&query_it(&fresh, false, &terms))
    );
    for flags in [
        &["-c"][..],
        &["--count-matches"],
        &["-l"],
        &["--matches"],
        &["-o", "-C", "1"],
    ] {
        for term in ["printk", "/pr[i1]ntk/", "aa", "deep", "now"] {
            assert_eq!(
                printed(&stale, flags, &[term]),
                printed(&fresh, flags, &[term]),
                "{flags:?} {term}"
            );
        }
    }
}

#[test]
fn walk_record_missing_falls_back_and_malformed_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (folder, output) = files_index(tmp.path());
    std::fs::create_dir(folder.join("sub")).unwrap();
    std::fs::write(folder.join("sub/x.c"), b"printk added").unwrap();
    std::fs::remove_file(output.join("dirs.stamps")).unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nsub/x.c\n");
    std::fs::write(output.join("dirs.stamps"), b"fmdirs\0\x01\x01").unwrap();
    let err = query_it_err(&output, false, &["printk"]);
    assert!(
        err.contains("dirs.stamps") && err.contains("tampered"),
        "got: {err}"
    );
}

#[test]
fn index_dir_under_root_is_not_an_added_file() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), &[("a.c", b"printk\n")]);
    let output = folder.join("idx");
    index_it(&folder, &output, false);
    let got = totals(&query_it(&output, false, &["corpus_root", "printk"]));
    assert_eq!(got.iter().map(|t| t.1).collect::<Vec<_>>(), [0, 1]);
}

#[test]
fn files_index_answers_like_the_stored_docs_index() {
    let tmp = tempfile::tempdir().unwrap();
    let (folder, output) = files_index(tmp.path());
    let shard = tmp.path().join("idx-shard");
    index_it(&folder, &shard, true);
    let terms = ["printk", "/pr[i1]ntk/", "/no|tail/", "missing"];
    let counts = |r: OutputReport| {
        r.queries
            .iter()
            .map(|q| (q.total_matches, q.docs_matched))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        counts(query_it(&output, false, &terms)),
        counts(query_it(&shard, false, &terms))
    );
    let blobs = |dir: &Path| std::fs::read(dir.join(BLOBS_SHARD)).unwrap();
    assert_eq!(blobs(&output), blobs(&shard));
}

#[test]
fn moved_tree_answers_through_its_dotdir() {
    let tmp = tempfile::tempdir().unwrap();
    let (folder, output) = files_index(tmp.path());
    index_it(&folder, &folder.join(INDEX_DIR), false);
    let moved = tmp.path().join("moved");
    std::fs::rename(&folder, &moved).unwrap();
    let err = query_it_err(&output, true, &["pr[i1]ntk"]);
    assert!(
        err.contains("is not a directory") && err.contains("fmgrep index"),
        "got: {err}"
    );
    for start in [moved.join(INDEX_DIR), moved.clone()] {
        let out = query_it(&start, true, &["pr[i1]ntk"]);
        assert_eq!(out.queries[0].total_matches, 2, "{}", start.display());
    }
}

#[test]
fn moved_stored_docs_index_needs_no_root() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), &[("a.c", b"printk")]);
    index_it(&folder, &folder.join(INDEX_DIR), true);
    let moved = tmp.path().join("moved");
    std::fs::rename(&folder, &moved).unwrap();
    let out = query_it(&moved, false, &["printk"]);
    assert_eq!(out.queries[0].total_matches, 1);
}

#[test]
fn missing_corpus_root_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (folder, output) = files_index(tmp.path());
    std::fs::remove_dir_all(&folder).unwrap();
    let err = query_it_err(&output, false, &["printk"]);
    let root = std::fs::canonicalize(tmp.path()).unwrap().join("corpus");
    assert!(err.contains(&root.display().to_string()), "got: {err}");
    std::fs::remove_file(output.join(crate::source::DOCS_STAMPS)).unwrap();
    let err = query_it_err(&output, false, &["printk"]);
    assert!(
        err.contains("cannot read") && err.contains("docs.stamps"),
        "got: {err}"
    );
}

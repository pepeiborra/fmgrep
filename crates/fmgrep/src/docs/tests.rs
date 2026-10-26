use super::*;

use std::ops::Range;

use crate::Document;

/// Everything the streamed corpus yields, as one already-final cut.
fn one_cut(shard: &Path, docs: &Docs, range: Range<usize>) -> Result<Vec<Document>, String> {
    let cuts = Cuts::closed(
        Some(shard.to_path_buf()),
        vec![range.clone()],
        &docs.lens,
        &docs.paths,
    )?;
    let cut = cuts.get(0).expect("a one-slot plan closes slot 0");
    cuts.read(&cut)
}

/// A corpus with nesting, an empty doc, a hidden entry and a NUL file (the
/// last two are skipped by the walk).
fn stage_corpus(dir: &Path) {
    std::fs::write(dir.join("b.c"), b"second doc").unwrap();
    std::fs::write(dir.join("a.c"), b"printk hello").unwrap();
    std::fs::write(dir.join("e.c"), b"").unwrap();
    std::fs::write(dir.join(".hidden"), b"pruned").unwrap();
    std::fs::write(dir.join("nul.bin"), b"has\x00nul").unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub").join("c.c"), b"nested").unwrap();
}

/// Stream with a build that consumes nothing: the write pass alone.
fn stream(folder: &Path, output: &Path) -> Docs {
    std::fs::create_dir_all(output).unwrap();
    stream_while(folder, output, u64::MAX, true, |_| Ok(()))
        .unwrap()
        .0
}

#[test]
fn stream_writes_the_shard_and_the_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    std::fs::create_dir_all(&folder).unwrap();
    stage_corpus(&folder);
    let shard = tmp.path().join("idx");
    let docs = stream(&folder, &shard);

    let oracle = crate::walk::discover_index_docs(&folder).unwrap();
    assert_eq!(docs.num_docs(), oracle.len());
    assert_eq!(
        docs.lens,
        oracle
            .iter()
            .map(|d| d.bytes.len() as u64)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        docs.paths,
        oracle.iter().map(|d| d.path.clone()).collect::<Vec<_>>()
    );
    let want: Vec<u8> = oracle.iter().flat_map(|d| d.bytes.clone()).collect();
    assert_eq!(std::fs::read(shard.join(DOCS_SHARD)).unwrap(), want);
    assert_eq!(docs.num_bytes, want.len() as u64);
}

#[test]
fn stream_never_reads_the_index_dir_it_writes() {
    // Default output is `<folder>/fmgrep`: the shard being written is not a
    // document of the corpus it holds.
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().to_path_buf();
    stage_corpus(&folder);
    let docs = stream(&folder, &folder.join("idx"));
    assert_eq!(
        docs.paths,
        vec!["a.c", "b.c", "e.c", "sub/c.c"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
    );
}

#[test]
fn read_returns_global_ids_and_paths_for_a_range() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    std::fs::create_dir_all(&folder).unwrap();
    stage_corpus(&folder);
    let idx = tmp.path().join("idx");
    let docs = stream(&folder, &idx);
    let all = crate::walk::discover_index_docs(&folder).unwrap();
    for start in 0..=all.len() {
        for end in start..=all.len() {
            let got = one_cut(&idx.join(DOCS_SHARD), &docs, start..end).unwrap();
            assert_eq!(got.len(), end - start, "{start}..{end}");
            for (doc, want) in got.iter().zip(&all[start..end]) {
                assert_eq!(
                    (doc.id, &doc.path, &doc.bytes),
                    (want.id, &want.path, &want.bytes)
                );
            }
        }
    }
}

#[test]
fn read_rejects_an_out_of_range_span() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    std::fs::create_dir_all(&folder).unwrap();
    stage_corpus(&folder);
    let idx = tmp.path().join("idx");
    let docs = stream(&folder, &idx);
    let n = docs.num_docs();
    let backwards = Range { start: 2, end: 1 };
    for range in [n..n + 1, 0..n + 5, backwards] {
        let err = one_cut(&idx.join(DOCS_SHARD), &docs, range.clone())
            .err()
            .unwrap_or_else(|| panic!("range {range:?} must be refused"));
        assert!(err.contains("out of range"), "got: {err}");
    }
}

/// A build that gives up is the reported error, not the stream's abort.
#[test]
fn a_failing_build_reports_its_own_error() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    std::fs::create_dir_all(&folder).unwrap();
    stage_corpus(&folder);
    let output = tmp.path().join("idx");
    std::fs::create_dir_all(&output).unwrap();
    let built = stream_while(&folder, &output, 1, true, |cuts| {
        cuts.get(0);
        Err::<(), String>("build refused".to_string())
    });
    let Err(err) = built else {
        panic!("a failing build must fail the stream");
    };
    assert_eq!(err, "build refused");
}

/// A stream that dies before its first cut still reports through the feed:
/// the build must see the I/O cause, not an empty plan.
#[test]
fn a_shard_that_cannot_be_created_is_the_reported_error() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    std::fs::create_dir_all(&folder).unwrap();
    stage_corpus(&folder);
    let missing = tmp.path().join("no").join("such").join("dir");
    let seen = std::sync::Mutex::new(None);
    let built = stream_while(&folder, &missing, 1, true, |cuts| {
        assert!(cuts.get(0).is_none(), "a dead stream closes no cut");
        *seen.lock().unwrap() = cuts.error();
        Ok(())
    });
    let Err(err) = built else {
        panic!("an uncreatable docs.shard must fail the stream");
    };
    assert!(err.contains("cannot write"), "got: {err}");
    assert_eq!(seen.into_inner().unwrap().as_deref(), Some(err.as_str()));
}

/// Every cut of a files-layout stream, read back by the build.
fn stream_files(folder: &Path, output: &Path) -> Result<(Docs, Vec<Document>), String> {
    std::fs::create_dir_all(output).unwrap();
    stream_while(folder, output, 1, false, |cuts| {
        let mut read = Vec::new();
        let mut slot = 0;
        while let Some(cut) = cuts.get(slot) {
            read.extend(cuts.read(&cut)?);
            slot += 1;
        }
        Ok(read)
    })
}

#[test]
fn files_stream_writes_no_docs_shard_and_records_stamps() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    std::fs::create_dir_all(&folder).unwrap();
    stage_corpus(&folder);
    let idx = tmp.path().join("idx");
    let (docs, _) = stream_files(&folder, &idx).unwrap();
    assert_eq!(std::fs::read_dir(&idx).unwrap().count(), 0, "no docs.shard");
    let root = std::fs::canonicalize(&folder).unwrap();
    assert_eq!(docs.root.as_deref(), root.to_str());
    for (i, path) in docs.paths.iter().enumerate() {
        let meta = std::fs::metadata(folder.join(path)).unwrap();
        let want = crate::Stamp::of(&meta);
        assert_eq!(
            (docs.lens[i], docs.mtimes[i]),
            (want.len, want.mtime_ns),
            "{path}"
        );
    }
}

#[test]
fn files_cut_reads_back_the_walk_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    std::fs::create_dir_all(&folder).unwrap();
    stage_corpus(&folder);
    let (_, read) = stream_files(&folder, &tmp.path().join("idx")).unwrap();
    let oracle = crate::walk::discover_index_docs(&folder).unwrap();
    assert_eq!(read.len(), oracle.len());
    for (got, want) in read.iter().zip(&oracle) {
        assert_eq!(
            (got.id, &got.path, &got.bytes),
            (want.id, &want.path, &want.bytes)
        );
    }
}

#[test]
fn file_changed_while_indexing_is_refused_by_path() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    std::fs::create_dir_all(&folder).unwrap();
    stage_corpus(&folder);
    let output = tmp.path().join("idx");
    std::fs::create_dir_all(&output).unwrap();
    // Grown between the walk and a re-read of its cut: the first read may be
    // the handed-over bytes, the second always reads the file.
    let built = stream_while(&folder, &output, 1, false, |cuts| {
        let cut = cuts.get(0).expect("slot 0");
        std::fs::write(folder.join(&cut.paths[0]), b"printk hello, changed").unwrap();
        cuts.read(&cut).and_then(|_| cuts.read(&cut))
    });
    let Err(err) = built else {
        panic!("a file changed while indexing must be refused");
    };
    assert!(
        err.contains("a.c") && err.contains("changed while indexing"),
        "got: {err}"
    );
}

#[test]
fn the_last_cut_is_handed_over_without_a_reread() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    std::fs::create_dir_all(&folder).unwrap();
    stage_corpus(&folder);
    let output = tmp.path().join("idx");
    std::fs::create_dir_all(&output).unwrap();
    let (_, read) = stream_while(&folder, &output, u64::MAX, false, |cuts| {
        let cut = cuts.get(0).expect("one byte cut");
        assert!(cuts.get(1).is_none(), "the stream has ended");
        std::fs::remove_dir_all(&folder).unwrap();
        let first = cuts.read(&cut)?;
        assert!(
            cuts.read(&cut).is_err(),
            "a second read re-reads the removed files"
        );
        Ok(first)
    })
    .unwrap();
    let paths: Vec<&str> = read.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(paths, ["a.c", "b.c", "e.c", "sub/c.c"]);
    assert_eq!(read[0].bytes, b"printk hello");
}

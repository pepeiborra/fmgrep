use super::*;

use crate::Stamp;

/// A staged document source: `path` is the shard or the corpus root.
struct Fix {
    store_docs: bool,
    path: PathBuf,
    lens: Vec<u64>,
    files: Vec<String>,
    /// Holds `docs.stamps` for the files layout.
    index: PathBuf,
}

/// `docs.shard`, then the files layout.
const SOURCES: [bool; 2] = [true, false];

fn stage(dir: &Path, docs: &[&[u8]], store_docs: bool) -> Fix {
    if store_docs {
        stage_shard(dir, docs)
    } else {
        stage_files(dir, docs)
    }
}

/// One file per doc under `<dir>/corpus`, stamped into `<dir>/idx`.
fn stage_files(dir: &Path, docs: &[&[u8]]) -> Fix {
    let root = dir.join("corpus");
    let index = dir.join("idx");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    let files: Vec<String> = (0..docs.len()).map(|i| format!("d{i:04}.c")).collect();
    let mut mtimes = Vec::new();
    for (name, d) in files.iter().zip(docs) {
        std::fs::write(root.join(name), d).unwrap();
        mtimes.push(Stamp::of(&std::fs::metadata(root.join(name)).unwrap()).mtime_ns);
    }
    crate::source::write_stamps(&index, &mtimes).unwrap();
    Fix {
        store_docs: false,
        path: root,
        lens: docs.iter().map(|d| d.len() as u64).collect(),
        files,
        index,
    }
}

fn reader(fix: &Fix) -> DocStore {
    if fix.store_docs {
        DocStore::shard(fix.path.clone(), &fix.lens).unwrap()
    } else {
        let stamps = Stamps::open(&fix.index, fix.lens.len()).unwrap();
        let files = Files::open(fix.path.clone(), stamps, fix.lens.len()).unwrap();
        DocStore::files(files, &fix.lens)
    }
}

/// Collect [`fetch_stream_parallel`] into the ascending vector the pre-
/// streaming API returned, so these assertions stay value-for-value.
fn fetch_map_parallel<T: Send, F: Fn(usize, &[u8]) -> T + Sync>(
    shard: &mut DocStore,
    fix: &Fix,
    doc_ids: &[usize],
    max_threads: usize,
    map: F,
) -> Result<Vec<(usize, T)>, String> {
    let mut out = Vec::new();
    fetch_stream_parallel(
        shard,
        &fix.files,
        doc_ids,
        max_threads,
        &map,
        &mut |doc, _, v| {
            out.push((doc, v));
            Ok(())
        },
    )?;
    Ok(out)
}

/// Per-doc `(doc_idx, count)` hits in ascending doc order.
type FetchResults = Vec<(usize, u64)>;
/// `(fetches, hits, cache_len, cached_bytes)`.
type FetchState = (u64, u64, usize, u64);
/// Two-list run: both result vecs plus `(fetches, hits, cache_len)`.
type TwoListOutcome = (FetchResults, FetchResults, (u64, u64, usize));

/// Write a separator-free shard of `docs`.
fn stage_shard(dir: &Path, docs: &[&[u8]]) -> Fix {
    let path = dir.join("docs.shard");
    let mut bytes = Vec::new();
    for d in docs {
        bytes.extend_from_slice(d);
    }
    std::fs::write(&path, &bytes).unwrap();
    Fix {
        store_docs: true,
        path,
        lens: docs.iter().map(|d| d.len() as u64).collect(),
        files: Vec::new(),
        index: dir.to_path_buf(),
    }
}

/// Forced-serial vs forced-parallel vs the production thread count over
/// the same candidates on fresh readers: identical counts, counters and
/// cache state. Returns the agreed result.
fn serial_vs_parallel<F>(fix: &Fix, ids: &[usize], finalize: &F) -> FetchResults
where
    F: Fn(usize, &[u8]) -> u64 + Sync,
{
    fn run<F>(fix: &Fix, ids: &[usize], threads: usize, finalize: &F) -> (FetchResults, FetchState)
    where
        F: Fn(usize, &[u8]) -> u64 + Sync,
    {
        let mut shard = reader(fix);
        let res = fetch_map_parallel(&mut shard, fix, ids, threads, finalize).unwrap();
        let state = (
            shard.fetches(),
            shard.hits(),
            shard.cache.len(),
            shard.cached_bytes,
        );
        assert!(shard.cache.len() <= fix.lens.len());
        assert!(shard.cached_bytes <= shard.max_cached_bytes);
        (res, state)
    }
    let (s1, st1) = run(fix, ids, 1, finalize);
    let (s8, st8) = run(fix, ids, 8, finalize);
    let (sn, stn) = run(fix, ids, fetch_threads(), finalize);
    assert_eq!(
        s1, s8,
        "forced-parallel (8 threads) must equal forced-serial"
    );
    assert_eq!(s1, sn, "production thread count must equal forced-serial");
    assert_eq!(
        st1, st8,
        "parallel must leave identical cache state/counters"
    );
    assert_eq!(
        st1, stn,
        "production thread count must leave identical state"
    );
    assert_eq!(s1.len(), ids.len());
    assert!(s1.windows(2).all(|w| w[0].0 < w[1].0));
    s1
}

/// Two overlapping candidate lists on one shared reader (the cross-term
/// cache shape).
fn run_two_lists<F>(
    fix: &Fix,
    list_a: &[usize],
    list_b: &[usize],
    threads: usize,
    finalize: &F,
) -> TwoListOutcome
where
    F: Fn(usize, &[u8]) -> u64 + Sync,
{
    let mut shard = reader(fix);
    let a = fetch_map_parallel(&mut shard, fix, list_a, threads, finalize).unwrap();
    let b = fetch_map_parallel(&mut shard, fix, list_b, threads, finalize).unwrap();
    assert!(shard.cache.len() <= fix.lens.len());
    assert!(shard.cached_bytes <= shard.max_cached_bytes);
    (a, b, (shard.fetches(), shard.hits(), shard.cache.len()))
}

/// Non-overlapping occurrences of `needle`, the finalize stand-in.
fn hits_of(needle: &[u8], raw: &[u8]) -> u64 {
    if needle.is_empty() || raw.len() < needle.len() {
        return 0;
    }
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= raw.len() {
        if &raw[i..i + needle.len()] == needle {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

/// Each fetched doc's bytes, in emission order.
fn bytes_of(shard: &mut DocStore, fix: &Fix, ids: &[usize]) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::new();
    fetch_stream_parallel(shard, &fix.files, ids, 8, &|_, _| (), &mut |_, raw, ()| {
        out.push(raw.to_vec());
        Ok(())
    })?;
    Ok(out)
}

#[test]
fn cross_term_cache_dedups_and_bypasses_exactly() {
    for store_docs in SOURCES {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage(
            tmp.path(),
            &[b"printk hello printk", b"nothing", b""],
            store_docs,
        );
        // Repeat fetches of the same doc hit the cache; empty docs are never
        // stored (their fetch reads nothing).
        let mut shard = reader(&fix);
        assert_eq!(
            bytes_of(&mut shard, &fix, &[0]).unwrap(),
            [b"printk hello printk"]
        );
        assert_eq!(
            bytes_of(&mut shard, &fix, &[0]).unwrap(),
            [b"printk hello printk"]
        );
        assert_eq!(
            (shard.fetches(), shard.hits()),
            (1, 1),
            "store_docs={store_docs}"
        );
        assert_eq!(
            bytes_of(&mut shard, &fix, &[2]).unwrap(),
            [Vec::<u8>::new()]
        );
        assert!(!shard.cache.contains_key(&2));
        assert_eq!(shard.fetches(), 2);
        // A 1-byte ceiling forces every non-empty fetch off-cache: exact
        // bytes, nothing pinned, no hits.
        let mut capped = reader(&fix);
        capped.max_cached_bytes = 1;
        let first = bytes_of(&mut capped, &fix, &[0]).unwrap();
        assert_eq!(bytes_of(&mut capped, &fix, &[0]).unwrap(), first);
        assert_eq!(first, [b"printk hello printk"]);
        assert_eq!((capped.fetches(), capped.hits()), (2, 0));
        assert!(capped.cache.is_empty());
        // Out-of-range ids are a loud error, never a panic.
        assert!(bytes_of(&mut reader(&fix), &fix, &[99]).is_err());
    }
}

#[test]
fn parallel_fetch_map_matches_serial() {
    let tmp = tempfile::tempdir().unwrap();
    let files: &[&[u8]] = &[
        b"printk hello printk",
        b"nothing here",
        b"printk",
        b"zzz",
        b"printk printk printk",
    ];
    let fix = stage_shard(tmp.path(), files);
    let ids: Vec<usize> = (0..files.len()).collect();
    let got = serial_vs_parallel(&fix, &ids, &|_, raw: &[u8]| hits_of(b"printk", raw));
    assert_eq!(got, vec![(0, 2), (1, 0), (2, 1), (3, 0), (4, 3)]);
    // An empty candidate list stays empty and fetches nothing.
    let mut shard = reader(&fix);
    assert!(
        fetch_map_parallel(&mut shard, &fix, &[], 8, |_, _| 1u64)
            .unwrap()
            .is_empty()
    );
    assert_eq!(shard.fetches(), 0);
}

#[test]
fn parallel_fetch_stress_overlapping_candidates_with_cache() {
    // Two overlapping lists share one reader: combined counts equal the
    // forced-serial oracle, hits prove the dedup, and repeated 8-thread
    // runs are bit-identical (no order-dependent combine).
    let tmp = tempfile::tempdir().unwrap();
    let files: &[&[u8]] = &[
        b"printk hello printk",
        b"nothing here",
        b"printk",
        b"zzz",
        b"printk printk printk",
        b"printk",
    ];
    let fix = stage_shard(tmp.path(), files);
    let list_a = [0usize, 1, 2, 3, 4];
    let list_b = [2usize, 3, 4, 5];
    let finalize = |_: usize, raw: &[u8]| hits_of(b"printk", raw);
    let (oa, ob, ostate) = run_two_lists(&fix, &list_a, &list_b, 1, &finalize);
    assert_eq!(oa, vec![(0, 2), (1, 0), (2, 1), (3, 0), (4, 3)]);
    assert_eq!(ob, vec![(2, 1), (3, 0), (4, 3), (5, 1)]);
    // 6 physical reads (doc 3 cached after list A), 3 hits, 6 cached docs.
    assert_eq!(ostate, (6, 3, 6));
    for _ in 0..3 {
        let (sa, sb, sstate) = run_two_lists(&fix, &list_a, &list_b, 8, &finalize);
        assert_eq!((sa, sb, sstate), (oa.clone(), ob.clone(), ostate));
    }
}

#[test]
fn streamed_fetch_matches_serial_across_batches() {
    // A candidate list several times FETCH_STREAM_BATCH still answers
    // exactly at every thread count, with identical cache state.
    let n = 3 * FETCH_STREAM_BATCH + 5;
    let owned: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            if i % 7 == 3 {
                b"z".to_vec()
            } else {
                format!("printk payload {i} {}", "x".repeat(i % 13)).into_bytes()
            }
        })
        .collect();
    let files: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
    for store_docs in SOURCES {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage(tmp.path(), &files, store_docs);
        let lens = &fix.lens;
        let ids: Vec<usize> = (0..n).collect();
        let finalize = |_: usize, raw: &[u8]| raw.len() as u64;
        let want: Vec<(usize, u64)> = ids.iter().map(|&i| (i, lens[i])).collect();
        assert_eq!(
            serial_vs_parallel(&fix, &ids, &finalize),
            want,
            "store_docs={store_docs}"
        );
        // An overlapping second list shares the cache across lists, serial
        // and parallel alike.
        let evens: Vec<usize> = (0..n).step_by(2).collect();
        let want_b: Vec<(usize, u64)> = evens.iter().map(|&i| (i, lens[i])).collect();
        let (oa, ob, ostate) = run_two_lists(&fix, &ids, &evens, 1, &finalize);
        assert_eq!(oa, want);
        assert_eq!(ob, want_b);
        for _ in 0..2 {
            let (sa, sb, sstate) = run_two_lists(&fix, &ids, &evens, 8, &finalize);
            assert_eq!((sa, sb, sstate), (oa.clone(), ob.clone(), ostate));
        }
    }
}

#[test]
fn zero_length_candidates_need_no_shard() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = Fix {
        store_docs: true,
        path: tmp.path().join("absent.shard"),
        lens: vec![0u64, 0],
        files: Vec::new(),
        index: tmp.path().to_path_buf(),
    };
    let mut shard = reader(&fix);
    let got = fetch_map_parallel(&mut shard, &fix, &[0, 1], 8, |_, raw| raw.len() as u64)
        .expect("empty candidates must not open the shard");
    assert_eq!(got, vec![(0, 0), (1, 0)]);
    assert_eq!(shard.fetches(), 2);
}

#[test]
fn missing_shard_fails_loudly_on_a_real_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_shard(tmp.path(), &[b"printk"]);
    std::fs::remove_file(&fix.path).unwrap();
    let mut shard = reader(&fix);
    let err = fetch_map_parallel(&mut shard, &fix, &[0], 8, |_, raw| raw.len() as u64).unwrap_err();
    assert!(err.contains("cannot read"), "got: {err}");
    assert!(err.contains("docs.shard"), "must name the shard: {err}");
}

#[test]
fn pread_exact_reads_at_offsets_without_moving_cursor() {
    use std::io::{Read as _, Seek as _, Write as _};
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("shard.bin");
    let content: Vec<u8> = (0u8..200).collect();
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&content).unwrap();
    }
    let mut file = std::fs::File::open(&path).unwrap();
    file.seek(std::io::SeekFrom::Start(77)).unwrap();
    for (off, len) in [(0u64, 200usize), (0, 1), (77, 13), (199, 1), (150, 50)] {
        let mut buf = vec![0u8; len];
        pread_exact(&file, &mut buf, off).unwrap();
        assert_eq!(buf, content[off as usize..off as usize + len]);
    }
    assert_eq!(file.stream_position().unwrap(), 77, "cursor must not move");
    // Empty read at/past EOF: Ok, no I/O.
    pread_exact(&file, &mut [], 10_000).unwrap();
    // Short and fully-past-EOF reads are loud, never silent short vecs.
    for off in [196u64, 10_000] {
        let mut buf = vec![0u8; 8];
        let err = pread_exact(&file, &mut buf, off).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }
    let mut one = [0u8; 1];
    file.read_exact(&mut one).unwrap();
    assert_eq!(one, [77]);
}

#[test]
fn fetch_buffer_recycles_capacity_and_zeroes() {
    let mut pool = Vec::new();
    let mut pool_bytes = 0u64;
    let b1 = fetch_buffer(&mut pool, &mut pool_bytes, 10);
    assert_eq!((b1.len(), b1.capacity()), (10, 10));
    assert_eq!(pool_bytes, 0);
    // A dirty returned buffer is reused for a smaller request and
    // re-zeroed.
    let mut dirty = b1;
    dirty.copy_from_slice(&[0xABu8; 10]);
    pool_bytes += dirty.capacity() as u64;
    pool.push(dirty);
    let b2 = fetch_buffer(&mut pool, &mut pool_bytes, 8);
    assert_eq!(b2.len(), 8);
    assert!(b2.capacity() >= 10, "must reuse pooled capacity");
    assert_eq!(b2, vec![0u8; 8], "reused buf must be zeroed");
    assert!(pool.is_empty(), "reused buf must pop from the pool");
    assert_eq!(pool_bytes, 0, "pop must release the byte accounting");
    // An oversized request allocates fresh rather than over-growing.
    assert_eq!(
        fetch_buffer(&mut pool, &mut pool_bytes, 10_000).len(),
        10_000
    );
}

/// `n` docs of distinct non-empty payloads, staged as one shard.
fn stage_many(dir: &Path, n: usize) -> Fix {
    let owned: Vec<Vec<u8>> = (0..n)
        .map(|i| format!("doc {i} {}", "x".repeat(i % 11)).into_bytes())
        .collect();
    let files: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
    stage_shard(dir, &files)
}

#[test]
fn emission_is_ascending_and_starts_before_the_last_fetch() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let n = 3 * FETCH_STREAM_BATCH + 5;
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_many(tmp.path(), n);
    let ids: Vec<usize> = (0..n).collect();
    let mapped = AtomicUsize::new(0);
    let map = |_: usize, raw: &[u8]| {
        mapped.fetch_add(1, Ordering::SeqCst);
        raw.len()
    };
    let mut seen: Vec<usize> = Vec::new();
    let mut at_first = usize::MAX;
    let mut shard = reader(&fix);
    fetch_stream_parallel(&mut shard, &fix.files, &ids, 8, &map, &mut |doc, _, _| {
        if seen.is_empty() {
            at_first = mapped.load(Ordering::SeqCst);
        }
        seen.push(doc);
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, ids);
    assert!(
        at_first <= FETCH_STREAM_BATCH,
        "first emit must precede the last fetch: {at_first} of {n} mapped"
    );
}

#[test]
fn peak_live_mapped_values_is_one_batch() {
    use std::sync::atomic::{AtomicIsize, Ordering};

    /// Live instances of the mapped value, so the sink's drop is observable.
    struct Live<'a>(&'a AtomicIsize);
    impl Drop for Live<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    let n = 4 * FETCH_STREAM_BATCH + 7;
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_many(tmp.path(), n);
    let ids: Vec<usize> = (0..n).collect();
    let live = AtomicIsize::new(0);
    let map = |_: usize, _: &[u8]| {
        live.fetch_add(1, Ordering::SeqCst);
        Live(&live)
    };
    let mut peak = 0isize;
    let mut shard = reader(&fix);
    fetch_stream_parallel(&mut shard, &fix.files, &ids, 8, &map, &mut |_, _, v| {
        peak = peak.max(live.load(Ordering::SeqCst));
        drop(v);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        live.load(Ordering::SeqCst),
        0,
        "every value must be dropped"
    );
    assert!(
        peak <= FETCH_STREAM_BATCH as isize,
        "peak live mapped values {peak} exceeds one batch"
    );
}

#[test]
fn emit_error_aborts_the_query_loudly() {
    let n = 2 * FETCH_STREAM_BATCH;
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_many(tmp.path(), n);
    let ids: Vec<usize> = (0..n).collect();
    let mut seen = 0usize;
    let mut shard = reader(&fix);
    let err = fetch_stream_parallel(
        &mut shard,
        &fix.files,
        &ids,
        8,
        &|_, raw| raw.len(),
        &mut |_, _, _| {
            seen += 1;
            if seen == 3 {
                Err("error: sink said no".to_string())
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();
    assert_eq!(err, "error: sink said no");
    assert_eq!(seen, 3, "emission must stop at the failing document");
}

#[test]
#[should_panic(expected = "boom")]
fn a_map_panic_resumes_on_the_caller() {
    let n = 2 * FETCH_STREAM_BATCH;
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_many(tmp.path(), n);
    let ids: Vec<usize> = (0..n).collect();
    let mut shard = reader(&fix);
    let _ = fetch_map_parallel(&mut shard, &fix, &ids, 8, |doc, raw| {
        if doc == FETCH_STREAM_BATCH + 7 {
            panic!("boom");
        }
        raw.len()
    });
}

#[test]
fn cached_empty_and_ranged_docs_emit_in_doc_order() {
    // Docs 1 and 4 are empty, doc 0 and 3 are pre-cached: a plan mixing all
    // three slot kinds emits ascending and leaves a pure-range run's state.
    let files: &[&[u8]] = &[b"printk", b"", b"printk printk", b"zz", b"", b"printk"];
    for store_docs in SOURCES {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage(tmp.path(), files, store_docs);
        let ids: Vec<usize> = (0..files.len()).collect();
        let finalize = |_: usize, raw: &[u8]| hits_of(b"printk", raw);

        let mut warm = reader(&fix);
        assert_eq!(
            fetch_map_parallel(&mut warm, &fix, &[0, 3], 8, finalize)
                .unwrap()
                .len(),
            2
        );
        let mixed = fetch_map_parallel(&mut warm, &fix, &ids, 8, finalize).unwrap();
        assert_eq!(
            mixed,
            vec![(0, 1), (1, 0), (2, 2), (3, 0), (4, 0), (5, 1)],
            "store_docs={store_docs}"
        );
        assert_eq!((warm.fetches(), warm.hits()), (6, 2));

        let mut cold = reader(&fix);
        assert_eq!(
            fetch_map_parallel(&mut cold, &fix, &ids, 8, finalize).unwrap(),
            mixed
        );
        assert_eq!(
            (cold.cache.len(), cold.cached_bytes),
            (warm.cache.len(), warm.cached_bytes)
        );
    }
}

#[test]
fn files_fetch_reads_original_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"\xef\xbb\xbfprintk", b"", b"zz"]);
    let mut store = reader(&fix);
    let got = bytes_of(&mut store, &fix, &[0, 1, 2]).unwrap();
    assert_eq!(
        got,
        [b"\xef\xbb\xbfprintk".to_vec(), vec![], b"zz".to_vec()]
    );
}

/// A files-layout store after the load stat, and a query begun.
fn loaded(fix: &Fix) -> DocStore {
    let mut store = reader(fix);
    store.stat_all(&fix.files);
    store.begin();
    store
}

#[test]
fn files_changed_since_load_read_current_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk", b"zz"]);
    let mut store = loaded(&fix);
    std::fs::write(fix.path.join(&fix.files[1]), b"zzz").unwrap();
    assert_eq!(
        bytes_of(&mut store, &fix, &[0, 1]).unwrap(),
        [b"printk".to_vec(), b"zzz".to_vec()]
    );
    store.begin();
    assert_eq!(
        store.stale(),
        [1],
        "a read that finds a change marks the doc"
    );
    // Same size, new mtime: read whole all the same.
    let file = std::fs::File::options()
        .write(true)
        .open(fix.path.join(&fix.files[0]))
        .unwrap();
    file.set_modified(std::time::UNIX_EPOCH).unwrap();
    std::fs::write(fix.path.join(&fix.files[0]), b"PRINTK").unwrap();
    let mut store = reader(&fix);
    assert_eq!(bytes_of(&mut store, &fix, &[0]).unwrap(), [b"PRINTK"]);
    store.begin();
    assert_eq!(store.stale(), [0]);
}

#[test]
fn files_missing_at_read_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk", b"zz"]);
    let mut store = loaded(&fix);
    std::fs::remove_file(fix.path.join(&fix.files[0])).unwrap();
    assert_eq!(bytes_of(&mut store, &fix, &[0, 1]).unwrap(), [b"zz"]);
    store.begin();
    let (mut exact, mut candidates) = (vec![(0, 1), (1, 1)], vec![0, 1]);
    store.screen(&mut exact, &mut candidates);
    assert_eq!((exact, candidates), (vec![(1, 1)], vec![1]));
}

#[test]
fn stale_scan_merges_into_ascending_stream() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk", b"zz", b"printk zz", b"aa"]);
    std::fs::write(fix.path.join(&fix.files[1]), b"zzzz").unwrap();
    std::fs::remove_file(fix.path.join(&fix.files[2])).unwrap();
    let mut store = loaded(&fix);
    assert_eq!(store.stale(), [1]);
    assert_eq!(store.changed(), [1, 2]);
    for _ in 0..2 {
        let (mut exact, mut candidates) = (vec![(0, 1), (1, 2), (2, 3)], vec![1, 2, 3]);
        store.screen(&mut exact, &mut candidates);
        assert_eq!((exact, candidates), (vec![(0, 1)], vec![3]));
    }
}

#[test]
fn stale_bytes_stay_cached_across_terms() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk", b"zz"]);
    std::fs::write(fix.path.join(&fix.files[1]), b"zz and more").unwrap();
    let mut store = loaded(&fix);
    store.max_cached_bytes = 0;
    for _ in 0..3 {
        assert_eq!(
            bytes_of(&mut store, &fix, &[0, 1]).unwrap(),
            [b"printk".to_vec(), b"zz and more".to_vec()]
        );
    }
    assert_eq!(
        (store.fetches(), store.hits()),
        (4, 2),
        "doc 0 re-read per term, doc 1 once"
    );
    store.begin();
    assert_eq!(
        (store.fetches(), store.cache.len()),
        (0, 0),
        "a new query reads again"
    );
}

#[test]
fn stale_doc_turned_binary_is_excluded() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk", b"zz"]);
    std::fs::write(fix.path.join(&fix.files[1]), b"zz\0").unwrap();
    let mut store = loaded(&fix);
    assert_eq!(store.stale(), [1]);
    assert_eq!(bytes_of(&mut store, &fix, &[0, 1]).unwrap(), [b"printk"]);
    store.begin();
    assert!(store.stale().is_empty());
    assert_eq!(store.changed(), [1]);
}

#[test]
fn empty_recorded_doc_that_grew_is_read() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"", b"zz"]);
    std::fs::write(fix.path.join(&fix.files[0]), b"printk").unwrap();
    let mut store = loaded(&fix);
    assert_eq!(bytes_of(&mut store, &fix, &[0]).unwrap(), [b"printk"]);
}

/// A files-source store with `added` written under the root and discovered.
fn discovered(fix: &Fix, added: &[(&str, &[u8])]) -> DocStore {
    for (rel, bytes) in added {
        std::fs::write(fix.path.join(rel), bytes).unwrap();
    }
    let mut store = reader(fix);
    store.stat_all(&fix.files);
    store.discover(&fix.files);
    store.begin();
    store
}

#[test]
fn added_docs_stream_in_path_order() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"zero", b"one", b"two"]);
    let added: [(&str, &[u8]); 4] = [
        ("a.c", b"A"),
        ("d0000a.c", b"between"),
        ("e.c", b"last"),
        ("bin.c", b"\0"),
    ];
    let mut store = discovered(&fix, &added);
    assert_eq!(store.stale(), [3, 4, 5]);
    assert!(
        store.changed().is_empty(),
        "the screen never sees an added doc"
    );
    let mut ids = vec![0, 1, 2, 3, 4, 5];
    store.paths(&fix.files).sort(&mut ids);
    assert_eq!(ids, [3, 0, 4, 1, 2, 5]);
    let got = bytes_of(&mut store, &fix, &ids).unwrap();
    assert_eq!(
        got,
        [&b"A"[..], b"zero", b"between", b"one", b"two", b"last"]
    );
}

#[test]
fn added_doc_is_read_whole_and_cached_across_terms() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk"]);
    let mut store = discovered(&fix, &[("e.c", b"added printk")]);
    store.max_cached_bytes = 0;
    for _ in 0..3 {
        assert_eq!(
            bytes_of(&mut store, &fix, &[0, 1]).unwrap(),
            [&b"printk"[..], b"added printk"]
        );
    }
    assert_eq!(
        (store.fetches(), store.hits()),
        (4, 2),
        "doc 0 re-read per term, doc 1 once"
    );
    std::fs::remove_file(fix.path.join("e.c")).unwrap();
    store.begin();
    assert_eq!(
        bytes_of(&mut store, &fix, &[0, 1]).unwrap(),
        [b"printk"],
        "a missing added doc is skipped"
    );
    store.begin();
    assert!(store.stale().is_empty());
    assert!(
        bytes_of(&mut store, &fix, &[1])
            .unwrap_err()
            .contains("out of range")
    );
}

#[test]
fn paths_cmp_orders_added_ids_among_manifest_ids() {
    let files: Vec<String> = ["b", "d"].map(String::from).to_vec();
    let paths = Paths {
        files: &files,
        added: ["a", "c", "e"].map(String::from).into(),
    };
    assert_eq!(
        [paths.path(1), paths.path(2), paths.path(4), paths.path(5)],
        [Some("d"), Some("a"), Some("e"), None]
    );
    assert!(paths.cmp(0, 1).is_lt() && paths.cmp(2, 0).is_lt());
    assert!(paths.cmp(3, 1).is_lt() && paths.cmp(4, 1).is_gt());
    let mut ids = vec![0, 1, 2, 3, 4];
    paths.sort(&mut ids);
    assert_eq!(ids, [2, 0, 3, 1, 4]);
    let mut manifest_only = vec![0, 1];
    paths.sort(&mut manifest_only);
    assert_eq!(manifest_only, [0, 1]);
}

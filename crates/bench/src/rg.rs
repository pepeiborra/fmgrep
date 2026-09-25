//! ripgrep reference row: one `rg` spawn per term over the run's staged tree
//! ([`StagedDocs`], `{id:08}.<ext>`), the binary `$FMGREP_BENCH_RG_BIN` or
//! `rg` on `PATH`. Opening requires `rg --files` to list exactly the staged
//! set. Counts are `--count-matches` rows attributed by staged name; literals
//! run `-F` line by line, regexes add `--null-data` so a match may span
//! `\n`. Overlapping matches are not counted (`aa` in `aaaa` is 2), literals
//! containing `\n` cannot match, and `^`/`$` anchor at `\n`. Exit 1 is a zero
//! count; exit 2 panics, or is a [`RegexError`] on the regex path. Every spawn
//! notifies the thread-local [`SpawnObserver`] with the child pid.

#[cfg(test)]
use crate::Corpus;
use crate::{RegexError, StagedDocs, UNIFIED_SEPARATOR, UnifiedIndex, UnifiedRegexSearch};
use std::cell::RefCell;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

/// Walk flags shared by the coverage check and every search, so both see
/// the same file set.
const WALK_FLAGS: [&str; 2] = ["--no-ignore", "--no-require-git"];

/// Which benchmark phase a spawned `rg` child belongs to.
///
/// Opening spawns the `--version` probe and the `--files` walk check, both
/// [`SpawnPhase::Index`]; every search spawn is [`SpawnPhase::Query`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnPhase {
    Index,
    Query,
}

/// Harness hook observing each spawned `rg` child.
///
/// Called synchronously on the benchmarking thread:
/// [`child_spawned`](Self::child_spawned) immediately after `spawn()` (the
/// pid is live, so a sampler started here catches even a millisecond child),
/// [`child_reaped`](Self::child_reaped) once the child is reaped (a
/// `RUSAGE_CHILDREN` watermark read here attributes to exactly this child).
/// Methods take `&self`, so stateful implementations need interior
/// mutability. Observers only measure; they never affect query results.
pub trait SpawnObserver {
    fn child_spawned(&self, pid: u32, phase: SpawnPhase);
    fn child_reaped(&self);
}

thread_local! {
    /// Installed by [`with_spawn_observer`]; `None` means measure nothing.
    static SPAWN_OBSERVER: RefCell<Option<Rc<dyn SpawnObserver>>> = RefCell::new(None);
}

/// Run `f` with `observer` notified of every `rg` spawn on this thread.
///
/// The previous observer is restored afterwards, so calls nest safely.
pub fn with_spawn_observer<R>(observer: Rc<dyn SpawnObserver>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Rc<dyn SpawnObserver>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SPAWN_OBSERVER.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let previous = SPAWN_OBSERVER.with(|slot| slot.borrow_mut().replace(observer));
    let _restore = Restore(previous);
    f()
}

/// Notify the installed observer of a fresh spawn (no-op when unset).
fn notify_spawn(pid: u32, phase: SpawnPhase) {
    SPAWN_OBSERVER.with(|slot| {
        if let Some(obs) = slot.borrow().as_ref() {
            obs.child_spawned(pid, phase);
        }
    });
}

/// Notify the installed observer that the active child was reaped (no-op).
fn notify_reaped() {
    SPAWN_OBSERVER.with(|slot| {
        if let Some(obs) = slot.borrow().as_ref() {
            obs.child_reaped();
        }
    });
}

/// Resolve the `rg` binary: `$FMGREP_BENCH_RG_BIN`, else `rg` on `PATH`.
fn rg_binary() -> PathBuf {
    std::env::var_os("FMGREP_BENCH_RG_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rg"))
}

/// First line of `rg --version` for the binary the rg row runs; `None` when
/// it cannot be run.
pub fn rg_version() -> Option<String> {
    let out = Command::new(rg_binary())
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Spawn `bin` with `args`, notify the observer, and return its output.
fn run_rg(bin: &Path, args: &[&OsStr], phase: SpawnPhase) -> std::process::Output {
    let child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "cannot run `rg` binary at {} ({e}) — install ripgrep 15.x or set FMGREP_BENCH_RG_BIN",
                bin.display(),
            )
        });
    notify_spawn(child.id(), phase);
    let out = child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("failed to wait on {}: {e}", bin.display()));
    notify_reaped();
    out
}

/// Check the binary runs (`rg --version`); panic with a helpful message if not.
fn probe_rg(bin: &Path) {
    let out = run_rg(bin, &[OsStr::new("--version")], SpawnPhase::Index);
    if out.status.success() {
        return;
    }
    panic!(
        "`rg` probe failed: {} exited with {} (stderr: {}) — install ripgrep or set FMGREP_BENCH_RG_BIN",
        bin.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim(),
    );
}

/// Raw bytes (a term, or a path `rg` printed) as an `OsStr`, without loss
/// on Unix.
fn bytes_as_os_str(term: &[u8]) -> &OsStr {
    #[cfg(unix)]
    {
        std::os::unix::ffi::OsStrExt::from_bytes(term)
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(term).expect("non-UTF8 bytes on non-Unix platform")
    }
}

/// Panic unless the exit status is a searched-something status: 0 (matches)
/// or 1 (none). Exit 2 and signals are real failures, never a zero count.
fn require_search_status(out: &std::process::Output) {
    if out.status.success() || out.status.code() == Some(1) {
        return;
    }
    panic!(
        "`rg` failed with status {} (stderr: {})",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim(),
    );
}

/// Count literal matches of `term` in the single file `path`.
#[cfg(test)]
fn rg_count_file(bin: &Path, path: &Path, term: &[u8]) -> u64 {
    let out = run_rg(
        bin,
        &[
            OsStr::new("--count-matches"),
            OsStr::new("-F"),
            OsStr::new("-a"),
            OsStr::new("--no-filename"),
            OsStr::new("--no-ignore"),
            OsStr::new("--no-require-git"),
            OsStr::new("-e"),
            bytes_as_os_str(term),
            OsStr::new("--"),
            path.as_os_str(),
        ],
        SpawnPhase::Query,
    );
    require_search_status(&out);
    if out.status.code() == Some(1) {
        return 0;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or_else(|e| {
            panic!(
                "unparsable `rg --count-matches` output {:?}: {e}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
}

/// Doc id of a path `rg` printed; a name we did not stage, or an id past
/// the staging, panics rather than being attributed.
fn staged_id(path: &Path, ext: &str, num_docs: usize) -> usize {
    path.file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| StagedDocs::file_index(name, ext))
        .filter(|&id| id < num_docs)
        .unwrap_or_else(|| {
            panic!(
                "`rg` reported {}, which is not one of the {num_docs} staged document(s)",
                path.display()
            )
        })
}

/// Panics unless `rg --files` over `dir` lists every id in `0..num_docs`
/// exactly once and nothing else.
fn check_walk(bin: &Path, dir: &Path, ext: &str, num_docs: usize) {
    let mut args: Vec<&OsStr> = vec![OsStr::new("--files")];
    args.extend(WALK_FLAGS.iter().map(OsStr::new));
    args.extend([OsStr::new("--"), dir.as_os_str()]);
    let out = run_rg(bin, &args, SpawnPhase::Index);
    require_search_status(&out);
    let mut seen = vec![false; num_docs];
    for line in out.stdout.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let path = Path::new(bytes_as_os_str(line));
        let id = staged_id(path, ext, num_docs);
        assert!(!seen[id], "`rg --files` listed {} twice", path.display());
        seen[id] = true;
    }
    if let Some(missing) = seen.iter().position(|&s| !s) {
        panic!(
            "`rg --files` over {} skips staged document {missing}",
            dir.display()
        );
    }
}

/// Per-doc counts off `--count-matches` rows (`path:N`, terminated by `\n`,
/// or NUL under `--null-data`). A malformed or repeated row panics.
fn parse_counts(stdout: &[u8], ext: &str, num_docs: usize) -> Vec<u64> {
    let mut counts = vec![0u64; num_docs];
    for row in stdout
        .split(|&b| b == b'\n' || b == b'\0')
        .filter(|r| !r.is_empty())
    {
        let colon = row
            .iter()
            .rposition(|&b| b == b':')
            .unwrap_or_else(|| panic!("unparsable `rg --count-matches` row: {row:?}"));
        let n: u64 = std::str::from_utf8(&row[colon + 1..])
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("unparsable `rg --count-matches` count: {row:?}"));
        let path = Path::new(bytes_as_os_str(&row[..colon]));
        let id = staged_id(path, ext, num_docs);
        assert_eq!(counts[id], 0, "`rg` reported {} twice", path.display());
        counts[id] = n;
    }
    counts
}

/// Turn a per-doc count vector into the sorted, zero-free hit list the
/// unified contract requires.
fn into_hits(counts: Vec<u64>) -> Vec<(usize, u64)> {
    counts
        .into_iter()
        .enumerate()
        .filter(|&(_, c)| c > 0)
        .collect()
}

/// Per-document rg oracle: one `rg` run per (document, term) over the corpus
/// files themselves. Test-only — the bench roster carries the corpus-wide
/// [`RgUnifiedIndex`] row.
#[cfg(test)]
struct RgIndex {
    /// Absolute path and length of each corpus document.
    docs: Vec<(PathBuf, u64)>,
    rg_bin: PathBuf,
}

#[cfg(test)]
impl RgIndex {
    fn build(corpus: Corpus<'_>) -> Self {
        let rg_bin = rg_binary();
        probe_rg(&rg_bin);
        Self {
            docs: (0..corpus.num_docs())
                .map(|id| {
                    let path = corpus.doc_path(id).expect("id is in range");
                    (path, corpus.docs[id].len)
                })
                .collect(),
            rg_bin,
        }
    }

    fn count(&self, doc_idx: usize, term: &[u8]) -> u64 {
        let Some((path, len)) = self.docs.get(doc_idx) else {
            return 0;
        };
        if term.is_empty() || *len < term.len() as u64 {
            return 0;
        }
        rg_count_file(&self.rg_bin, path, term)
    }
}

/// ripgrep CLI reference row over a staged tree: no index, one `rg` run per
/// term over the directory.
pub struct RgUnifiedIndex {
    dir: PathBuf,
    ext: String,
    num_docs: usize,
    rg_bin: PathBuf,
}

impl RgUnifiedIndex {
    /// Probes `rg`, then requires `rg --files` over `staged` to list exactly
    /// the staged set.
    pub fn open(staged: &StagedDocs, ext: &str) -> Self {
        let rg_bin = rg_binary();
        probe_rg(&rg_bin);
        check_walk(&rg_bin, staged.dir(), ext, staged.num_docs());
        Self {
            dir: staged.dir().to_owned(),
            ext: ext.to_owned(),
            num_docs: staged.num_docs(),
            rg_bin,
        }
    }

    /// One `--count-matches` spawn over the tree; `mode` is the matcher flag.
    fn spawn_count(&self, mode: &str, pattern: &[u8]) -> std::process::Output {
        let mut args: Vec<&OsStr> = ["--count-matches", "--with-filename", "--no-heading", "-a"]
            .iter()
            .map(OsStr::new)
            .collect();
        args.push(OsStr::new(mode));
        args.extend(WALK_FLAGS.iter().map(OsStr::new));
        args.extend([
            OsStr::new("-e"),
            bytes_as_os_str(pattern),
            OsStr::new("--"),
            self.dir.as_os_str(),
        ]);
        run_rg(&self.rg_bin, &args, SpawnPhase::Query)
    }

    fn hits(&self, stdout: &[u8]) -> Vec<(usize, u64)> {
        into_hits(parse_counts(stdout, &self.ext, self.num_docs))
    }
}

impl UnifiedIndex for RgUnifiedIndex {
    fn name(&self) -> &str {
        "rg"
    }

    fn search(&self, term: &[u8]) -> Vec<(usize, u64)> {
        if term.is_empty() || term.contains(&UNIFIED_SEPARATOR) {
            return Vec::new();
        }
        let out = self.spawn_count("-F", term);
        require_search_status(&out);
        self.hits(&out.stdout)
    }

    fn num_files_indexed(&self) -> usize {
        // `open` checked the walk covers exactly the staged set.
        self.num_docs
    }

    fn index_size_bytes(&self) -> u64 {
        0
    }
}

impl UnifiedRegexSearch for RgUnifiedIndex {
    /// `rg`'s own regex engine (no `-F`), one record per document.
    fn search_regex(&self, pattern: &str) -> Result<Vec<(usize, u64)>, RegexError> {
        if pattern.is_empty() {
            return Err(RegexError::new(
                pattern,
                "empty regex pattern: rejected by the native-regex contract (not match-everywhere)",
            ));
        }
        if pattern.as_bytes().contains(&UNIFIED_SEPARATOR) {
            return Ok(Vec::new());
        }
        let out = self.spawn_count("--null-data", pattern.as_bytes());
        if out.status.code() == Some(2) {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            return Err(RegexError::new(
                pattern,
                format!("`rg` failed (exit 2): {stderr}"),
            ));
        }
        require_search_status(&out);
        Ok(self.hits(&out.stdout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocMeta, discover_index_meta, run_unified_benchmark};
    use std::cell::RefCell;

    const EXT: &str = "bin";

    /// Corpus written to a temp dir, plus its descriptors and bytes.
    struct Fixture {
        dir: tempfile::TempDir,
        docs: Vec<DocMeta>,
        bytes: Vec<Vec<u8>>,
    }

    impl Fixture {
        fn new(files: &[(&str, &[u8])]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            for (rel, bytes) in files {
                std::fs::write(dir.path().join(rel), bytes).unwrap();
            }
            let docs = discover_index_meta(dir.path()).unwrap();
            let bytes = docs
                .iter()
                .map(|d| std::fs::read(dir.path().join(&d.path)).unwrap())
                .collect();
            Self { dir, docs, bytes }
        }

        fn corpus(&self) -> Corpus<'_> {
            Corpus {
                root: self.dir.path(),
                docs: &self.docs,
            }
        }

        fn num_docs(&self) -> usize {
            self.docs.len()
        }

        fn stage(&self) -> StagedDocs {
            StagedDocs::stage(self.corpus(), EXT).unwrap()
        }
    }

    fn docs() -> Fixture {
        Fixture::new(&[
            ("a.c", b"printk(KERN_INFO \"hi\"); printk again"),
            ("b.c", b"kmalloc and mutex_lock here"),
            ("c.c", b"aaaa"),
        ])
    }

    /// The rg row over `staged`.
    fn open(staged: &StagedDocs) -> RgUnifiedIndex {
        RgUnifiedIndex::open(staged, EXT)
    }

    fn naive(hay: &[u8], needle: &[u8]) -> u64 {
        if needle.is_empty() || hay.len() < needle.len() {
            return 0;
        }
        let mut n = 0;
        for i in 0..=hay.len() - needle.len() {
            if &hay[i..i + needle.len()] == needle {
                n += 1;
            }
        }
        n
    }

    #[test]
    fn counts_agree_with_naive_scan_on_overlap_free_terms() {
        let fx = docs();
        let idx = RgIndex::build(fx.corpus());
        // Overlap-free literals (incl. single-char and empty/missing terms).
        for term in ["printk", "kmalloc", "mutex_lock", "missing", "a", "aa", ""] {
            // "aa" in "aaaa" is the documented overlap exception — checked
            // separately below; skip doc 2 for it here.
            for (i, bytes) in fx.bytes.iter().enumerate() {
                if term == "aa" && i == 2 {
                    continue;
                }
                assert_eq!(
                    idx.count(i, term.as_bytes()),
                    naive(bytes, term.as_bytes()),
                    "term={term:?} doc={i}"
                );
            }
        }
        assert_eq!(idx.count(99, b"printk"), 0); // out-of-range doc
        assert_eq!(idx.count(0, b""), 0); // empty term
    }

    #[test]
    fn overlapping_matches_are_documented_non_overlapping() {
        // ripgrep advances past each match: "aa" in "aaaa" == 2, whereas the
        // naive scan counts overlapping occurrences == 3.
        let fx = docs();
        let idx = RgIndex::build(fx.corpus());
        assert_eq!(idx.count(2, b"aa"), 2);
        assert_eq!(naive(b"aaaa", b"aa"), 3);
    }

    /// `(total_matches, docs_matched)` per term off the per-document oracle.
    fn per_doc_totals(idx: &RgIndex, num_docs: usize, terms: &[&str]) -> Vec<(u64, usize)> {
        terms
            .iter()
            .map(|term| {
                let counts: Vec<u64> = (0..num_docs)
                    .map(|i| idx.count(i, term.as_bytes()))
                    .collect();
                (
                    counts.iter().sum(),
                    counts.iter().filter(|&&c| c > 0).count(),
                )
            })
            .collect()
    }

    #[test]
    fn unified_search_agrees_with_per_doc_rg_counts() {
        let fx = docs();
        let per_doc = RgIndex::build(fx.corpus());
        let staged = fx.stage();
        let unified = open(&staged);
        assert_eq!(unified.name(), "rg");
        for term in ["printk", "kmalloc", "mutex_lock", "missing", "a", ""] {
            let mut from_unified = vec![0u64; fx.num_docs()];
            let hits = unified.search(term.as_bytes());
            // Sorted by doc_idx, no zero-count entries, no duplicates.
            let mut last_idx = None;
            for &(i, c) in &hits {
                assert!(c > 0, "zero-count hit for term={term:?} doc={i}");
                assert!(
                    last_idx.is_none_or(|prev| prev < i),
                    "hits not strictly sorted for term={term:?}"
                );
                last_idx = Some(i);
                from_unified[i] = c;
            }
            for (i, bytes) in fx.bytes.iter().enumerate() {
                assert_eq!(
                    from_unified[i],
                    per_doc.count(i, term.as_bytes()),
                    "term={term:?} doc={i}"
                );
                assert_eq!(
                    from_unified[i],
                    naive(bytes, term.as_bytes()),
                    "term={term:?} doc={i} vs naive"
                );
            }
        }
        assert_eq!(unified.search(b"aa"), vec![(2, 2)]);
        assert_eq!(unified.search(b"a\x00b"), vec![]);
        assert_eq!(unified.search(b""), vec![]);

        let r = run_unified_benchmark(|| open(&staged), &["printk", "spin_lock"]).unwrap();
        assert_eq!(r.index_name, "rg");
        assert_eq!(r.queries[0].total_matches, 2);
        assert_eq!(r.queries[0].docs_matched, 1);
        assert_eq!(r.queries[1].total_matches, 0);
        assert_eq!(r.index_size_bytes, 0);
        assert_eq!((r.mode, r.ready_time), (crate::QueryMode::Scan, None));
    }

    /// Recording observer for hook tests: every spawn/reap pair appends
    /// exactly one phase entry (balanced calls), in spawn order.
    struct RecordingObserver {
        phases: RefCell<Vec<SpawnPhase>>,
        reaps: RefCell<usize>,
    }

    impl SpawnObserver for RecordingObserver {
        fn child_spawned(&self, _pid: u32, phase: SpawnPhase) {
            self.phases.borrow_mut().push(phase);
        }
        fn child_reaped(&self) {
            *self.reaps.borrow_mut() += 1;
        }
    }

    fn recorder() -> Rc<RecordingObserver> {
        Rc::new(RecordingObserver {
            phases: RefCell::new(Vec::new()),
            reaps: RefCell::new(0),
        })
    }

    #[test]
    fn observer_sees_probe_as_index_and_queries_as_query() {
        let fx = docs();
        let obs = recorder();
        let totals = with_spawn_observer(obs.clone(), || {
            let idx = RgIndex::build(fx.corpus());
            per_doc_totals(&idx, fx.num_docs(), &["printk", "missing"])
        });
        assert_eq!(totals[0].0, 2);
        let phases = obs.phases.borrow();
        // 1 probe + one spawn per (doc, term) that reaches rg: doc 2
        // ("aaaa") is shorter than both terms, so no spawn for it.
        assert_eq!(phases.len(), 1 + 2 * 2);
        assert_eq!(phases[0], SpawnPhase::Index);
        assert!(phases[1..].iter().all(|&p| p == SpawnPhase::Query));
        assert_eq!(*obs.reaps.borrow(), phases.len());
    }

    #[test]
    fn observer_sees_one_query_spawn_per_unified_term() {
        let fx = docs();
        let staged = fx.stage();
        let obs = recorder();
        let report = with_spawn_observer(obs.clone(), || {
            run_unified_benchmark(|| open(&staged), &["printk", "missing"]).unwrap()
        });
        assert_eq!(report.queries[0].total_matches, 2);
        let phases = obs.phases.borrow();
        // Probe + walk check, then 1 spawn per term.
        assert_eq!(
            *phases,
            vec![
                SpawnPhase::Index,
                SpawnPhase::Index,
                SpawnPhase::Query,
                SpawnPhase::Query
            ]
        );
        assert_eq!(*obs.reaps.borrow(), phases.len());
    }

    #[test]
    fn observer_queries_match_unobserved_counts() {
        let fx = docs();
        let terms = ["printk", "spin_lock"];
        let observed = with_spawn_observer(recorder(), || {
            per_doc_totals(&RgIndex::build(fx.corpus()), fx.num_docs(), &terms)
        });
        let plain = per_doc_totals(&RgIndex::build(fx.corpus()), fx.num_docs(), &terms);
        assert_eq!(observed, plain);
    }

    #[test]
    #[should_panic(expected = "cannot run `rg` binary")]
    fn probe_of_missing_binary_panics_loudly() {
        probe_rg(Path::new("/nonexistent-dir-xyz/rg"));
    }

    #[test]
    fn walk_check_rejects_foreign_and_missing_files() {
        let fx = docs();
        let staged = fx.stage();
        std::fs::write(staged.dir().join("foreign.txt"), b"printk").unwrap();
        let Err(err) = std::panic::catch_unwind(|| open(&staged)) else {
            panic!("walk check passed")
        };
        assert!(
            err.downcast_ref::<String>()
                .unwrap()
                .contains("foreign.txt")
        );

        let staged = fx.stage();
        std::fs::remove_file(staged.dir().join(StagedDocs::file_name(1, EXT))).unwrap();
        let Err(err) = std::panic::catch_unwind(|| open(&staged)) else {
            panic!("walk check passed")
        };
        assert!(
            err.downcast_ref::<String>()
                .unwrap()
                .contains("skips staged document 1")
        );
    }

    #[test]
    fn staged_id_rejects_foreign_and_out_of_range_names() {
        assert_eq!(staged_id(Path::new("/s/00000001.bin"), EXT, 2), 1);
        for bad in [
            "/s/00000002.bin",
            "/s/00000001.c",
            "/s/lookup.bin",
            "/s/x:1",
        ] {
            assert!(
                std::panic::catch_unwind(|| staged_id(Path::new(bad), EXT, 2)).is_err(),
                "{bad}"
            );
        }
        assert_eq!(parse_counts(b"/s:x/00000001.bin:3\n", EXT, 2), vec![0, 3]);
        assert_eq!(parse_counts(b"/s/00000000.bin:2\0", EXT, 2), vec![2, 0]);
        for bad in [
            &b"/s/00000000.bin:1\n/s/00000000.bin:1\n"[..],
            b"/s/00000000.bin\n",
        ] {
            assert!(std::panic::catch_unwind(|| parse_counts(bad, EXT, 2)).is_err());
        }
    }

    #[test]
    fn replicated_staging_counts_each_replica() {
        let fx = docs();
        let staged = StagedDocs::stage_replicated(fx.corpus(), EXT, 2).unwrap();
        let unified = open(&staged);
        assert_eq!(unified.num_files_indexed(), 6);
        assert_eq!(unified.search(b"printk"), vec![(0, 2), (3, 2)]);
        assert_eq!(
            unified.search_regex("printk|kmalloc").unwrap(),
            vec![(0, 2), (1, 1), (3, 2), (4, 1)]
        );
    }

    /// Native regex path: hand-computed expectations, no emulation (cross-
    /// engine agreement is pinned by `--verify` on real corpora).
    #[test]
    fn search_regex_serves_native_regex_counts() {
        let fx = docs();
        let staged = fx.stage();
        let unified = open(&staged);
        for (pattern, expected) in [
            ("printk|kmalloc", vec![(0, 2), (1, 1)]),
            (r"printk\([^)]*\)", vec![(0, 1)]),
            (r"\w+_lock", vec![(1, 1)]),
            ("a+", vec![(0, 2), (1, 2), (2, 1)]),
            ("missing", vec![]),
        ] {
            assert_eq!(
                unified.search_regex(pattern).unwrap(),
                expected,
                "pattern={pattern:?}"
            );
        }
        assert_eq!(unified.search_regex("aa").unwrap(), vec![(2, 2)]);
        for term in ["printk", "kmalloc", "mutex_lock", "missing", "a"] {
            let via_regex = unified.search_regex(term).unwrap();
            let via_literal = unified.search(term.as_bytes());
            assert_eq!(via_regex, via_literal, "term={term:?}");
        }
        assert_eq!(unified.search_regex("").unwrap_err().pattern, "");
        assert_eq!(unified.search_regex("a\x00b").unwrap(), vec![]);
        let err = unified.search_regex("([").unwrap_err();
        assert_eq!(err.pattern, "([");
        assert!(!err.message.is_empty());
        assert_eq!(unified.num_files_indexed(), fx.num_docs());
        assert_eq!(unified.index_size_bytes(), 0);
    }

    #[test]
    fn search_regex_counts_multiline_matches() {
        // `--null-data` makes each file one record, so `[^)]*` spans `\n`.
        let fx = Fixture::new(&[
            ("multi.c", b"printk(KERN_INFO \"a\"\n \"b\"); tail"),
            ("single.c", b"printk(one); printk(two)"),
        ]);
        let staged = fx.stage();
        assert_eq!(
            open(&staged).search_regex(r"printk\([^)]*\)").unwrap(),
            vec![(0, 1), (1, 2)]
        );
    }

    #[test]
    fn search_regex_anchors_at_document_start() {
        // A file start is a true haystack start; `^` also anchors after `\n`.
        let fx = Fixture::new(&[("a.c", b"foo one\nfoo two\n"), ("b.c", b"foo three\n")]);
        let staged = fx.stage();
        assert_eq!(
            open(&staged).search_regex("^foo").unwrap(),
            vec![(0, 2), (1, 1)]
        );
    }

    #[test]
    fn search_regex_never_matches_across_documents() {
        let fx = Fixture::new(&[
            ("a.c", b"lead printk(open"),
            ("b.c", b"close) trail"),
            ("c.c", b"printk(real)"),
        ]);
        let staged = fx.stage();
        assert_eq!(
            open(&staged).search_regex(r"printk\([^)]*\)").unwrap(),
            vec![(2, 1)]
        );
    }

    #[test]
    fn search_regex_serves_the_linux_regex_term_set() {
        // Read through terms::term_set so the two cannot drift apart.
        let fx = docs();
        let staged = fx.stage();
        let unified = open(&staged);
        for pattern in crate::terms::term_set("linux").unwrap().regexes() {
            let hits = unified.search_regex(pattern).unwrap_or_else(|e| {
                panic!("linux regex pattern {pattern:?} must be servable by rg: {e}")
            });
            let mut last_idx = None;
            for &(i, c) in &hits {
                assert!(c > 0, "zero-count hit for pattern={pattern:?} doc={i}");
                assert!(
                    last_idx.is_none_or(|prev| prev < i),
                    "hits not strictly sorted for pattern={pattern:?}"
                );
                last_idx = Some(i);
            }
        }
    }

    #[test]
    fn rg_version_names_ripgrep() {
        let version = rg_version().expect("rg runs");
        assert!(version.starts_with("ripgrep "), "{version}");
    }

    #[test]
    fn one_runner_serves_mixed_literal_and_regex_terms() {
        let fx = docs();
        let staged = fx.stage();
        let report =
            run_unified_benchmark(|| open(&staged), &["printk", "/printk|kmalloc/"]).unwrap();
        assert_eq!(report.index_name, "rg");
        assert_eq!(report.num_files_indexed, fx.num_docs());
        assert_eq!(report.memory, None);
        let rows: Vec<(&str, u64, usize, bool)> = report
            .queries
            .iter()
            .map(|q| {
                (
                    q.term.as_str(),
                    q.total_matches,
                    q.docs_matched,
                    q.counts_exact,
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![("printk", 2, 1, true), ("/printk|kmalloc/", 3, 2, false),]
        );
        let err = run_unified_benchmark(|| open(&staged), &["printk", "/([/"]).unwrap_err();
        assert_eq!(err.pattern, "([");
    }

    #[test]
    fn counts_large_match_volume() {
        let fx = Fixture::new(&[("big.c", b"ab\n".repeat(50_000).as_slice())]);
        let staged = fx.stage();
        let unified = open(&staged);
        assert_eq!(unified.search(b"a"), vec![(0, 50_000)]);
        assert_eq!(unified.search_regex("a").unwrap(), vec![(0, 50_000)]);
    }

    #[test]
    fn unified_row_counts_only_enumerated_files() {
        // A NUL-bearing file is never enumerated, so never staged.
        let fx = Fixture::new(&[
            ("a.c", b"printk here"),
            ("blob.bin", b"printk\x00here"),
            ("b.c", b"printk there"),
        ]);
        assert_eq!(fx.num_docs(), 2);
        let staged = fx.stage();
        let unified = open(&staged);
        assert_eq!(unified.num_files_indexed(), 2);
        assert_eq!(unified.search(b"printk"), vec![(0, 1), (1, 1)]);
    }
}

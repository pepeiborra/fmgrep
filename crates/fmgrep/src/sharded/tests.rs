use super::*;

mod build;
mod count;
mod docfm;
mod walk;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::blobs::{create_sink, finish_sink};
use crate::manifest::BLOBS_SHARD;
use crate::matching::MatchMode;
use crate::plan::plan_shards;
use crate::probe::candidate_docs;
use crate::sharded::build::build_plan;
use crate::sharded::shard::Probe;

/// rg's `-i` mode, the one every fold plan is built under.
fn ci() -> MatchMode {
    MatchMode {
        ignore_case: true,
        ..MatchMode::default()
    }
}

/// Live built items, so a build's residency can be asserted.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static MAX_LIVE: AtomicUsize = AtomicUsize::new(0);
/// The counters are global: every test that reads them runs alone.
static COUNTER: Mutex<()> = Mutex::new(());
/// One-shot: the first N builds to arrive release each other, proving N
/// really overlap with no sleep; every later build passes straight through,
/// so more shards than the window cannot hang the suite. Unset outside the
/// tests that arm it.
static BARRIER: Mutex<Option<std::sync::Arc<Arrivals>>> = Mutex::new(None);

struct Arrivals {
    want: usize,
    seen: Mutex<usize>,
    open: std::sync::Condvar,
}

impl Arrivals {
    fn wait(&self) {
        let mut seen = self.seen.lock().unwrap();
        if *seen >= self.want {
            return;
        }
        *seen += 1;
        if *seen >= self.want {
            self.open.notify_all();
            return;
        }
        while *seen < self.want {
            seen = self.open.wait(seen).unwrap();
        }
    }
}

fn set_barrier(n: usize) {
    *BARRIER.lock().unwrap() = Some(std::sync::Arc::new(Arrivals {
        want: n,
        seen: Mutex::new(0),
        open: std::sync::Condvar::new(),
    }));
}

fn clear_barrier() {
    *BARRIER.lock().unwrap() = None;
}

fn wait_at_barrier() {
    let barrier = BARRIER.lock().unwrap().clone();
    if let Some(barrier) = barrier {
        barrier.wait();
    }
}

/// A built item counted in [`LIVE`] until dropped.
struct Counted;

impl Counted {
    /// Counts itself live, then waits at the barrier.
    fn build(_cut: &Cut) -> Result<Self, String> {
        let live = LIVE.fetch_add(1, Ordering::SeqCst) + 1;
        MAX_LIVE.fetch_max(live, Ordering::SeqCst);
        wait_at_barrier();
        Ok(Self)
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

fn docs_of(bytes: &[&[u8]]) -> Vec<Document> {
    bytes
        .iter()
        .enumerate()
        .map(|(id, b)| Document {
            id,
            path: format!("f{id}.c"),
            bytes: b.to_vec(),
        })
        .collect()
}

fn fixture() -> Vec<Document> {
    docs_of(&[
        b"",
        b"printk here printk",
        b"aaaa",
        b"",
        b"nothing at all",
        b"tail printk",
    ])
}

/// The cuts `docs` written verbatim into `dir` closes at `cap`: exactly the
/// slots the live stream would announce, already final. Fixtures hold shapes
/// the corpus walk skips (NUL bytes) or rejects (no documents).
fn stage(dir: &Path, docs: &[Document], cap: u64) -> Cuts {
    let shard = dir.join(crate::manifest::DOCS_SHARD);
    let bytes: Vec<u8> = docs.iter().flat_map(|d| d.bytes.clone()).collect();
    std::fs::write(&shard, bytes).unwrap();
    let lens = doc_lens(docs);
    let paths: Vec<String> = docs.iter().map(|d| d.path.clone()).collect();
    Cuts::closed(Some(shard), plan_shards(&lens, cap), &lens, &paths).unwrap()
}

/// The in-memory cuts of `docs` at `cap`: no shard to read from.
fn resident_cuts(docs: &[Document], cap: u64) -> Cuts {
    let lens = doc_lens(docs);
    let paths: Vec<String> = docs.iter().map(|d| d.path.clone()).collect();
    Cuts::closed(None, plan_shards(&lens, cap), &lens, &paths).unwrap()
}

fn doc_lens(docs: &[Document]) -> Vec<u64> {
    docs.iter().map(|d| d.bytes.len() as u64).collect()
}

/// Persist every slot into a real `blobs.shard`.
fn persist(index: &DocfmIndex, dir: &Path) -> (Vec<String>, Vec<u64>) {
    let mut sink = create_sink(dir).unwrap();
    for slot in 0..index.blob_slots() {
        sink.write_slot(|out| index.encode_into(slot, out)).unwrap();
    }
    finish_sink(sink).unwrap()
}

fn load_from(
    dir: &Path,
    labels: &[String],
    lens: &[u64],
    doc_lens: &[u64],
) -> Result<DocfmIndex, String> {
    DocfmIndex::load(&BlobSlots::open(dir, labels, lens)?, doc_lens)
}

/// Persist every slot into a real `blobs.shard` and load it back.
fn round_trip(index: &DocfmIndex, dir: &Path, docs: &[Document]) -> DocfmIndex {
    let (labels, lens) = persist(index, dir);
    load_from(dir, &labels, &lens, &doc_lens(docs)).unwrap()
}

/// Literals, an anchored pattern, an anchorless one and two folded ones.
const TERMS: [&str; 6] = ["printk", "aa", "a", "missing", "tail", ""];
const PATTERNS: [&str; 3] = ["pr[i1]ntk", "a+", "no|tail"];
const CI_PATTERNS: [&str; 2] = ["PRINTK", "Tail"];

fn prepared() -> Vec<PreparedPattern> {
    PATTERNS
        .iter()
        .map(|p| PreparedPattern::prepare(p).expect("valid pattern"))
        .chain(
            CI_PATTERNS
                .iter()
                .map(|p| PreparedPattern::prepare_with(p, ci()).expect("valid pattern")),
        )
        .collect()
}

fn queries<'a>(patterns: &'a [PreparedPattern]) -> Vec<Query<'a>> {
    let mut qs: Vec<Query<'a>> = TERMS.iter().map(|t| Query::Literal(t.as_bytes())).collect();
    qs.extend(patterns.iter().map(Query::Regex));
    qs
}

/// Comparable answer shape: exact hits plus the raw-needing docs, `All`
/// expanded. Timings are excluded — they are not reproducible.
type Answers = Vec<(Vec<(usize, u64)>, Vec<usize>)>;

fn shape(answers: Vec<Answer>, num_docs: usize) -> Answers {
    answers
        .into_iter()
        .map(|a| (a.exact, candidate_docs(a.candidates, num_docs)))
        .collect()
}

fn answers_of(index: &DocfmIndex, qs: &[Query<'_>]) -> Answers {
    shape(index.probe_all(qs), index.num_files_indexed())
}

fn panic_message(f: impl FnOnce()) -> String {
    let err =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err("expected a panic");
    err.downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(ToString::to_string))
        .unwrap_or_default()
}

fn hits(index: &DocfmIndex, term: &str) -> Vec<(usize, u64)> {
    index.probe_literal(term.as_bytes())
}

fn candidates(index: &DocfmIndex, pattern: &str, num_docs: usize) -> Vec<usize> {
    let prepared = PreparedPattern::prepare(pattern).expect("valid pattern");
    candidate_docs(index.regex_candidates(&prepared), num_docs)
}

#[test]
fn counts_are_shard_count_invariant() {
    let docs = fixture();
    let whole = DocfmIndex::build_with_cap(&docs, 1 << 20);
    assert_eq!(whole.blob_slots(), 1);
    for cap in [1u64, 5, 12, 19, 40, 1 << 20] {
        let index = DocfmIndex::build_with_cap(&docs, cap);
        for term in ["printk", "aa", "a", "missing", "tail", "g h"] {
            assert_eq!(hits(&index, term), hits(&whole, term), "cap={cap} {term:?}");
        }
        for pattern in ["printk", "a+", "pr[i1]ntk", "missing", "[a-z]+"] {
            assert_eq!(
                candidates(&index, pattern, docs.len()),
                candidates(&whole, pattern, docs.len()),
                "cap={cap} {pattern:?}"
            );
        }
        assert_eq!(index.num_files_indexed(), docs.len(), "cap={cap}");
    }
    // Overlapping counts survive the split: "aa" in "aaaa" is 3.
    assert_eq!(hits(&whole, "aa"), vec![(2, 3)]);
    assert_eq!(hits(&whole, "printk"), vec![(1, 2), (5, 1)]);
}

#[test]
fn merged_hits_are_ascending_and_disjoint() {
    let docs = fixture();
    let index = DocfmIndex::build_with_cap(&docs, 6);
    assert!(index.blob_slots() > 1);
    let merged = hits(&index, "a");
    let mut seen: Option<usize> = None;
    for &(doc, count) in &merged {
        assert!(count > 0);
        assert!(
            seen.is_none_or(|prev| prev < doc),
            "not ascending: {merged:?}"
        );
        seen = Some(doc);
    }
}

#[test]
fn probes_do_not_depend_on_the_pool_size() {
    let docs = fixture();
    let index = DocfmIndex::build_with_cap(&docs, 6);
    assert!(index.blob_slots() > 1);
    let probe = |threads: usize| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        pool.install(|| {
            let hits: Vec<_> = ["printk", "aa", "a", "tail"]
                .map(|t| hits(&index, t))
                .into();
            let candidates: Vec<_> = ["printk", "a+", "pr[i1]ntk", "[a-z]+"]
                .map(|p| candidates(&index, p, docs.len()))
                .into();
            (hits, candidates)
        })
    };
    let one = probe(1);
    assert_eq!(probe(4), one);
    for h in &one.0 {
        assert!(h.windows(2).all(|w| w[0].0 < w[1].0), "{h:?}");
    }
    for c in &one.1 {
        assert!(c.windows(2).all(|w| w[0] < w[1]), "{c:?}");
    }
}

#[test]
fn empty_docs_partition_out_within_their_shard() {
    let docs = fixture();
    let index = DocfmIndex::build_with_cap(&docs, 6);
    // Empty docs keep their ids and never take a hit.
    for (doc, _) in hits(&index, "a") {
        assert!(!docs[doc].bytes.is_empty());
    }
    assert_eq!(hits(&index, "printk"), vec![(1, 2), (5, 1)]);
    // Anchorless patterns still cover them.
    assert_eq!(candidates(&index, "a*", docs.len()), vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn empty_and_separator_terms_match_nothing() {
    let docs = fixture();
    let index = DocfmIndex::build_with_cap(&docs, 6);
    for term in ["", "\0", "a\0", "\0printk"] {
        assert!(hits(&index, term).is_empty(), "term={term:?}");
    }
}

#[test]
fn load_rebuilds_the_plan_from_labels_and_doc_lens() {
    let docs = fixture();
    let dir = tempfile::tempdir().unwrap();
    let built = DocfmIndex::build_with_cap(&docs, 12);
    assert!(built.blob_slots() > 1);
    // No environment at load: the plan comes from the labels alone.
    let back = round_trip(&built, dir.path(), &docs);
    assert_eq!(back.blob_slots(), built.blob_slots());
    assert_eq!(back.num_files_indexed(), built.num_files_indexed());
    assert_eq!(back.index_size_bytes(), built.index_size_bytes());
    for term in ["printk", "aa", "missing", "tail"] {
        assert_eq!(hits(&back, term), hits(&built, term), "term={term:?}");
    }
    let labels: Vec<String> = (0..built.blob_slots())
        .map(|s| built.encode_into(s, &mut Vec::new()).unwrap())
        .collect();
    assert!(labels.iter().all(|l| l.starts_with("shard-")), "{labels:?}");
}

#[test]
fn missing_or_skewed_shard_blob_rejected_loudly() {
    let docs = fixture();
    let dir = tempfile::tempdir().unwrap();
    let built = DocfmIndex::build_with_cap(&docs, 12);
    let (labels, lens) = persist(&built, dir.path());
    assert!(lens.len() > 1);
    let doc_lens = doc_lens(&docs);
    let shard_path = dir.path().join(BLOBS_SHARD);
    // A blob shard that does not match its lengths is loud.
    let mut short = std::fs::read(&shard_path).unwrap();
    short.truncate(short.len() - 1);
    std::fs::write(&shard_path, &short).unwrap();
    let err = load_from(dir.path(), &labels, &lens, &doc_lens)
        .err()
        .expect("a short blob shard must be refused");
    assert!(err.contains("tampered"), "got: {err}");
    // An unparsable label and an empty inventory are each a tampered
    // inventory.
    std::fs::write(&shard_path, b"").unwrap();
    for bad in [vec!["index.bin".to_string()], Vec::new()] {
        let zeros = vec![0u64; bad.len()];
        let err = load_from(dir.path(), &bad, &zeros, &doc_lens)
            .err()
            .unwrap_or_else(|| panic!("labels {bad:?} must be refused"));
        assert!(
            err.contains("blob(s)") && err.contains("tampered"),
            "got: {err}"
        );
    }
}

/// An inventory that fails the plan is refused before the shard is mapped.
#[test]
fn mapped_load_keeps_the_plan_refusal() {
    let docs = fixture();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(BLOBS_SHARD), b"").unwrap();
    let labels = ["shard-00000000-00000003.bin".to_string()];
    let err = load_from(dir.path(), &labels, &[0], &doc_lens(&docs))
        .err()
        .expect("must refuse");
    assert!(err.contains("cover 3 of 6 doc(s)"), "got: {err}");
}

#[test]
fn a_view_error_names_the_slot() {
    let docs = docs_of(&[b"good", b"also good"]);
    let dir = tempfile::tempdir().unwrap();
    let (labels, lens) = persist(&DocfmIndex::build_with_cap(&docs, 5), dir.path());
    assert_eq!(labels.len(), 2);
    let shard_path = dir.path().join(BLOBS_SHARD);
    let mut bytes = std::fs::read(&shard_path).unwrap();
    let second = lens[0] as usize;
    bytes[second..second + 8].copy_from_slice(b"XXXXXXXX");
    std::fs::write(&shard_path, &bytes).unwrap();
    let err = load_from(dir.path(), &labels, &lens, &doc_lens(&docs))
        .err()
        .expect("must refuse");
    assert!(
        err.starts_with("shard slot 1: ") && err.contains("corrupt docfm blob"),
        "got: {err}"
    );
}

/// Honest count and locate over `docs`' shard text, in docfm's units: the
/// located value is the hit's document ordinal.
struct Scan {
    text: Vec<u8>,
}

impl Scan {
    fn of(docs: &[Document]) -> Self {
        let mut text = Vec::new();
        for d in docs {
            text.extend_from_slice(&d.bytes);
            text.push(UNIFIED_SEPARATOR);
        }
        Self { text }
    }

    fn count(&self, term: &[u8]) -> u64 {
        crate::pattern::count_overlapping(&self.text, &memchr::memmem::Finder::new(term))
    }

    fn locate(&self, term: &[u8], visit: &mut dyn FnMut(usize)) {
        if term.is_empty() || self.text.len() < term.len() {
            return;
        }
        for at in 0..=self.text.len() - term.len() {
            if &self.text[at..at + term.len()] == term {
                visit(self.text[..at].iter().filter(|&&b| b == 0).count());
            }
        }
    }
}

fn probes_of(docs: &[Document]) -> Vec<Probe> {
    sample_probes(&docs.iter().collect::<Vec<_>>())
}

#[test]
fn self_check_rejects_a_mislocating_locate() {
    let docs = docs_of(&[b"printk here printk", b"aaaa", b"tail printk"]);
    let scan = Scan::of(&docs);
    let probes = probes_of(&docs);
    let count = |t: &[u8]| scan.count(t);
    let honest = |t: &[u8], v: &mut dyn FnMut(usize)| scan.locate(t, v);
    assert!(self_check(&count, &honest, docs.len(), &probes));
    // Honest counts, but every locate claims one hit in document 0.
    let lying = |t: &[u8], v: &mut dyn FnMut(usize)| {
        if !t.is_empty() {
            v(0);
        }
    };
    assert!(!self_check(&count, &lying, docs.len(), &probes));
    // A hit past the shard's documents is stray.
    let stray = |t: &[u8], v: &mut dyn FnMut(usize)| scan.locate(t, &mut |d| v(d + docs.len()));
    assert!(!self_check(&count, &stray, docs.len(), &probes));
}

/// The check scores each probe against its own document, so a locate that
/// attributes every hit to the neighbouring one is still refused.
#[test]
fn off_by_one_locate_is_rejected() {
    let docs = docs_of(&[b"abcdefgh", b"ijklmnop"]);
    let scan = Scan::of(&docs);
    let count = |t: &[u8]| scan.count(t);
    let shifting = |t: &[u8], v: &mut dyn FnMut(usize)| scan.locate(t, &mut |d| v(1 - d));
    assert!(!self_check(
        &count,
        &shifting,
        docs.len(),
        &probes_of(&docs)
    ));
}

/// A probe whose short prefixes are too common is located at a rarer longer
/// one, or only counted when every prefix is too common.
#[test]
fn common_probes_locate_within_budget() {
    let common = b"abcd".repeat(2000);
    let docs = docs_of(&[&common, b"abcd, then a rare tail", &common]);
    let scan = Scan::of(&docs);
    let count = |t: &[u8]| scan.count(t);
    let bounded = |t: &[u8], v: &mut dyn FnMut(usize)| {
        let mut hits = 0;
        scan.locate(t, &mut |d| {
            hits += 1;
            v(d)
        });
        assert!(hits <= 1024, "{hits} hits located for {t:?}");
    };
    assert!(self_check(&count, &bounded, docs.len(), &probes_of(&docs)));
    DocfmIndex::build_with_cap(&docs, 1 << 20);
}

/// At most two probes per document and never two at the same offset, so a
/// one-byte document contributes one.
#[test]
fn probes_are_sampled_once_per_offset() {
    let docs = docs_of(&[b"printk here printk", b"a", b"aaaa"]);
    assert_eq!(probes_of(&docs).len(), 5);
}

/// Both entry points refuse a degenerate corpus by panicking with the same
/// message; `build_into` must not downgrade the refusal to an `Err`.
#[test]
fn degenerate_corpora_refuse_loudly() {
    let cases: Vec<(Vec<Document>, &str)> = vec![
        (Vec::new(), "no indexable bytes"),
        (docs_of(&[b"", b""]), "no indexable bytes"),
        (docs_of(&[b"has\0nul"]), "separator byte"),
    ];
    // Every case here is one shard; a refusal with shards in flight is
    // `build::a_panicking_build_fails_loudly_instead_of_hanging`.
    type Entry<'a> = (&'a str, &'a dyn Fn(&[Document]));
    let entries: [Entry; 2] = [
        ("build_with_cap", &|docs: &[Document]| {
            DocfmIndex::build_with_cap(docs, 1 << 20);
        }),
        ("build_into", &|docs: &[Document]| {
            let dir = tempfile::tempdir().unwrap();
            let staged = stage(dir.path(), docs, 1 << 20);
            let mut sink = create_sink(dir.path()).unwrap();
            let out = DocfmIndex::build_into(&staged, 1 << 20, None, false, &mut sink);
            unreachable!("build_into returned {:?} instead of refusing", out.is_ok());
        }),
    ];
    for (entry, run) in entries {
        for (docs, want) in &cases {
            let msg = panic_message(|| run(docs));
            assert!(msg.contains(want), "{entry}: want {want:?}, got: {msg:?}");
            assert!(msg.contains("docfm build"), "{entry}: got: {msg:?}");
        }
    }
}

/// The planned length counts every separator.
#[test]
fn over_limit_counts_every_separator() {
    // 4 + 1 + 3 + 1 == 9 text bytes.
    let cuts = resident_cuts(&docs_of(&[b"aaaa", b"", b"bbb"]), 1 << 20);
    let cut = cuts.get(0).unwrap();
    assert!(over_limit(&cut, 9).is_ok());
    let err = over_limit(&cut, 8).unwrap_err();
    for want in [
        "shard-00000000-00000003.bin",
        "9 text bytes",
        "8-byte limit",
        "FMGREP_SHARD_BYTES",
    ] {
        assert!(err.contains(want), "want {want:?}, got: {err}");
    }
}

/// A shard over docfm's text limit is refused before its own documents are
/// read or its structure built.
#[test]
fn over_limit_shard_is_refused_before_its_own_construction() {
    // A synthetic over-limit plan costs no allocation: `docs.shard` is empty,
    // so any read of the 2 GiB document it claims would fail loudly instead.
    let synthetic = tempfile::tempdir().unwrap();
    let shard = synthetic.path().join(crate::manifest::DOCS_SHARD);
    std::fs::write(&shard, b"").unwrap();
    let lens = [2u64 << 30];
    let huge = Cuts::closed(
        Some(shard),
        plan_shards(&lens, u64::MAX),
        &lens,
        &["big.c".to_string()],
    )
    .unwrap();
    let mut sink = create_sink(synthetic.path()).unwrap();
    let err = DocfmIndex::build_into(&huge, u64::MAX, None, false, &mut sink)
        .expect_err("an over-limit shard must be refused");
    for want in [
        "docfm build".to_string(),
        "2147483649 text bytes".to_string(),
        format!("{}-byte limit", ::docfm::MAX_TEXT_BYTES),
    ] {
        assert!(err.contains(&want), "want {want:?}, got: {err}");
    }
}

/// Both persist paths must write the same blob shard, byte for byte, and the
/// shards in flight must not change one byte of it.
#[test]
fn build_into_matches_the_resident_persist_path() {
    let docs = fixture();
    for (cap, window) in [1u64, 5, 12, 19, 40, 1 << 20]
        .into_iter()
        .flat_map(|cap| [1usize, 2, 4].map(|t| (cap, t)))
    {
        let woven = tempfile::tempdir().unwrap();
        let staged = stage(woven.path(), &docs, cap);
        let mut sink = create_sink(woven.path()).unwrap();
        let size = DocfmIndex::build_into(&staged, cap, Some(window), false, &mut sink).unwrap();
        let (labels, lens) = finish_sink(sink).unwrap();

        let resident = DocfmIndex::build_with_cap(&docs, cap);
        let plain = tempfile::tempdir().unwrap();
        let (want_labels, want_lens) = persist(&resident, plain.path());

        let at = format!("cap={cap} window={window}");
        assert_eq!(labels, want_labels, "{at}");
        assert_eq!(lens, want_lens, "{at}");
        assert_eq!(
            std::fs::read(woven.path().join(BLOBS_SHARD)).unwrap(),
            std::fs::read(plain.path().join(BLOBS_SHARD)).unwrap(),
            "{at}"
        );
        assert_eq!(size, resident.index_size_bytes(), "{at}");
        let back = load_from(woven.path(), &labels, &lens, &doc_lens(&docs)).unwrap();
        for term in ["printk", "aa", "a", "missing", "tail"] {
            assert_eq!(hits(&back, term), hits(&resident, term), "{at} {term:?}");
        }
    }
}

/// The scheduler holds at most `window` built items, and really builds that
/// many at once: the one-shot gate inside `build` only opens when
/// `min(window, shards)` builds are inside it.
#[test]
fn shards_in_flight_stay_within_the_window() {
    let _lock = COUNTER.lock().unwrap_or_else(|e| e.into_inner());
    let docs = fixture();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    for window in [1usize, 4] {
        LIVE.store(0, Ordering::SeqCst);
        MAX_LIVE.store(0, Ordering::SeqCst);
        let cuts = resident_cuts(&docs, 6);
        let shards = plan_shards(&doc_lens(&docs), 6).len();
        assert!(shards > 1);
        set_barrier(window.min(shards));
        let mut emitted = 0;
        build_plan(
            &pool,
            &cuts,
            window,
            &Counted::build,
            &mut |_slot, built| {
                drop(built);
                emitted += 1;
                Ok(())
            },
        )
        .unwrap();
        clear_barrier();
        assert_eq!(emitted, shards);
        assert!(MAX_LIVE.load(Ordering::SeqCst) <= window, "window={window}");
        assert_eq!(LIVE.load(Ordering::SeqCst), 0, "window={window}");
    }
}

#[test]
fn out_of_range_slot_refuses() {
    let docs = fixture();
    let index = DocfmIndex::build_with_cap(&docs, 1 << 20);
    let err = index
        .encode_into(index.blob_slots(), &mut Vec::new())
        .expect_err("an out-of-range slot must refuse");
    assert!(err.contains("out of range"), "got: {err}");
}

/// The build is called exactly once per planned shard, with that shard's
/// range: no call spans two shards, so the documents in flight are one
/// shard's per task. At a window of one the calls are the plan, in order.
#[test]
fn one_shard_of_documents_resident_at_a_time() {
    let docs = fixture();
    let plan = plan_shards(&doc_lens(&docs), 6);
    assert!(plan.len() > 1);
    for window in [1usize, 4] {
        let ranges = Mutex::new(Vec::new());
        let mut emitted = Vec::new();
        let cuts = resident_cuts(&docs, 6);
        build_shards(
            &cuts,
            6,
            Some(window),
            &|cut| {
                ranges.lock().unwrap().push(cut.docs.clone());
                Ok(())
            },
            &mut |slot, ()| {
                emitted.push(slot);
                Ok(())
            },
        )
        .unwrap();
        let mut got = ranges.into_inner().unwrap();
        if window > 1 {
            got.sort_by_key(|r| r.start);
        }
        assert_eq!(got, plan, "window={window}");
        let want: Vec<usize> = (0..plan.len()).collect();
        assert_eq!(emitted, want, "window={window}");
    }
}

/// Four docs in one shard, the second empty.
fn walk_fixture() -> DocfmIndex {
    DocfmIndex::build_with_cap(&docs_of(&[b"kkkk", b"", b"kKk kk", b"xxxx"]), 1 << 20)
}

/// A walk that gave up keeps every doc id the shard answers for, empties
/// included: widening is the only safe give-up.
#[test]
fn a_walk_that_gave_up_keeps_the_whole_shard() {
    let index = walk_fixture();
    let (meta, shard) = (&index.metas[0], &index.shards[0]);
    assert_eq!(meta.docs_at(shard, None, None), vec![0, 1, 2, 3]);
}

/// With a required literal in hand, give-up prunes with it instead of
/// keeping the whole shard.
#[test]
fn a_walk_that_gave_up_falls_back_to_the_anchor() {
    let index = walk_fixture();
    let (meta, shard) = (&index.metas[0], &index.shards[0]);
    assert_eq!(meta.docs_at(shard, None, Some(b"kk")), vec![0, 2]);
    assert_eq!(meta.docs_at(shard, None, Some(b"zz")), Vec::<usize>::new());
}

/// One pass answers every query as the per-query probes do.
#[test]
fn probe_all_matches_per_query_probes() {
    let docs = fixture();
    let dir = tempfile::tempdir().unwrap();
    let built = DocfmIndex::build_with_cap(&docs, 6);
    let patterns = prepared();
    let qs = queries(&patterns);
    for index in [round_trip(&built, dir.path(), &docs), built] {
        let want: Answers = qs
            .iter()
            .map(|q| match *q {
                Query::Literal(term) => (index.probe_literal(term), Vec::new()),
                Query::Regex(p) => (
                    Vec::new(),
                    candidate_docs(index.regex_candidates(p), docs.len()),
                ),
                Query::Count(_) | Query::LiteralWithin { .. } => unreachable!("not queried"),
            })
            .collect();
        assert_eq!(answers_of(&index, &qs), want);
    }
}

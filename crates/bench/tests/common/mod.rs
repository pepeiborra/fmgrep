//! Shared fixtures and naive index impls for the bench test binaries.
#![allow(dead_code)]

use bench::{
    BenchmarkReport, Corpus, DocMeta, Document, RegexError, TraceOptions, UNIFIED_SEPARATOR,
    UnifiedCorpus, UnifiedIndex, UnifiedRegexSearch, discover_index_meta,
    write_jsonl_trace_with_options,
};

/// A corpus on disk plus its descriptors; keep it alive while an index built
/// over it is in use.
pub struct Fixture {
    dir: tempfile::TempDir,
    docs: Vec<DocMeta>,
}

impl Fixture {
    pub fn new(files: &[(&str, &[u8])]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for (rel, bytes) in files {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
        let docs = discover_index_meta(dir.path()).unwrap();
        assert_eq!(
            docs.len(),
            files.len(),
            "fixture files must all be indexable"
        );
        Self { dir, docs }
    }

    pub fn corpus(&self) -> Corpus<'_> {
        Corpus {
            root: self.dir.path(),
            docs: &self.docs,
        }
    }

    pub fn num_docs(&self) -> usize {
        self.docs.len()
    }
}

/// Read every document of `corpus` back into memory, in id order.
pub fn read_corpus_docs(corpus: Corpus<'_>) -> Result<Vec<Document>, String> {
    corpus
        .docs
        .iter()
        .enumerate()
        .map(|(id, d)| {
            let path = corpus.root.join(&d.path);
            std::fs::read(&path)
                .map(|bytes| Document {
                    id,
                    path: d.path.clone(),
                    bytes,
                })
                .map_err(|e| format!("error: cannot read {}: {e}", path.display()))
        })
        .collect()
}

/// [`read_corpus_docs`], which must succeed (test impls only).
pub fn read_docs(corpus: Corpus<'_>) -> Vec<Document> {
    read_corpus_docs(corpus).unwrap()
}

/// Two-doc corpus: one with two `printk`s, one with none.
pub fn tiny_docs() -> Fixture {
    Fixture::new(&[("a.c", b"printk hello printk"), ("b.c", b"nothing here")])
}

/// Index that drops its last input doc: the report must carry the indexed
/// count, not the input length.
pub struct SkippyUnified {
    kept: usize,
}

impl SkippyUnified {
    pub fn build(corpus: Corpus<'_>) -> Self {
        Self {
            kept: corpus.num_docs().saturating_sub(1),
        }
    }
}

impl UnifiedIndex for SkippyUnified {
    fn name(&self) -> &str {
        "skippy-unified"
    }
    fn search(&self, term: &[u8]) -> Vec<(usize, u64)> {
        if term != b"printk" {
            return Vec::new();
        }
        (0..self.kept).map(|i| (i, 1)).collect()
    }
    fn num_files_indexed(&self) -> usize {
        self.kept
    }
    fn index_size_bytes(&self) -> u64 {
        0
    }
}

impl UnifiedRegexSearch for SkippyUnified {
    fn search_regex(&self, _pattern: &str) -> Result<Vec<(usize, u64)>, RegexError> {
        panic!("literal fixture: no regex term belongs in its runs");
    }
}

/// Three-doc corpus with an empty middle document.
pub fn unified_docs() -> Fixture {
    Fixture::new(&[
        ("a.c", b"printk hello"),
        ("b.c", b""),
        ("c.c", b"printk printk"),
    ])
}

/// Naive unified impl: owns a [`UnifiedCorpus`], scans each doc slice
/// with an overlapping naive scan. Reference semantics for tests.
pub struct DummyUnified {
    corpus: UnifiedCorpus,
}

impl DummyUnified {
    pub fn build(corpus: Corpus<'_>) -> Self {
        Self {
            corpus: UnifiedCorpus::concatenate(&read_docs(corpus)),
        }
    }
}

impl UnifiedIndex for DummyUnified {
    fn name(&self) -> &str {
        "dummy-unified"
    }
    fn search(&self, term: &[u8]) -> Vec<(usize, u64)> {
        if term.is_empty() || term.contains(&UNIFIED_SEPARATOR) {
            return Vec::new();
        }
        let mut hits = Vec::new();
        // Every document is followed by exactly one separator, so splitting
        // on it yields the documents in id order (plus a trailing empty).
        let docs = self
            .corpus
            .bytes()
            .split(|&b| b == UNIFIED_SEPARATOR)
            .take(self.corpus.num_docs());
        for (i, hay) in docs.enumerate() {
            if hay.len() < term.len() {
                continue;
            }
            let mut n = 0u64;
            for p in 0..=hay.len() - term.len() {
                if &hay[p..p + term.len()] == term {
                    n += 1;
                }
            }
            if n > 0 {
                hits.push((i, n));
            }
        }
        hits
    }
    fn index_size_bytes(&self) -> u64 {
        self.corpus.len() as u64
    }
    fn num_files_indexed(&self) -> usize {
        self.corpus.num_docs()
    }
}

impl UnifiedRegexSearch for DummyUnified {
    fn search_regex(&self, _pattern: &str) -> Result<Vec<(usize, u64)>, RegexError> {
        panic!("literal fixture: no regex term belongs in its runs");
    }
}

/// Dispatch mock: `search` and `search_regex` answer with distinct marker
/// counts, so a mixed run's per-term rows say which path each term took.
pub struct KindMock;

impl UnifiedIndex for KindMock {
    fn name(&self) -> &str {
        "kind-mock"
    }
    fn search(&self, _term: &[u8]) -> Vec<(usize, u64)> {
        vec![(0, 1)]
    }
    fn num_files_indexed(&self) -> usize {
        3
    }
    fn index_size_bytes(&self) -> u64 {
        0
    }
}

impl UnifiedRegexSearch for KindMock {
    fn search_regex(&self, _pattern: &str) -> Result<Vec<(usize, u64)>, RegexError> {
        Ok(vec![(0, 2)])
    }
}

/// Fake regex-capable unified engine: its literal `search` panics when
/// called, so a green run proves the regex runner engaged the engine's own
/// regex path and never a scan fallback. Engagements land in the
/// thread-local counter below (thread-local because `#[test]`s run in
/// parallel on shared threads).
pub struct MockRegexUnified;

// Out-of-band engagement counter for the mock above (plain comment:
// rustdoc documents no macro invocation).
thread_local! {
    pub static UNIFIED_REGEX_CALLS: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

impl UnifiedIndex for MockRegexUnified {
    fn name(&self) -> &str {
        "mock-regex-unified"
    }
    fn search(&self, _term: &[u8]) -> Vec<(usize, u64)> {
        panic!("literal search() must not be engaged on a regex run");
    }
    fn num_files_indexed(&self) -> usize {
        3
    }
    fn index_size_bytes(&self) -> u64 {
        11
    }
}

impl UnifiedRegexSearch for MockRegexUnified {
    fn search_regex(&self, pattern: &str) -> Result<Vec<(usize, u64)>, RegexError> {
        UNIFIED_REGEX_CALLS.with(|c| c.set(c.get() + 1));
        match pattern {
            "mock.*" => Ok(vec![(0, 2), (2, 1)]),
            _ => Err(RegexError::new(pattern, "unsupported mock pattern")),
        }
    }
}

/// Fresh trace dir + path for one test.
pub fn trace_test_path(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("bench-test-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("trace.jsonl");
    // A previous crashed run must not pollute this one.
    std::fs::remove_file(&path).ok();
    (dir, path)
}

/// Overwrite `path` with `report`'s lines under the default options.
pub fn write_jsonl_trace(
    report: &BenchmarkReport,
    path: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    write_jsonl_trace_with_options(report, path, &TraceOptions::default())
}

/// Append `report`'s lines, stamping `run_id` and `num_docs`.
pub fn write_jsonl_trace_with_run_id(
    report: &BenchmarkReport,
    path: impl AsRef<std::path::Path>,
    run_id: &str,
    num_docs: usize,
) -> std::io::Result<()> {
    write_jsonl_trace_with_options(
        report,
        path,
        &TraceOptions {
            run_id: Some(run_id.to_owned()),
            num_docs,
            append: true,
            ..TraceOptions::default()
        },
    )
}

/// [`write_jsonl_trace_with_run_id`] plus the corpus id.
pub fn write_jsonl_trace_with_run_id_and_corpus(
    report: &BenchmarkReport,
    path: impl AsRef<std::path::Path>,
    run_id: &str,
    corpus: &str,
    num_docs: usize,
) -> std::io::Result<()> {
    write_jsonl_trace_with_options(
        report,
        path,
        &TraceOptions {
            run_id: Some(run_id.to_owned()),
            corpus: Some(corpus.to_owned()),
            num_docs,
            append: true,
            ..TraceOptions::default()
        },
    )
}

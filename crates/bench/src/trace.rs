//! JSONL trace: one self-contained JSON object per query plus a final
//! index-summary line.

use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::report::{BenchmarkReport, LoadSpan, MemoryStats, QueryMode};

/// `skip_serializing_if` helper for `fixture`: omit the field when false.
fn is_false(b: &bool) -> bool {
    !*b
}

/// Omit `num_replicas` for an unreplicated corpus.
fn is_one(n: &usize) -> bool {
    *n == 1
}

#[derive(Serialize)]
struct IndexSummary<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    index_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    corpus: Option<&'a str>,
    /// Git HEAD SHA of the benchmarked checkout (provenance); omitted when
    /// unknown (fixture/synthetic fallbacks).
    #[serde(skip_serializing_if = "Option::is_none")]
    corpus_revision: Option<&'a str>,
    /// True when the benchmarked docs came from the tiny built-in fixture or
    /// the synthetic fallback instead of a real checkout.
    #[serde(skip_serializing_if = "is_false")]
    fixture: bool,
    /// Replica count of a `<base>-N` corpus; omitted when 1.
    #[serde(skip_serializing_if = "is_one")]
    num_replicas: usize,
    /// Comma-joined member ids of a merged corpus; omitted for a single member.
    #[serde(skip_serializing_if = "Option::is_none")]
    corpus_members: Option<&'a str>,
    mode: QueryMode,
    /// Server spawn to ready; omitted for scan rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    ready_us: Option<u64>,
    index_time_us: u64,
    index_size_bytes: u64,
    num_docs: usize,
    /// Files the index actually covers (from the report, not the harness
    /// input length) — the skew signal next to `num_docs`.
    num_files_indexed: usize,
    num_queries: usize,
    timestamp_ms: u128,
    // Report-level RSS (flat optional fields); omitted when unmeasured.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    memory: Option<MemoryStats>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    load: Option<LoadSpan>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    engine_binary: Option<&'a EngineBinary>,
}

/// The engine binary a row ran, as `bin_path` / `bin_mtime_ms` /
/// `bin_size_bytes` on the index line.
#[derive(Debug, Clone, Serialize)]
pub struct EngineBinary {
    #[serde(rename = "bin_path")]
    pub path: String,
    #[serde(rename = "bin_mtime_ms")]
    pub mtime_ms: u128,
    #[serde(rename = "bin_size_bytes")]
    pub size_bytes: u64,
}

#[derive(Serialize)]
struct QueryTrace<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    index_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    corpus: Option<&'a str>,
    /// Git HEAD SHA of the benchmarked checkout (denormalized like
    /// run_id/corpus); omitted when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    corpus_revision: Option<&'a str>,
    /// True on fixture/synthetic fallback lines; omitted when false.
    #[serde(skip_serializing_if = "is_false")]
    fixture: bool,
    /// Replica count of a `<base>-N` corpus; omitted when 1.
    #[serde(skip_serializing_if = "is_one")]
    num_replicas: usize,
    /// Comma-joined member ids of a merged corpus; omitted for a single member.
    #[serde(skip_serializing_if = "Option::is_none")]
    corpus_members: Option<&'a str>,
    mode: QueryMode,
    term: &'a str,
    latency_us: u64,
    total_matches: u64,
    docs_matched: usize,
    /// Indexed file count, denormalized onto every line like
    /// run_id/corpus/memory so per-term rows stay self-describing.
    num_files_indexed: usize,
    timestamp_ms: u128,
    // Report-level RSS stamped on every line (same denormalized style as
    // run_id/corpus); omitted when unmeasured.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    memory: Option<MemoryStats>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    load: Option<LoadSpan>,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Options for [`write_jsonl_trace_with_options`].
#[derive(Debug, Clone)]
pub struct TraceOptions {
    /// Run identifier stamped on every emitted line (e.g. `bench-<millis>-<pid>`).
    /// `None` omits the field.
    pub run_id: Option<String>,
    /// Corpus id stamped on every emitted line (e.g. `"linux"`).
    /// `None` omits the field.
    pub corpus: Option<String>,
    /// Git HEAD SHA of the benchmarked checkout, stamped on every line.
    /// `None` omits the field.
    pub corpus_revision: Option<String>,
    /// Set when the benchmarked docs came from the tiny built-in fixture or the
    /// synthetic fallback, so fixture numbers can never be mistaken for corpus
    /// numbers.
    pub fixture: bool,
    /// Number of docs benchmarked; recorded on the index-summary line.
    pub num_docs: usize,
    /// When true, append to `path` instead of truncating it, so traces for
    /// several impls can share one file.
    pub append: bool,
    /// Replicas of the base corpus in this run (1 = unreplicated).
    pub num_replicas: usize,
    /// Member ids of a merged corpus, comma-joined; `None` for a single member.
    pub corpus_members: Option<String>,
    /// The row's engine binary; `None` omits it (rg, in-process rows).
    pub engine_binary: Option<EngineBinary>,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            run_id: None,
            corpus: None,
            corpus_revision: None,
            fixture: false,
            num_docs: 0,
            append: false,
            num_replicas: 1,
            corpus_members: None,
            engine_binary: None,
        }
    }
}

pub fn write_jsonl_trace_with_options(
    report: &BenchmarkReport,
    path: impl AsRef<Path>,
    opts: &TraceOptions,
) -> std::io::Result<()> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(opts.append)
        .truncate(!opts.append)
        .open(path.as_ref())?;
    let mut w = BufWriter::new(f);
    let ts = now_ms();
    let run_id = opts.run_id.as_deref();
    let corpus = opts.corpus.as_deref();
    let corpus_revision = opts.corpus_revision.as_deref();
    let corpus_members = opts.corpus_members.as_deref();
    let num_replicas = opts.num_replicas;
    for q in &report.queries {
        let line = QueryTrace {
            kind: "query",
            index_name: &report.index_name,
            run_id,
            corpus,
            corpus_revision,
            fixture: opts.fixture,
            num_replicas,
            corpus_members,
            mode: report.mode,
            term: &q.term,
            latency_us: q.latency_us,
            total_matches: q.total_matches,
            docs_matched: q.docs_matched,
            num_files_indexed: report.num_files_indexed,
            timestamp_ms: ts,
            memory: report.memory,
            load: report.load,
        };
        let s = serde_json::to_string(&line).map_err(std::io::Error::other)?;
        writeln!(w, "{s}")?;
    }
    let summary = IndexSummary {
        kind: "index",
        index_name: &report.index_name,
        run_id,
        corpus,
        corpus_revision,
        fixture: opts.fixture,
        num_replicas,
        corpus_members,
        mode: report.mode,
        ready_us: report.ready_time.map(|d| d.as_micros() as u64),
        index_time_us: report.index_time.as_micros() as u64,
        index_size_bytes: report.index_size_bytes,
        num_docs: opts.num_docs,
        num_files_indexed: report.num_files_indexed,
        num_queries: report.queries.len(),
        timestamp_ms: ts,
        memory: report.memory,
        load: report.load,
        engine_binary: opts.engine_binary.as_ref(),
    };
    let s = serde_json::to_string(&summary).map_err(std::io::Error::other)?;
    writeln!(w, "{s}")?;
    w.flush()?;
    Ok(())
}

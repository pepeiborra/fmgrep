//! Benchmark reports and the in-process runners that produce them.

use std::time::{Duration, Instant};

use serde::Serialize;

use crate::RegexError;
use crate::index::UnifiedRegexSearch;
use crate::regex_pattern;

/// Per-query measurements.
#[derive(Debug, Clone, Serialize)]
pub struct QueryMetrics {
    pub term: String,
    pub latency_us: u64,
    pub total_matches: u64,
    pub docs_matched: usize,
    /// False when the two counts above are document-wide rather than
    /// ripgrep's line-oriented, non-overlapping ones.
    pub counts_exact: bool,
    #[serde(skip)]
    pub latency: Duration,
}

/// Per-phase memory for one benchmark run, in bytes, measured harness-side
/// on the row's children. RSS counts every resident page, file-backed
/// included; footprint is anonymous memory (macOS `phys_footprint`, which
/// counts compressed pages and may exceed RSS; Linux resident minus shared).
/// `None` means unmeasured (`n/a`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MemoryStats {
    /// Index-phase peak RSS (sampler max, possibly raised to the `getrusage`
    /// whole-child peak).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_peak_rss_bytes: Option<u64>,
    /// Index-phase mean RSS over attributed sampler readings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_avg_rss_bytes: Option<u64>,
    /// Query-phase peak RSS (same anchoring rule as the index peak).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_peak_rss_bytes: Option<u64>,
    /// Query-phase mean RSS (`None` when no sample landed in the phase).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_avg_rss_bytes: Option<u64>,
    /// Whole-child `getrusage` peak anchoring the phase peaks (`None` when
    /// the child set no new watermark or `getrusage` was unavailable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_peak_rss_bytes: Option<u64>,
    /// Footprint sampler maxima and means, per phase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_peak_footprint_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_avg_footprint_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_peak_footprint_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_avg_footprint_bytes: Option<u64>,
}

/// 1/5/15-minute load averages at a row's start and end.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LoadSpan {
    pub loadavg_start: [f64; 3],
    pub loadavg_end: [f64; 3],
}

/// How a row's per-term latencies were measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryMode {
    /// One server per row, one `--server` process per term, timed spawn to exit.
    Serve,
    /// One search or scan per term, no long-lived server.
    Scan,
}

/// Whole-benchmark report.
#[derive(Debug, Clone)]
pub struct BenchmarkReport {
    pub index_name: String,
    pub index_time: Duration,
    pub index_size_bytes: u64,
    /// Files the index actually covers, read off the built index, never a
    /// copy of the harness input length; `--verify` requires rows to agree
    /// within 0.1%.
    pub num_files_indexed: usize,
    pub queries: Vec<QueryMetrics>,
    /// Harness-measured RSS; `None` for in-process runs.
    pub memory: Option<MemoryStats>,
    /// Set harness-side around the whole row; `None` when unsampled.
    pub load: Option<LoadSpan>,
    pub mode: QueryMode,
    /// Server spawn to ready; `None` unless `mode` is [`QueryMode::Serve`].
    pub ready_time: Option<Duration>,
}

impl QueryMetrics {
    fn new(
        term: String,
        latency: Duration,
        total_matches: u64,
        docs_matched: usize,
        counts_exact: bool,
    ) -> Self {
        Self {
            term,
            latency_us: latency.as_micros() as u64,
            total_matches,
            docs_matched,
            counts_exact,
            latency,
        }
    }
}

/// Build `I` once via `build` (timing it), then time each term: a
/// `/.../`-spelled term goes to [`UnifiedRegexSearch::search_regex`], any
/// other to [`crate::UnifiedIndex::search`]. `total_matches` is the sum of
/// per-doc counts, `docs_matched` the number of hit pairs.
///
/// Regex rows carry `counts_exact: false`: the regex path searches whole
/// documents as one record, so a match may cross a line.
///
/// The first empty or invalid pattern aborts with [`RegexError`]: no partial
/// report, never silent zeros. Implementations need not refuse empty
/// patterns themselves.
pub fn run_unified_benchmark<I: UnifiedRegexSearch>(
    build: impl FnOnce() -> I,
    terms: &[&str],
) -> Result<BenchmarkReport, RegexError> {
    let start = Instant::now();
    let index = build();
    let index_time = start.elapsed();
    let index_name = index.name().to_owned();
    let index_size_bytes = index.index_size_bytes();
    let num_files_indexed = index.num_files_indexed();

    let mut queries = Vec::with_capacity(terms.len());
    for term in terms {
        let pattern = regex_pattern(term);
        if pattern == Some("") {
            return Err(RegexError::new(
                term,
                "empty regex pattern: rejected by the native-regex contract (not match-everywhere)",
            ));
        }
        let q_start = Instant::now();
        let hits = match pattern {
            Some(p) => index.search_regex(p)?,
            None => index.search(term.as_bytes()),
        };
        let mut total_matches = 0u64;
        let mut docs_matched = 0usize;
        for (_doc_idx, c) in &hits {
            if *c > 0 {
                docs_matched += 1;
                total_matches += *c;
            }
        }
        let latency = q_start.elapsed();
        queries.push(QueryMetrics::new(
            term.to_string(),
            latency,
            total_matches,
            docs_matched,
            pattern.is_none(),
        ));
    }

    Ok(BenchmarkReport {
        index_name,
        index_time,
        index_size_bytes,
        num_files_indexed,
        queries,
        memory: None,
        load: None,
        mode: QueryMode::Scan,
        ready_time: None,
    })
}

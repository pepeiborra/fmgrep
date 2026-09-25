use bench::BenchmarkReport;

/// Pairwise agreement check: every impl must report identical
/// `total_matches`/`docs_matched` for each term, plus indexed-file-count
/// agreement within 0.1% (see [`verify_file_counts`]). Returns
/// human-readable mismatch rows; empty means agreement (also empty for
/// < 2 impls). Every row names `corpus_id` so multi-corpus failures identify
/// what diverged, and names any row whose counts are not rg-exact (see
/// [`inexact_note`]). Equality is required whatever the flag says.
pub fn verify_reports(corpus_id: &str, reports: &[BenchmarkReport]) -> Vec<String> {
    let mut diffs = Vec::new();
    if reports.len() < 2 {
        return diffs;
    }
    // File-set skew makes the per-term totals incomparable, so it is
    // reported alongside (never instead of) the term diffs.
    diffs.extend(verify_file_counts(corpus_id, reports));
    let base = &reports[0];
    for other in &reports[1..] {
        if other.queries.len() != base.queries.len() {
            diffs.push(format!(
                "[{corpus_id}] query count differs: {} ran {} terms, {} ran {}",
                base.index_name,
                base.queries.len(),
                other.index_name,
                other.queries.len()
            ));
            continue;
        }
        for (bq, oq) in base.queries.iter().zip(other.queries.iter()) {
            if bq.term != oq.term {
                diffs.push(format!(
                    "[{corpus_id}] term order differs at position: {} has {:?}, {} has {:?}",
                    base.index_name, bq.term, other.index_name, oq.term
                ));
                continue;
            }
            let note = inexact_note(
                (&base.index_name, bq.counts_exact),
                (&other.index_name, oq.counts_exact),
            );
            if bq.total_matches != oq.total_matches {
                diffs.push(format!(
                    "[{corpus_id}] term {:?}: total_matches {}={} vs {}={}{note}",
                    bq.term, base.index_name, bq.total_matches, other.index_name, oq.total_matches
                ));
            }
            if bq.docs_matched != oq.docs_matched {
                diffs.push(format!(
                    "[{corpus_id}] term {:?}: docs_matched {}={} vs {}={}{note}",
                    bq.term, base.index_name, bq.docs_matched, other.index_name, oq.docs_matched
                ));
            }
        }
    }
    diffs
}

/// Suffix naming the rows of a mismatch whose counts are not ripgrep's, so a
/// failure says whether it is a bug or a semantics gap. Empty when both rows
/// are rg-exact.
fn inexact_note(base: (&str, bool), other: (&str, bool)) -> String {
    let names: Vec<&str> = [base, other]
        .into_iter()
        .filter(|&(_, exact)| !exact)
        .map(|(name, _)| name)
        .collect();
    if names.is_empty() {
        return String::new();
    }
    format!(" (counts not rg-exact: {})", names.join(", "))
}

/// Indexed-file-count agreement over the rows' own
/// [`BenchmarkReport::num_files_indexed`]: with `lo`/`hi` the min/max, the
/// run fails iff `(hi - lo) * 1000 > hi` (a spread over 0.1%, integer-exact).
/// All-zero rows agree; fewer than 2 rows is vacuous. A failure yields a
/// summary row naming the extremes plus the `row=count` listing.
fn verify_file_counts(corpus_id: &str, reports: &[BenchmarkReport]) -> Vec<String> {
    if reports.len() < 2 {
        return Vec::new();
    }
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    let mut lo_name = "";
    let mut hi_name = "";
    for r in reports {
        if r.num_files_indexed < lo {
            lo = r.num_files_indexed;
            lo_name = &r.index_name;
        }
        if r.num_files_indexed > hi {
            hi = r.num_files_indexed;
            hi_name = &r.index_name;
        }
    }
    // All-zero rows agree trivially (and this guards the division below).
    if hi == 0 {
        return Vec::new();
    }
    // Relative spread must stay within 0.1%: (hi - lo) / hi <= 1/1000.
    if (hi - lo) * 1000 <= hi {
        return Vec::new();
    }
    let spread_pct = (hi - lo) as f64 * 100.0 / hi as f64;
    let mut listing = String::new();
    for (i, r) in reports.iter().enumerate() {
        if i > 0 {
            listing.push_str(", ");
        }
        listing.push_str(&format!("{}={}", r.index_name, r.num_files_indexed));
    }
    vec![
        format!(
            "[{corpus_id}] file-count agreement: relative spread {spread_pct:.3}% exceeds 0.100% tolerance (min {lo} by {lo_name}, max {hi} by {hi_name}) — rows indexed different file sets"
        ),
        format!("[{corpus_id}] file counts: {listing}"),
    ]
}

/// Replica sanity for a `<base>-N` corpus: every count must be a multiple
/// of N, since each document is present N times. Not a substitute for the
/// cross-row agreement above — it catches a replica an engine deduplicated
/// or dropped.
pub fn verify_replica_multiples(
    corpus_id: &str,
    replicas: usize,
    reports: &[BenchmarkReport],
) -> Vec<String> {
    let mut diffs = Vec::new();
    if replicas < 2 {
        return diffs;
    }
    let n = replicas as u64;
    for report in reports {
        for q in &report.queries {
            if !q.total_matches.is_multiple_of(n) {
                diffs.push(format!(
                    "[{corpus_id}] term {:?}: total_matches {} is not a multiple of {replicas} ({})",
                    q.term, q.total_matches, report.index_name
                ));
            }
            if !(q.docs_matched as u64).is_multiple_of(n) {
                diffs.push(format!(
                    "[{corpus_id}] term {:?}: docs_matched {} is not a multiple of {replicas} ({})",
                    q.term, q.docs_matched, report.index_name
                ));
            }
        }
    }
    diffs
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use bench::{MemoryStats, QueryMetrics};

    /// Minimal report with `n` indexed files and no queries (term
    /// agreement is orthogonal — these tests pin the file-count check).
    fn file_report(name: &str, n: usize) -> BenchmarkReport {
        BenchmarkReport {
            index_name: name.to_string(),
            index_time: Duration::ZERO,
            index_size_bytes: 0,
            num_files_indexed: n,
            queries: Vec::new(),
            memory: None,
            load: None,
            mode: bench::QueryMode::Scan,
            ready_time: None,
        }
    }

    fn query_report(name: &str, rows: &[(&str, u64, usize)]) -> BenchmarkReport {
        BenchmarkReport {
            index_name: name.to_string(),
            index_time: Duration::ZERO,
            index_size_bytes: 0,
            num_files_indexed: 0,
            queries: rows
                .iter()
                .map(|(term, total, docs)| QueryMetrics {
                    term: (*term).to_string(),
                    latency_us: 0,
                    total_matches: *total,
                    docs_matched: *docs,
                    counts_exact: true,
                    latency: Duration::ZERO,
                })
                .collect(),
            memory: None,
            load: None,
            mode: bench::QueryMode::Scan,
            ready_time: None,
        }
    }

    #[test]
    fn replica_counts_must_be_multiples_of_n() {
        let reports = vec![query_report("rg", &[("printk", 12, 8)])];
        assert!(verify_replica_multiples("ghc-2", 2, &reports).is_empty());
        assert!(verify_replica_multiples("ghc-4", 4, &reports).is_empty());
        // An unreplicated run is not subject to the check.
        let odd = vec![query_report("rg", &[("printk", 7, 3)])];
        assert!(verify_replica_multiples("ghc", 1, &odd).is_empty());
    }

    #[test]
    fn replica_multiple_violation_names_row_and_term() {
        let reports = vec![query_report("fmgrep", &[("printk", 7, 3)])];
        let diffs = verify_replica_multiples("ghc-2", 2, &reports);
        assert_eq!(diffs.len(), 2, "both counts are odd: {diffs:?}");
        for d in &diffs {
            assert!(d.contains("[ghc-2]"), "{d}");
            assert!(d.contains("printk"), "{d}");
            assert!(d.contains("fmgrep"), "{d}");
            assert!(d.contains("multiple of 2"), "{d}");
        }
    }

    #[test]
    fn file_counts_agree_when_identical() {
        let reports = vec![
            file_report("rg", 200),
            file_report("fmgrep", 200),
            file_report("fmgrep-store-docs", 200),
        ];
        assert!(verify_file_counts("linux", &reports).is_empty());
        // Also quiet through the combined --verify entry point.
        assert!(verify_reports("linux", &reports).is_empty());
    }

    #[test]
    fn file_counts_all_zero_agree() {
        // Empty corpus: agreement, not a division by zero.
        let reports = vec![file_report("rg", 0), file_report("fmgrep-store-docs", 0)];
        assert!(verify_file_counts("linux", &reports).is_empty());
    }

    #[test]
    fn file_counts_single_impl_is_vacuous() {
        assert!(verify_file_counts("linux", &[file_report("rg", 200)]).is_empty());
        assert!(verify_file_counts("linux", &[]).is_empty());
    }

    #[test]
    fn file_count_off_by_one_trips_on_small_corpus() {
        // One silently dropped file out of 200 is a 0.5% spread — well over
        // the 0.1% tolerance, so the seeded-skew negative proof trips here.
        let reports = vec![
            file_report("rg", 200),
            file_report("fmgrep", 200),
            file_report("fmgrep-store-docs", 199),
        ];
        let diffs = verify_file_counts("linux", &reports);
        assert_eq!(diffs.len(), 2);
        // Summary names the corpus, the extreme impls, and the counts.
        assert!(diffs[0].contains("[linux]"), "corpus: {}", diffs[0]);
        assert!(
            diffs[0].contains("fmgrep-store-docs"),
            "outlier impl: {}",
            diffs[0]
        );
        assert!(diffs[0].contains("199"), "outlier count: {}", diffs[0]);
        assert!(diffs[0].contains("200"), "max count: {}", diffs[0]);
        // The listing carries every row's own count.
        assert!(
            diffs[1].contains("fmgrep-store-docs=199"),
            "listing: {}",
            diffs[1]
        );
        assert!(diffs[1].contains("rg=200"), "listing: {}", diffs[1]);
        // And the combined --verify entry point surfaces it too.
        let combined = verify_reports("linux", &reports);
        assert!(
            combined.iter().any(|d| d.contains("file-count")),
            "combined diffs must include the file-count rows: {combined:?}"
        );
    }

    #[test]
    fn file_count_tolerance_boundary_is_inclusive() {
        // Exactly 0.1% spread passes: (2000-1998)*1000 == 2000, not > 2000.
        let ok = vec![file_report("a", 2000), file_report("b", 1998)];
        assert!(verify_file_counts("linux", &ok).is_empty());
        // One file further (0.15%) trips.
        let bad = vec![file_report("a", 2000), file_report("b", 1997)];
        assert_eq!(verify_file_counts("linux", &bad).len(), 2);
    }

    /// One regex-term report row: the term is `/.../`-spelled with
    /// metacharacters inside, memory is harness-measured.
    fn regex_report(name: &str, total: u64, docs_matched: usize, files: usize) -> BenchmarkReport {
        BenchmarkReport {
            index_name: name.to_string(),
            index_time: Duration::ZERO,
            index_size_bytes: 0,
            num_files_indexed: files,
            queries: vec![QueryMetrics {
                term: r"/printk\([^)]*\)/".to_string(),
                latency_us: 7,
                total_matches: total,
                docs_matched,
                counts_exact: true,
                latency: Duration::from_micros(7),
            }],
            memory: Some(MemoryStats {
                index_peak_rss_bytes: Some(1 << 20),
                index_avg_rss_bytes: Some(1 << 19),
                query_peak_rss_bytes: Some(1 << 20),
                query_avg_rss_bytes: Some(1 << 19),
                child_peak_rss_bytes: Some(1 << 20),
                ..MemoryStats::default()
            }),
            load: None,
            mode: bench::QueryMode::Scan,
            ready_time: None,
        }
    }

    #[test]
    fn verify_compares_regex_term_rows_like_any_other() {
        // Every row of a run answers the same mixed term list, so a regex
        // term is verified exactly like a literal one: metacharacter terms
        // verify cleanly across rows, file-count agreement applies, and a
        // diverging count fails loudly naming the term.
        let base = regex_report("fmgrep", 3, 1, 200);
        let same = regex_report("fmgrep-store-docs", 3, 1, 200);
        assert!(verify_reports("linux", &[base.clone(), same]).is_empty());
        // A regex count divergence fails with the term named.
        let mut skewed = regex_report("rg", 4, 1, 200);
        let diffs = verify_reports("linux", &[base.clone(), skewed.clone()]);
        assert_eq!(diffs.len(), 1, "diffs: {diffs:?}");
        // (Terms render Debug-escaped in diff rows, so pin the literal core
        // rather than the raw metacharacters.)
        assert!(diffs[0].contains("printk"), "term: {}", diffs[0]);
        assert!(diffs[0].contains("total_matches"), "field: {}", diffs[0]);
        // Rows that agree on the numbers stay quiet whatever the flag says.
        let mut inexact = regex_report("naive", 3, 1, 200);
        inexact.queries[0].counts_exact = false;
        assert!(verify_reports("linux", &[base.clone(), inexact.clone()]).is_empty());
        // A mismatch involving one names it, so a failure separates a bug
        // from a semantics gap.
        inexact.queries[0].total_matches = 4;
        let diffs = verify_reports("linux", &[base.clone(), inexact]);
        assert_eq!(diffs.len(), 1, "diffs: {diffs:?}");
        assert!(
            diffs[0].ends_with("(counts not rg-exact: naive)"),
            "annotation: {}",
            diffs[0]
        );
        // File-set skew trips per regex row too (one dropped file of 200).
        skewed.num_files_indexed = 199;
        let diffs = verify_reports("linux", &[base, skewed]);
        assert!(
            diffs.iter().any(|d| d.contains("file-count")),
            "file-count rows must fire on regex rows: {diffs:?}"
        );
    }
}

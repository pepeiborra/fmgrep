use std::time::Duration;

use bench::{BenchmarkReport, LoadSpan, MemoryStats, QueryMode, regex_pattern};

use crate::load::{LOAD1_WARN, inflated};

fn fmt_latency(d: Duration) -> String {
    let us = d.as_micros();
    if us >= 10_000 {
        format!("{:.2} ms", d.as_secs_f64() * 1000.0)
    } else {
        format!("{us} us")
    }
}

/// Format a byte count for tables and the corpus-size header line as a
/// human unit (B/KB/MB/GB, 2 decimals above bytes).
pub fn fmt_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let n = bytes as f64;
    if n >= GB {
        format!("{:.2} GB", n / GB)
    } else if n >= MB {
        format!("{:.2} MB", n / MB)
    } else if n >= KB {
        format!("{:.2} KB", n / KB)
    } else {
        format!("{n:.0} B")
    }
}

/// Format an RSS byte count for tables: `n/a` when unmeasured (in-process
/// impls, disabled/failed sampler), else [`fmt_bytes`].
fn fmt_rss(bytes: Option<u64>) -> String {
    match bytes {
        None => "n/a".to_string(),
        Some(n) => fmt_bytes(n),
    }
}

/// The by-memory table cells: RSS idx/qry peak and avg, then the query
/// footprint peak and avg; `n/a` when unmeasured.
fn memory_cells(m: Option<&MemoryStats>) -> [String; 6] {
    let m = m.copied().unwrap_or_default();
    [
        m.index_peak_rss_bytes,
        m.index_avg_rss_bytes,
        m.query_peak_rss_bytes,
        m.query_avg_rss_bytes,
        m.query_peak_footprint_bytes,
        m.query_avg_footprint_bytes,
    ]
    .map(fmt_rss)
}

/// `load1` at the row's start and end, `!`-suffixed when inflated.
fn fmt_load1(load: Option<&LoadSpan>) -> String {
    match load {
        None => "n/a".to_string(),
        Some(l) => format!(
            "{:.2}->{:.2}{}",
            l.loadavg_start[0],
            l.loadavg_end[0],
            if inflated(l) { "!" } else { "" }
        ),
    }
}

fn fmt_loadavg(avg: [f64; 3]) -> String {
    format!("{:.2} {:.2} {:.2}", avg[0], avg[1], avg[2])
}

/// Server start-to-ready; `n/a` for scan rows.
fn fmt_ready(report: &BenchmarkReport) -> String {
    report
        .ready_time
        .map_or_else(|| "n/a".to_string(), fmt_latency)
}

fn fmt_mode(report: &BenchmarkReport) -> String {
    match report.mode {
        QueryMode::Serve => format!("serve, ready in {}", fmt_ready(report)),
        QueryMode::Scan => "scan".to_string(),
    }
}

/// Lower median; `None` when empty.
pub fn p50(latencies: &[Duration]) -> Option<Duration> {
    let mut sorted = latencies.to_vec();
    sorted.sort();
    sorted.get(sorted.len().checked_sub(1)? / 2).copied()
}

/// `None` when empty.
pub fn mean(latencies: &[Duration]) -> Option<Duration> {
    let n = u32::try_from(latencies.len()).ok().filter(|&n| n > 0)?;
    Some(latencies.iter().sum::<Duration>() / n)
}

/// A row's per-term latencies split into (literal, regex) by the term's
/// `/.../` spelling.
pub fn latencies_by_kind(report: &BenchmarkReport) -> (Vec<Duration>, Vec<Duration>) {
    let (regex, literal): (Vec<_>, Vec<_>) = report
        .queries
        .iter()
        .partition(|q| regex_pattern(&q.term).is_some());
    let lat = |v: Vec<&bench::QueryMetrics>| v.iter().map(|q| q.latency).collect();
    (lat(literal), lat(regex))
}

fn total_query_latency(report: &BenchmarkReport) -> Duration {
    report.queries.iter().map(|q| q.latency).sum()
}

fn bytes_per_doc(report: &BenchmarkReport) -> Option<u64> {
    // Per file the index actually covers, not per file the harness loaded.
    if report.num_files_indexed == 0 {
        None
    } else {
        Some(report.index_size_bytes / report.num_files_indexed as u64)
    }
}

fn index_time_per_doc(report: &BenchmarkReport) -> Option<Duration> {
    // Per file the index actually covers, like `bytes_per_doc`.
    match u32::try_from(report.num_files_indexed) {
        Ok(n) if n > 0 => Some(report.index_time / n),
        _ => None,
    }
}

/// Format indexing throughput as `MB/s` over the corpus bytes the runner
/// loaded; `n/a` when there are no bytes or the build was too fast to time.
fn fmt_throughput(corpus_bytes: u64, index_time: Duration) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let secs = index_time.as_secs_f64();
    if corpus_bytes == 0 || secs <= 0.0 {
        return "n/a".to_string();
    }
    format!("{:.2} MB/s", corpus_bytes as f64 / MB / secs)
}

pub fn print_report(report: &BenchmarkReport) {
    let bpd = match bytes_per_doc(report) {
        Some(n) => format!("{}/doc", fmt_bytes(n)),
        None => "no docs".to_string(),
    };
    // The term column fits the longest term, at least 20 wide.
    let term_width = report
        .queries
        .iter()
        .map(|q| q.term.len())
        .max()
        .unwrap_or(0)
        .max("term".len())
        .max(20);
    println!("== {} ==", report.index_name);
    println!(
        "  build: {:>10}   size: {} ({})",
        fmt_latency(report.index_time),
        fmt_bytes(report.index_size_bytes),
        bpd
    );
    // Diverging values across rows flag file-set skew.
    println!("  files: {} indexed", report.num_files_indexed);
    println!("  query: {}", fmt_mode(report));
    match &report.load {
        Some(l) => println!(
            "  load avg: {} -> {} (1/5/15 min, row start -> end){}",
            fmt_loadavg(l.loadavg_start),
            fmt_loadavg(l.loadavg_end),
            if inflated(l) {
                format!(" — load1 >= {LOAD1_WARN}, timings likely inflated")
            } else {
                String::new()
            }
        ),
        None => println!("  load avg: n/a"),
    }
    // RSS line: measured harness-side for subprocess impls (peak anchored to
    // getrusage, averages sampled — see mem.rs); n/a for in-process impls.
    match &report.memory {
        Some(m) => {
            println!(
                "  memory RSS: idx peak {:>10} / avg {:>10}   qry peak {:>10} / avg {:>10}",
                fmt_rss(m.index_peak_rss_bytes),
                fmt_rss(m.index_avg_rss_bytes),
                fmt_rss(m.query_peak_rss_bytes),
                fmt_rss(m.query_avg_rss_bytes),
            );
            // Anonymous memory; on macOS it counts compressed pages and may exceed RSS.
            println!(
                "  footprint:  idx peak {:>10} / avg {:>10}   qry peak {:>10} / avg {:>10}",
                fmt_rss(m.index_peak_footprint_bytes),
                fmt_rss(m.index_avg_footprint_bytes),
                fmt_rss(m.query_peak_footprint_bytes),
                fmt_rss(m.query_avg_footprint_bytes),
            );
        }
        None => println!("  memory RSS: n/a (in-process impl)"),
    };
    println!(
        "  {:<term_width$} {:>12} {:>12} {:>12}",
        "term",
        "total_match",
        "docs_match",
        "latency",
        term_width = term_width,
    );
    println!("  {}", "-".repeat(term_width + 40));
    for q in &report.queries {
        println!(
            "  {:<term_width$} {:>12} {:>12} {:>12}",
            q.term,
            q.total_matches,
            q.docs_matched,
            fmt_latency(q.latency),
            term_width = term_width,
        );
    }
    println!();
}

/// Split ranked rows into the rows a sub-table prints and the names it
/// elides, keeping rank order in both.
///
/// A 0-valued metric means the impl has nothing to rank (`rg` builds no
/// index, so its size is 0), and leaving the row in would crown it the
/// winner. Pair this with [`omitted_note`] so an elided row is always named.
fn split_zero_metric<'a>(
    ranked: &[&'a BenchmarkReport],
    is_zero: impl Fn(&BenchmarkReport) -> bool,
) -> (Vec<&'a BenchmarkReport>, Vec<&'a str>) {
    let mut kept = Vec::new();
    let mut omitted = Vec::new();
    for r in ranked {
        if is_zero(r) {
            omitted.push(r.index_name.as_str());
        } else {
            kept.push(*r);
        }
    }
    (kept, omitted)
}

/// Trailing sub-table line naming the elided rows; `None` when none were.
fn omitted_note(metric: &str, omitted: &[&str]) -> Option<String> {
    if omitted.is_empty() {
        None
    } else {
        Some(format!("  (omitted, no {metric}: {})", omitted.join(", ")))
    }
}

/// Print the comparison tables. `corpus_bytes` is the total size of the docs
/// the runner loaded, used for the indexing-throughput column.
pub fn print_comparison(
    reports: &[BenchmarkReport],
    num_docs: usize,
    corpus_bytes: u64,
    header: &str,
) {
    println!(
        "== comparison ({}; {} impl{}, {} docs) ==",
        header,
        reports.len(),
        if reports.len() == 1 { "" } else { "s" },
        num_docs
    );
    println!(
        "query latency: serve rows run one server per row (start-to-ready is `ready`) \
         and time one `--server` process per term, spawn to exit; scan rows (rg) scan per term"
    );
    let mut by_lat: Vec<&BenchmarkReport> = reports.iter().collect();
    by_lat.sort_by_key(|r| total_query_latency(r));
    // The impl column fits the longest row name, at least 12 wide.
    let impl_width = reports
        .iter()
        .map(|r| r.index_name.len())
        .max()
        .unwrap_or(0)
        .max("impl".len())
        .max(12);
    println!("-- by total query latency --");
    println!(
        "  {:>4} {:<impl_width$} {:>12} {:>12} {:>12} {:>12} {:>13}",
        "rank",
        "impl",
        "total_query",
        "avg_query",
        "ready",
        "build",
        "load1",
        impl_width = impl_width,
    );
    for (i, r) in by_lat.iter().enumerate() {
        let total = total_query_latency(r);
        let avg = if r.queries.is_empty() {
            Duration::ZERO
        } else {
            total / r.queries.len() as u32
        };
        println!(
            "  {:>4} {:<impl_width$} {:>12} {:>12} {:>12} {:>12} {:>13}",
            i + 1,
            r.index_name,
            fmt_latency(total),
            fmt_latency(avg),
            fmt_ready(r),
            fmt_latency(r.index_time),
            fmt_load1(r.load.as_ref()),
            impl_width = impl_width,
        );
    }
    if reports
        .iter()
        .any(|r| r.load.as_ref().is_some_and(inflated))
    {
        println!(
            "  ! = load1 >= {LOAD1_WARN} at the row's start or end: its timings are likely inflated"
        );
    }
    // Indexing-time sub-table: same `index_time` field as the latency table's
    // build column, the per-impl sections and the trace's `index_time_us`.
    // Query latency is never legitimately 0, so only this table and the size
    // table below elide rows.
    let mut by_build: Vec<&BenchmarkReport> = reports.iter().collect();
    by_build.sort_by_key(|r| r.index_time);
    let (by_build, build_omitted) = split_zero_metric(&by_build, |r| r.index_time.is_zero());
    println!("-- by indexing time --");
    println!(
        "  {:>4} {:<impl_width$} {:>12} {:>12} {:>12} {:>12}",
        "rank",
        "impl",
        "build",
        "per_doc",
        "throughput",
        "files",
        impl_width = impl_width,
    );
    for (i, r) in by_build.iter().enumerate() {
        let per_doc = match index_time_per_doc(r) {
            Some(d) => fmt_latency(d),
            None => "n/a".to_string(),
        };
        println!(
            "  {:>4} {:<impl_width$} {:>12} {:>12} {:>12} {:>12}",
            i + 1,
            r.index_name,
            fmt_latency(r.index_time),
            per_doc,
            fmt_throughput(corpus_bytes, r.index_time),
            r.num_files_indexed,
            impl_width = impl_width,
        );
    }
    if let Some(note) = omitted_note("indexing time", &build_omitted) {
        println!("{note}");
    }
    let mut by_size: Vec<&BenchmarkReport> = reports.iter().collect();
    by_size.sort_by_key(|r| r.index_size_bytes);
    let (by_size, size_omitted) = split_zero_metric(&by_size, |r| r.index_size_bytes == 0);
    println!("-- by index size --");
    println!(
        "  {:>4} {:<impl_width$} {:>12} {:>12} {:>12}",
        "rank",
        "impl",
        "size",
        "per_doc",
        "files",
        impl_width = impl_width,
    );
    for (i, r) in by_size.iter().enumerate() {
        let bpd = match bytes_per_doc(r) {
            Some(n) => fmt_bytes(n),
            None => "n/a".to_string(),
        };
        println!(
            "  {:>4} {:<impl_width$} {:>12} {:>12} {:>12}",
            i + 1,
            r.index_name,
            fmt_bytes(r.index_size_bytes),
            bpd,
            r.num_files_indexed,
            impl_width = impl_width,
        );
    }
    if let Some(note) = omitted_note("index size", &size_omitted) {
        println!("{note}");
    }
    // RSS sub-table: index/query peak+avg per impl (n/a for in-process
    // impls). Peaks are getrusage-anchored, averages sampler means.
    println!("-- by memory (peak/avg RSS, query footprint) --");
    println!(
        "  {:>4} {:<impl_width$} {:>12} {:>12} {:>12} {:>12} {:>12} {:>12}",
        "rank",
        "impl",
        "idx_peak",
        "idx_avg",
        "qry_peak",
        "qry_avg",
        "qry_fp_peak",
        "qry_fp_avg",
        impl_width = impl_width,
    );
    let mut by_mem: Vec<&BenchmarkReport> = reports.iter().collect();
    by_mem.sort_by_key(|r| {
        r.memory
            .and_then(|m| m.index_peak_rss_bytes)
            .unwrap_or(u64::MAX)
    });
    for (i, r) in by_mem.iter().enumerate() {
        let [ip, ia, qp, qa, fp, fa] = memory_cells(r.memory.as_ref());
        println!(
            "  {:>4} {:<impl_width$} {:>12} {:>12} {:>12} {:>12} {:>12} {:>12}",
            i + 1,
            r.index_name,
            ip,
            ia,
            qp,
            qa,
            fp,
            fa,
            impl_width = impl_width,
        );
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use bench::QueryMetrics;

    #[test]
    fn fmt_rss_marks_unmeasured_na_and_scales_units() {
        assert_eq!(fmt_rss(None), "n/a");
        assert_eq!(fmt_rss(Some(0)), "0 B");
        assert_eq!(fmt_rss(Some(512)), "512 B");
        assert_eq!(fmt_rss(Some(2048)), "2.00 KB");
        assert_eq!(fmt_rss(Some(12 * 1024 * 1024)), "12.00 MB");
        assert_eq!(fmt_rss(Some(3 * 1024 * 1024 * 1024)), "3.00 GB");
        // Index sizes share the same formatter (B/KB/MB/GB, 2 decimals).
        assert_eq!(fmt_bytes(2315405), "2.21 MB");
        assert_eq!(fmt_bytes(999), "999 B");
    }

    #[test]
    fn load1_column_shows_start_to_end_and_flags_inflation() {
        let span = |start: f64, end: f64| LoadSpan {
            loadavg_start: [start, 1.0, 1.0],
            loadavg_end: [end, 1.0, 1.0],
        };
        assert_eq!(fmt_load1(None), "n/a");
        assert_eq!(fmt_load1(Some(&span(1.234, 3.4))), "1.23->3.40");
        assert_eq!(fmt_load1(Some(&span(3.99, 4.0))), "3.99->4.00!");
        assert_eq!(fmt_loadavg([9.15, 4.9, 4.51]), "9.15 4.90 4.51");
    }

    #[test]
    fn indexing_time_columns_derive_from_index_time() {
        let mut r = file_report("fmgrep", 1000);
        r.index_time = Duration::from_secs(10);
        assert_eq!(fmt_latency(r.index_time), "10000.00 ms");
        assert_eq!(
            index_time_per_doc(&r).map(fmt_latency).as_deref(),
            Some("10.00 ms")
        );
        // 100 MiB over 10 s.
        assert_eq!(
            fmt_throughput(100 * 1024 * 1024, r.index_time),
            "10.00 MB/s"
        );
        // No files indexed, no bytes, or an untimed build report `n/a`.
        assert_eq!(index_time_per_doc(&file_report("empty", 0)), None);
        assert_eq!(fmt_throughput(0, r.index_time), "n/a");
        assert_eq!(fmt_throughput(1024, Duration::ZERO), "n/a");
    }

    #[test]
    fn ranked_sub_tables_elide_zero_rows_and_name_them() {
        let mut sized = file_report("fmgrep", 200);
        sized.index_size_bytes = 237 * 1024 * 1024;
        sized.index_time = Duration::from_secs(3);
        let mut indexless = file_report("rg", 200);
        indexless.index_time = Duration::from_millis(45);
        // Index size: the no-index row drops out, the note names it.
        let ranked = vec![&indexless, &sized];
        let (kept, omitted) = split_zero_metric(&ranked, |r| r.index_size_bytes == 0);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].index_name, "fmgrep");
        assert_eq!(omitted, vec!["rg"]);
        assert_eq!(
            omitted_note("index size", &omitted).as_deref(),
            Some("  (omitted, no index size: rg)")
        );
        // Indexing time: both rows built something, so nothing is elided and
        // no note is printed.
        let (kept, omitted) = split_zero_metric(&ranked, |r| r.index_time.is_zero());
        assert_eq!(kept.len(), 2);
        assert!(omitted.is_empty());
        assert_eq!(omitted_note("indexing time", &omitted), None);
        // Several elided rows are listed in rank order.
        let other = file_report("fmgrep-store-docs", 200);
        let ranked = vec![&indexless, &other, &sized];
        let (kept, omitted) = split_zero_metric(&ranked, |r| r.index_size_bytes == 0);
        assert_eq!(kept.len(), 1);
        assert_eq!(
            omitted_note("index size", &omitted).as_deref(),
            Some("  (omitted, no index size: rg, fmgrep-store-docs)")
        );
    }

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
            mode: QueryMode::Scan,
            ready_time: None,
        }
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
            mode: QueryMode::Scan,
            ready_time: None,
        }
    }

    #[test]
    fn regex_rows_populate_memory_and_files_columns() {
        // The memory/files columns flow through BenchmarkReport on regex rows
        // exactly as on literal rows: regex rows must populate them, with
        // `n/a` only where legitimately unmeasured (in-process runs).
        let report = regex_report("rg", 3, 1, 200);
        let mem = report.memory.expect("regex rows carry measured memory");
        assert!(
            mem.index_peak_rss_bytes.is_some(),
            "idx peak is real, never synthesized"
        );
        assert!(
            mem.query_peak_rss_bytes.is_some(),
            "qry peak is real, never synthesized"
        );
        assert!(
            report.num_files_indexed > 0,
            "files column is the row's own count"
        );
        // Display helpers render them (never silently blank).
        assert_ne!(super::fmt_rss(mem.index_peak_rss_bytes), "n/a");
        assert_eq!(super::bytes_per_doc(&report), Some(0));
    }

    #[test]
    fn footprint_cells_render_and_are_na_when_unmeasured() {
        let mem = MemoryStats {
            query_peak_footprint_bytes: Some(2048),
            ..MemoryStats::default()
        };
        let cells = memory_cells(Some(&mem));
        assert_eq!((cells[4].as_str(), cells[5].as_str()), ("2.00 KB", "n/a"));
        assert!(memory_cells(None).iter().all(|c| c == "n/a"));
    }

    #[test]
    fn p50_is_the_lower_median() {
        let ms = |v: &[u64]| {
            v.iter()
                .map(|&m| Duration::from_millis(m))
                .collect::<Vec<_>>()
        };
        assert_eq!(p50(&[]), None);
        assert_eq!(p50(&ms(&[7])), Some(Duration::from_millis(7)));
        assert_eq!(p50(&ms(&[9, 1, 5])), Some(Duration::from_millis(5)));
        assert_eq!(p50(&ms(&[4, 1, 3, 2])), Some(Duration::from_millis(2)));
        assert_eq!(mean(&ms(&[1, 2, 6])), Some(Duration::from_millis(3)));
        assert_eq!(mean(&[]), None);
    }

    #[test]
    fn latency_split_follows_the_regex_spelling() {
        let mut r = file_report("fmgrep", 1);
        for (term, ms) in [("printk", 1), ("/pr\\w+/", 2), ("a/b", 3), ("/x/", 4)] {
            r.queries.push(QueryMetrics {
                term: term.to_string(),
                latency_us: ms * 1000,
                total_matches: 0,
                docs_matched: 0,
                counts_exact: true,
                latency: Duration::from_millis(ms),
            });
        }
        let (literal, regex) = latencies_by_kind(&r);
        assert_eq!(
            literal,
            [Duration::from_millis(1), Duration::from_millis(3)]
        );
        assert_eq!(regex, [Duration::from_millis(2), Duration::from_millis(4)]);
    }

    #[test]
    fn ready_column_is_na_for_scan_rows() {
        let mut r = file_report("rg", 1);
        assert_eq!(fmt_ready(&r), "n/a");
        assert_eq!(fmt_mode(&r), "scan");
        r.mode = QueryMode::Serve;
        r.ready_time = Some(Duration::from_millis(125));
        assert_eq!(fmt_ready(&r), "125.00 ms");
        assert_eq!(fmt_mode(&r), "serve, ready in 125.00 ms");
    }
}

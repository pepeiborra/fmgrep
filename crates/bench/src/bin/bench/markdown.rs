//! The `--results` file: one run as markdown, for a terminal or a CI job
//! summary.

use std::fmt::Write;
use std::time::Duration;

use bench::terms::Source;
use bench::{BenchmarkReport, QueryMode};

use crate::load::{LOAD1_WARN, inflated};
use crate::machine::Stamps;
use crate::report::{fmt_bytes, latencies_by_kind, mean, p50};

/// What was benchmarked.
pub struct RunInfo {
    pub corpus: String,
    /// The pinned checkout of a single-corpus selection.
    pub source: Option<Source>,
    pub revision: Option<String>,
    pub fixture: bool,
    pub files: usize,
    pub bytes: u64,
    pub literals: usize,
    pub regexes: usize,
    /// `--terms` replaced the corpus's query set.
    pub custom_terms: bool,
}

/// `None` for `verify` when `--verify` was not given; `Err` holds the
/// mismatch lines.
pub fn render(
    run: &RunInfo,
    stamps: &Stamps,
    reports: &[BenchmarkReport],
    verify: Option<Result<usize, &[String]>>,
) -> String {
    let mut md = String::new();
    let w = &mut md;
    let _ = writeln!(w, "## fmgrep benchmark: {}\n", run.corpus);
    if run.fixture {
        let _ = writeln!(
            w,
            "**FIXTURE corpus: not a real checkout, so these are not corpus numbers.**\n"
        );
    }
    let _ = writeln!(w, "- corpus: {} files, {}", run.files, fmt_bytes(run.bytes));
    let _ = writeln!(w, "- revision: {}", revision(run));
    let _ = writeln!(
        w,
        "- queries: {} ({} literal, {} regex){}",
        run.literals + run.regexes,
        run.literals,
        run.regexes,
        if run.custom_terms {
            ", from --terms"
        } else {
            ""
        }
    );
    let _ = writeln!(w, "- machine: {}; {}", stamps.machine, stamps.os);
    let _ = writeln!(w, "- versions: {}; {}", stamps.fmgrep, stamps.rg);
    let _ = writeln!(w, "- date: {}\n", stamps.date);

    summary_table(w, reports);
    let _ = writeln!(
        w,
        "\nLatency is per query, spawn to exit: one `rg` process over a staged \
         copy of the corpus, or one `fmgrep --server` process against one \
         `fmgrep serve`. Footprint is the peak anonymous memory while querying. \
         load1 is the 1-minute load average at the row's start and end; `!` \
         marks {LOAD1_WARN} or more, where timings are likely inflated (rg \
         itself loads every core, so the row after it often starts high).\n"
    );
    match verify {
        None => {
            let _ = writeln!(w, "verify: not run");
        }
        Some(Ok(queries)) => {
            let _ = writeln!(
                w,
                "verify: OK, {} rows agree on {queries} queries",
                reports.len()
            );
        }
        Some(Err(diffs)) => {
            let _ = writeln!(w, "verify: FAILED, {} difference(s)\n", diffs.len());
            let _ = writeln!(w, "```");
            for d in diffs {
                let _ = writeln!(w, "{d}");
            }
            let _ = writeln!(w, "```");
        }
    }
    per_query_table(w, reports);
    md
}

fn revision(run: &RunInfo) -> String {
    let rev = run.revision.as_deref();
    match (&run.source, rev) {
        (Some(src), Some(rev)) if rev == src.commit => {
            format!("{} @ {rev} (pinned)", src.repo_url)
        }
        (Some(src), Some(rev)) => format!(
            "{} @ {rev} (differs from pinned {}; not comparable)",
            src.repo_url, src.commit
        ),
        (Some(src), None) => format!(
            "unknown (not a git checkout; pinned {} @ {})",
            src.repo_url, src.commit
        ),
        (None, Some(rev)) => rev.to_string(),
        (None, None) => "unknown".to_string(),
    }
}

fn ms(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else if ms >= 100.0 {
        format!("{ms:.0} ms")
    } else if ms >= 10.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{ms:.2} ms")
    }
}

fn opt<T>(v: Option<T>, f: impl Fn(T) -> String) -> String {
    v.map_or_else(|| "-".to_string(), f)
}

fn summary_table(w: &mut String, reports: &[BenchmarkReport]) {
    let _ = writeln!(
        w,
        "| row | mode | index build | index size | ready | literal p50 | literal mean \
         | regex p50 | regex mean | query footprint | index peak RSS | load1 |"
    );
    let _ = writeln!(w, "|---|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|");
    for r in reports {
        let serve = r.mode == QueryMode::Serve;
        let (literal, regex) = latencies_by_kind(r);
        let mem = r.memory.unwrap_or_default();
        let cells = [
            r.index_name.clone(),
            match r.mode {
                QueryMode::Serve => "serve".to_string(),
                QueryMode::Scan => "scan".to_string(),
            },
            opt(serve.then_some(r.index_time), ms),
            opt(serve.then_some(r.index_size_bytes), fmt_bytes),
            opt(r.ready_time, ms),
            opt(p50(&literal), ms),
            opt(mean(&literal), ms),
            opt(p50(&regex), ms),
            opt(mean(&regex), ms),
            opt(mem.query_peak_footprint_bytes, fmt_bytes),
            opt(mem.index_peak_rss_bytes.filter(|_| serve), fmt_bytes),
            opt(r.load, |l| {
                format!(
                    "{:.2}->{:.2}{}",
                    l.loadavg_start[0],
                    l.loadavg_end[0],
                    if inflated(&l) { "!" } else { "" }
                )
            }),
        ];
        let _ = writeln!(w, "| {} |", cells.join(" | "));
    }
}

/// A term as a table cell: a code span with `|` escaped.
fn cell(term: &str) -> String {
    let ticks = if term.contains('`') { "``" } else { "`" };
    format!("{ticks}{}{ticks}", term.replace('|', "\\|"))
}

fn per_query_table(w: &mut String, reports: &[BenchmarkReport]) {
    let Some(base) = reports.first() else {
        return;
    };
    let others = &reports[1..];
    let _ = writeln!(w, "\n<details><summary>Per query</summary>\n");
    let mut header = vec!["query", "matches", "files"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    header.extend(reports.iter().map(|r| r.index_name.clone()));
    header.extend(others.iter().map(|r| match others.len() {
        1 => "speedup".to_string(),
        _ => format!("speedup {}", r.index_name),
    }));
    let _ = writeln!(w, "| {} |", header.join(" | "));
    let align: Vec<&str> = std::iter::once("---")
        .chain(std::iter::repeat_n("--:", header.len() - 1))
        .collect();
    let _ = writeln!(w, "|{}|", align.join("|"));
    for (i, q) in base.queries.iter().enumerate() {
        let mut cells = vec![
            cell(&q.term),
            q.total_matches.to_string(),
            q.docs_matched.to_string(),
        ];
        let latency = |r: &BenchmarkReport| r.queries.get(i).map(|q| q.latency);
        cells.extend(reports.iter().map(|r| opt(latency(r), ms)));
        cells.extend(others.iter().map(|r| {
            opt(latency(r).filter(|d| !d.is_zero()), |d| {
                let x = q.latency.as_secs_f64() / d.as_secs_f64();
                if x >= 10.0 {
                    format!("{x:.0}x")
                } else {
                    format!("{x:.1}x")
                }
            })
        }));
        let _ = writeln!(w, "| {} |", cells.join(" | "));
    }
    let _ = writeln!(w, "\n</details>");
}

#[cfg(test)]
mod tests {
    use super::*;
    use bench::{LoadSpan, MemoryStats, QueryMetrics};

    const PIN: &str = "4cf7628aa66f56000c29aef93924b6b8515db8f7";

    fn run() -> RunInfo {
        RunInfo {
            corpus: "ghc".into(),
            source: Some(Source {
                repo_url: "https://github.com/ghc/ghc.git".into(),
                commit: PIN.into(),
            }),
            revision: Some(PIN.into()),
            fixture: false,
            files: 26787,
            bytes: 88_781_005,
            literals: 1,
            regexes: 1,
            custom_terms: false,
        }
    }

    fn stamps() -> Stamps {
        Stamps {
            machine: "Apple M5, 10 cores, 16 GB".into(),
            os: "macOS 26.0 (Darwin 25.5.0 arm64)".into(),
            rg: "ripgrep 15.2.0".into(),
            fmgrep: "fmgrep 0.1.0 (abc1234)".into(),
            date: "2026-09-24T12:00:00Z".into(),
        }
    }

    fn report(name: &str, mode: QueryMode, lat_ms: [u64; 2]) -> BenchmarkReport {
        let serve = mode == QueryMode::Serve;
        BenchmarkReport {
            index_name: name.into(),
            index_time: Duration::from_millis(if serve { 1733 } else { 45 }),
            index_size_bytes: if serve { 118_134_000 } else { 0 },
            num_files_indexed: 26787,
            queries: ["HscEnv", "/stg_\\w+_info|x/"]
                .iter()
                .zip(lat_ms)
                .map(|(term, ms)| QueryMetrics {
                    term: term.to_string(),
                    latency_us: ms * 1000,
                    total_matches: 42,
                    docs_matched: 7,
                    counts_exact: true,
                    latency: Duration::from_millis(ms),
                })
                .collect(),
            memory: Some(MemoryStats {
                index_peak_rss_bytes: Some(300 << 20),
                query_peak_footprint_bytes: Some(12 << 20),
                ..MemoryStats::default()
            }),
            load: Some(LoadSpan {
                loadavg_start: [1.0, 0.0, 0.0],
                loadavg_end: [5.0, 0.0, 0.0],
            }),
            mode,
            ready_time: serve.then(|| Duration::from_millis(33)),
        }
    }

    fn reports() -> Vec<BenchmarkReport> {
        vec![
            report("rg", QueryMode::Scan, [450, 900]),
            report("fmgrep", QueryMode::Serve, [3, 9]),
        ]
    }

    fn summary_rows(md: &str) -> Vec<&str> {
        md.lines()
            .skip_while(|l| !l.starts_with("| row |"))
            .skip(2)
            .take_while(|l| l.starts_with('|'))
            .collect()
    }

    #[test]
    fn summary_table_has_one_line_per_row_in_roster_order() {
        let md = render(&run(), &stamps(), &reports(), Some(Ok(2)));
        let rows = summary_rows(&md);
        assert_eq!(rows.len(), 2, "{md}");
        assert!(rows[0].starts_with("| rg | scan |"), "{}", rows[0]);
        assert!(
            rows[1].starts_with("| fmgrep | serve | 1.73 s | 112.66 MB | 33.0 ms | 3.00 ms | 3.00 ms | 9.00 ms | 9.00 ms | 12.00 MB | 300.00 MB | 1.00->5.00! |"),
            "{}",
            rows[1]
        );
        assert!(md.contains("verify: OK, 2 rows agree on 2 queries"), "{md}");
        assert!(
            md.contains("ripgrep 15.2.0") && md.contains("Apple M5"),
            "{md}"
        );
    }

    #[test]
    fn scan_rows_show_no_build_size_or_ready() {
        let md = render(&run(), &stamps(), &reports(), None);
        let cells: Vec<&str> = summary_rows(&md)[0].split(" | ").collect();
        assert_eq!(&cells[2..5], ["-", "-", "-"], "{cells:?}");
        assert_eq!(cells[10], "-", "no index, so no index RSS: {cells:?}");
        assert_eq!(cells[5], "450 ms", "{cells:?}");
        assert!(md.contains("verify: not run"), "{md}");
    }

    #[test]
    fn per_query_table_lists_every_term_with_both_latencies() {
        let md = render(&run(), &stamps(), &reports(), Some(Ok(2)));
        assert!(
            md.contains("| query | matches | files | rg | fmgrep | speedup |"),
            "{md}"
        );
        assert!(
            md.contains("| `HscEnv` | 42 | 7 | 450 ms | 3.00 ms | 150x |"),
            "{md}"
        );
        assert!(
            md.contains("| `/stg_\\w+_info\\|x/` | 42 | 7 | 900 ms | 9.00 ms | 100x |"),
            "{md}"
        );
    }

    #[test]
    fn verify_failure_is_in_the_report() {
        let diffs = vec!["[ghc] term \"HscEnv\": total_matches rg=42 vs fmgrep=41".to_string()];
        let md = render(&run(), &stamps(), &reports(), Some(Err(&diffs)));
        assert!(md.contains("verify: FAILED, 1 difference(s)"), "{md}");
        assert!(md.contains(&diffs[0]), "{md}");
        assert!(!md.contains("verify: OK"), "{md}");
    }

    #[test]
    fn fixture_corpus_is_flagged() {
        let mut info = run();
        info.fixture = true;
        let md = render(&info, &stamps(), &reports(), None);
        assert!(md.contains("**FIXTURE corpus"), "{md}");
        assert!(!render(&run(), &stamps(), &reports(), None).contains("FIXTURE"));
    }

    #[test]
    fn revision_off_the_pin_is_flagged() {
        let md = render(&run(), &stamps(), &reports(), None);
        assert!(md.contains(&format!("@ {PIN} (pinned)")), "{md}");
        let mut info = run();
        info.revision = Some("0".repeat(40));
        let md = render(&info, &stamps(), &reports(), None);
        assert!(md.contains(&format!("differs from pinned {PIN}")), "{md}");
        assert!(!md.contains("(pinned)"), "{md}");
    }

    #[test]
    fn unknown_stamps_render_as_unknown() {
        let unknown = Stamps {
            machine: crate::machine::UNKNOWN.into(),
            os: crate::machine::UNKNOWN.into(),
            rg: crate::machine::UNKNOWN.into(),
            fmgrep: crate::machine::UNKNOWN.into(),
            date: "2026-09-24T12:00:00Z".into(),
        };
        let mut info = run();
        info.revision = None;
        let md = render(&info, &unknown, &reports(), None);
        assert!(md.contains("- machine: unknown; unknown"), "{md}");
        assert!(md.contains("- versions: unknown; unknown"), "{md}");
        assert!(md.contains("- revision: unknown"), "{md}");
    }
}

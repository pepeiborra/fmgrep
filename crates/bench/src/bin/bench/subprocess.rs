//! fmgrep rows: an `index` spawn over the shared staged tree, then one
//! `serve` over the built index answering one `--server` spawn per term (see
//! [`crate::serve`]). Build time and size come from the `index` summary;
//! every `--server` report must agree with it on index name, file count and
//! size. Any child failure fails the row. Reports are named by the row.

use std::path::Path;
use std::time::Duration;

use bench::{BenchmarkReport, MemoryStats, QueryMode};
use serde::Deserialize;

use crate::child_spawn::{child_timeout, parse_json, resolve_child_bin, run_spawn};
use crate::mem::{opt_max, phase_stats, split};
use crate::serve::{Served, TermCounts, serve_row};

mod engine_server;
use engine_server::EngineServer;

/// Extension of the staged corpus files handed to the child binaries; the
/// shared staging in `run.rs` writes them under this name.
pub const STAGED_EXT: &str = "bin";

/// Which child binary to run, and how to index.
pub struct ChildSpec {
    /// Binary file name, e.g. `"fmgrep"`.
    pub bin_name: &'static str,
    /// Env var overriding binary discovery, e.g. `"FMGREP_BENCH_BIN"`.
    pub bin_env_var: &'static str,
    /// The row's table name, which names its report.
    pub label: &'static str,
    /// `index --store-docs`.
    pub store_docs: bool,
}

fn index_argv(spec: &ChildSpec, staged: &Path, index_dir: &Path) -> Vec<String> {
    let mut argv = vec![
        "index".to_string(),
        staged.display().to_string(),
        "--output".to_string(),
        index_dir.display().to_string(),
    ];
    if spec.store_docs {
        argv.push("--store-docs".to_string());
    }
    argv.push("--json".to_string());
    argv
}

/// Listens on an ephemeral loopback port; the bound address is the first
/// stdout line.
fn serve_argv(index_dir: &Path) -> Vec<String> {
    vec!["serve".to_string(), index_dir.display().to_string()]
}

/// Counts-only, so the wire stays the four counting fields; `--` keeps a
/// dash-leading term out of flag parsing, and a `/.../` term travels
/// verbatim (that spelling is what makes it a regex).
fn client_argv(addr: &str, term: &str) -> Vec<String> {
    vec![
        "--server".to_string(),
        addr.to_string(),
        "--json".to_string(),
        "--approx-counts".to_string(),
        "--".to_string(),
        term.to_string(),
    ]
}

fn shutdown_argv(addr: &str) -> Vec<String> {
    vec![
        "--server".to_string(),
        addr.to_string(),
        "--shutdown".to_string(),
    ]
}

/// `index --json` stdout summary mirror; `path`/`num_bytes` are not parsed.
#[derive(Debug, Deserialize)]
struct IndexSummary {
    index_name: String,
    store_docs: bool,
    num_files: usize,
    index_size_bytes: u64,
    build_time_us: u64,
}

/// One per-term result, mirroring the `--json` report schema. The engine's
/// own `latency_us` is not parsed: row latency is the client round-trip.
#[derive(Debug, Deserialize)]
struct ChildQuery {
    term: String,
    total_matches: u64,
    docs_matched: usize,
    /// Absent on the wire means rg-exact.
    #[serde(default = "yes")]
    counts_exact: bool,
}

fn yes() -> bool {
    true
}

/// `--server --json` stdout report mirror, for exactly one term.
#[derive(Debug, Deserialize)]
struct ClientReport {
    index_name: String,
    index_size_bytes: u64,
    num_files_indexed: usize,
    queries: Vec<ChildQuery>,
}

/// One client's counts for `term`; `Err` unless the served index is the one
/// `index` built and the report echoes exactly `term`.
fn client_counts(
    index: &IndexSummary,
    term: &str,
    client: ClientReport,
) -> Result<TermCounts, String> {
    if client.index_name != index.index_name {
        return Err(format!(
            "index/serve index_name skew: index built {:?}, server loaded {:?}",
            index.index_name, client.index_name
        ));
    }
    if client.num_files_indexed != index.num_files {
        return Err(format!(
            "index/serve file-count skew: index built {} file(s), server loaded {}",
            index.num_files, client.num_files_indexed
        ));
    }
    if client.index_size_bytes != index.index_size_bytes {
        return Err(format!(
            "index/serve size skew: index measured {} byte(s), server loaded {}",
            index.index_size_bytes, client.index_size_bytes
        ));
    }
    match &client.queries[..] {
        [q] if q.term == term => Ok(TermCounts {
            total_matches: q.total_matches,
            docs_matched: q.docs_matched,
            counts_exact: q.counts_exact,
        }),
        qs => {
            let echoed: Vec<&str> = qs.iter().map(|q| q.term.as_str()).collect();
            Err(format!(
                "client for term {term:?} answered terms {echoed:?}"
            ))
        }
    }
}

/// `Err` unless the child built the layout the row asked for.
fn check_summary(spec: &ChildSpec, summary: &IndexSummary) -> Result<(), String> {
    if summary.store_docs != spec.store_docs {
        return Err(format!(
            "child index summary store_docs skew: harness asked {}, child built {}",
            spec.store_docs, summary.store_docs
        ));
    }
    Ok(())
}

fn into_report(
    spec: &ChildSpec,
    index: &IndexSummary,
    served: Served,
    memory: MemoryStats,
) -> BenchmarkReport {
    BenchmarkReport {
        index_name: spec.label.to_string(),
        index_time: Duration::from_micros(index.build_time_us),
        index_size_bytes: index.index_size_bytes,
        num_files_indexed: index.num_files,
        queries: served.queries,
        memory: Some(memory),
        load: None,
        mode: QueryMode::Serve,
        ready_time: Some(served.ready_time),
    }
}

/// Run one engine row over the shared read-only staging `staged`.
pub fn run_child(
    spec: &ChildSpec,
    staged: &Path,
    terms: &[&str],
) -> Result<BenchmarkReport, String> {
    let bin = resolve_child_bin(spec)?;
    let index_parent =
        tempfile::tempdir().map_err(|e| format!("failed to create index dir: {e}"))?;
    let index_dir = index_parent.path().join("index");

    let argv = index_argv(spec, staged, &index_dir);
    let label = format!("{}#index", spec.bin_name);
    let spawned = run_spawn(&bin, &argv, &label, child_timeout())?;
    let summary: IndexSummary = parse_json(&spawned.stdout, "index", &argv)?;
    check_summary(spec, &summary)?;
    let (index_rss, index_fp) = split(&spawned.samples);
    let (index_peak_rss_bytes, index_avg_rss_bytes) = phase_stats(&index_rss, spawned.child_peak);
    let (index_peak_footprint_bytes, index_avg_footprint_bytes) = phase_stats(&index_fp, None);

    let mut server = EngineServer::new(&bin, spec.bin_name, &index_dir, &summary, child_timeout());
    let served = serve_row(&mut server, terms)?;
    let memory = MemoryStats {
        index_peak_rss_bytes,
        index_avg_rss_bytes,
        query_peak_rss_bytes: served.query_peak_rss_bytes,
        query_avg_rss_bytes: served.query_avg_rss_bytes,
        child_peak_rss_bytes: opt_max(spawned.child_peak, served.reaped_peak),
        index_peak_footprint_bytes,
        index_avg_footprint_bytes,
        query_peak_footprint_bytes: served.query_peak_footprint_bytes,
        query_avg_footprint_bytes: served.query_avg_footprint_bytes,
    };
    Ok(into_report(spec, &summary, served, memory))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ChildSpec {
        ChildSpec {
            bin_name: "fmgrep",
            bin_env_var: "FMGREP_BENCH_BIN",
            label: "fmgrep",
            store_docs: false,
        }
    }

    fn stored() -> ChildSpec {
        ChildSpec {
            label: "fmgrep-store-docs",
            store_docs: true,
            ..spec()
        }
    }

    #[test]
    fn index_argv_names_subcommand_and_json() {
        assert_eq!(
            index_argv(&spec(), Path::new("/tmp/stage"), Path::new("/tmp/idx")),
            vec!["index", "/tmp/stage", "--output", "/tmp/idx", "--json"]
        );
    }

    #[test]
    fn index_argv_carries_store_docs_flag() {
        assert_eq!(
            index_argv(&stored(), Path::new("/tmp/stage"), Path::new("/tmp/idx")),
            vec![
                "index",
                "/tmp/stage",
                "--output",
                "/tmp/idx",
                "--store-docs",
                "--json",
            ]
        );
    }

    #[test]
    fn summary_store_docs_skew_is_refused() {
        assert!(check_summary(&spec(), &summary()).is_ok());
        let err = check_summary(&stored(), &summary()).unwrap_err();
        assert!(err.contains("store_docs skew"), "{err}");
    }

    #[test]
    fn report_is_named_by_the_row_label() {
        let report = into_report(
            &stored(),
            &summary(),
            served(Vec::new()),
            MemoryStats::default(),
        );
        assert_eq!(report.index_name, "fmgrep-store-docs");
    }

    #[test]
    fn server_argv_is_counts_only_and_carries_the_term_verbatim() {
        for term in ["printk", "/pr[i1]ntk/", "-x"] {
            assert_eq!(
                client_argv("127.0.0.1:9", term),
                vec![
                    "--server",
                    "127.0.0.1:9",
                    "--json",
                    "--approx-counts",
                    "--",
                    term,
                ]
            );
        }
        assert_eq!(serve_argv(Path::new("/tmp/idx")), vec!["serve", "/tmp/idx"]);
        assert_eq!(
            shutdown_argv("127.0.0.1:9"),
            vec!["--server", "127.0.0.1:9", "--shutdown"]
        );
    }

    pub fn summary() -> IndexSummary {
        serde_json::from_str(
            r#"{"index_name":"fmgrep","store_docs":false,"path":"/tmp/idx","num_files":2,"num_bytes":20,"index_size_bytes":456,"build_time_us":123}"#,
        )
        .unwrap()
    }

    fn client_wire(size: u64, files: usize, name: &str, queries: &str) -> ClientReport {
        serde_json::from_str(&format!(
            r#"{{"index_name":"{name}","index_time_us":9,"index_size_bytes":{size},"num_files_indexed":{files},"queries":[{queries}]}}"#,
        ))
        .unwrap()
    }

    fn printk(extra: &str) -> String {
        format!(r#"{{"term":"printk","latency_us":7,"total_matches":2,"docs_matched":1{extra}}}"#)
    }

    fn served(queries: Vec<bench::QueryMetrics>) -> Served {
        Served {
            queries,
            ready_time: Duration::from_millis(40),
            query_peak_rss_bytes: Some(900),
            query_avg_rss_bytes: Some(850),
            reaped_peak: None,
            query_peak_footprint_bytes: Some(90),
            query_avg_footprint_bytes: Some(85),
        }
    }

    #[test]
    fn combined_report_takes_build_from_index_and_latency_from_the_client() {
        let counts = client_counts(
            &summary(),
            "printk",
            client_wire(456, 2, "fmgrep", &printk("")),
        )
        .unwrap();
        assert_eq!((counts.total_matches, counts.docs_matched), (2, 1));
        let q = bench::QueryMetrics {
            term: "printk".into(),
            latency_us: 2500,
            total_matches: counts.total_matches,
            docs_matched: counts.docs_matched,
            counts_exact: counts.counts_exact,
            latency: Duration::from_micros(2500),
        };
        let report = into_report(&spec(), &summary(), served(vec![q]), MemoryStats::default());
        assert_eq!(report.index_name, "fmgrep");
        assert_eq!(report.num_files_indexed, 2);
        assert_eq!(report.index_time, Duration::from_micros(123));
        assert_eq!(report.index_size_bytes, 456);
        assert_eq!(
            report.queries[0].latency_us, 2500,
            "the client wall, not the engine's 7 us"
        );
        assert_eq!(report.mode, QueryMode::Serve);
        assert_eq!(report.ready_time, Some(Duration::from_millis(40)));
    }

    #[test]
    fn missing_counts_exact_parses_as_rg_exact() {
        let exact = |extra: &str| {
            client_counts(
                &summary(),
                "printk",
                client_wire(456, 2, "fmgrep", &printk(extra)),
            )
            .unwrap()
            .counts_exact
        };
        assert!(exact(""));
        assert!(!exact(r#","counts_exact":false"#));
    }

    #[test]
    fn into_report_carries_harness_memory_through() {
        let mem = MemoryStats {
            index_peak_rss_bytes: Some(1000),
            index_avg_rss_bytes: Some(800),
            query_peak_rss_bytes: Some(900),
            query_avg_rss_bytes: Some(850),
            child_peak_rss_bytes: Some(1000),
            query_avg_footprint_bytes: Some(85),
            ..MemoryStats::default()
        };
        let report = into_report(&spec(), &summary(), served(Vec::new()), mem);
        assert_eq!(report.memory, Some(mem));
    }

    #[test]
    fn client_report_skew_is_rejected() {
        let q = printk("");
        assert!(client_counts(&summary(), "printk", client_wire(456, 2, "other", &q)).is_err());
        assert!(client_counts(&summary(), "printk", client_wire(456, 3, "fmgrep", &q)).is_err());
        assert!(client_counts(&summary(), "printk", client_wire(1, 2, "fmgrep", &q)).is_err());
    }

    #[test]
    fn client_report_must_echo_exactly_its_term() {
        let q = printk("");
        let wire = |queries: &str| client_wire(456, 2, "fmgrep", queries);
        assert!(client_counts(&summary(), "other", wire(&q)).is_err());
        assert!(client_counts(&summary(), "printk", wire("")).is_err());
        assert!(client_counts(&summary(), "printk", wire(&format!("{q},{q}"))).is_err());
    }

    #[test]
    fn client_report_without_file_count_is_rejected() {
        // Required on the wire: a stale child must not report 0 files.
        let wire = r#"{"index_name":"fmgrep","index_time_us":1,"index_size_bytes":1,"queries":[]}"#;
        assert!(serde_json::from_str::<ClientReport>(wire).is_err());
    }

    #[test]
    fn staged_dir_holds_one_entry_per_enumerated_doc() {
        // The children see exactly the enumerated set, named by doc id.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.c"), b"printk").unwrap();
        std::fs::write(dir.path().join("b.c"), b"kmalloc").unwrap();
        std::fs::write(dir.path().join("blob.bin"), b"a\x00b").unwrap();
        let docs = bench::discover_index_meta(dir.path()).unwrap();
        let staged = bench::StagedDocs::stage(
            bench::Corpus {
                root: dir.path(),
                docs: &docs,
            },
            STAGED_EXT,
        )
        .unwrap();
        let mut names: Vec<String> = std::fs::read_dir(staged.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["00000000.bin", "00000001.bin"]);
        assert_eq!(
            std::fs::read(staged.dir().join("00000001.bin")).unwrap(),
            b"kmalloc"
        );
    }
}

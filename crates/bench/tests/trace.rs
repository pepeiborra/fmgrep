//! JSONL trace schema tests, driven through the public API.

mod common;

use bench::{
    EngineBinary, LoadSpan, MemoryStats, QueryMode, TraceOptions, run_unified_benchmark,
    write_jsonl_trace_with_options,
};
use common::{
    DummyUnified, MockRegexUnified, tiny_docs, trace_test_path, unified_docs, write_jsonl_trace,
    write_jsonl_trace_with_run_id, write_jsonl_trace_with_run_id_and_corpus,
};

#[test]
fn write_trace_emits_jsonl() {
    let fx = tiny_docs();
    let report = run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk"]).unwrap();
    let dir = std::env::temp_dir().join(format!("bench-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("trace.jsonl");
    write_jsonl_trace(&report, &path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    // 1 query line + 1 index summary line
    assert_eq!(lines.len(), 2);
    let q: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(q["type"], "query");
    assert_eq!(q["term"], "printk");
    assert_eq!(q["total_matches"], 2);
    assert_eq!(q["num_files_indexed"], 2);
    let s: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(s["type"], "index");
    assert_eq!(s["index_name"], "dummy-unified");
    assert_eq!(s["num_files_indexed"], 2);
    // The harness input length travels separately as `num_docs` (here 0:
    // default options pass no input size); the indexed count is what the
    // report carried.
    assert_eq!(s["num_docs"], 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn trace_with_run_id_appends_and_stamps() {
    let fx = tiny_docs();
    let a = run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk"]).unwrap();
    let b = run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["missing"]).unwrap();
    let (dir, path) = trace_test_path("runid");
    write_jsonl_trace_with_run_id(&a, &path, "run-1", 2).unwrap();
    write_jsonl_trace_with_run_id(&b, &path, "run-1", 2).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    // (1 query + 1 summary) x 2 impls, appended into one file.
    assert_eq!(lines.len(), 4);
    let mut saw_query = 0;
    let mut saw_index = 0;
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["run_id"], "run-1");
        assert_eq!(v["index_name"], "dummy-unified");
        match v["type"].as_str() {
            Some("query") => {
                saw_query += 1;
                assert!(v.get("num_docs").is_none());
            }
            Some("index") => {
                saw_index += 1;
                assert_eq!(v["num_docs"], 2);
            }
            other => panic!("unexpected type: {other:?}"),
        }
    }
    assert_eq!(saw_query, 2);
    assert_eq!(saw_index, 2);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn trace_with_corpus_stamps_every_line() {
    let fx = tiny_docs();
    let report = run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk"]).unwrap();
    let (dir, path) = trace_test_path("corpus");
    write_jsonl_trace_with_run_id_and_corpus(&report, &path, "run-1", "chromium", 2).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["corpus"], "chromium");
        assert_eq!(v["run_id"], "run-1");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn trace_stamps_provenance_and_fixture_flag() {
    // Real-checkout lines carry corpus_revision and omit `fixture`;
    // fixture lines carry fixture:true and omit corpus_revision.
    let fx = tiny_docs();
    let report = run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk"]).unwrap();
    let (dir, path) = trace_test_path("prov");
    write_jsonl_trace_with_options(
        &report,
        &path,
        &TraceOptions {
            run_id: Some("run-1".into()),
            corpus: Some("chromium".into()),
            corpus_revision: Some("abc123".into()),
            fixture: false,
            num_docs: 2,
            append: true,
            ..TraceOptions::default()
        },
    )
    .unwrap();
    write_jsonl_trace_with_options(
        &report,
        &path,
        &TraceOptions {
            run_id: Some("run-1".into()),
            corpus: Some("chromium".into()),
            corpus_revision: None,
            fixture: true,
            num_docs: 2,
            append: true,
            ..TraceOptions::default()
        },
    )
    .unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 4);
    for line in &lines[..2] {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["corpus_revision"], "abc123");
        assert!(
            v.get("fixture").is_none(),
            "real line must omit fixture: {line}"
        );
    }
    for line in &lines[2..] {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["fixture"], true);
        assert!(
            v.get("corpus_revision").is_none(),
            "fixture line must omit corpus_revision: {line}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn trace_omits_memory_fields_when_unmeasured() {
    // In-process reports (memory: None) carry no RSS keys on any line.
    let fx = tiny_docs();
    let report = run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk"]).unwrap();
    assert_eq!(report.memory, None);
    let (dir, path) = trace_test_path("nomem");
    write_jsonl_trace_with_run_id_and_corpus(&report, &path, "run-1", "linux", 2).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content.lines().count(), 2);
    for line in content.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        for key in [
            "index_peak_rss_bytes",
            "index_avg_rss_bytes",
            "query_peak_rss_bytes",
            "query_avg_rss_bytes",
            "child_peak_rss_bytes",
            "index_peak_footprint_bytes",
            "index_avg_footprint_bytes",
            "query_peak_footprint_bytes",
            "query_avg_footprint_bytes",
        ] {
            assert!(v.get(key).is_none(), "unmeasured line must omit {key}");
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn trace_stamps_memory_fields_when_measured() {
    // Subprocess reports stamp the same RSS fields on query lines and
    // the index summary line (denormalized, like run_id/corpus).
    let fx = tiny_docs();
    let mut report =
        run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk"]).unwrap();
    report.memory = Some(MemoryStats {
        index_peak_rss_bytes: Some(1_000_000),
        index_avg_rss_bytes: Some(800_000),
        query_peak_rss_bytes: Some(900_000),
        query_avg_rss_bytes: Some(850_000),
        child_peak_rss_bytes: Some(1_000_000),
        index_peak_footprint_bytes: Some(100_000),
        index_avg_footprint_bytes: Some(80_000),
        query_peak_footprint_bytes: Some(90_000),
        query_avg_footprint_bytes: None,
    });
    let (dir, path) = trace_test_path("mem");
    write_jsonl_trace_with_run_id_and_corpus(&report, &path, "run-1", "linux", 2).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["index_peak_rss_bytes"], 1_000_000);
        assert_eq!(v["index_avg_rss_bytes"], 800_000);
        assert_eq!(v["query_peak_rss_bytes"], 900_000);
        assert_eq!(v["query_avg_rss_bytes"], 850_000);
        assert_eq!(v["child_peak_rss_bytes"], 1_000_000);
        assert_eq!(v["index_peak_footprint_bytes"], 100_000);
        assert_eq!(v["index_avg_footprint_bytes"], 80_000);
        assert_eq!(v["query_peak_footprint_bytes"], 90_000);
        assert!(
            v.get("query_avg_footprint_bytes").is_none(),
            "unmeasured field omitted"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn trace_stamps_load_span_on_every_line_and_omits_it_when_unsampled() {
    let fx = tiny_docs();
    let mut report =
        run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk"]).unwrap();
    let (dir, path) = trace_test_path("load");
    write_jsonl_trace(&report, &path).unwrap();
    for line in std::fs::read_to_string(&path).unwrap().lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v.get("loadavg_start").is_none() && v.get("loadavg_end").is_none());
    }
    report.load = Some(LoadSpan {
        loadavg_start: [1.5, 2.25, 3.0],
        loadavg_end: [4.0, 2.5, 3.0],
    });
    write_jsonl_trace(&report, &path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let kinds: Vec<String> = content
        .lines()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["loadavg_start"], serde_json::json!([1.5, 2.25, 3.0]));
            assert_eq!(v["loadavg_end"], serde_json::json!([4.0, 2.5, 3.0]));
            v["type"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(kinds, ["query", "index"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn trace_options_overwrite_omits_run_id_by_default() {
    let fx = tiny_docs();
    let report = run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk"]).unwrap();
    let (dir, path) = trace_test_path("opts");
    // Default options: no run_id.
    write_jsonl_trace_with_options(&report, &path, &TraceOptions::default()).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content.lines().count(), 2);
    for line in content.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v.get("run_id").is_none(), "run_id must be omitted: {line}");
    }
    // append=false truncates: rewriting keeps exactly 2 lines.
    write_jsonl_trace_with_options(
        &report,
        &path,
        &TraceOptions {
            run_id: Some("run-2".into()),
            corpus: None,
            corpus_revision: None,
            fixture: false,
            num_docs: 2,
            append: false,
            ..TraceOptions::default()
        },
    )
    .unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["run_id"], "run-2");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn trace_carries_regex_term_rows() {
    // A regex term's row traces like any other: the `/.../` spelling is the
    // term verbatim, engine counts and file coverage travel unchanged.
    let fx = unified_docs();
    let report = run_unified_benchmark(|| MockRegexUnified, &["/mock.*/"]).unwrap();
    let (dir, path) = trace_test_path("regex-report");
    write_jsonl_trace_with_options(
        &report,
        &path,
        &TraceOptions {
            run_id: Some("run-rx".into()),
            corpus: Some("linux".into()),
            corpus_revision: Some("abc123".into()),
            fixture: false,
            num_docs: fx.num_docs(),
            append: true,
            ..TraceOptions::default()
        },
    )
    .unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["corpus"], "linux");
        assert_eq!(v["run_id"], "run-rx");
    }
    let q: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(q["type"], "query");
    assert_eq!(q["term"], "/mock.*/");
    assert_eq!(q["total_matches"], 3);
    assert_eq!(q["docs_matched"], 2);
    let s: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(s["type"], "index");
    assert_eq!(s["index_name"], "mock-regex-unified");
    assert_eq!(s["num_files_indexed"], 3);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn trace_stamps_mode_on_every_line_and_ready_on_the_index_line() {
    let fx = tiny_docs();
    let mut report =
        run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk", "missing"]).unwrap();
    let (dir, path) = trace_test_path("mode");
    let lines = |report: &bench::BenchmarkReport| -> Vec<serde_json::Value> {
        write_jsonl_trace(report, &path).unwrap();
        std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    for v in lines(&report) {
        assert_eq!(v["mode"], "scan");
        assert!(v.get("ready_us").is_none(), "scan rows have no ready: {v}");
    }
    report.mode = QueryMode::Serve;
    report.ready_time = Some(std::time::Duration::from_micros(1234));
    let served = lines(&report);
    assert_eq!(served.len(), 3);
    for v in &served {
        assert_eq!(v["mode"], "serve");
    }
    assert!(served[..2].iter().all(|v| v.get("ready_us").is_none()));
    assert_eq!(served[2]["type"], "index");
    assert_eq!(served[2]["ready_us"], 1234);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn trace_stamps_the_engine_binary_on_the_index_line_only() {
    let fx = tiny_docs();
    let report = run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk"]).unwrap();
    let (dir, path) = trace_test_path("engine-bin");
    write_jsonl_trace_with_options(
        &report,
        &path,
        &TraceOptions {
            num_docs: 2,
            engine_binary: Some(EngineBinary {
                path: "/x/fmgrep".into(),
                mtime_ms: 1_790_000_000_000,
                size_bytes: 4096,
            }),
            ..TraceOptions::default()
        },
    )
    .unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<serde_json::Value> = content
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(lines[0].get("bin_path").is_none(), "{}", lines[0]);
    assert_eq!(lines[1]["type"], "index");
    assert_eq!(lines[1]["bin_path"], "/x/fmgrep");
    assert_eq!(lines[1]["bin_mtime_ms"], 1_790_000_000_000u64);
    assert_eq!(lines[1]["bin_size_bytes"], 4096);
    std::fs::remove_dir_all(&dir).ok();
}

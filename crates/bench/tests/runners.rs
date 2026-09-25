//! Runner contract tests: one runner over mixed literal + `/.../` terms.

mod common;

use bench::{UnifiedIndex, run_unified_benchmark};
use common::{
    DummyUnified, KindMock, MockRegexUnified, SkippyUnified, UNIFIED_REGEX_CALLS, tiny_docs,
    trace_test_path, unified_docs, write_jsonl_trace_with_run_id,
};

#[test]
fn report_carries_the_indexed_count_not_the_input_length() {
    let fx = tiny_docs();
    assert_eq!(fx.num_docs(), 2);
    let report = run_unified_benchmark(|| SkippyUnified::build(fx.corpus()), &["printk"]).unwrap();
    assert_eq!(report.num_files_indexed, 1);
    let (dir, path) = trace_test_path("skew");
    write_jsonl_trace_with_run_id(&report, &path, "run-1", fx.num_docs()).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    // Query line and summary line both carry the indexed count (1) while
    // the harness input length travels separately as num_docs (2).
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["num_files_indexed"], 1);
    }
    let s: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(s["num_docs"], 2);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unified_search_contract_holds() {
    let fx = unified_docs();
    let idx = DummyUnified::build(fx.corpus());
    assert_eq!(idx.name(), "dummy-unified");
    // Sorted by doc_idx, zero-count docs omitted.
    assert_eq!(idx.search(b"printk"), vec![(0, 1), (2, 2)]);
    assert_eq!(idx.search(b"missing"), vec![]);
    assert_eq!(idx.search(b""), vec![]);
    // Separator-containing terms never match.
    assert_eq!(idx.search(b"a\x00b"), vec![]);
    // Empty middle doc never reported.
    assert!(!idx.search(b"printk").iter().any(|&(i, _)| i == 1));
}

#[test]
fn run_unified_benchmark_totals_match_the_naive_scan() {
    let fx = unified_docs();
    let docs = common::read_corpus_docs(fx.corpus()).unwrap();
    let report =
        run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk", "missing"]).unwrap();
    assert_eq!(report.index_name, "dummy-unified");
    assert_eq!(report.num_files_indexed, fx.num_docs());
    for q in &report.queries {
        let term = q.term.as_bytes();
        let counts: Vec<u64> = docs
            .iter()
            .map(|d| {
                (0..d.bytes.len().saturating_sub(term.len() - 1))
                    .filter(|&i| d.bytes[i..].starts_with(term))
                    .count() as u64
            })
            .collect();
        assert_eq!(
            q.total_matches,
            counts.iter().sum::<u64>(),
            "term {:?}",
            q.term
        );
        assert_eq!(
            q.docs_matched,
            counts.iter().filter(|&&c| c > 0).count(),
            "term {:?}",
            q.term
        );
    }
}

#[test]
fn unified_report_reuses_trace_schema() {
    let fx = unified_docs();
    let report = run_unified_benchmark(|| DummyUnified::build(fx.corpus()), &["printk"]).unwrap();
    let (dir, path) = trace_test_path("unified");
    write_jsonl_trace_with_run_id(&report, &path, "run-u", fx.num_docs()).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    let q: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(q["type"], "query");
    assert_eq!(q["term"], "printk");
    assert_eq!(q["total_matches"], 3);
    assert_eq!(q["docs_matched"], 2);
    assert_eq!(q["run_id"], "run-u");
    let s: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(s["type"], "index");
    assert_eq!(s["index_name"], "dummy-unified");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unified_regex_runner_engages_engine_regex_path_not_literal() {
    UNIFIED_REGEX_CALLS.with(|c| c.set(0));
    let report = run_unified_benchmark(|| MockRegexUnified, &["/mock.*/"]).unwrap();
    assert_eq!(report.index_name, "mock-regex-unified");
    assert_eq!(report.num_files_indexed, 3);
    assert_eq!(report.queries.len(), 1);
    assert_eq!(report.queries[0].term, "/mock.*/");
    assert_eq!(report.queries[0].total_matches, 3);
    assert_eq!(report.queries[0].docs_matched, 2);
    // `rg`'s regex path searches whole documents as one record, so no regex
    // row here claims rg's line-oriented counts.
    assert!(!report.queries[0].counts_exact);
    // Airtight dispatch proof: exactly one regex-path engagement for
    // the whole run (the unified single search) — the panicking
    // literal search() was never called.
    assert_eq!(
        UNIFIED_REGEX_CALLS.with(|c| c.get()),
        1,
        "unified regex run must engage search_regex exactly once"
    );
    let err = run_unified_benchmark(|| MockRegexUnified, &["/([/"]).unwrap_err();
    assert_eq!(err.pattern, "([");
}

#[test]
fn regex_runners_reject_empty_pattern_loudly() {
    // Central guard in the runner (not per-engine): `//` never reaches
    // the engine — loud RegexError naming the term, never
    // match-everywhere zeros.
    UNIFIED_REGEX_CALLS.with(|c| c.set(0));
    let err = run_unified_benchmark(|| MockRegexUnified, &["//"]).unwrap_err();
    assert_eq!(err.pattern, "//");
    assert!(err.to_string().contains("empty"), "message: {err}");
    assert_eq!(
        UNIFIED_REGEX_CALLS.with(|c| c.get()),
        0,
        "empty pattern must be rejected before engaging the engine"
    );
}

#[test]
fn regex_multi_pattern_abort_order_discards_partial_results() {
    // First-invalid-pattern aborts: the second pattern must Err naming, discarding the first pattern's counts (no partial Ok).
    UNIFIED_REGEX_CALLS.with(|c| c.set(0));
    let err = run_unified_benchmark(|| MockRegexUnified, &["/mock.*/", "/([/"]).unwrap_err();
    assert_eq!(err.pattern, "([");
    assert!(err.to_string().contains("(["));
    assert_eq!(
        UNIFIED_REGEX_CALLS.with(|c| c.get()),
        2,
        "unified abort must follow one good search plus one failing call"
    );
}

#[test]
fn regex_runners_engage_once_per_pattern() {
    // With two valid patterns the runner engages search_regex exactly once
    // per pattern, and the panicking literal search() is never called. The
    // mock only serves "mock.*", so the second pattern reuses it:
    // engagement counting, not pattern variety, is what this pins.
    UNIFIED_REGEX_CALLS.with(|c| c.set(0));
    let report = run_unified_benchmark(|| MockRegexUnified, &["/mock.*/", "/mock.*/"]).unwrap();
    assert_eq!(report.queries.len(), 2);
    assert_eq!(
        UNIFIED_REGEX_CALLS.with(|c| c.get()),
        2,
        "unified regex run must engage search_regex once per pattern"
    );
}

#[test]
fn one_runner_dispatches_each_term_by_its_spelling() {
    // Mixed argv in one run: a bare term takes the literal path, a
    // `/.../` term the engine's regex path, and only the literal rows claim
    // rg-exact counts.
    let report = run_unified_benchmark(|| KindMock, &["printk", "/printk/", "a/b"]).unwrap();
    let rows: Vec<(&str, u64, bool)> = report
        .queries
        .iter()
        .map(|q| (q.term.as_str(), q.total_matches, q.counts_exact))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("printk", 1, true),
            ("/printk/", 2, false),
            ("a/b", 1, true)
        ]
    );
}

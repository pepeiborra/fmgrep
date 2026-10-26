use super::*;

use crate::matching::MatchMode;
use crate::query::QueryRequest;
use crate::sink::DocMatches;
use crate::sink::MatchSink;
use crate::wire::QueryReport;

const MATCHES: QueryOutput = QueryOutput::Matches {
    before: 0,
    after: 0,
    line_numbers: true,
};

/// One `doc` call: the document, its counts, and `(line, match columns,
/// text)` per emitted line.
#[derive(Debug)]
struct Seen {
    path: String,
    matched_lines: u64,
    matched_spans: u64,
    lines: Vec<(u64, Vec<u64>, String)>,
}

/// Records the sink contract per call.
#[derive(Debug, Default)]
struct Recorder {
    corpus: Option<(usize, u64)>,
    terms: Vec<String>,
    seen: Vec<Seen>,
    ended: usize,
}

impl MatchSink for Recorder {
    fn corpus(&mut self, docs: usize, bytes: u64) -> Result<(), String> {
        assert!(self.corpus.is_none(), "corpus must be announced once");
        self.corpus = Some((docs, bytes));
        Ok(())
    }
    fn term_start(&mut self, term: &str) -> Result<(), String> {
        self.terms.push(term.to_owned());
        Ok(())
    }
    fn doc(&mut self, m: DocMatches<'_>) -> Result<(), String> {
        self.seen.push(Seen {
            path: m.path.to_owned(),
            matched_lines: m.outcome.matched_lines,
            matched_spans: m.outcome.matched_spans,
            lines: m
                .outcome
                .lines
                .iter()
                .map(|l| {
                    (
                        l.line,
                        l.spans.iter().map(|&(off, _)| u64::from(off) + 1).collect(),
                        String::from_utf8_lossy(&m.raw[l.start..l.end]).into_owned(),
                    )
                })
                .collect(),
        });
        Ok(())
    }
    fn term_end(&mut self, _: &QueryReport) -> Result<(), String> {
        self.ended += 1;
        Ok(())
    }
}

/// `(path, line, match columns, text)` per emitted line of one query.
type Emitted = Vec<(String, u64, Vec<u64>, String)>;

impl Recorder {
    fn emitted(&self) -> Emitted {
        self.seen
            .iter()
            .flat_map(|s| {
                s.lines.iter().map(move |(line, cols, text)| {
                    (s.path.clone(), *line, cols.clone(), text.clone())
                })
            })
            .collect()
    }

    /// `(path, matched_lines, matched_spans)` per emitted document.
    fn shape(&self) -> Vec<(String, u64, u64)> {
        self.seen
            .iter()
            .map(|s| (s.path.clone(), s.matched_lines, s.matched_spans))
            .collect()
    }
}

fn query_out(
    index: &Path,
    regex: bool,
    terms: &[&str],
    output: QueryOutput,
) -> Result<(OutputReport, Recorder), String> {
    let manifest = read_manifest(index).unwrap();
    let terms: Vec<String> = terms.iter().map(|t| t.to_string()).collect();
    let req = QueryRequest {
        regex,
        ..request(index, &manifest, &terms, output)
    };
    let mut rec = Recorder::default();
    let out = run_query(&req, &mut rec)?;
    Ok((out, rec))
}

fn ok(index: &Path, regex: bool, terms: &[&str], output: QueryOutput) -> (OutputReport, Recorder) {
    query_out(index, regex, terms, output).unwrap()
}

#[test]
fn matches_mode_fetches_probe_exact_docs_too() {
    // The index answers `printk` exactly from the probe, so counts mode
    // fetches nothing — matches mode must fetch those docs anyway.
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let (out, rec) = ok(&index, false, &["printk"], MATCHES);
        assert_eq!(out.queries[0].total_matches, 4, "store_docs={store_docs}");
        assert_eq!(
            rec.emitted(),
            vec![
                ("a.c".to_string(), 1, vec![1], "printk one".to_string()),
                (
                    "a.c".to_string(),
                    3,
                    vec![1, 12],
                    "printk two printk".to_string()
                ),
                ("b.c".to_string(), 2, vec![6], "tail printk".to_string()),
            ],
            "store_docs={store_docs}"
        );
        assert_eq!(
            rec.shape()[0],
            ("a.c".to_string(), 2, 3),
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn counts_mode_report_has_only_the_counting_keys() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let (out, _) = ok(&index, false, &["printk", "/a+/"], QueryOutput::Counts);
        // A literal that cannot overlap itself counts as rg does; a pattern
        // never does.
        assert!(out.queries[0].counts_exact, "store_docs={store_docs}");
        assert!(!out.queries[1].counts_exact, "store_docs={store_docs}");
        let json = serde_json::to_string(&out.queries[0]).unwrap();
        let keys: Vec<&str> = json
            .split(",\"")
            .filter_map(|f| f.split("\":").next())
            .map(|k| k.trim_start_matches(['{', '"']))
            .collect();
        assert_eq!(
            keys,
            vec!["term", "latency_us", "total_matches", "docs_matched"],
            "store_docs={store_docs}: {json}"
        );
        // Only an inexact term grows the wire.
        let json = serde_json::to_string(&out.queries[1]).unwrap();
        assert!(
            json.contains("\"counts_exact\":false"),
            "store_docs={store_docs}: {json}"
        );
    }
}

#[test]
fn files_with_matches_needs_no_fetch_for_probe_exact_literals() {
    // The probe proves membership, so the shard can be gone entirely.
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    std::fs::remove_file(index.join(DOCS_SHARD)).unwrap();
    let (out, rec) = ok(&index, false, &["printk"], QueryOutput::FilesWithMatches);
    // Membership is the whole answer: no counts are claimed.
    assert_eq!(
        rec.shape(),
        vec![("a.c".to_string(), 0, 0), ("b.c".to_string(), 0, 0)]
    );
    assert_eq!(out.queries[0].total_matches, 2);
    assert_eq!(out.queries[0].docs_matched, 2);
    assert!(!out.queries[0].counts_exact);
}

#[test]
fn count_matches_total_is_the_matching_pass() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        // `aa` in `aaaa`: the probe counts 3 overlapping, rg counts 2.
        let (out, rec) = ok(&index, false, &["aa"], QueryOutput::CountMatches);
        assert_eq!(out.queries[0].total_matches, 2, "store_docs={store_docs}");
        assert_eq!(out.queries[0].docs_matched, 1, "store_docs={store_docs}");
        assert!(out.queries[0].counts_exact, "store_docs={store_docs}");
        assert_eq!(
            rec.shape(),
            vec![("b.c".to_string(), 1, 2)],
            "store_docs={store_docs}"
        );
        // A term that cannot overlap itself answers from the probe count.
        let (_, rec) = ok(&index, false, &["printk"], QueryOutput::CountMatches);
        let spans: Vec<u64> = rec.seen.iter().map(|s| s.matched_spans).collect();
        assert_eq!(spans, vec![3, 1], "store_docs={store_docs}");
    }
}

#[test]
fn count_lines_counts_matched_lines_not_matches() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let (out, rec) = ok(&index, false, &["printk"], QueryOutput::CountLines);
        let got: Vec<(u64, u64)> = rec
            .seen
            .iter()
            .map(|s| (s.matched_lines, s.matched_spans))
            .collect();
        assert_eq!(got, vec![(2, 3), (1, 1)], "store_docs={store_docs}");
        assert_eq!(out.queries[0].total_matches, 4, "store_docs={store_docs}");
        assert_eq!(out.queries[0].docs_matched, 2, "store_docs={store_docs}");
        assert!(rec.emitted().is_empty(), "store_docs={store_docs}");
    }
}

#[test]
fn matches_and_counts_agree_across_both_layouts() {
    let tmp = tempfile::tempdir().unwrap();
    let per_mode: Vec<Emitted> = LAYOUTS
        .into_iter()
        .map(|store_docs| {
            let index = staged(tmp.path(), store_docs);
            ok(&index, true, &["print[kf]", "a+", "tail"], MATCHES)
                .1
                .emitted()
        })
        .collect();
    assert_eq!(per_mode[0], per_mode[1]);
    assert!(!per_mode[0].is_empty());
}

#[test]
fn line_paths_are_manifest_relative() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    std::fs::create_dir_all(folder.join("deep/er")).unwrap();
    std::fs::write(folder.join("deep/er/x.c"), b"printk\n").unwrap();
    std::fs::write(folder.join("top.c"), b"printk\n").unwrap();
    for store_docs in LAYOUTS {
        let index = tmp.path().join(format!("idx-{store_docs}"));
        index_it(&folder, &index, store_docs);
        let (_, rec) = ok(&index, false, &["printk"], MATCHES);
        let paths: Vec<&str> = rec.seen.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["deep/er/x.c", "top.c"],
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn rejected_pattern_fails_before_any_doc_is_fetched() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    std::fs::remove_file(index.join(DOCS_SHARD)).unwrap();
    let err = query_out(&index, true, &["a\nb"], MATCHES).unwrap_err();
    assert!(
        err.contains("the literal \"\\n\" is not allowed in a regex"),
        "got: {err}"
    );
    // The same pattern still counts in the harness path: only match
    // extraction is line-oriented.
    assert!(query_out(&index, true, &["a\nb"], QueryOutput::Counts).is_ok());
}

#[test]
fn matches_mode_counts_are_the_matching_pass() {
    // `^` anchors the buffer in the counting pass (0 matches) and the line in
    // the scan: matches mode reports the scan's numbers.
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let (out, rec) = ok(&index, true, &["^tail"], MATCHES);
        assert_eq!(out.queries[0].total_matches, 1, "store_docs={store_docs}");
        assert_eq!(out.queries[0].docs_matched, 1, "store_docs={store_docs}");
        assert!(out.queries[0].counts_exact, "store_docs={store_docs}");
        assert_eq!(
            rec.emitted(),
            vec![("b.c".to_string(), 2, vec![1], "tail printk".to_string())],
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn counts_and_emit_split_on_self_overlap() {
    // The FM-index counts document-wide and overlapping; rg counts
    // non-overlapping spans. `aa` over `aaaa` is the smallest witness.
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let (counts, _) = ok(&index, false, &["aa"], QueryOutput::Counts);
        assert_eq!(
            counts.queries[0].total_matches, 3,
            "store_docs={store_docs}"
        );
        assert!(!counts.queries[0].counts_exact, "store_docs={store_docs}");
        let (spans, _) = ok(&index, false, &["aa"], QueryOutput::CountMatches);
        assert_eq!(spans.queries[0].total_matches, 2, "store_docs={store_docs}");
        assert!(spans.queries[0].counts_exact, "store_docs={store_docs}");
    }
}

const OUTPUTS: [QueryOutput; 4] = [
    MATCHES,
    QueryOutput::CountLines,
    QueryOutput::CountMatches,
    QueryOutput::FilesWithMatches,
];

#[test]
fn sink_sees_every_matching_doc_once_ascending() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        for output in OUTPUTS {
            let (_, rec) = ok(&index, false, &["printk"], output);
            let paths: Vec<&str> = rec.seen.iter().map(|s| s.path.as_str()).collect();
            assert_eq!(paths, ["a.c", "b.c"], "store_docs={store_docs} {output:?}");
            assert_eq!(
                rec.terms,
                vec!["printk".to_string()],
                "store_docs={store_docs} {output:?}"
            );
            assert_eq!(rec.ended, 1, "store_docs={store_docs} {output:?}");
            // The corpus shape rg's stats need: every doc, every byte.
            assert_eq!(
                rec.corpus,
                Some((2, 53)),
                "store_docs={store_docs} {output:?}"
            );
        }
    }
}

#[test]
fn probe_exact_and_stale_docs_merge_into_one_ascending_stream() {
    // Fresh docs answer from the probe, stale ones are fetched: the emission
    // must still be one ascending run.
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(
        tmp.path(),
        &[
            ("0.c", b"printk a"),
            ("1.c", b"printk b printk"),
            ("2.c", b"printk c"),
            ("3.c", b"nothing"),
            ("4.c", b"printk e"),
        ],
    );
    let index = tmp.path().join("idx");
    index_it(&folder, &index, false);
    std::fs::write(folder.join("1.c"), b"printk b printk printk").unwrap();
    std::fs::write(folder.join("3.c"), b"now printk").unwrap();
    let manifest = read_manifest(&index).unwrap();
    let terms = vec!["printk".to_string()];
    for output in OUTPUTS {
        let mut rec = Recorder::default();
        run_query(&request(&index, &manifest, &terms, output), &mut rec).unwrap();
        let paths: Vec<&str> = rec.seen.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["0.c", "1.c", "2.c", "3.c", "4.c"], "{output:?}");
    }
    // Count-matches keeps the probe's count for an unfetched doc and rg's
    // count of the current bytes for a stale one.
    let mut rec = Recorder::default();
    let req = request(&index, &manifest, &terms, QueryOutput::CountMatches);
    run_query(&req, &mut rec).unwrap();
    let spans: Vec<u64> = rec.seen.iter().map(|s| s.matched_spans).collect();
    assert_eq!(spans, vec![1, 3, 1, 1, 1]);
}

#[test]
fn counts_mode_never_touches_the_sink() {
    struct PanicSink;
    impl MatchSink for PanicSink {
        fn corpus(&mut self, _: usize, _: u64) -> Result<(), String> {
            panic!("counts mode announced the corpus")
        }
        fn term_start(&mut self, term: &str) -> Result<(), String> {
            panic!("counts mode started term {term:?}")
        }
        fn doc(&mut self, m: DocMatches<'_>) -> Result<(), String> {
            panic!("counts mode emitted doc {}", m.path)
        }
        fn term_end(&mut self, _: &QueryReport) -> Result<(), String> {
            panic!("counts mode ended a term")
        }
    }
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let manifest = read_manifest(&index).unwrap();
        let terms = vec!["printk".to_string()];
        let req = request(&index, &manifest, &terms, QueryOutput::Counts);
        let out = run_query(&req, &mut PanicSink).unwrap();
        assert_eq!(out.queries[0].total_matches, 4, "store_docs={store_docs}");
    }
}

#[test]
fn sink_error_fails_the_query_loudly_and_stops() {
    struct FailSecond(usize);
    impl MatchSink for FailSecond {
        fn doc(&mut self, _: DocMatches<'_>) -> Result<(), String> {
            self.0 += 1;
            if self.0 == 2 {
                return Err("error: sink said no".to_string());
            }
            Ok(())
        }
    }
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let manifest = read_manifest(&index).unwrap();
        let terms = vec!["printk".to_string()];
        let req = request(&index, &manifest, &terms, MATCHES);
        let mut sink = FailSecond(0);
        let err = run_query(&req, &mut sink).unwrap_err();
        assert_eq!(err, "error: sink said no", "store_docs={store_docs}");
        assert_eq!(sink.0, 2, "store_docs={store_docs}");
    }
}

#[test]
fn ignore_case_lines_are_the_folded_ones() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let folder = stage_corpus(
            tmp.path(),
            &[
                ("a.c", b"HscEnv here\nnothing\nhscenv there\n"),
                ("b.c", b"HSCENV\n"),
            ],
        );
        let index = tmp.path().join(format!("idx-{store_docs}"));
        index_it(&folder, &index, store_docs);
        let manifest = read_manifest(&index).unwrap();
        let terms = vec!["hscenv".to_string()];
        let req = QueryRequest {
            mode: MatchMode {
                ignore_case: true,
                ..MatchMode::default()
            },
            ..request(&index, &manifest, &terms, MATCHES)
        };
        let mut rec = Recorder::default();
        let out = run_query(&req, &mut rec).unwrap();
        assert_eq!(out.queries[0].total_matches, 3, "store_docs={store_docs}");
        assert_eq!(out.queries[0].docs_matched, 2, "store_docs={store_docs}");
        assert_eq!(
            rec.emitted(),
            vec![
                ("a.c".to_string(), 1, vec![1], "HscEnv here".to_string()),
                ("a.c".to_string(), 3, vec![1], "hscenv there".to_string()),
                ("b.c".to_string(), 1, vec![1], "HSCENV".to_string()),
            ],
            "store_docs={store_docs}"
        );
    }
}

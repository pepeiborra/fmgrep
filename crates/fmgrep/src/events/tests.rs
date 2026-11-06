use super::*;

use crate::matching::{MatchMode, Print, QueryOutput, strip_bom};
use crate::tests::support::search;

/// Every expectation is rg 15.2.0's stdout for the quoted command, with the
/// `elapsed` objects zeroed. Common flags:
/// `rg --no-ignore --no-require-git -a --sort path --json`.
const A: (&str, &[u8]) = (
    "a.txt",
    b"alpha one\nbeta\nalpha two\ndelta\nepsilon\nzeta\nalpha and alpha\neta\n",
);
const B: (&str, &[u8]) = ("sub/b.txt", b"first\nalpha in sub\nthird\n");

const MATCHES: QueryOutput = QueryOutput::Matches(Print {
    before: 0,
    after: 0,
    line_numbers: true,
    only_matching: false,
    color: false,
    path: false,
    json: true,
});

/// The stream with every wall clock blanked: `elapsed_total` emptied, each
/// file's `elapsed` zeroed.
fn blank_wall(text: &str) -> String {
    let file =
        regex::Regex::new(r#""elapsed":\{"secs":\d+,"nanos":\d+,"human":"[^"]*"\}"#).unwrap();
    let total = regex::Regex::new(r#""elapsed_total":\{[^}]*\}"#).unwrap();
    let zero = r#""elapsed":{"secs":0,"nanos":0,"human":"0.000000s"}"#;
    let text = file.replace_all(text, zero);
    total
        .replace_all(&text, r#""elapsed_total":{}"#)
        .into_owned()
}

/// Stream `terms` over `docs` the way the drive does.
fn render(docs: &[(&str, &[u8])], terms: &[&str]) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let mut sink = JsonSink::new(&mut buf);
    // The drive's view of a document: BOM stripped, here and in the corpus
    // byte count.
    sink.corpus(
        docs.len(),
        docs.iter().map(|(_, r)| strip_bom(r).len() as u64).sum(),
    )
    .unwrap();
    for term in terms {
        sink.term_start(term).unwrap();
        let s = search(term, MATCHES, MatchMode::default());
        for (path, raw) in docs {
            if let Some(f) = s.doc(path, strip_bom(raw)).unwrap() {
                let (printed, stats) = (&f.printed, &f.stats);
                sink.doc(DocMatches {
                    path,
                    printed,
                    stats,
                })
                .unwrap();
            }
        }
        sink.term_end(&QueryReport {
            term: (*term).to_owned(),
            latency_us: 0,
            total_matches: 0,
            docs_matched: 0,
            counts_exact: true,
            leaves: Vec::new(),
        })
        .unwrap();
    }
    sink.finish().unwrap();
    blank_wall(&String::from_utf8(buf).unwrap())
}

fn stream(lines: &[&str]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

#[test]
fn begin_match_end_and_summary_match_rg() {
    // rg --json alpha a.txt sub/b.txt
    assert_eq!(
        render(&[A, B], &["alpha"]),
        stream(&[
            r#"{"type":"begin","data":{"path":{"text":"a.txt"}}}"#,
            r#"{"type":"match","data":{"path":{"text":"a.txt"},"lines":{"text":"alpha one\n"},"line_number":1,"absolute_offset":0,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5}]}}"#,
            r#"{"type":"match","data":{"path":{"text":"a.txt"},"lines":{"text":"alpha two\n"},"line_number":3,"absolute_offset":15,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5}]}}"#,
            r#"{"type":"match","data":{"path":{"text":"a.txt"},"lines":{"text":"alpha and alpha\n"},"line_number":7,"absolute_offset":44,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5},{"match":{"text":"alpha"},"start":10,"end":15}]}}"#,
            r#"{"type":"end","data":{"path":{"text":"a.txt"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0.000000s"},"searches":1,"searches_with_match":1,"bytes_searched":64,"bytes_printed":636,"matched_lines":3,"matches":4}}}"#,
            r#"{"type":"begin","data":{"path":{"text":"sub/b.txt"}}}"#,
            r#"{"type":"match","data":{"path":{"text":"sub/b.txt"},"lines":{"text":"alpha in sub\n"},"line_number":2,"absolute_offset":6,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5}]}}"#,
            r#"{"type":"end","data":{"path":{"text":"sub/b.txt"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0.000000s"},"searches":1,"searches_with_match":1,"bytes_searched":25,"bytes_printed":238,"matched_lines":1,"matches":1}}}"#,
            r#"{"data":{"elapsed_total":{},"stats":{"bytes_printed":874,"bytes_searched":89,"elapsed":{"human":"0.000000s","nanos":0,"secs":0},"matched_lines":4,"matches":5,"searches":2,"searches_with_match":2}},"type":"summary"}"#,
        ])
    );
}

#[test]
fn a_leading_bom_leaves_the_stream_unshifted() {
    // rg --json '^module' bom.c plain.c, where bom.c opens with EF BB BF:
    // line 1 text, the submatch columns and bytes_searched all exclude it.
    const BOM: (&str, &[u8]) = ("bom.c", b"\xef\xbb\xbfmodule Foo where\nsecond line\n");
    const PLAIN: (&str, &[u8]) = ("plain.c", b"module Bar where\n");
    assert_eq!(
        render(&[BOM, PLAIN], &["/^module/"]),
        stream(&[
            r#"{"type":"begin","data":{"path":{"text":"bom.c"}}}"#,
            r#"{"type":"match","data":{"path":{"text":"bom.c"},"lines":{"text":"module Foo where\n"},"line_number":1,"absolute_offset":0,"submatches":[{"match":{"text":"module"},"start":0,"end":6}]}}"#,
            r#"{"type":"end","data":{"path":{"text":"bom.c"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0.000000s"},"searches":1,"searches_with_match":1,"bytes_searched":29,"bytes_printed":235,"matched_lines":1,"matches":1}}}"#,
            r#"{"type":"begin","data":{"path":{"text":"plain.c"}}}"#,
            r#"{"type":"match","data":{"path":{"text":"plain.c"},"lines":{"text":"module Bar where\n"},"line_number":1,"absolute_offset":0,"submatches":[{"match":{"text":"module"},"start":0,"end":6}]}}"#,
            r#"{"type":"end","data":{"path":{"text":"plain.c"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0.000000s"},"searches":1,"searches_with_match":1,"bytes_searched":17,"bytes_printed":239,"matched_lines":1,"matches":1}}}"#,
            r#"{"data":{"elapsed_total":{},"stats":{"bytes_printed":474,"bytes_searched":46,"elapsed":{"human":"0.000000s","nanos":0,"secs":0},"matched_lines":2,"matches":2,"searches":2,"searches_with_match":2}},"type":"summary"}"#,
        ])
    );
}

#[test]
fn a_term_with_no_match_emits_only_the_summary() {
    // rg --json zzz xs.txt
    assert_eq!(
        render(&[("xs.txt", b"ab\n")], &["zzz"]),
        stream(&[
            r#"{"data":{"elapsed_total":{},"stats":{"bytes_printed":0,"bytes_searched":3,"elapsed":{"human":"0.000000s","nanos":0,"secs":0},"matched_lines":0,"matches":0,"searches":1,"searches_with_match":0}},"type":"summary"}"#,
        ])
    );
}

#[test]
fn several_terms_concatenate_streams() {
    let both = render(&[A, B], &["delta", "alpha"]);
    let first = render(&[A, B], &["delta"]);
    let second = render(&[A, B], &["alpha"]);
    assert_eq!(both, format!("{first}{second}"));
    assert_eq!(both.matches(r#""type":"summary""#).count(), 2);
}

#[test]
fn bytes_printed_counts_begin_and_match_events_only() {
    let got = render(&[A, B], &["alpha"]);
    let mut pending = 0u64;
    let mut ends = Vec::new();
    for line in got.lines() {
        if line.starts_with(r#"{"type":"end""#) {
            ends.push(pending);
            pending = 0;
        } else if line.starts_with(r#"{"data""#) {
            break;
        } else {
            pending += line.len() as u64 + 1;
        }
    }
    assert_eq!(ends, vec![636, 238]);
    for want in ends {
        assert!(got.contains(&format!(r#""bytes_printed":{want}"#)), "{got}");
    }
}

#[test]
fn summary_keys_are_sorted() {
    let got = render(&[A], &["alpha"]);
    let summary = got.lines().next_back().unwrap();
    assert!(summary.starts_with(r#"{"data":"#) && summary.ends_with(r#""type":"summary"}"#));
    let keys: Vec<&str> = [
        "bytes_printed",
        "bytes_searched",
        "elapsed",
        "matched_lines",
        "matches",
        "searches",
        "searches_with_match",
    ]
    .to_vec();
    let at: Vec<usize> = keys
        .iter()
        .map(|k| summary.find(&format!("\"{k}\":")).unwrap())
        .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{summary}");
}

#[test]
fn broken_pipe_is_not_an_error() {
    struct Hangup;
    impl Write for Hangup {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = JsonSink::new(Hangup);
    sink.corpus(1, 3).unwrap();
    sink.term_start("alpha").unwrap();
    let f = search("alpha", MATCHES, MatchMode::default());
    let f = f.doc(A.0, A.1).unwrap().unwrap();
    sink.doc(DocMatches {
        path: A.0,
        printed: &f.printed,
        stats: &f.stats,
    })
    .unwrap();
    sink.term_end(&QueryReport {
        term: "alpha".to_owned(),
        latency_us: 0,
        total_matches: 0,
        docs_matched: 0,
        counts_exact: true,
        leaves: Vec::new(),
    })
    .unwrap();
    sink.finish().unwrap();
}

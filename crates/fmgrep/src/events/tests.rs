use super::*;

use crate::matching::{LineMatcher, MatchMode, QueryOutput, strip_bom};

/// Every expectation is rg 15.2.0's stdout for the quoted command, with the
/// `elapsed` objects zeroed: a file's elapsed is always zero here and the
/// summary's is `latency_us`, pinned at 0 by [`render`]. Common flags:
/// `rg --no-ignore --no-require-git -a --sort path --json`.
const A: (&str, &[u8]) = (
    "a.txt",
    b"alpha one\nbeta\nalpha two\ndelta\nepsilon\nzeta\nalpha and alpha\neta\n",
);
const B: (&str, &[u8]) = ("sub/b.txt", b"first\nalpha in sub\nthird\n");

const MATCHES: QueryOutput = QueryOutput::Matches {
    before: 0,
    after: 0,
    line_numbers: true,
};

fn cfg() -> JsonConfig {
    JsonConfig {
        line_numbers: true,
        multiline: false,
    }
}

/// A `/.../`-wrapped term is a pattern, as on the CLI.
fn matcher_for(term: &str, mode: MatchMode) -> LineMatcher {
    match crate::regex_pattern(term) {
        Some(p) => LineMatcher::regex(p, None, mode).unwrap(),
        None => LineMatcher::literal(term.as_bytes(), mode).unwrap(),
    }
}

/// `elapsed_total` is the one wall clock in the stream; blank it so the rest
/// can be compared byte for byte.
fn blank_wall(text: &str) -> String {
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        match (line.find("\"elapsed_total\":{"), line.find("},\"stats\"")) {
            (Some(a), Some(b)) => {
                out.push_str(&line[..a]);
                out.push_str("\"elapsed_total\":{}");
                out.push_str(&line[b + 1..]);
            }
            _ => out.push_str(line),
        }
    }
    out
}

/// Stream `terms` over `docs` the way the drive does: real extraction, one
/// ascending `doc` call per matching document.
fn render(docs: &[(&str, &[u8])], terms: &[&str], output: QueryOutput, cfg: JsonConfig) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let mode = MatchMode {
        multiline: cfg.multiline,
        dotall: false,
        ignore_case: false,
    };
    let mut sink = JsonSink::new(&mut buf, cfg);
    // The drive's view of a document: BOM stripped, here and in the corpus
    // byte count.
    sink.corpus(
        docs.len(),
        docs.iter().map(|(_, r)| strip_bom(r).len() as u64).sum(),
    )
    .unwrap();
    for term in terms {
        sink.term_start(term).unwrap();
        let matcher = matcher_for(term, mode);
        for (path, raw) in docs {
            let raw = &strip_bom(raw);
            let outcome = matcher.scan(raw, output);
            if outcome.matched_lines == 0 {
                continue;
            }
            sink.doc(DocMatches {
                path,
                raw,
                outcome: &outcome,
            })
            .unwrap();
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
        render(&[A, B], &["alpha"], MATCHES, cfg()),
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
fn context_events_carry_empty_submatches() {
    // rg --json -C1 alpha a.txt
    assert_eq!(
        render(
            &[A],
            &["alpha"],
            QueryOutput::Matches {
                before: 1,
                after: 1,
                line_numbers: true
            },
            cfg()
        ),
        stream(&[
            r#"{"type":"begin","data":{"path":{"text":"a.txt"}}}"#,
            r#"{"type":"match","data":{"path":{"text":"a.txt"},"lines":{"text":"alpha one\n"},"line_number":1,"absolute_offset":0,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5}]}}"#,
            r#"{"type":"context","data":{"path":{"text":"a.txt"},"lines":{"text":"beta\n"},"line_number":2,"absolute_offset":10,"submatches":[]}}"#,
            r#"{"type":"match","data":{"path":{"text":"a.txt"},"lines":{"text":"alpha two\n"},"line_number":3,"absolute_offset":15,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5}]}}"#,
            r#"{"type":"context","data":{"path":{"text":"a.txt"},"lines":{"text":"delta\n"},"line_number":4,"absolute_offset":25,"submatches":[]}}"#,
            r#"{"type":"context","data":{"path":{"text":"a.txt"},"lines":{"text":"zeta\n"},"line_number":6,"absolute_offset":39,"submatches":[]}}"#,
            r#"{"type":"match","data":{"path":{"text":"a.txt"},"lines":{"text":"alpha and alpha\n"},"line_number":7,"absolute_offset":44,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5},{"match":{"text":"alpha"},"start":10,"end":15}]}}"#,
            r#"{"type":"context","data":{"path":{"text":"a.txt"},"lines":{"text":"eta\n"},"line_number":8,"absolute_offset":60,"submatches":[]}}"#,
            r#"{"type":"end","data":{"path":{"text":"a.txt"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0.000000s"},"searches":1,"searches_with_match":1,"bytes_searched":64,"bytes_printed":1160,"matched_lines":3,"matches":4}}}"#,
            r#"{"data":{"elapsed_total":{},"stats":{"bytes_printed":1160,"bytes_searched":64,"elapsed":{"human":"0.000000s","nanos":0,"secs":0},"matched_lines":3,"matches":4,"searches":1,"searches_with_match":1}},"type":"summary"}"#,
        ])
    );
}

#[test]
fn no_line_numbers_nulls_the_field() {
    // rg --json -N alpha sub/b.txt
    let got = render(
        &[B],
        &["alpha"],
        MATCHES,
        JsonConfig {
            line_numbers: false,
            multiline: false,
        },
    );
    assert!(
        got.contains(
            r#"{"type":"match","data":{"path":{"text":"sub/b.txt"},"lines":{"text":"alpha in sub\n"},"line_number":null,"absolute_offset":6,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5}]}}"#
        ),
        "{got}"
    );
    // rg's -N run reports 241 printed bytes, 3 more than the -n run's 238.
    assert!(got.contains(r#""bytes_printed":241"#), "{got}");
}

#[test]
fn non_utf8_line_is_base64_bytes() {
    // rg --json alpha e.bin, where e.bin is "alpha\xfb\xff\xbe\xef\xfe\n"
    assert_eq!(
        render(
            &[("e.bin", b"alpha\xfb\xff\xbe\xef\xfe\n")],
            &["alpha"],
            MATCHES,
            cfg()
        ),
        stream(&[
            r#"{"type":"begin","data":{"path":{"text":"e.bin"}}}"#,
            r#"{"type":"match","data":{"path":{"text":"e.bin"},"lines":{"bytes":"YWxwaGH7/77v/go="},"line_number":1,"absolute_offset":0,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5}]}}"#,
            r#"{"type":"end","data":{"path":{"text":"e.bin"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0.000000s"},"searches":1,"searches_with_match":1,"bytes_searched":11,"bytes_printed":233,"matched_lines":1,"matches":1}}}"#,
            r#"{"data":{"elapsed_total":{},"stats":{"bytes_printed":233,"bytes_searched":11,"elapsed":{"human":"0.000000s","nanos":0,"secs":0},"matched_lines":1,"matches":1,"searches":1,"searches_with_match":1}},"type":"summary"}"#,
        ])
    );
}

#[test]
fn crlf_and_eof_lines_keep_their_terminator() {
    // rg --json alpha crlf.txt, then rg --json alpha noeol.txt
    let crlf = render(
        &[("crlf.txt", b"x\r\ny\r\nalpha\r\n")],
        &["alpha"],
        MATCHES,
        cfg(),
    );
    assert!(
        crlf.contains(r#""lines":{"text":"alpha\r\n"},"line_number":3,"absolute_offset":6"#),
        "{crlf}"
    );
    assert!(crlf.contains(r#""bytes_printed":231"#), "{crlf}");
    let noeol = render(
        &[("noeol.txt", b"alpha one\nalpha")],
        &["alpha"],
        MATCHES,
        cfg(),
    );
    assert!(
        noeol.contains(r#""lines":{"text":"alpha"},"line_number":2,"absolute_offset":10"#),
        "{noeol}"
    );
    assert!(noeol.contains(r#""bytes_printed":411"#), "{noeol}");
}

#[test]
fn nullable_pattern_emits_every_empty_submatch() {
    // rg --json 'x*' xs.txt, where xs.txt is "ab\n"
    assert_eq!(
        render(&[("xs.txt", b"ab\n")], &["/x*/"], MATCHES, cfg()),
        stream(&[
            r#"{"type":"begin","data":{"path":{"text":"xs.txt"}}}"#,
            r#"{"type":"match","data":{"path":{"text":"xs.txt"},"lines":{"text":"ab\n"},"line_number":1,"absolute_offset":0,"submatches":[{"match":{"text":""},"start":0,"end":0},{"match":{"text":""},"start":1,"end":1},{"match":{"text":""},"start":2,"end":2}]}}"#,
            r#"{"type":"end","data":{"path":{"text":"xs.txt"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0.000000s"},"searches":1,"searches_with_match":1,"bytes_searched":3,"bytes_printed":297,"matched_lines":1,"matches":3}}}"#,
            r#"{"data":{"elapsed_total":{},"stats":{"bytes_printed":297,"bytes_searched":3,"elapsed":{"human":"0.000000s","nanos":0,"secs":0},"matched_lines":1,"matches":3,"searches":1,"searches_with_match":1}},"type":"summary"}"#,
        ])
    );
}

#[test]
fn spanless_match_at_an_unterminated_eof_is_a_match_event_with_no_submatch() {
    // rg --json '$' b.txt, where b.txt is "hello\nworld"
    assert_eq!(
        render(&[("b.txt", b"hello\nworld")], &["/$/"], MATCHES, cfg()),
        stream(&[
            r#"{"type":"begin","data":{"path":{"text":"b.txt"}}}"#,
            r#"{"type":"match","data":{"path":{"text":"b.txt"},"lines":{"text":"hello\n"},"line_number":1,"absolute_offset":0,"submatches":[{"match":{"text":""},"start":5,"end":5}]}}"#,
            r#"{"type":"match","data":{"path":{"text":"b.txt"},"lines":{"text":"world"},"line_number":2,"absolute_offset":6,"submatches":[]}}"#,
            r#"{"type":"end","data":{"path":{"text":"b.txt"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0.000000s"},"searches":1,"searches_with_match":1,"bytes_searched":11,"bytes_printed":345,"matched_lines":2,"matches":1}}}"#,
            r#"{"data":{"elapsed_total":{},"stats":{"bytes_printed":345,"bytes_searched":11,"elapsed":{"human":"0.000000s","nanos":0,"secs":0},"matched_lines":2,"matches":1,"searches":1,"searches_with_match":1}},"type":"summary"}"#,
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
        render(&[BOM, PLAIN], &["/^module/"], MATCHES, cfg()),
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
        render(&[("xs.txt", b"ab\n")], &["zzz"], MATCHES, cfg()),
        stream(&[
            r#"{"data":{"elapsed_total":{},"stats":{"bytes_printed":0,"bytes_searched":3,"elapsed":{"human":"0.000000s","nanos":0,"secs":0},"matched_lines":0,"matches":0,"searches":1,"searches_with_match":0}},"type":"summary"}"#,
        ])
    );
}

#[test]
fn several_terms_concatenate_streams() {
    let both = render(&[A, B], &["delta", "alpha"], MATCHES, cfg());
    let first = render(&[A, B], &["delta"], MATCHES, cfg());
    let second = render(&[A, B], &["alpha"], MATCHES, cfg());
    assert_eq!(both, format!("{first}{second}"));
    assert_eq!(both.matches(r#""type":"summary""#).count(), 2);
}

#[test]
fn bytes_printed_counts_begin_and_match_events_only() {
    let got = render(&[A, B], &["alpha"], MATCHES, cfg());
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
    let got = render(&[A], &["alpha"], MATCHES, cfg());
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
    let mut sink = JsonSink::new(Hangup, cfg());
    sink.corpus(1, 3).unwrap();
    sink.term_start("alpha").unwrap();
    let matcher = matcher_for("alpha", MatchMode::default());
    let outcome = matcher.scan(A.1, MATCHES);
    sink.doc(DocMatches {
        path: A.0,
        raw: A.1,
        outcome: &outcome,
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

/// rg `-U`.
fn multi_cfg() -> JsonConfig {
    JsonConfig {
        line_numbers: true,
        multiline: true,
    }
}

#[test]
fn a_multiline_block_is_one_match_event() {
    // rg --json -U 'a$' a.txt: the run of matched lines 6-8 is one event,
    // whose submatches are block-relative.
    assert_eq!(
        render(&[A], &["/a$/"], MATCHES, multi_cfg()),
        stream(&[
            r#"{"type":"begin","data":{"path":{"text":"a.txt"}}}"#,
            r#"{"type":"match","data":{"path":{"text":"a.txt"},"lines":{"text":"beta\n"},"line_number":2,"absolute_offset":10,"submatches":[{"match":{"text":"a"},"start":3,"end":4}]}}"#,
            r#"{"type":"match","data":{"path":{"text":"a.txt"},"lines":{"text":"delta\n"},"line_number":4,"absolute_offset":25,"submatches":[{"match":{"text":"a"},"start":4,"end":5}]}}"#,
            r#"{"type":"match","data":{"path":{"text":"a.txt"},"lines":{"text":"zeta\nalpha and alpha\neta\n"},"line_number":6,"absolute_offset":39,"submatches":[{"match":{"text":"a"},"start":3,"end":4},{"match":{"text":"a"},"start":19,"end":20},{"match":{"text":"a"},"start":23,"end":24}]}}"#,
            r#"{"type":"end","data":{"path":{"text":"a.txt"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0.000000s"},"searches":1,"searches_with_match":1,"bytes_searched":64,"bytes_printed":666,"matched_lines":5,"matches":5}}}"#,
            r#"{"data":{"elapsed_total":{},"stats":{"bytes_printed":666,"bytes_searched":64,"elapsed":{"human":"0.000000s","nanos":0,"secs":0},"matched_lines":5,"matches":5,"searches":1,"searches_with_match":1}},"type":"summary"}"#,
        ])
    );
}

#[test]
fn a_multiline_submatch_carries_its_terminators_and_context_stays_per_line() {
    // rg --json -U -C1 'one\nbeta' a.txt
    assert_eq!(
        render(
            &[A],
            &["/one\\nbeta/"],
            QueryOutput::Matches {
                before: 1,
                after: 1,
                line_numbers: true
            },
            multi_cfg()
        ),
        stream(&[
            r#"{"type":"begin","data":{"path":{"text":"a.txt"}}}"#,
            r#"{"type":"match","data":{"path":{"text":"a.txt"},"lines":{"text":"alpha one\nbeta\n"},"line_number":1,"absolute_offset":0,"submatches":[{"match":{"text":"one\nbeta"},"start":6,"end":14}]}}"#,
            r#"{"type":"context","data":{"path":{"text":"a.txt"},"lines":{"text":"alpha two\n"},"line_number":3,"absolute_offset":15,"submatches":[]}}"#,
            r#"{"type":"end","data":{"path":{"text":"a.txt"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0.000000s"},"searches":1,"searches_with_match":1,"bytes_searched":64,"bytes_printed":374,"matched_lines":2,"matches":1}}}"#,
            r#"{"data":{"elapsed_total":{},"stats":{"bytes_printed":374,"bytes_searched":64,"elapsed":{"human":"0.000000s","nanos":0,"secs":0},"matched_lines":2,"matches":1,"searches":1,"searches_with_match":1}},"type":"summary"}"#,
        ])
    );
}

#[test]
fn multiline_blocks_merge_without_line_numbers() {
    let unnumbered = || JsonConfig {
        line_numbers: false,
        multiline: true,
    };
    let off = QueryOutput::Matches {
        before: 0,
        after: 0,
        line_numbers: false,
    };
    // Lines 1-4 of A hold "a": one block.
    for term in ["a", "alpha"] {
        assert_eq!(
            render(&[A, B], &[term], off, unnumbered()),
            render(&[A, B], &[term], MATCHES, unnumbered()),
            "{term}"
        );
    }
}

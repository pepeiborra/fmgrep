use super::*;

const MATCHES: QueryOutput = QueryOutput::Matches {
    before: 0,
    after: 0,
};

/// `(line, text, spans)` per emitted line.
type Emitted = Vec<(u64, String, Vec<(u32, u32)>)>;

fn lit(term: &str) -> LineMatcher {
    LineMatcher::literal(term.as_bytes(), MatchMode::default()).unwrap()
}

fn re(src: &str) -> LineMatcher {
    LineMatcher::regex(src, None, MatchMode::default()).unwrap()
}

/// rg `-U`.
const MULTI: MatchMode = MatchMode {
    multiline: true,
    dotall: false,
    ignore_case: false,
};

fn multi(src: &str) -> LineMatcher {
    LineMatcher::regex(src, None, MULTI).unwrap()
}

fn hits(m: &LineMatcher, raw: &str) -> Emitted {
    hits_out(m, raw, MATCHES)
}

fn hits_out(m: &LineMatcher, raw: &str, out: QueryOutput) -> Emitted {
    let bytes = raw.as_bytes();
    m.scan(bytes, out)
        .lines
        .into_iter()
        .map(|l| {
            let text = String::from_utf8_lossy(&bytes[l.start..l.end]).into_owned();
            (l.line, text, l.spans)
        })
        .collect()
}

fn counts(m: &LineMatcher, raw: &str) -> (u64, u64) {
    let o = m.scan(raw.as_bytes(), QueryOutput::CountLines);
    (o.matched_lines, o.matched_spans)
}

fn count_matches(m: &LineMatcher, raw: &str) -> u64 {
    m.scan(raw.as_bytes(), QueryOutput::CountMatches)
        .count_matches()
}

#[test]
fn line_text_excludes_newline_and_keeps_carriage_return() {
    assert_eq!(
        hits(&lit("x"), "x\r\ny\r\n"),
        vec![(1, "x\r".to_string(), vec![(0, 1)])]
    );
    // No --crlf: `$` does not match before the carriage return.
    assert!(hits(&re("x$"), "x\r\n").is_empty());
    assert_eq!(hits(&re("x\r$"), "x\r\n").len(), 1);
}

#[test]
fn literal_spans_group_by_line_with_multiple_matches_per_line() {
    assert_eq!(
        hits(&lit("ab"), "ab cab\nno\nxab\n"),
        vec![
            (1, "ab cab".to_string(), vec![(0, 2), (4, 2)]),
            (3, "xab".to_string(), vec![(1, 2)]),
        ]
    );
    assert_eq!(counts(&lit("ab"), "ab cab\nno\nxab\n"), (2, 3));
}

#[test]
fn adjacent_and_repeated_literal_matches_are_not_merged() {
    // Non-overlapping like rg: `aa` in `aaaa` is two spans, kept separate.
    assert_eq!(
        hits(&lit("aa"), "aaaa\n"),
        vec![(1, "aaaa".to_string(), vec![(0, 2), (2, 2)])]
    );
}

#[test]
fn match_at_eof_without_trailing_newline() {
    assert_eq!(
        hits(&lit("tail"), "head\ntail"),
        vec![(2, "tail".to_string(), vec![(0, 4)])]
    );
    assert_eq!(hits(&re("tail\\z"), "head\ntail").len(), 1);
}

#[test]
fn empty_doc_and_empty_line_yield_no_hits() {
    assert!(hits(&lit("a"), "").is_empty());
    assert!(hits(&re("a"), "").is_empty());
    assert!(hits(&lit("a"), "\n\n").is_empty());
    // An empty literal term matches nothing, like the count contract.
    assert!(hits(&lit(""), "abc\n").is_empty());
}

#[test]
fn nullable_pattern_matches_every_line_and_drops_the_buffer_end_span() {
    // Measured against rg 15.2.0 `--count-matches 'x*'`.
    for (raw, want) in [
        ("abc\n", 4u64),
        ("abc", 3),
        ("ab\n", 3),
        ("ab", 2),
        ("a\nb\n", 4),
        ("a\nb", 3),
    ] {
        assert_eq!(count_matches(&re("x*"), raw), want, "raw={raw:?}");
        assert_eq!(counts(&re("x*"), raw).1, want, "raw={raw:?}");
    }
}

#[test]
fn zero_width_match_at_an_unterminated_eof_matches_the_line_without_a_span() {
    // rg 15.2.0 on "hello\nworld": `$` reports line 2 with an empty submatch
    // list, `-c` 2 and `--count-matches` 2, but `--json` stats.matches 1.
    let raw = "hello\nworld";
    for src in ["$", "c*$", "\\z"] {
        assert_eq!(
            hits(&re(src), raw),
            vec![
                (1, "hello".to_string(), vec![(5, 0)]),
                (2, "world".to_string(), vec![]),
            ],
            "src={src:?}"
        );
        assert_eq!(counts(&re(src), raw), (2, 1), "src={src:?}");
        assert_eq!(count_matches(&re(src), raw), 2, "src={src:?}");
        // Terminated, the same pattern keeps every span.
        assert_eq!(counts(&re(src), "hello\nworld\n"), (2, 2), "src={src:?}");
        assert_eq!(count_matches(&re(src), "hello\nworld\n"), 2, "src={src:?}");
    }
    // rg 15.2.0 `x$` on "abc" exits 1: the anchor still needs its literal.
    assert!(hits(&re("x$"), "abc").is_empty());
    // A spanless line is a match for `-l` too.
    let o = re("$").scan(b"world", QueryOutput::FilesWithMatches);
    assert_eq!(
        (o.matched_lines, o.matched_spans, o.count_matches()),
        (1, 0, 1)
    );
}

#[test]
fn a_leading_bom_is_not_part_of_line_1() {
    // rg 15.2.0 strips EF BB BF before searching: a BOM-only file has no
    // line, and line 1's text and columns start after the BOM.
    assert!(strip_bom(b"\xef\xbb\xbf").is_empty());
    assert_eq!(strip_bom(b"\xef\xbb\xbf\n"), b"\n");
    // Only a LEADING BOM, and only once.
    assert_eq!(strip_bom(b"a\xef\xbb\xbf"), b"a\xef\xbb\xbf");
    assert_eq!(strip_bom(b"\xef\xbb\xbf\xef\xbb\xbfx").len(), 4);
    let one = strip_bom(b"\xef\xbb\xbfmodule Foo where\nsecond line\n");
    let one = std::str::from_utf8(one).unwrap();
    assert_eq!(
        hits(&re("^module"), one),
        vec![(1, "module Foo where".to_string(), vec![(0, 6)])]
    );
    let crlf = strip_bom(b"\xef\xbb\xbfalpha\r\nbeta\r\n");
    let crlf = std::str::from_utf8(crlf).unwrap();
    assert_eq!(
        hits(&lit("alpha"), crlf),
        vec![(1, "alpha\r".to_string(), vec![(0, 5)])]
    );
}

#[test]
fn newline_bearing_pattern_and_term_are_refused_with_rgs_wording() {
    for src in ["a\nb", "[\n]", "a\\nb", "\\x0a"] {
        let err = LineMatcher::regex(src, None, MatchMode::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains(NEWLINE_BANNED), "src={src:?} got: {err}");
    }
    let err = LineMatcher::literal(b"a\nb", MatchMode::default()).unwrap_err();
    assert!(err.contains(NEWLINE_BANNED), "got: {err}");
}

#[test]
fn class_keeps_newline_free_members() {
    // `[\nc]` compiles and matches `c`, never a terminator.
    assert_eq!(
        hits(&re("[\\nc]"), "c\nd\n"),
        vec![(1, "c".to_string(), vec![(0, 1)])]
    );
}

#[test]
fn no_construct_crosses_a_terminator() {
    for src in ["a\\sb", "(?s)a.b", "a[^q]b"] {
        assert!(hits(&re(src), "a\nb").is_empty(), "src={src:?}");
    }
}

#[test]
fn anchors_are_line_anchors() {
    let doc = "def\nabc\nghi\n";
    for (src, line) in [("\\Aabc", 2u64), ("abc\\z", 2), ("^abc", 2), ("abc$", 2)] {
        let got = hits(&re(src), doc);
        assert_eq!(got.len(), 1, "src={src:?} got {got:?}");
        assert_eq!(got[0].0, line, "src={src:?}");
    }
}

#[test]
fn anchor_prefilter_agrees_with_the_unfiltered_scan() {
    let mut doc = String::new();
    for i in 0..200 {
        doc.push_str(match i % 5 {
            0 => "printk(x)\n",
            1 => "nothing here\n",
            2 => "printk(a) printk(bb)\n",
            3 => "\n",
            _ => "prefix printk\n",
        });
    }
    for src in ["printk\\([^)]*\\)", "printk", "print(k|f)"] {
        let anchored = LineMatcher::regex(src, Some(b"print"), MatchMode::default()).unwrap();
        let plain = re(src);
        assert_eq!(hits(&anchored, &doc), hits(&plain, &doc), "src={src:?}");
        assert_eq!(counts(&anchored, &doc), counts(&plain, &doc), "src={src:?}");
    }
}

#[test]
fn context_lines_are_captured_and_deduplicated() {
    let doc = "1\n2\nhit\n4\n5\nhit\n7\n";
    let got = hits_out(
        &lit("hit"),
        doc,
        QueryOutput::Matches {
            before: 1,
            after: 1,
        },
    );
    let want: Vec<u64> = vec![2, 3, 4, 5, 6, 7];
    assert_eq!(got.iter().map(|h| h.0).collect::<Vec<_>>(), want);
    // Context lines carry text but no spans; matched lines carry both.
    assert!(got[0].2.is_empty() && got[1].2 == vec![(0, 3)]);
    // Overlapping windows emit each line once.
    let wide = hits_out(
        &lit("hit"),
        doc,
        QueryOutput::Matches {
            before: 3,
            after: 3,
        },
    );
    assert_eq!(wide.len(), 7);
    assert_eq!(wide.iter().filter(|h| !h.2.is_empty()).count(), 2);
}

#[test]
fn files_with_matches_stops_at_the_first_span() {
    let o = lit("a").scan(b"a a\na\n", QueryOutput::FilesWithMatches);
    assert_eq!((o.matched_lines, o.matched_spans), (1, 1));
    assert!(o.lines.is_empty());
}

#[test]
fn literal_exactness_predicate() {
    for term in [&b""[..], b"\n", b"a\nb", b"aa", b"abab"] {
        assert!(
            !literal_counts_are_exact(term),
            "term={:?}",
            String::from_utf8_lossy(term)
        );
    }
    for term in [&b"printk"[..], b"a", b"ab"] {
        assert!(
            literal_counts_are_exact(term),
            "term={:?}",
            String::from_utf8_lossy(term)
        );
    }
}

#[test]
fn self_overlapping_detects_borders() {
    for term in ["aa", "aba", "abab"] {
        assert!(self_overlapping(term.as_bytes()), "term={term}");
    }
    for term in ["", "a", "ab", "abc", "printk"] {
        assert!(!self_overlapping(term.as_bytes()), "term={term}");
    }
}

#[test]
fn a_multiline_match_marks_every_line_it_touches() {
    // rg -Un 'one\nbeta'
    assert_eq!(
        hits(&multi("one\\nbeta"), "alpha one\nbeta two\ngamma\n"),
        vec![
            (1, "alpha one".to_string(), vec![(6, 3)]),
            (2, "beta two".to_string(), vec![(0, 4)]),
        ]
    );
    // rg -Un 'd\n': the terminator ends the match, so the next line is
    // untouched.
    assert_eq!(
        hits(&multi("d\\n"), "ab\ncd\nef\n"),
        vec![(2, "cd".to_string(), vec![(1, 1)])]
    );
    // rg -Un '\nc': line 1 is matched through its terminator alone and
    // carries no span, which is why rg leaves it uncoloured and drops it
    // from -o.
    assert_eq!(
        hits(&multi("\\nc"), "ab\ncd\nef\n"),
        vec![
            (1, "ab".to_string(), vec![]),
            (2, "cd".to_string(), vec![(0, 1)]),
        ]
    );
    // An empty line inside the match is matched too.
    assert_eq!(
        hits(&multi("a\\n\\nb"), "a\n\nb\n"),
        vec![
            (1, "a".to_string(), vec![(0, 1)]),
            (2, String::new(), vec![]),
            (3, "b".to_string(), vec![(0, 1)]),
        ]
    );
}

#[test]
fn multiline_counts_matches_where_the_line_scan_counts_lines() {
    // rg -Uc 'b\nc|f\ng' = 2, --count-matches = 2, --json matched_lines 4.
    let o = multi("b\\nc|f\\ng").scan(b"ab\ncd\nef\ngh\n", QueryOutput::CountLines);
    assert_eq!((o.matched_lines, o.matched_spans), (4, 2));
    assert_eq!((o.count_lines(), o.count_matches()), (2, 2));
    // The line scan, which -U keeps for a term that cannot match a
    // terminator, still counts lines under -c.
    let o = lit("x").scan(b"xx yy\nzz\n", QueryOutput::CountLines);
    assert_eq!((o.count_lines(), o.count_matches()), (1, 2));
}

#[test]
fn a_zero_width_match_at_the_document_end_is_dropped() {
    // rg -Un '\z' on an unterminated document: the last line matches with
    // no submatch.
    assert_eq!(
        hits(&multi("\\z"), "head\nalpha last"),
        vec![(2, "alpha last".to_string(), vec![])]
    );
    // rg -U '\z' on a terminated one matches nothing at all.
    assert!(hits(&multi("\\z"), "head\nalpha last\n").is_empty());
    // It counts nowhere, but still marks its line: rg -Uc '(?s)$' = 1 with
    // two matched lines.
    let o = multi("(?s)$").scan(b"head\nalpha last", QueryOutput::CountLines);
    assert_eq!(
        (o.matched_lines, o.count_lines(), o.count_matches()),
        (2, 1, 1)
    );
}

#[test]
fn only_terms_that_can_match_a_terminator_leave_the_line_scan() {
    // grep-regex's test, which decides rg's core: a `\n` in a literal or
    // class, or any anchor except the word boundaries.
    for src in [
        "a\\nb", "[^a]", "(?s).", "a\\sb", "^a", "a$", "\\Aa", "a\\z",
    ] {
        let m = LineMatcher::regex(src, None, MULTI).unwrap();
        assert!(matches!(m, LineMatcher::MultiRegex { .. }), "src={src:?}");
    }
    for src in ["a", "a.b", "\\ba\\b", "[abc]+", "a|bc"] {
        let m = LineMatcher::regex(src, None, MULTI).unwrap();
        assert!(matches!(m, LineMatcher::Regex { .. }), "src={src:?}");
    }
    // `.` only reaches the terminator under --multiline-dotall.
    let dotall = MatchMode {
        multiline: true,
        dotall: true,
        ignore_case: false,
    };
    let m = LineMatcher::regex("a.b", None, dotall).unwrap();
    assert!(matches!(m, LineMatcher::MultiRegex { .. }));
    assert_eq!(hits(&m, "a\nb\n").len(), 2);
    // Without -U nothing leaves the line scan.
    let m = LineMatcher::regex("^a$", None, MatchMode::default()).unwrap();
    assert!(matches!(m, LineMatcher::Regex { .. }));
}

#[test]
fn a_newline_bearing_literal_is_a_document_scan_under_u() {
    let m = LineMatcher::literal(b"a\nb", MULTI).unwrap();
    assert!(matches!(m, LineMatcher::MultiLiteral(_)));
    assert_eq!(
        hits(&m, "xa\nbc\n"),
        vec![
            (1, "xa".to_string(), vec![(1, 1)]),
            (2, "bc".to_string(), vec![(0, 1)]),
        ]
    );
    // memmem is non-overlapping, like rg.
    let o = LineMatcher::literal(b"a\na", MULTI)
        .unwrap()
        .scan(b"a\na\na\na\n", QueryOutput::CountMatches);
    assert_eq!(o.count_matches(), 2);
}

#[test]
fn the_required_literal_prunes_whole_documents_under_u() {
    let anchored = LineMatcher::regex("print\\nk", Some(b"print"), MULTI).unwrap();
    assert!(hits(&anchored, "nothing here\n").is_empty());
    assert_eq!(hits(&anchored, "print\nk\n").len(), 2);
}

/// `-i` and `-U` compose: the multiline test reads the folded pattern, so a
/// term takes the same core it would take case-sensitively.
#[test]
fn ignore_case_composes_with_multiline() {
    const CI_MULTI: MatchMode = MatchMode {
        multiline: true,
        dotall: false,
        ignore_case: true,
    };
    let m = LineMatcher::regex("alpha\nbeta", None, CI_MULTI).unwrap();
    assert!(matches!(m, LineMatcher::MultiRegex { .. }));
    assert_eq!(
        hits(&m, "ALPHA\nBeta\n"),
        vec![
            (1, "ALPHA".to_string(), vec![(0, 5)]),
            (2, "Beta".to_string(), vec![(0, 4)]),
        ]
    );
    // A folded term that cannot hold a terminator keeps the line scan.
    let plain = LineMatcher::regex("alpha", None, CI_MULTI).unwrap();
    assert!(matches!(plain, LineMatcher::Regex { .. }));
    // The literal path folds nothing, so `-i` reaches it only as a pattern.
    let ci_lit = LineMatcher::regex("ALPHA\nBETA", None, CI_MULTI).unwrap();
    assert_eq!(count_matches(&ci_lit, "alpha\nbeta\n"), 1);
}

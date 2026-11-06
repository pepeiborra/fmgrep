use super::*;

const NEWLINE_BANNED: &str = "the literal \"\\n\" is not allowed in a regex";

fn lines(path: bool, before: usize, after: usize) -> QueryOutput {
    QueryOutput::Matches(Print {
        before,
        after,
        line_numbers: true,
        path,
        ..Print::default()
    })
}

fn printed(term: &str, output: QueryOutput, path: &str, doc: &[u8]) -> String {
    let m = matcher(term, None, MatchMode::default()).unwrap().unwrap();
    let found = DocSearch::new(m, output, MatchMode::default()).doc(path, doc);
    String::from_utf8(found.unwrap().unwrap().printed).unwrap()
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
    let doc = strip_bom(b"\xef\xbb\xbfalpha\r\nbeta\r\n");
    assert_eq!(
        printed("alpha", lines(false, 0, 0), "a", doc),
        "1:alpha\r\n"
    );
}

#[test]
fn newline_bearing_pattern_and_term_are_refused_with_rgs_wording() {
    for src in ["a\nb", "[\n]", "a\\nb", "\\x0a"] {
        let err = matcher("", Some(src), MatchMode::default()).unwrap_err();
        assert!(err.contains(NEWLINE_BANNED), "src={src:?} got: {err}");
    }
    let err = matcher("a\nb", None, MatchMode::default()).unwrap_err();
    assert!(err.contains(NEWLINE_BANNED), "got: {err}");
}

#[test]
fn a_literal_term_matches_its_bytes() {
    let doc = b"{-# LANGUAGE\na.c\nabc\n";
    assert_eq!(
        printed("{-# LANG", lines(false, 0, 0), "", doc),
        "1:{-# LANGUAGE\n"
    );
    assert_eq!(printed("a.c", lines(false, 0, 0), "", doc), "2:a.c\n");
}

#[test]
fn a_per_line_path_is_printed_as_given() {
    // Never resolved: the path need not exist.
    let doc = b"x alpha\nbeta\n";
    assert_eq!(
        printed("alpha", lines(true, 0, 1), "no/./such/../a.txt", doc),
        "no/./such/../a.txt:1:x alpha\nno/./such/../a.txt-2-beta\n"
    );
}

#[test]
fn a_reused_printer_separates_nothing_itself() {
    // Blank lines and `--` between files are the caller's.
    let m = matcher("alpha", None, MatchMode::default())
        .unwrap()
        .unwrap();
    let search = DocSearch::new(m, lines(true, 1, 0), MatchMode::default());
    for path in ["a", "b"] {
        let found = search.doc(path, b"x\nalpha\n").unwrap().unwrap();
        let want = format!("{path}-1-x\n{path}:2:alpha\n");
        assert_eq!(String::from_utf8(found.printed).unwrap(), want);
    }
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

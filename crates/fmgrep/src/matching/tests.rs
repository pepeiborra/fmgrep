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

/// rg's colours: the line number, a match.
const LINE: &str = "\x1b[0m\x1b[32m";
const MATCH: &str = "\x1b[0m\x1b[1m\x1b[31m";
const RESET: &str = "\x1b[0m";

/// A file of `n` lines `hit <i>`, and between them, when `gaps`, a line
/// `-` that does not match.
fn hits(n: u64, gaps: bool) -> Vec<u8> {
    let gap = if gaps { "-\n" } else { "" };
    (1..=n)
        .map(|i| format!("hit {i}\n{gap}"))
        .collect::<String>()
        .into_bytes()
}

fn found(term: &str, output: QueryOutput, mode: MatchMode, doc: &[u8]) -> Found {
    let m = matcher(term, None, mode).unwrap().unwrap();
    DocSearch::new(m, output, mode)
        .doc("", doc)
        .unwrap()
        .unwrap()
}

fn colored(print: Print) -> QueryOutput {
    QueryOutput::Matches(Print {
        line_numbers: true,
        color: true,
        ..print
    })
}

#[test]
fn matches_past_the_first_colored_lines_print_without_colour() {
    let n = COLORED_LINES + 3;
    let line = |i: u64| match i <= COLORED_LINES {
        true => format!("{LINE}{i}{RESET}:{MATCH}hit{RESET} {i}\n"),
        false => format!("{LINE}{i}{RESET}:hit {i}\n"),
    };
    let want: String = (1..=n).map(line).collect();
    let got = found(
        "hit",
        colored(Print::default()),
        MatchMode::default(),
        &hits(n, false),
    );
    assert_eq!(String::from_utf8(got.printed).unwrap(), want);
    assert_eq!(got.count, n);
    // With --stats: the same bytes, and every match counted.
    let print = Print {
        stats: true,
        ..Print::default()
    };
    let got = found("hit", colored(print), MatchMode::default(), &hits(n, false));
    assert_eq!(String::from_utf8(got.printed).unwrap(), want);
    assert_eq!((got.count, got.stats.matched_lines()), (n, n));
    // Exactly the bound: every match in colour.
    let got = found(
        "hit",
        colored(Print::default()),
        MatchMode::default(),
        &hits(COLORED_LINES, false),
    );
    assert_eq!(
        String::from_utf8(got.printed).unwrap(),
        (1..=COLORED_LINES).map(line).collect::<String>()
    );
}

#[test]
fn context_lines_and_breaks_print_alike_past_the_bound() {
    // -A1 over hit / - lines: every `-` is context, no `--` break; -B0 -A0
    // over the same file breaks nothing either. Past the bound only the
    // match colour goes.
    let n = COLORED_LINES + 2;
    let line = |i: u64| {
        let hit = match i <= COLORED_LINES {
            true => format!("{MATCH}hit{RESET}"),
            false => "hit".to_string(),
        };
        format!(
            "{LINE}{}{RESET}:{hit} {i}\n{LINE}{}{RESET}--\n",
            2 * i - 1,
            2 * i
        )
    };
    let print = Print {
        after: 1,
        ..Print::default()
    };
    let got = found("hit", colored(print), MatchMode::default(), &hits(n, true));
    assert_eq!(
        String::from_utf8(got.printed).unwrap(),
        (1..=n).map(line).collect::<String>()
    );
    // Without context, a `--` between the groups of matched lines.
    let doc: Vec<u8> = (1..=n)
        .flat_map(|i| format!("hit {i}\n\n\n").into_bytes())
        .collect();
    let print = Print {
        after: 0,
        before: 0,
        ..Print::default()
    };
    let got = String::from_utf8(found("hit", colored(print), MatchMode::default(), &doc).printed)
        .unwrap();
    assert_eq!(got.lines().count() as u64, n);
    assert_eq!(got.matches(MATCH).count() as u64, COLORED_LINES);
}

#[test]
fn only_matching_and_no_colour_are_unbounded() {
    let n = COLORED_LINES + 3;
    // -o colours every match: each is found to be printed.
    let print = Print {
        only_matching: true,
        ..Print::default()
    };
    let got = found("hit", colored(print), MatchMode::default(), &hits(n, false));
    let got = String::from_utf8(got.printed).unwrap();
    assert_eq!(got.matches(MATCH).count() as u64, n);
    // Without colour nothing changes at the bound.
    let plain = QueryOutput::Matches(Print {
        line_numbers: true,
        ..Print::default()
    });
    let got = found("hit", plain, MatchMode::default(), &hits(n, false));
    assert_eq!(
        String::from_utf8(got.printed).unwrap(),
        (1..=n)
            .map(|i| format!("{i}:hit {i}\n"))
            .collect::<String>()
    );
}

#[test]
fn a_multiline_match_counts_its_lines_toward_the_bound() {
    // -U: each match `hit <i>\n-` spans two lines, and a blank line keeps
    // the matches apart (adjacent ones print as one block). The block that
    // starts below the bound keeps its colour.
    let mode = MatchMode {
        multiline: true,
        ..MatchMode::default()
    };
    let n = COLORED_LINES / 2 + 2;
    let doc: String = (1..=n).map(|i| format!("hit {i}\n-\n\n")).collect();
    let m = matcher("", Some(r"hit \d+\n-"), mode).unwrap().unwrap();
    let got = DocSearch::new(m, colored(Print::default()), mode)
        .doc("", doc.as_bytes())
        .unwrap()
        .unwrap();
    let text = String::from_utf8(got.printed).unwrap();
    // Two coloured pieces a match (one per line), for the first
    // COLORED_LINES / 2 matches.
    assert_eq!(text.matches(MATCH).count() as u64, COLORED_LINES);
    assert_eq!(got.count, 2 * n);
    assert!(
        text.ends_with(&format!("{LINE}{}{RESET}:-\n", 3 * n - 1)),
        "{text}"
    );
    // Matches on adjacent lines print as one block, which starts at line
    // 1: it keeps its colour whole.
    let n = COLORED_LINES + 3;
    let m = matcher("", Some(r"\d+$"), mode).unwrap().unwrap();
    let got = DocSearch::new(m, colored(Print::default()), mode)
        .doc("", &hits(n, false))
        .unwrap()
        .unwrap();
    let text = String::from_utf8(got.printed).unwrap();
    assert_eq!(text.matches(MATCH).count() as u64, n);
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

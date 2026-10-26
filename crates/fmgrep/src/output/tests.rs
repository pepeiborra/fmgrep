use super::*;

use crate::matching::{LineMatcher, MatchMode, QueryOutput, strip_bom};

/// Every expectation is rg 15.2.0's stdout for the quoted command, with the
/// directory prefix rg echoes stripped: our paths are manifest-relative.
/// Common flags: `rg --no-ignore --no-require-git -a --sort path`.
const FIXTURE: [(&str, &[u8]); 2] = [
    (
        "a.txt",
        b"alpha one\nbeta\nalpha two\ndelta\nepsilon\nzeta\nalpha and alpha\neta\n",
    ),
    ("sub/b.txt", b"first\nalpha in sub\nthird\n"),
];

const MATCHES: QueryOutput = QueryOutput::Matches {
    before: 0,
    after: 0,
};

fn cfg(format: Format) -> OutputConfig {
    OutputConfig {
        format,
        heading: true,
        line_numbers: true,
        context: false,
        multiline: false,
        color: false,
        banner: false,
    }
}

/// A `/.../`-wrapped term is a pattern, as on the CLI.
fn matcher_for(term: &str, mode: MatchMode) -> LineMatcher {
    match crate::regex_pattern(term) {
        Some(p) => LineMatcher::regex(p, None, mode).unwrap(),
        None => LineMatcher::literal(term.as_bytes(), mode).unwrap(),
    }
}

/// Render `terms` over `docs` the way the drive does: real extraction, one
/// ascending `doc` call per matching document.
fn render(
    cfg: OutputConfig,
    output: QueryOutput,
    docs: &[(&str, &[u8])],
    terms: &[&str],
) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    let mut w = MatchWriter::new(&mut buf, cfg);
    for term in terms {
        w.term_start(term).unwrap();
        let matcher = matcher_for(
            term,
            MatchMode {
                multiline: cfg.multiline,
                dotall: false,
                ignore_case: false,
            },
        );
        for (path, raw) in docs {
            // The drive's view of a document: BOM stripped.
            let raw = &strip_bom(raw);
            let outcome = matcher.scan(raw, output);
            if outcome.matched_lines == 0 {
                continue;
            }
            w.doc(DocMatches {
                path,
                raw,
                outcome: &outcome,
            })
            .unwrap();
        }
    }
    w.finish().unwrap();
    buf
}

fn alpha(cfg: OutputConfig, output: QueryOutput) -> String {
    String::from_utf8(render(cfg, output, &FIXTURE, &["alpha"])).unwrap()
}

fn context(before: usize, after: usize) -> QueryOutput {
    QueryOutput::Matches { before, after }
}

fn with_context(format: Format, heading: bool, on: bool) -> OutputConfig {
    OutputConfig {
        heading,
        context: on,
        ..cfg(format)
    }
}

#[test]
fn grouped_default_matches_rg() {
    // rg --heading -n alpha
    assert_eq!(
        alpha(cfg(Format::Lines), MATCHES),
        "a.txt\n1:alpha one\n3:alpha two\n7:alpha and alpha\n\nsub/b.txt\n2:alpha in sub\n"
    );
}

#[test]
fn no_heading_matches_rg() {
    // rg --no-heading -n alpha
    let c = OutputConfig {
        heading: false,
        ..cfg(Format::Lines)
    };
    assert_eq!(
        alpha(c, MATCHES),
        "a.txt:1:alpha one\na.txt:3:alpha two\na.txt:7:alpha and alpha\nsub/b.txt:2:alpha in sub\n"
    );
}

#[test]
fn no_line_numbers_drops_the_field() {
    // rg --heading -N alpha
    let c = OutputConfig {
        line_numbers: false,
        ..cfg(Format::Lines)
    };
    assert_eq!(
        alpha(c, MATCHES),
        "a.txt\nalpha one\nalpha two\nalpha and alpha\n\nsub/b.txt\nalpha in sub\n"
    );
    // rg --no-heading -N alpha
    let c = OutputConfig {
        heading: false,
        ..c
    };
    assert_eq!(
        alpha(c, MATCHES),
        "a.txt:alpha one\na.txt:alpha two\na.txt:alpha and alpha\nsub/b.txt:alpha in sub\n"
    );
}

#[test]
fn only_matching_prints_one_line_per_span() {
    // rg --heading -n -o alpha  (line 7 holds two matches)
    assert_eq!(
        alpha(cfg(Format::OnlyMatching), MATCHES),
        "a.txt\n1:alpha\n3:alpha\n7:alpha\n7:alpha\n\nsub/b.txt\n2:alpha\n"
    );
    // rg --no-heading -n -o alpha
    let c = OutputConfig {
        heading: false,
        ..cfg(Format::OnlyMatching)
    };
    assert_eq!(
        alpha(c, MATCHES),
        "a.txt:1:alpha\na.txt:3:alpha\na.txt:7:alpha\na.txt:7:alpha\nsub/b.txt:2:alpha\n"
    );
    // rg --heading -N -o alpha
    let c = OutputConfig {
        line_numbers: false,
        ..cfg(Format::OnlyMatching)
    };
    assert_eq!(
        alpha(c, MATCHES),
        "a.txt\nalpha\nalpha\nalpha\nalpha\n\nsub/b.txt\nalpha\n"
    );
}

#[test]
fn counts_modes_print_path_and_number() {
    // rg -c alpha
    assert_eq!(
        alpha(cfg(Format::CountLines), QueryOutput::CountLines),
        "a.txt:3\nsub/b.txt:1\n"
    );
    // rg --count-matches alpha
    assert_eq!(
        alpha(cfg(Format::CountMatches), QueryOutput::CountMatches),
        "a.txt:4\nsub/b.txt:1\n"
    );
    // rg -l alpha
    assert_eq!(
        alpha(cfg(Format::FilesWithMatches), QueryOutput::FilesWithMatches),
        "a.txt\nsub/b.txt\n"
    );
}

#[test]
fn context_separators_follow_rg() {
    // rg --heading -n -A1 alpha
    assert_eq!(
        alpha(with_context(Format::Lines, true, true), context(0, 1)),
        "a.txt\n1:alpha one\n2-beta\n3:alpha two\n4-delta\n--\n7:alpha and alpha\n8-eta\n\n\
         sub/b.txt\n2:alpha in sub\n3-third\n"
    );
    // rg --heading -n -B1 alpha
    assert_eq!(
        alpha(with_context(Format::Lines, true, true), context(1, 0)),
        "a.txt\n1:alpha one\n2-beta\n3:alpha two\n--\n6-zeta\n7:alpha and alpha\n\n\
         sub/b.txt\n1-first\n2:alpha in sub\n"
    );
    // rg --heading -n -C2 alpha  (every line covered: no separator at all)
    assert_eq!(
        alpha(with_context(Format::Lines, true, true), context(2, 2)),
        "a.txt\n1:alpha one\n2-beta\n3:alpha two\n4-delta\n5-epsilon\n6-zeta\n\
         7:alpha and alpha\n8-eta\n\nsub/b.txt\n1-first\n2:alpha in sub\n3-third\n"
    );
    // rg --no-heading -n -C1 alpha  (a `--` between files as well)
    assert_eq!(
        alpha(with_context(Format::Lines, false, true), context(1, 1)),
        "a.txt:1:alpha one\na.txt-2-beta\na.txt:3:alpha two\na.txt-4-delta\n--\n\
         a.txt-6-zeta\na.txt:7:alpha and alpha\na.txt-8-eta\n--\n\
         sub/b.txt-1-first\nsub/b.txt:2:alpha in sub\nsub/b.txt-3-third\n"
    );
    // rg --no-heading -n -A1 alpha
    assert_eq!(
        alpha(with_context(Format::Lines, false, true), context(0, 1)),
        "a.txt:1:alpha one\na.txt-2-beta\na.txt:3:alpha two\na.txt-4-delta\n--\n\
         a.txt:7:alpha and alpha\na.txt-8-eta\n--\n\
         sub/b.txt:2:alpha in sub\nsub/b.txt-3-third\n"
    );
    // rg --heading -n -o -C1 alpha: context lines print whole, matches alone.
    assert_eq!(
        alpha(
            with_context(Format::OnlyMatching, true, true),
            context(1, 1)
        ),
        "a.txt\n1:alpha\n2-beta\n3:alpha\n4-delta\n--\n6-zeta\n7:alpha\n7:alpha\n8-eta\n\n\
         sub/b.txt\n1-first\n2:alpha\n3-third\n"
    );
    // rg --heading -n -C0 alpha prints no `--` at all: the default output.
    assert_eq!(
        alpha(with_context(Format::Lines, true, false), context(0, 0)),
        alpha(cfg(Format::Lines), MATCHES)
    );
}

#[test]
fn color_palette_is_rgs() {
    // rg --heading -n --color always alpha
    let c = OutputConfig {
        color: true,
        ..cfg(Format::Lines)
    };
    assert_eq!(
        alpha(c, MATCHES),
        "\x1b[0m\x1b[35ma.txt\x1b[0m\n\
         \x1b[0m\x1b[32m1\x1b[0m:\x1b[0m\x1b[1m\x1b[31malpha\x1b[0m one\n\
         \x1b[0m\x1b[32m3\x1b[0m:\x1b[0m\x1b[1m\x1b[31malpha\x1b[0m two\n\
         \x1b[0m\x1b[32m7\x1b[0m:\x1b[0m\x1b[1m\x1b[31malpha\x1b[0m and \x1b[0m\x1b[1m\x1b[31malpha\x1b[0m\n\
         \n\x1b[0m\x1b[35msub/b.txt\x1b[0m\n\
         \x1b[0m\x1b[32m2\x1b[0m:\x1b[0m\x1b[1m\x1b[31malpha\x1b[0m in sub\n"
    );
    // rg --no-heading -n -C1 --color always alpha: separators and the `--`
    // stay uncoloured.
    let c = OutputConfig {
        color: true,
        ..with_context(Format::Lines, false, true)
    };
    let got = alpha(c, context(1, 1));
    assert!(
        got.contains("\x1b[0m\x1b[35ma.txt\x1b[0m-\x1b[0m\x1b[32m2\x1b[0m-beta\n"),
        "{got}"
    );
    assert!(
        got.contains("-eta\n--\n\x1b[0m\x1b[35msub/b.txt\x1b[0m-"),
        "{got}"
    );
    // rg -c --color always alpha / rg -l --color always alpha
    let c = OutputConfig {
        color: true,
        ..cfg(Format::CountLines)
    };
    assert_eq!(
        alpha(c, QueryOutput::CountLines),
        "\x1b[0m\x1b[35ma.txt\x1b[0m:3\n\x1b[0m\x1b[35msub/b.txt\x1b[0m:1\n"
    );
    let c = OutputConfig {
        color: true,
        ..cfg(Format::FilesWithMatches)
    };
    assert_eq!(
        alpha(c, QueryOutput::FilesWithMatches),
        "\x1b[0m\x1b[35ma.txt\x1b[0m\n\x1b[0m\x1b[35msub/b.txt\x1b[0m\n"
    );
}

#[test]
fn non_utf8_and_crlf_lines_pass_through_verbatim() {
    // rg --no-heading -n alpha over these two files: the `\r` of a CRLF line
    // is kept and the high bytes are not replaced.
    let docs: [(&str, &[u8]); 2] = [
        ("bin.txt", b"alpha \xff\xfe caf\xc3\xa9\nplain\n"),
        ("crlf.txt", b"x\n\nalpha\r\ny\n"),
    ];
    let c = OutputConfig {
        heading: false,
        ..cfg(Format::Lines)
    };
    assert_eq!(
        render(c, MATCHES, &docs, &["alpha"]),
        b"bin.txt:1:alpha \xff\xfe caf\xc3\xa9\ncrlf.txt:3:alpha\r\n".to_vec()
    );
    // -o writes the matched bytes, multi-byte span included.
    let c = OutputConfig {
        heading: false,
        ..cfg(Format::OnlyMatching)
    };
    assert_eq!(
        render(c, MATCHES, &docs, &["café"]),
        b"bin.txt:1:caf\xc3\xa9\n".to_vec()
    );
}

#[test]
fn banner_only_with_several_terms() {
    let c = OutputConfig {
        banner: true,
        ..cfg(Format::Lines)
    };
    assert_eq!(
        String::from_utf8(render(c, MATCHES, &FIXTURE, &["zeta", "first"])).unwrap(),
        "== zeta ==\na.txt\n6:zeta\n\n== first ==\nsub/b.txt\n1:first\n"
    );
    // One term prints exactly rg's bytes.
    assert_eq!(
        alpha(cfg(Format::Lines), MATCHES),
        String::from_utf8(render(cfg(Format::Lines), MATCHES, &FIXTURE, &["alpha"])).unwrap()
    );
}

/// A last line with no terminator: rg reports its zero-width match as a
/// matched line with no span. Fixture: `printf 'hello\nworld' > b.txt`.
const NOEOL: [(&str, &[u8]); 1] = [("b.txt", b"hello\nworld")];

#[test]
fn spanless_match_at_an_unterminated_eof_prints_as_a_match() {
    let dollar = |c| String::from_utf8(render(c, MATCHES, &NOEOL, &["/$/"])).unwrap();
    // rg --heading -n '$'
    assert_eq!(dollar(cfg(Format::Lines)), "b.txt\n1:hello\n2:world\n");
    // rg --no-heading -n '$'
    let no_heading = OutputConfig {
        heading: false,
        ..cfg(Format::Lines)
    };
    assert_eq!(dollar(no_heading), "b.txt:1:hello\nb.txt:2:world\n");
    // rg --heading -n -o '$': the spanless line prints whole, `:` separator.
    assert_eq!(dollar(cfg(Format::OnlyMatching)), "b.txt\n1:\n2:world\n");
    // rg --no-heading -n -o '$'
    let only_flat = OutputConfig {
        heading: false,
        ..cfg(Format::OnlyMatching)
    };
    assert_eq!(dollar(only_flat), "b.txt:1:\nb.txt:2:world\n");
    // rg -c '$' / rg --count-matches '$' / rg -l '$'
    assert_eq!(dollar(cfg(Format::CountLines)), "b.txt:2\n");
    assert_eq!(dollar(cfg(Format::CountMatches)), "b.txt:2\n");
    assert_eq!(dollar(cfg(Format::FilesWithMatches)), "b.txt\n");
}

#[test]
fn a_leading_bom_is_absent_from_the_printed_line() {
    // rg --heading -n '^module' bom.c plain.c, and the same with -o 'o+'.
    const DOCS: [(&str, &[u8]); 2] = [
        ("bom.c", b"\xef\xbb\xbfmodule Foo where\nsecond line\n"),
        ("plain.c", b"module Bar where\n"),
    ];
    assert_eq!(
        String::from_utf8(render(cfg(Format::Lines), MATCHES, &DOCS, &["/^module/"])).unwrap(),
        "bom.c\n1:module Foo where\n\nplain.c\n1:module Bar where\n"
    );
    assert_eq!(
        String::from_utf8(render(cfg(Format::OnlyMatching), MATCHES, &DOCS, &["/o+/"])).unwrap(),
        "bom.c\n1:o\n1:oo\n2:o\n\nplain.c\n1:o\n"
    );
}

#[test]
fn broken_pipe_is_not_an_error() {
    /// Every write fails with `kind`.
    struct Fail(std::io::ErrorKind);

    impl Write for Fail {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(self.0))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(self.0))
        }
    }

    let feed = |sink: Fail| {
        let mut w = MatchWriter::new(sink, cfg(Format::Lines));
        let matcher = matcher_for("alpha", MatchMode::default());
        let outcome = matcher.scan(FIXTURE[0].1, MATCHES);
        w.term_start("alpha")?;
        w.doc(DocMatches {
            path: FIXTURE[0].0,
            raw: FIXTURE[0].1,
            outcome: &outcome,
        })?;
        w.finish()
    };
    assert_eq!(feed(Fail(std::io::ErrorKind::BrokenPipe)), Ok(()));
    let err = feed(Fail(std::io::ErrorKind::PermissionDenied)).unwrap_err();
    assert!(err.starts_with(WRITE_FAILED), "got: {err}");
}

/// rg `-U`, whose only printing change is `-o`.
fn multi(format: Format) -> OutputConfig {
    OutputConfig {
        multiline: true,
        ..cfg(format)
    }
}

fn multi_render(c: OutputConfig, output: QueryOutput, term: &str) -> String {
    String::from_utf8(render(c, output, &FIXTURE, &[term])).unwrap()
}

#[test]
fn multiline_prints_every_line_the_match_touches() {
    // rg --heading -n -U 'alpha one\nbeta'
    assert_eq!(
        multi_render(multi(Format::Lines), MATCHES, "/alpha one\\nbeta/"),
        "a.txt\n1:alpha one\n2:beta\n"
    );
    // rg --heading -n -U '\nbeta': line 1 is matched through its terminator.
    assert_eq!(
        multi_render(multi(Format::Lines), MATCHES, "/\\nbeta/"),
        "a.txt\n1:alpha one\n2:beta\n"
    );
}

#[test]
fn multiline_only_matching_prints_the_per_line_pieces() {
    // rg --heading -n -U -o 'one\nbeta'
    assert_eq!(
        multi_render(multi(Format::OnlyMatching), MATCHES, "/one\\nbeta/"),
        "a.txt\n1:one\n2:beta\n"
    );
    // rg --heading -n -U -o '\nbeta': the line the match only reaches
    // through its terminator prints nothing at all.
    assert_eq!(
        multi_render(multi(Format::OnlyMatching), MATCHES, "/\\nbeta/"),
        "a.txt\n2:beta\n"
    );
}

#[test]
fn multiline_colours_the_per_line_pieces_only() {
    // rg --heading -n -U --color always '\nbeta'
    let c = OutputConfig {
        color: true,
        ..multi(Format::Lines)
    };
    assert_eq!(
        multi_render(c, MATCHES, "/\\nbeta/"),
        "\u{1b}[0m\u{1b}[35ma.txt\u{1b}[0m\n\u{1b}[0m\u{1b}[32m1\u{1b}[0m:alpha one\n\u{1b}[0m\u{1b}[32m2\u{1b}[0m:\u{1b}[0m\u{1b}[1m\u{1b}[31mbeta\u{1b}[0m\n"
    );
}

#[test]
fn multiline_counts_are_match_counts() {
    // rg -U -c 'a\n' and rg -U --count-matches 'a\n'
    assert_eq!(
        multi_render(multi(Format::CountLines), QueryOutput::CountLines, "/a\\n/"),
        "a.txt:5\n"
    );
    assert_eq!(
        multi_render(
            multi(Format::CountMatches),
            QueryOutput::CountMatches,
            "/a\\n/"
        ),
        "a.txt:5\n"
    );
}

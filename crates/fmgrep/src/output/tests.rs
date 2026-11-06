use super::*;

use crate::matching::{MatchMode, Print, QueryOutput, strip_bom};
use crate::tests::support::search;

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

const MATCHES: QueryOutput = QueryOutput::Matches(Print {
    before: 0,
    after: 0,
    line_numbers: true,
    only_matching: false,
    color: false,
    path: false,
    json: false,
});

fn cfg(format: Format) -> OutputConfig {
    OutputConfig {
        format,
        heading: true,
        context: false,
        color: false,
        banner: false,
        filename: true,
    }
}

/// Render `terms` over `docs` the way the drive does, `output`'s colour and
/// per-line path taken from `cfg`.
fn render_in(
    cfg: OutputConfig,
    output: QueryOutput,
    mode: MatchMode,
    docs: &[(&str, &[u8])],
    terms: &[&str],
) -> Vec<u8> {
    let output = match output {
        QueryOutput::Matches(p) => QueryOutput::Matches(Print {
            color: cfg.color,
            path: cfg.filename && !cfg.heading,
            ..p
        }),
        o => o,
    };
    let mut buf: Vec<u8> = Vec::new();
    let mut w = MatchWriter::new(&mut buf, cfg);
    for term in terms {
        w.term_start(term).unwrap();
        let s = search(term, output, mode);
        for (path, raw) in docs {
            if let Some(f) = s.doc(path, strip_bom(raw)).unwrap() {
                let (printed, stats) = (&f.printed, &f.stats);
                w.doc(DocMatches {
                    path,
                    printed,
                    stats,
                })
                .unwrap();
            }
        }
    }
    w.finish().unwrap();
    buf
}

fn render(
    cfg: OutputConfig,
    output: QueryOutput,
    docs: &[(&str, &[u8])],
    terms: &[&str],
) -> Vec<u8> {
    render_in(cfg, output, MatchMode::default(), docs, terms)
}

fn alpha(cfg: OutputConfig, output: QueryOutput) -> String {
    String::from_utf8(render(cfg, output, &FIXTURE, &["alpha"])).unwrap()
}

fn context(before: usize, after: usize) -> QueryOutput {
    QueryOutput::Matches(Print {
        before,
        after,
        ..unwrap(MATCHES)
    })
}

fn unwrap(output: QueryOutput) -> Print {
    match output {
        QueryOutput::Matches(p) => p,
        _ => unreachable!(),
    }
}

fn only(output: QueryOutput) -> QueryOutput {
    QueryOutput::Matches(Print {
        only_matching: true,
        ..unwrap(output)
    })
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
fn counts_modes_print_path_and_number() {
    // rg -c alpha
    assert_eq!(
        alpha(cfg(Format::Count), QueryOutput::CountLines),
        "a.txt:3\nsub/b.txt:1\n"
    );
    // rg --count-matches alpha
    assert_eq!(
        alpha(cfg(Format::Count), QueryOutput::CountMatches),
        "a.txt:4\nsub/b.txt:1\n"
    );
    // rg -l alpha
    assert_eq!(
        alpha(cfg(Format::FilesWithMatches), QueryOutput::FilesWithMatches),
        "a.txt\nsub/b.txt\n"
    );
}

#[test]
fn no_file_name_for_one_file_matches_rg() {
    let one = |format: Format, heading: bool, output: QueryOutput| {
        let c = OutputConfig {
            heading,
            context: output == context(1, 1),
            filename: false,
            ..cfg(format)
        };
        String::from_utf8(render(c, output, &FIXTURE[..1], &["alpha"])).unwrap()
    };
    // rg --heading -n alpha a.txt
    assert_eq!(
        one(Format::Lines, true, MATCHES),
        "1:alpha one\n3:alpha two\n7:alpha and alpha\n"
    );
    // rg --no-heading -n -C1 alpha a.txt
    assert_eq!(
        one(Format::Lines, false, context(1, 1)),
        "1:alpha one\n2-beta\n3:alpha two\n4-delta\n--\n6-zeta\n7:alpha and alpha\n8-eta\n"
    );
    // rg --heading -n -o alpha a.txt
    assert_eq!(
        one(Format::Lines, true, only(MATCHES)),
        "1:alpha\n3:alpha\n7:alpha\n7:alpha\n"
    );
    // rg -c alpha a.txt; rg --count-matches alpha a.txt; rg -l alpha a.txt
    assert_eq!(one(Format::Count, true, QueryOutput::CountLines), "3\n");
    assert_eq!(one(Format::Count, true, QueryOutput::CountMatches), "4\n");
    assert_eq!(
        one(
            Format::FilesWithMatches,
            true,
            QueryOutput::FilesWithMatches
        ),
        "a.txt\n"
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
        alpha(with_context(Format::Lines, true, true), only(context(1, 1))),
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
        ..cfg(Format::Count)
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
        String::from_utf8(render(cfg(Format::Lines), only(MATCHES), &DOCS, &["/o+/"])).unwrap(),
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
        let s = search("alpha", MATCHES, MatchMode::default());
        let f = s.doc(FIXTURE[0].0, FIXTURE[0].1)?.unwrap();
        w.term_start("alpha")?;
        w.doc(DocMatches {
            path: FIXTURE[0].0,
            printed: &f.printed,
            stats: &f.stats,
        })?;
        w.finish()
    };
    assert_eq!(feed(Fail(std::io::ErrorKind::BrokenPipe)), Ok(()));
    let err = feed(Fail(std::io::ErrorKind::PermissionDenied)).unwrap_err();
    assert!(err.starts_with(WRITE_FAILED), "got: {err}");
}

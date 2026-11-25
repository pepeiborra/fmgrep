use clap::CommandFactory;

use super::*;
use crate::cli::{Outcome, dispatch_to};

const AND_JSON: &str = r#"{"and":[{"term":"a"},{"regex":"b+"}]}"#;
const AND_TEXT: &str = "a AND /b+/";

fn parse(words: &[&str]) -> Result<Cli, String> {
    Cli::try_parse_from(words).map_err(|e| e.to_string())
}

fn cli(words: &[&str]) -> Cli {
    parse(words).unwrap_or_else(|e| panic!("{words:?}: {e}"))
}

fn search(words: &[&str]) -> Search {
    let cli = cli(words);
    assert!(cli.command.is_none(), "{words:?} is a subcommand");
    cli.search
}

fn params_in(words: &[&str], input: &str, printing: bool, tty: bool) -> QueryParams {
    let params = search(words).params(&mut input.as_bytes(), printing, tty);
    params.unwrap_or_else(|e| panic!("{words:?}: {e}"))
}

/// As the human search builds it, off a terminal.
fn params(words: &[&str]) -> QueryParams {
    params_in(words, "", true, false)
}

fn err(words: &[&str]) -> String {
    match parse(words) {
        Err(e) => e,
        Ok(cli) => match cli.search.params(&mut &b""[..], true, false) {
            Err(e) => e,
            Ok(_) => panic!("{words:?} must be refused"),
        },
    }
}

type Edit = fn(&mut QueryParams);

fn lines(edit: Edit) -> QueryParams {
    let mut p = QueryParams {
        regex: true,
        mode: Mode::Lines,
        line_numbers: false,
        heading: false,
        ..QueryParams::default()
    };
    edit(&mut p);
    p
}

#[test]
fn clap_definition_is_valid() {
    Cli::command().debug_assert();
}

#[test]
fn rg_flags_set_the_params() {
    let cases: [(&[&str], Edit); 21] = [
        (&["-i"], |p| p.ignore_case = true),
        (&["-U"], |p| p.multiline = true),
        (&["--multiline-dotall"], |p| p.dotall = true),
        (&["b|c"], |p| p.terms = vec!["b|c".into()]),
        // v0.1.0's flag: a no-op now, for every PATTERN is a regex.
        (&["--regex", "b|c"], |p| p.terms = vec!["b|c".into()]),
        (&["-F", "b|c"], |p| {
            p.regex = false;
            p.terms = vec!["b|c".into()];
        }),
        (&["--fixed-strings", "a"], |p| {
            p.regex = false;
            p.terms = vec!["a".into()];
        }),
        (&["--", "--regex"], |p| p.terms = vec!["--regex".into()]),
        (&["--", "--json"], |p| p.terms = vec!["--json".into()]),
        (&["-e", "--json"], |p| p.terms = vec!["--json".into()]),
        (&["-n"], |p| p.line_numbers = true),
        (&["-n", "-N"], |_| {}),
        (&["--heading"], |p| p.heading = true),
        (&["--heading", "--no-heading"], |_| {}),
        (&["--color", "always"], |p| p.color = Color::Always),
        (&["-c"], |p| p.mode = Mode::Count),
        (&["--count-matches"], |p| p.mode = Mode::CountMatches),
        (&["-l"], |p| p.mode = Mode::Files),
        (&["-o"], |p| p.mode = Mode::Only),
        (&["-il", "x"], |p| {
            p.ignore_case = true;
            p.mode = Mode::Files;
            p.terms = vec!["x".into()];
        }),
        (&["-iC2"], |p| {
            p.ignore_case = true;
            p.context = Context {
                before: 2,
                after: 2,
            };
        }),
    ];
    for (words, edit) in cases {
        assert_eq!(params(words), lines(edit), "{words:?}");
    }
}

#[test]
fn the_last_case_flag_wins_and_smart_case_reads_the_patterns() {
    for (words, ignore) in [
        (&["-i", "x"][..], true),
        (&["-s", "x"], false),
        (&["-i", "-s", "x"], false),
        (&["-s", "-i", "x"], true),
        (&["-i", "-S", "Foo"], false),
        (&["-S", "-i", "Foo"], true),
        (&["-S", "-s", "foo"], false),
        // Smart case: some literal, and none of them uppercase.
        (&["-S", "foo"], true),
        (&["-S", "Foo"], false),
        (&["-S", "fOo"], false),
        (&["-S", "-e", "foo", "-e", "Bar"], false),
        (&["-S", "-e", "foo", "-e", "bar"], true),
        (&["-S", "foo.*\\w"], true),
        (&["-S", "\\S+"], false),
        (&["-S", "\\p{Lu}foo"], true),
        (&["-S", "foo[A-Z]"], false),
        (&["-S", "\\w+\\.rs"], true),
        (&["-S", "a\\.B"], false),
        (&["-S", "-F", "a.B"], false),
        (&["-S", "-F", "a.b"], true),
        (&["-S", "-F", "\\S"], false),
        (&["-S", "-F", "\\s"], true),
        (&["-S", ""], false),
        (&["-S", "("], false),
        (&["-S", "--and", "-e", "foo", "-e", "Bar"], false),
        (&["-S", "--or", "-e", "foo", "-e", "bar"], true),
        (&["-S", "--expr", "foo AND /ba+/"], true),
        (&["-S", "--expr", "foo AND Bar"], false),
        (
            &["-S", "--expr-json", r#"{"or":[{"term":"a"},{"term":"B"}]}"#],
            false,
        ),
    ] {
        assert_eq!(params(words).ignore_case, ignore, "{words:?}");
    }
    // -s and -S need no wire field: a request carries what they decided.
    assert_eq!(params(&["-s", "x"]), params(&["x"]));
    assert_eq!(params(&["-S", "x"]), params(&["-i", "x"]));
}

#[test]
fn word_and_line_flags_override_each_other() {
    let got = |words: &[&str]| {
        let p = params(words);
        (p.word_regexp, p.line_regexp)
    };
    assert_eq!(got(&["a"]), (false, false));
    assert_eq!(got(&["-w", "a"]), (true, false));
    assert_eq!(got(&["--word-regexp", "a"]), (true, false));
    assert_eq!(got(&["-x", "a"]), (false, true));
    assert_eq!(got(&["--line-regexp", "a"]), (false, true));
    // The last of the two wins, as in rg.
    assert_eq!(got(&["-w", "-x", "a"]), (false, true));
    assert_eq!(got(&["-x", "-w", "a"]), (true, false));
    // Each reaches every leaf of an expression.
    assert_eq!(got(&["-w", "--and", "-e", "a", "-e", "b"]), (true, false));
    assert_eq!(got(&["-x", "--expr", "a OR b"]), (false, true));
}

#[test]
fn last_mode_flag_wins() {
    for (words, want) in [
        (&["-c", "-l"][..], Mode::Files),
        (&["-l", "-c"], Mode::Count),
        (&["-lc"], Mode::Count),
        (&["-l", "--count-matches"], Mode::CountMatches),
        (&["-o", "-c", "-o"], Mode::Only),
        (&["-c", "--approx-counts"], Mode::Counts),
        (&["--approx-counts", "-c"], Mode::Count),
    ] {
        assert_eq!(params(words).mode, want, "{words:?}");
    }
}

#[test]
fn params_pick_the_mode() {
    for (words, printing, want) in [
        (&["x"][..], true, Mode::Lines),
        (&["x"], false, Mode::Counts),
        (&["--json", "x"], true, Mode::Counts),
        (&["--json", "-C2", "x"], true, Mode::Counts),
        (&["--json", "--matches"], true, Mode::JsonMatches),
        (&["--json", "-o"], false, Mode::JsonMatches),
        (&["--json", "-l"], true, Mode::Files),
        (&["--json", "-c"], true, Mode::Count),
        (&["--json", "--count-matches"], true, Mode::CountMatches),
        (
            &["--json", "--matches", "--approx-counts"],
            true,
            Mode::Counts,
        ),
        (&["--approx-counts", "-C2"], true, Mode::Counts),
    ] {
        let got = params_in(words, "", printing, false).mode;
        assert_eq!(got, want, "{words:?} printing {printing}");
    }
    for words in [
        &["-c"][..],
        &["-l"],
        &["--count-matches"],
        &["--matches"],
        &["-o"],
    ] {
        assert_eq!(
            params_in(words, "", false, false),
            params_in(words, "", true, false),
            "{words:?}"
        );
    }
    let p = params_in(&["-B1", "-A2", "-N", "--no-heading", "x"], "", true, true);
    assert_eq!(
        p.context,
        Context {
            before: 1,
            after: 2
        }
    );
    assert!(!p.line_numbers && !p.heading && p.color == Color::Always);
    let words = ["--json", "-C2", "-N", "--color", "always", "x"];
    assert_eq!(
        params_in(&words, "", true, true),
        QueryParams {
            terms: vec!["x".into()],
            regex: true,
            ..QueryParams::default()
        },
        "a counts request carries no printer fields"
    );
    assert!(params(&["--json", "--matches"]).line_numbers);
    let json = params(&["--json", "-N", "--matches"]);
    assert!(json.mode == Mode::JsonMatches && !json.line_numbers);
    let p = params(&["-U", "x"]);
    assert!(p.multiline);
}

/// `--stats` reaches the request where it prints: after lines and `-o`.
#[test]
fn stats_is_for_the_printed_lines() {
    for (words, want) in [
        (&["--stats", "x"][..], true),
        (&["--stats", "-o", "x"], true),
        (&["--stats", "-C1", "x"], true),
        (&["x"], false),
        (&["-c", "-o", "--stats", "x"], true),
        (&["--stats", "--json", "--matches", "x"], false),
        (&["--stats", "--json", "x"], false),
        (&["--stats", "--approx-counts", "x"], false),
    ] {
        let p = params(words);
        assert_eq!(
            (p.stats, p.output_config().stats),
            (want, want),
            "{words:?}"
        );
    }
}

/// rg prints its block under -c, --count-matches and -l too; fmgrep
/// refuses `--stats` there rather than print less than rg.
#[test]
fn stats_refuses_the_counting_modes() {
    for (words, flag) in [
        (&["--stats", "-c", "x"][..], "--count"),
        (&["-c", "--stats", "x"], "--count"),
        (&["--stats", "--count-matches", "x"], "--count-matches"),
        (&["--stats", "-l", "x"], "--files-with-matches"),
        (&["-o", "-l", "--stats", "x"], "--files-with-matches"),
    ] {
        let e = err(words);
        assert!(
            e.contains("cannot be used with")
                && e.contains("'--stats'")
                && e.contains(&format!("'{flag}'")),
            "{words:?}: {e}"
        );
    }
}

#[test]
fn context_flags_follow_rg_precedence() {
    let ctx = |words: &[&str]| {
        let c = params(words).context;
        (c.before, c.after)
    };
    assert_eq!(ctx(&["-C2"]), (2, 2));
    assert_eq!(ctx(&["-A3"]), (0, 3));
    assert_eq!(ctx(&["-B1", "-A2"]), (1, 2));
    assert_eq!(ctx(&["-C2", "-A0"]), (2, 0));
    assert_eq!(ctx(&["-A0", "-C2"]), (2, 0));
    assert_eq!(ctx(&["--after-context", "1", "-C", "5"]), (5, 1));
    assert!(!params(&["-C0"]).output_config().context);
    assert!(params(&["-C1"]).output_config().context);
}

#[test]
fn terminal_defaults_follow_rg() {
    for (words, tty, want) in [
        (&["-i"][..], false, (false, false, false)),
        (&["-i"], true, (true, true, true)),
        (&["-n"], false, (false, true, false)),
        (&["--heading"], false, (false, false, true)),
        (&["-N"], true, (true, false, true)),
        (&["--no-heading"], true, (true, true, false)),
        (&["--color", "never"], true, (false, true, true)),
        (&["--color", "always"], false, (true, false, false)),
    ] {
        let p = params_in(words, "", true, tty);
        let c = p.output_config();
        assert_eq!((c.color, p.line_numbers, c.heading), want, "{words:?}");
    }
}

#[test]
fn banner_needs_several_terms() {
    // A search of the command line has one PATTERN, or one expression; only
    // a raw request carries several terms.
    assert!(!params(&["one"]).output_config().banner);
    assert!(!params(&["-e", "one", "-e", "two"]).output_config().banner);
    let raw = QueryParams {
        terms: vec!["one".into(), "two".into()],
        ..QueryParams::default()
    };
    assert!(raw.output_config().banner);
}

#[test]
fn only_the_first_word_selects_a_subcommand() {
    assert!(matches!(
        cli(&["serve", "--stdio"]).command,
        Some(Command::Serve(_))
    ));
    assert!(matches!(
        cli(&["index", "--store-docs"]).command,
        Some(Command::Index(_))
    ));
    assert!(matches!(
        cli(&["status"]).command,
        Some(Command::Status(StatusArgs {
            path: None,
            json: false,
            server: None,
        }))
    ));
    match cli(&["status", "idx", "--json", "--server", "h:1"]).command {
        Some(Command::Status(a)) => assert_eq!(
            (a.path, a.json, a.server),
            (Some("idx".into()), true, Some("h:1".into()))
        ),
        _ => panic!("status idx --json --server h:1 is the command"),
    }
    for (words, terms) in [
        (&["--", "index"][..], &["index"][..]),
        (&["--", "serve", "x"], &["serve", "x"]),
        (&["--", "status"], &["status"]),
        (&["-i", "status"], &["status"]),
        (&["x", "status"], &["x", "status"]),
        (&["Status", "x"], &["Status", "x"]),
        (&["indexes", "x"], &["indexes", "x"]),
        (&["Serve", "x"], &["Serve", "x"]),
        (&["x", "index"], &["x", "index"]),
        (&["-i", "serve"], &["serve"]),
        (&["--index", "d", "serve"], &["serve"]),
        (&["help"], &["help"]),
        (&["--", "-V"], &["-V"]),
    ] {
        assert_eq!(search(words).positionals, terms, "{words:?}");
    }
    assert_eq!(
        search(&["--index", "idx", "x"]).index,
        Some(PathBuf::from("idx"))
    );
}

#[test]
fn and_or_flags_equal_their_expr_json() {
    let same = |flags: &[&str], json: &str| {
        assert_eq!(params(flags), params(&["--expr-json", json]), "{flags:?}");
    };
    // A pattern that is nothing but a literal is a term; any other is a regex.
    same(&["--and", "-e", "a", "-e", "b+"], AND_JSON);
    same(&["-e", "a", "-e", "b+", "--and"], AND_JSON);
    same(&["--and", "--and", "-e", "a", "-e", "b+"], AND_JSON);
    same(&["--regex", "--and", "-e", "a", "-e", "b+"], AND_JSON);
    same(
        &["-F", "--and", "-e", "a", "-e", "b+"],
        r#"{"and":[{"term":"a"},{"term":"b+"}]}"#,
    );
    same(
        &["--or", "-e", "/a/", "-e", "b\\.c"],
        r#"{"or":[{"term":"/a/"},{"term":"b.c"}]}"#,
    );
    same(
        &["-F", "--or", "-e", "a", "-e", ""],
        r#"{"or":[{"term":"a"},{"regex":""}]}"#,
    );
    same(
        &["--or", "-e", "a", "-e", "", "-e", "c"],
        r#"{"or":[{"term":"a"},{"regex":""},{"term":"c"}]}"#,
    );
    same(
        &["--or", "-e", "-x", "-e", "--and", "-e", ""],
        r#"{"or":[{"term":"-x"},{"term":"--and"},{"regex":""}]}"#,
    );
    assert_eq!(
        params(&["-i", "-U", "-l", "--and", "-e", "a", "-e", "b+"]),
        QueryParams {
            multiline: true,
            ..params(&["--expr-json", AND_JSON, "-i", "-l"])
        }
    );
    assert_eq!(
        params_in(&["--expr-json=-"], AND_JSON, true, false),
        params(&["--expr-json", AND_JSON])
    );
    assert_eq!(
        params(&["--expr", "x", "--expr", "-"]).expr,
        Some(Expr::Term("-".into()))
    );
}

#[test]
fn expr_text_error_carries_a_caret() {
    assert_eq!(
        err(&["--expr", "a b"]),
        "error: invalid --expr: expected AND, OR or ')' (quote a term that holds spaces or parentheses)\n  a b\n    ^"
    );
    assert_eq!(
        err(&["--server", "a:1", "--expr", "   "]),
        "error: invalid --expr: empty expression\n     \n  ^"
    );
}

#[test]
fn rejects_bad_argv() {
    for words in [
        &["serve", "--listen", "127.0.0.1:0", "--stdio"][..],
        &["--server="],
        &["--server", "a:1", "--status"],
        &["--shutdown", "--index", "d"],
        &["--server", "a:1", "--shutdown", "--or"],
        &["--server", "a:1", "--shutdown", "--expr", AND_TEXT],
        &["--server", "a:1", "--index", "d", "x"],
        &["--ignore-local-changes", "--index", "d", "x"],
        &["status", "--server", "a:1", "--ignore-local-changes"],
        &["--server", "a:1", "--ignore-local-changes", "--shutdown"],
        &["--and", "--or", "a", "b"],
        &["--and", "--expr-json", AND_JSON],
        &["--or", "--expr-json", AND_JSON],
        &["--or", "--expr", AND_TEXT],
        &["--regex", "--expr", AND_TEXT],
        &["--expr", AND_TEXT, "--expr-json", AND_JSON],
        &["--expr-json", "-", "--", "x"],
        &["--regex", "--expr-json", AND_JSON],
    ] {
        let e = err(words);
        assert!(e.starts_with("error: "), "{words:?}: {e}");
    }
    for (words, want) in [
        (&["--and"][..], "error: --and needs at least two patterns"),
        (&["--or", "a"], "error: --or needs at least two patterns"),
        (
            &["--server", "a:1", "--or", "-e", "a"],
            "error: --or needs at least two patterns",
        ),
        (&["--expr-json", "{"], "error: invalid --expr-json: EOF"),
        (
            &["--expr-json", r#"{"and":[]}"#],
            "error: invalid --expr-json: an and/or expression needs at least one operand",
        ),
        (
            &["--expr-json", r#"{"not":{"term":"a"}}"#],
            "error: invalid --expr-json: unknown variant `not`",
        ),
    ] {
        let e = err(words);
        assert!(e.starts_with(want), "{words:?}: {e}");
    }
}

#[test]
fn help_and_version_print_to_stdout() {
    let run = |words: &[&str]| {
        let mut out = Vec::new();
        let done = dispatch_to(words, &mut &b""[..], &mut out, false);
        assert!(matches!(done, Ok(Outcome::Done)), "{words:?}");
        String::from_utf8(out).unwrap()
    };
    assert!(run(&["-h"]).contains("Usage: fmgrep [OPTIONS] PATTERN [PATH]..."));
    assert_eq!(
        run(&["-V"]),
        format!("fmgrep {}\n", env!("CARGO_PKG_VERSION"))
    );
    for words in [&[][..], &["--"]] {
        let mut out = Vec::new();
        let e = dispatch_to(words, &mut &b""[..], &mut out, false)
            .err()
            .unwrap();
        assert!(
            e.contains("Usage: fmgrep") && out.is_empty(),
            "{words:?}: {e}"
        );
    }
}

// Tests run in the crate directory, which holds `src` and `Cargo.toml`.

#[test]
fn glob_flags_reach_the_params() {
    let p = params(&[
        "-g", "*.rs", "--iglob", "*.MD", "-g", "!src/**", "--glob=b", "x",
    ]);
    assert_eq!(p.globs, ["*.rs", "!src/**", "b"]);
    assert_eq!(p.iglobs, ["*.MD"]);
    assert!(!p.glob_case_insensitive);
    assert!(params(&["--glob-case-insensitive", "x"]).glob_case_insensitive);
    assert_eq!(
        err(&["-g", "[", "x"]),
        "error: invalid glob '[': unclosed character class; missing ']'"
    );
}

/// The PATTERNs and PATHs of `words`.
fn split(words: &[&str]) -> (Vec<String>, Vec<String>) {
    let s = search(words);
    (s.split().0, s.paths())
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

#[test]
fn the_first_positional_is_the_pattern_and_the_rest_are_paths() {
    assert_eq!(
        split(&["a", "src", "Cargo.toml"]),
        (strings(&["a"]), strings(&["src", "Cargo.toml"]))
    );
    // The first is always the PATTERN, a file's name or not.
    assert_eq!(
        split(&["src", "Cargo.toml"]),
        (strings(&["src"]), strings(&["Cargo.toml"]))
    );
    // A word that names no file is a PATH too, and the search says it
    // does not exist: it never turns into another PATTERN.
    assert_eq!(
        split(&["a", "src", "nosuch"]),
        (strings(&["a"]), strings(&["src", "nosuch"]))
    );
    assert_eq!(split(&["a", "/tmp/"]).1, ["/tmp/"]);
    assert_eq!(split(&["a"]), (strings(&["a"]), vec![]));
    assert_eq!(split(&["--json"]).0, Vec::<String>::new());
    // `--` ends the flags, and the first word after it is the PATTERN.
    assert_eq!(
        split(&["--", "-x", "src"]),
        (strings(&["-x"]), strings(&["src"]))
    );
    assert_eq!(split(&["-i", "--", "status"]).0, ["status"]);
}

#[test]
fn with_e_or_under_expr_every_positional_is_a_path() {
    assert_eq!(
        split(&["-e", "a", "src", "Cargo.toml"]),
        (strings(&["a"]), strings(&["src", "Cargo.toml"]))
    );
    assert_eq!(
        split(&["-e", "a", "--", "-b"]),
        (strings(&["a"]), strings(&["-b"]))
    );
    let (terms, paths) = split(&["--expr", AND_TEXT, "src", "Cargo.toml"]);
    assert_eq!((terms.len(), paths), (0, strings(&["src", "Cargo.toml"])));
    assert_eq!(split(&["--expr-json", AND_JSON, "--", "src"]).1, ["src"]);
    // A word that names no file is no PATTERN there either.
    assert_eq!(split(&["--expr", AND_TEXT, "src", "x"]).1, ["src", "x"]);
}

#[test]
fn a_dash_is_a_positional_like_any_other() {
    // Stdin is never read: the first positional is the PATTERN, the rest
    // are PATHs, and a `-` is one that names a file only if one has the name.
    assert_eq!(split(&["a", "-"]), (strings(&["a"]), strings(&["-"])));
    assert_eq!(split(&["a", "-", "-"]).1, ["-", "-"]);
    assert_eq!(split(&["-e", "a", "-"]).1, ["-"]);
    assert_eq!(split(&["--expr", AND_TEXT, "-"]).1, ["-"]);
    assert_eq!(split(&["--", "-"]), (strings(&["-"]), vec![]));
    assert_eq!(split(&["-", "a"]), (strings(&["-"]), strings(&["a"])));
}

#[test]
fn one_file_path_drops_the_file_name() {
    let shows_name = |words: &[&str]| params(words).filename;
    assert!(!shows_name(&["a", "Cargo.toml"]));
    assert!(shows_name(&["a", "src"]));
    assert!(shows_name(&["a", "Cargo.toml", "src"]));
    // As rg, a PATH that does not exist counts: it was told of several.
    assert!(shows_name(&["a", "Cargo.toml", "nosuch"]));
    assert!(shows_name(&["a"]));
    assert!(!shows_name(&["--expr", AND_TEXT, "Cargo.toml"]));
    assert!(!shows_name(&["-e", "a", "Cargo.toml"]));
    assert!(!params(&["-l", "a", "Cargo.toml"]).output_config().filename);
}

#[test]
fn and_or_count_patterns_without_their_paths() {
    assert_eq!(
        params(&["--and", "-e", "a", "-e", "b+", "src"]),
        params(&["--expr-json", AND_JSON, "src"])
    );
    // The PATHs of rg's form are not patterns: `--and A B` is a search for
    // A in the PATH B, and one pattern is no conjunction.
    for words in [
        &["--and", "a", "b+"][..],
        &["--and", "a", "src"],
        &["--and", "-e", "a", "src"],
        &["--and"],
    ] {
        assert_eq!(
            err(words),
            "error: --and needs at least two patterns, each given with -e: fmgrep --and -e A -e B",
            "{words:?}"
        );
    }
    assert_eq!(
        err(&["--or", "-e", "a", "src"]),
        "error: --or needs at least two patterns, each given with -e: fmgrep --or -e A -e B"
    );
}

#[test]
fn e_gives_the_patterns_and_every_positional_is_a_path() {
    // -e takes its value even when it starts with a hyphen, as in rg.
    for words in [
        &["-e", "-x"][..],
        &["--regexp", "-x"],
        &["--regexp=-x"],
        &["-e-x"],
        &["-e", "-x", "src"],
    ] {
        assert_eq!(params(words).terms, ["-x"], "{words:?}");
    }
    // Several make one search of any of them: the `--or` expression.
    assert_eq!(
        params(&["-e", "a", "-e", "b+"]),
        params(&["--or", "-e", "a", "-e", "b+"])
    );
    assert_eq!(
        params(&["-e", "a", "-e", "b+", "src"]),
        params(&[
            "--expr-json",
            r#"{"or":[{"term":"a"},{"regex":"b+"}]}"#,
            "src"
        ])
    );
    assert_eq!(
        params(&["-F", "-e", "a.b", "-e", "foo("]),
        params(&["--expr-json", r#"{"or":[{"term":"a.b"},{"term":"foo("}]}"#])
    );
    assert_eq!(
        params(&["-e", "", "-e", "a"]).expr,
        Some(Expr::Or(vec![
            Expr::Regex(String::new()),
            Expr::Term("a".into())
        ]))
    );
    // One is the search of that pattern, as the positional is.
    assert_eq!(params(&["-e", "a.b"]), params(&["a.b"]));
    assert_eq!(
        params(&["-e", "a", "-e", "a"]).expr,
        params(&["--or", "-e", "a", "-e", "a"]).expr
    );
    // The smart case of -e reads the patterns.
    assert!(params(&["-S", "-e", "foo", "-e", "bar"]).ignore_case);
    assert!(!params(&["-S", "-e", "foo", "-e", "Bar"]).ignore_case);
    // A PATTERN given by -e is no flag of an expression, and the reverse.
    for words in [
        &["-e", "a", "--expr", AND_TEXT][..],
        &["-e", "a", "--expr-json", AND_JSON],
        &["--server", "a:1", "--shutdown", "-e", "a"],
    ] {
        let e = err(words);
        assert!(e.starts_with("error: "), "{words:?}: {e}");
    }
}

#[test]
fn flags_that_ask_for_what_an_index_lacks_say_why() {
    for (words, want) in [
        (
            &["--hidden", "x"][..],
            "--hidden cannot work on an index: hidden files are not indexed",
        ),
        (
            &["-.", "x"],
            "-. cannot work on an index: hidden files are not indexed",
        ),
        (
            &["-uu", "x"],
            "-u cannot work on an index: hidden and ignored files are not indexed",
        ),
        (
            &["x", "--unrestricted"],
            "--unrestricted cannot work on an index: hidden and ignored files are not indexed",
        ),
        (
            &["--no-ignore", "x"],
            "--no-ignore cannot work on an index: ignored files are not indexed",
        ),
        (
            &["--no-ignore-vcs", "x"],
            "--no-ignore-vcs cannot work on an index: ignored files are not indexed",
        ),
        (
            &["-iL", "x"],
            "-L cannot work on an index: symlinks are not indexed",
        ),
        (
            &["-a", "x"],
            "-a cannot work on an index: binary files are not indexed",
        ),
        (
            &["--binary", "x"],
            "--binary cannot work on an index: binary files are not indexed",
        ),
        (
            &["-z", "x"],
            "-z cannot work on an index: files are indexed as they are on disk, not decompressed",
        ),
        (
            &["--pre=cat", "x"],
            "--pre cannot work on an index: files are indexed as they are on disk, not preprocessed",
        ),
        (
            &["-v", "x"],
            "-v cannot work on an index: an inverted search reads every file, so the index cannot prune it",
        ),
        (
            &["--invert-match", "x"],
            "--invert-match cannot work on an index: an inverted search reads every file, so the index cannot prune it",
        ),
        (
            &["-E", "utf-16", "x"],
            "-E cannot work on an index: files are indexed as bytes, and fmgrep decodes none",
        ),
    ] {
        let e = dispatch_to(words, &mut &b""[..], &mut Vec::new(), false).err();
        assert_eq!(e, Some(format!("error: {want}; use rg")), "{words:?}");
    }
    // An index is built from files rg never lists, so `index` has no such
    // reason to give: the flag is only unknown to it.
    let e = dispatch_to(
        ["index", "--hidden", "."],
        &mut &b""[..],
        &mut Vec::new(),
        false,
    );
    let e = e.err().unwrap();
    assert!(e.starts_with("error: unexpected argument"), "{e}");
    // Any other flag rg has that fmgrep lacks is only unknown.
    for flag in ["-m", "--files", "-t"] {
        let e = dispatch_to([flag, "x"], &mut &b""[..], &mut Vec::new(), false);
        let e = e.err().unwrap();
        assert!(e.starts_with("error: unexpected argument"), "{flag}: {e}");
    }
}

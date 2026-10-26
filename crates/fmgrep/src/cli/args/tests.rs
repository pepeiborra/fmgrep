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
        mode: Mode::Lines,
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
    let cases: [(&[&str], Edit); 17] = [
        (&["-i"], |p| p.ignore_case = true),
        (&["-U"], |p| p.multiline = true),
        (&["--multiline-dotall"], |p| p.dotall = true),
        (&["--regex", "a", "/b|c/"], |p| {
            p.regex = true;
            p.terms = vec!["a".into(), "/b|c/".into()];
        }),
        (&["--", "--regex", "-i"], |p| {
            p.terms = vec!["--regex".into(), "-i".into()]
        }),
        (&["a", "--", "--json"], |p| {
            p.terms = vec!["a".into(), "--json".into()]
        }),
        (&["-N"], |p| p.line_numbers = false),
        (&["-N", "-n"], |_| {}),
        (&["--no-heading"], |p| p.heading = false),
        (&["--no-heading", "--heading"], |_| {}),
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
            ..QueryParams::default()
        },
        "a counts request carries no printer fields"
    );
    let json = params(&["--json", "-N", "--matches"]);
    assert!(json.mode == Mode::JsonMatches && !json.json_config().line_numbers);
    let p = params(&["-U", "x"]);
    assert!(p.multiline && p.json_config().multiline && p.output_config().multiline);
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
fn color_auto_follows_the_terminal_only() {
    for (words, tty, want) in [
        (&["-i"][..], false, false),
        (&["-i"], true, true),
        (&["--color", "never"], true, false),
        (&["--color", "always"], false, true),
    ] {
        let color = params_in(words, "", true, tty).output_config().color;
        assert_eq!(color, want, "{words:?}");
    }
}

#[test]
fn banner_needs_several_terms() {
    assert!(!params(&["one"]).output_config().banner);
    assert!(params(&["one", "two"]).output_config().banner);
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
    for (words, terms) in [
        (&["--", "index"][..], &["index"][..]),
        (&["--", "serve", "x"], &["serve", "x"]),
        (&["indexes", "x"], &["indexes", "x"]),
        (&["Serve", "x"], &["Serve", "x"]),
        (&["x", "index"], &["x", "index"]),
        (&["-i", "serve"], &["serve"]),
        (&["--index", "d", "serve"], &["serve"]),
        (&["help"], &["help"]),
        (&["--", "-V"], &["-V"]),
    ] {
        assert_eq!(search(words).terms, terms, "{words:?}");
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
    same(&["--and", "a", "/b+/"], AND_JSON);
    same(&["a", "/b+/", "--and"], AND_JSON);
    same(&["--and", "--and", "a", "/b+/"], AND_JSON);
    same(
        &["--regex", "--and", "a", "b+"],
        r#"{"and":[{"regex":"a"},{"regex":"b+"}]}"#,
    );
    same(
        &["--regex", "--or", "/a/", "b"],
        r#"{"or":[{"regex":"/a/"},{"regex":"b"}]}"#,
    );
    same(
        &["--or", "a", "//", "c"],
        r#"{"or":[{"term":"a"},{"regex":""},{"term":"c"}]}"#,
    );
    same(
        &["--or", "--", "-x", "--and", ""],
        r#"{"or":[{"term":"-x"},{"term":"--and"},{"term":""}]}"#,
    );
    assert_eq!(
        params(&["-i", "-U", "-l", "--and", "a", "/b+/"]),
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
        &["--status"],
        &["--shutdown"],
        &["--server", "a:1", "--status", "x"],
        &["--server", "a:1", "--status", "--shutdown"],
        &["--server", "a:1", "--status", "--expr-json", AND_JSON],
        &["--server", "a:1", "--status", "--and", "a", "b"],
        &["--server", "a:1", "--shutdown", "--or"],
        &["--server", "a:1", "--shutdown", "--expr", AND_TEXT],
        &["--server", "a:1", "--index", "d", "x"],
        &["--and", "--or", "a", "b"],
        &["--and", "--expr-json", AND_JSON],
        &["--or", "--expr-json", AND_JSON],
        &["--or", "--expr", AND_TEXT],
        &["--expr", AND_TEXT, "x"],
        &["--regex", "--expr", AND_TEXT],
        &["--expr", AND_TEXT, "--expr-json", AND_JSON],
        &["--expr-json", AND_JSON, "x"],
        &["--expr-json", "-", "--", "x"],
        &["--regex", "--expr-json", AND_JSON],
    ] {
        let e = err(words);
        assert!(e.starts_with("error: "), "{words:?}: {e}");
    }
    for (words, want) in [
        (&["--and"][..], "error: --and needs at least two terms"),
        (&["--or", "a"], "error: --or needs at least two terms"),
        (
            &["--server", "a:1", "--or", "a"],
            "error: --or needs at least two terms",
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
    assert!(run(&["-h"]).contains("Usage: fmgrep [OPTIONS] [TERM]..."));
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

/// The TERMs and PATHs of `words`.
fn split(words: &[&str]) -> (Vec<String>, Vec<String>) {
    let s = search(words);
    (params(words).terms, s.paths().unwrap())
}

#[test]
fn trailing_existing_paths_are_paths() {
    assert_eq!(
        split(&["a", "b", "src", "Cargo.toml"]),
        (
            vec!["a".into(), "b".into()],
            vec!["src".into(), "Cargo.toml".into()]
        )
    );
    let (terms, paths) = split(&["a", "src", "b"]);
    assert_eq!((terms.len(), paths.len()), (3, 0));
    assert!(!params(&["a", "src"]).output_config().banner);
}

#[test]
fn the_first_positional_is_always_a_term() {
    assert_eq!(
        split(&["src", "Cargo.toml"]),
        (vec!["src".into()], vec!["Cargo.toml".into()])
    );
    let (terms, paths) = split(&["a", "/src/"]);
    assert_eq!((terms.len(), paths.len()), (2, 0));
}

#[test]
fn under_expr_every_positional_is_a_path() {
    let (terms, paths) = split(&["--expr", AND_TEXT, "src", "Cargo.toml"]);
    assert_eq!(
        (terms.len(), paths),
        (0, vec!["src".into(), "Cargo.toml".into()])
    );
    assert_eq!(split(&["--expr-json", AND_JSON, "--", "src"]).1, ["src"]);
    assert_eq!(
        err(&["--expr", AND_TEXT, "src", "x"]),
        "error: x: No such file or directory"
    );
}

#[test]
fn one_file_path_drops_the_file_name() {
    let named = |words: &[&str]| params(words).filename;
    assert!(!named(&["a", "Cargo.toml"]));
    assert!(named(&["a", "src"]));
    assert!(named(&["a", "Cargo.toml", "src"]));
    assert!(named(&["a"]));
    assert!(!named(&["--expr", AND_TEXT, "Cargo.toml"]));
    assert!(!params(&["-l", "a", "Cargo.toml"]).output_config().filename);
}

#[test]
fn and_or_count_terms_without_their_paths() {
    assert_eq!(
        params(&["--and", "a", "/b+/", "src"]),
        params(&["--expr-json", AND_JSON, "src"])
    );
    assert_eq!(
        err(&["--or", "a", "src"]),
        "error: --or needs at least two terms"
    );
}

use super::*;

/// Fixture text: these tests only exercise argv.
fn spec() -> Cli {
    Cli {
        usage: "usage: bench-fixture [OPTIONS] TERM... | bench-fixture index [PATH] | bench-fixture serve [PATH]",
        help: "bench-fixture help",
    }
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

fn index_args(words: &[&str]) -> IndexArgs {
    match parse_args(&spec(), &argv(words)).unwrap() {
        Parsed::Cmd(Command::Index(a)) => a,
        _ => panic!("expected index for {words:?}"),
    }
}

fn query_args(words: &[&str]) -> QueryArgs {
    match parse_args(&spec(), &argv(words)).unwrap() {
        Parsed::Cmd(Command::Query(a)) => a,
        _ => panic!("expected query for {words:?}"),
    }
}

/// The fixture defaults, so a test only states what its flags change.
fn plain_query() -> QueryArgs {
    QueryArgs {
        index: None,
        regex: false,
        multiline: false,
        dotall: false,
        ignore_case: false,
        json: false,
        approx_counts: false,
        occurrences: false,
        format: None,
        before: 0,
        after: 0,
        heading: true,
        line_numbers: true,
        color: ColorMode::Auto,
        terms: vec![],
        expr: None,
        expr_stdin: false,
    }
}

#[test]
fn parses_index_argv() {
    assert_eq!(
        index_args(&["index", "corpus/"]),
        IndexArgs {
            folder: PathBuf::from("corpus/"),
            output: None,
            store_docs: false,
            json: false,
            verbose: false,
        }
    );
    // `=` form, flags before the folder, explicit output + --json.
    assert_eq!(
        index_args(&[
            "index",
            "--store-docs",
            "--output=/tmp/idx",
            "--json",
            "corpus",
        ]),
        IndexArgs {
            folder: PathBuf::from("corpus"),
            output: Some(PathBuf::from("/tmp/idx")),
            store_docs: true,
            json: true,
            verbose: false,
        }
    );
    // PATH defaults to the cwd.
    assert_eq!(
        index_args(&["index", "--store-docs"]).folder,
        PathBuf::from(".")
    );
    for flag in ["--verbose", "-v"] {
        let got = index_args(&["index", "corpus", flag]);
        assert!(got.verbose, "flag {flag}");
    }
}

#[test]
fn index_separator_keeps_dash_leading_folder() {
    // Everything after `--` is the folder positional, `--help` included.
    let got = index_args(&["index", "--store-docs", "--", "-dashdir"]);
    assert_eq!(got.folder, PathBuf::from("-dashdir"));
    assert!(got.store_docs);
    let got = index_args(&["index", "--", "--help"]);
    assert_eq!(got.folder, PathBuf::from("--help"));
}

#[test]
fn parses_query_argv() {
    assert_eq!(query_args(&["--"]), plain_query());
    assert_eq!(
        query_args(&[
            "--index",
            "/tmp/idx",
            "--regex",
            "--matches",
            "--json",
            "printk",
            "/a|b/",
        ]),
        QueryArgs {
            index: Some(PathBuf::from("/tmp/idx")),
            regex: true,
            json: true,
            format: Some(Format::Lines),
            terms: vec!["printk".to_string(), "/a|b/".to_string()],
            ..plain_query()
        }
    );
    // `--` keeps dash-leading terms out of flag parsing.
    let got = query_args(&["--", "--regex", "-x"]);
    assert_eq!(got.terms, vec!["--regex".to_string(), "-x".to_string()]);
    assert!(!got.regex);
}

#[test]
fn ignore_case_parses_in_both_spellings() {
    for words in [vec!["-i"], vec!["--ignore-case"]] {
        assert_eq!(
            query_args(&words),
            QueryArgs {
                ignore_case: true,
                ..plain_query()
            },
            "argv {words:?}"
        );
    }
    // A term of the same spelling after `--` stays a term.
    assert_eq!(query_args(&["--", "-i"]).terms, vec!["-i".to_string()]);
    assert!(!query_args(&["--", "-i"]).ignore_case);
}

#[test]
fn parses_output_flags() {
    for (words, want) in [
        (
            vec!["-N"],
            QueryArgs {
                line_numbers: false,
                ..plain_query()
            },
        ),
        (
            vec!["--no-line-number"],
            QueryArgs {
                line_numbers: false,
                ..plain_query()
            },
        ),
        (vec!["-N", "-n"], plain_query()),
        (
            vec!["--no-heading"],
            QueryArgs {
                heading: false,
                ..plain_query()
            },
        ),
        (vec!["--no-heading", "--heading"], plain_query()),
        (
            vec!["--color", "always"],
            QueryArgs {
                color: ColorMode::Always,
                ..plain_query()
            },
        ),
        (
            vec!["--color=never"],
            QueryArgs {
                color: ColorMode::Never,
                ..plain_query()
            },
        ),
        (
            vec!["-c"],
            QueryArgs {
                format: Some(Format::CountLines),
                ..plain_query()
            },
        ),
        (
            vec!["--count"],
            QueryArgs {
                format: Some(Format::CountLines),
                ..plain_query()
            },
        ),
        (
            vec!["--count-matches"],
            QueryArgs {
                format: Some(Format::CountMatches),
                ..plain_query()
            },
        ),
        (
            vec!["-l"],
            QueryArgs {
                format: Some(Format::FilesWithMatches),
                ..plain_query()
            },
        ),
        (
            vec!["--files-with-matches"],
            QueryArgs {
                format: Some(Format::FilesWithMatches),
                ..plain_query()
            },
        ),
        (
            vec!["-o"],
            QueryArgs {
                format: Some(Format::OnlyMatching),
                ..plain_query()
            },
        ),
        (
            vec!["--only-matching"],
            QueryArgs {
                format: Some(Format::OnlyMatching),
                ..plain_query()
            },
        ),
        (
            vec!["--approx-counts"],
            QueryArgs {
                approx_counts: true,
                ..plain_query()
            },
        ),
    ] {
        assert_eq!(query_args(&words), want, "argv {words:?}");
    }
}

#[test]
fn last_output_mode_flag_wins() {
    assert_eq!(
        query_args(&["-c", "-l"]).format,
        Some(Format::FilesWithMatches)
    );
    assert_eq!(query_args(&["-l", "-c"]).format, Some(Format::CountLines));
    assert_eq!(
        query_args(&["-l", "--count-matches"]).format,
        Some(Format::CountMatches)
    );
    assert_eq!(
        query_args(&["-o", "-c", "-o"]).format,
        Some(Format::OnlyMatching)
    );
    // `--approx-counts` is in the same last-wins family: it drops a printing
    // format, and a printing format drops it.
    let counts = query_args(&["-c", "--approx-counts"]);
    assert!(counts.approx_counts && counts.format.is_none());
    let printed = query_args(&["--approx-counts", "-c"]);
    assert!(!printed.approx_counts && printed.format == Some(Format::CountLines));
}

#[test]
fn occurrences_flag_joins_the_last_wins_family() {
    for (words, want) in [
        (&["--occurrences"][..], QueryOutput::Occurrences),
        (&["-c", "--occurrences"], QueryOutput::Occurrences),
        (&["--occurrences", "-c"], QueryOutput::CountLines),
        (
            &["--approx-counts", "--occurrences"],
            QueryOutput::Occurrences,
        ),
        (&["--occurrences", "--approx-counts"], QueryOutput::Counts),
        (
            &["--json", "--occurrences", "-C2"],
            QueryOutput::Occurrences,
        ),
    ] {
        let q = query_args(words);
        assert_eq!(
            (q.output(false), q.output(true)),
            (want, want),
            "argv {words:?}"
        );
    }
    assert!(!query_args(&["--json", "--occurrences"]).emits_events());
    assert!(
        parse_args(&spec(), &argv(&["--occurrences=1"]))
            .unwrap_err()
            .contains("unknown argument")
    );
}

#[test]
fn server_occurrences_sets_the_param() {
    let params = |words: &[&str]| match parsed(words) {
        Command::Client(ClientArgs {
            action: ClientAction::Query { params, .. },
            ..
        }) => params,
        other => panic!("expected a client query, got {other:?}"),
    };
    let on = params(&["--server", "x:1", "--occurrences", "a"]);
    assert!(on.occurrences);
    assert!(!params(&["--server", "x:1", "--occurrences", "--approx-counts", "a"]).occurrences);
    assert!(params(&["--server", "x:1", "--approx-counts", "--occurrences", "a"]).occurrences);
    let wire = |p: &QueryParams| serde_json::to_string(p).unwrap();
    assert!(wire(&on).contains(r#""occurrences":true"#));
    assert!(
        !wire(&QueryParams::default()).contains("occurrences"),
        "default requests keep their bytes"
    );
}

#[test]
fn context_flags_follow_rg_precedence() {
    let ctx = |words: &[&str]| {
        let q = query_args(words);
        (q.before, q.after)
    };
    assert_eq!(ctx(&["-C", "2"]), (2, 2));
    assert_eq!(ctx(&["-C2"]), (2, 2));
    assert_eq!(ctx(&["-C=2"]), (2, 2));
    assert_eq!(ctx(&["--context", "2"]), (2, 2));
    assert_eq!(ctx(&["-A3"]), (0, 3));
    assert_eq!(ctx(&["-B1", "-A2"]), (1, 2));
    // `-A`/`-B` override `-C` in either order.
    assert_eq!(ctx(&["-C2", "-A0"]), (2, 0));
    assert_eq!(ctx(&["-A0", "-C2"]), (2, 0));
    assert_eq!(ctx(&["--after-context", "1", "-C", "5"]), (5, 1));
    // -C0 leaves the `--` separator off.
    assert!(!query_args(&["-C0"]).config(false).context);
    assert!(query_args(&["-C1"]).config(false).context);
}

#[test]
fn json_defaults_to_counts_human_defaults_to_matches() {
    let q = query_args(&["-C1"]);
    assert_eq!(q.output(false), QueryOutput::Counts);
    assert_eq!(
        q.output(true),
        QueryOutput::Matches {
            before: 1,
            after: 1
        }
    );
    // An explicit mode flag means the same thing on both paths.
    for words in [
        vec!["-c"],
        vec!["-l"],
        vec!["--count-matches"],
        vec!["--matches"],
        vec!["-o"],
    ] {
        let q = query_args(&words);
        assert_eq!(q.output(false), q.output(true), "argv {words:?}");
    }
    // -o prints spans but drives the same extraction as --matches.
    assert_eq!(
        query_args(&["-o"]).output(true),
        query_args(&["--matches"]).output(true)
    );
}

#[test]
fn approx_counts_flag_pins_the_wire_drive_on_both_paths() {
    // What the harness spawns: the counts-only drive whatever the flagless
    // default does, and with the context flags still parsed but unused.
    for words in [
        vec!["--approx-counts"],
        vec!["--json", "--approx-counts"],
        vec!["--approx-counts", "-C2"],
    ] {
        let q = query_args(&words);
        assert_eq!(q.output(false), QueryOutput::Counts, "argv {words:?}");
        assert_eq!(q.output(true), QueryOutput::Counts, "argv {words:?}");
    }
}

#[test]
fn color_auto_follows_the_terminal_only() {
    for (words, tty, want) in [
        (vec!["--"], false, false),
        (vec!["--"], true, true),
        (vec!["--color", "never"], true, false),
        (vec!["--color", "always"], false, true),
    ] {
        assert_eq!(query_args(&words).config(tty).color, want, "argv {words:?}");
    }
}

#[test]
fn banner_needs_several_terms() {
    assert!(!query_args(&["one"]).config(false).banner);
    assert!(query_args(&["one", "two"]).config(false).banner);
}

#[test]
fn no_args_and_help_flags_print_usage() {
    for words in [vec![], vec!["--help"], vec!["-h"]] {
        assert_eq!(parse_args(&spec(), &argv(&words)).unwrap(), Parsed::Help);
    }
    for words in [
        vec!["index", "--help"],
        vec!["serve", "--help"],
        vec!["x", "-h"],
        vec!["--index", "x", "--help"],
    ] {
        assert_eq!(parse_args(&spec(), &argv(&words)).unwrap(), Parsed::Help);
    }
}

#[test]
fn rejects_bad_argv() {
    let s = spec();
    let bad = |words: &[&str]| parse_args(&s, &argv(words)).unwrap_err();
    // An index flag outside `index` is a search's unknown argument.
    assert!(bad(&["--store-docs", "x"]).contains("unknown argument"));
    // index: empty values, extra positionals, a valued switch.
    assert!(bad(&["index", "c", "--output"]).contains("--output requires a value"));
    assert!(bad(&["index", "c", "--output="]).contains("--output requires a value"));
    assert!(bad(&["index", "a", "b"]).contains("unexpected positional argument"));
    assert!(bad(&["index", "c", "--store-docs=x"]).contains("unknown argument"));
    // Bare markers take no value: any `=` spelling is an unknown argument,
    // never a silent mode flip.
    for spelling in [
        "--regex=false",
        "--regex=",
        "--json=true",
        "--matches=1",
        "--approx-counts=1",
        "--no-heading=0",
        "-N=1",
    ] {
        assert!(
            bad(&[spelling]).contains("unknown argument"),
            "spelling {spelling}"
        );
    }
    assert!(bad(&["--bogus"]).contains("unknown argument"));
    // Every argv error quotes the usage line.
    assert!(bad(&["--bogus"]).contains(s.usage));
}

#[test]
fn rejects_bad_context_and_color_values() {
    let s = spec();
    let bad = |words: &[&str]| parse_args(&s, &argv(words)).unwrap_err();
    assert_eq!(
        bad(&["-A", "x"]),
        "error: invalid -A \"x\"; expected a number"
    );
    assert_eq!(
        bad(&["-C-1"]),
        "error: invalid -C \"-1\"; expected a number"
    );
    assert_eq!(
        bad(&["--context", "1.5"]),
        "error: invalid --context \"1.5\"; expected a number"
    );
    assert!(bad(&["-B"]).contains("-B requires a value"));
    assert_eq!(
        bad(&["--color", "sometimes"]),
        "error: invalid --color \"sometimes\"; expected auto|always|never"
    );
    assert!(bad(&["--color"]).contains("--color requires a value"));
}

#[test]
fn json_matches_selects_the_event_stream() {
    for words in [
        &["--json", "--matches"][..],
        &["--json", "-o"][..],
        &["--json", "-C", "2", "--matches"][..],
    ] {
        assert!(query_args(words).emits_events(), "{words:?}");
    }
    for words in [
        &["--json"][..],
        &["--json", "--approx-counts"][..],
        &["--json", "-c"][..],
        &["--json", "-l"][..],
        &["--json", "--count-matches"][..],
        &["--json", "--matches", "--approx-counts"][..],
    ] {
        assert!(!query_args(words).emits_events(), "{words:?}");
    }
    assert!(
        !query_args(&["--json", "-N", "--matches"])
            .json_config()
            .line_numbers
    );
}

#[test]
fn multiline_flags_reach_the_matcher_and_both_printers() {
    for words in [vec!["-U"], vec!["--multiline"]] {
        let q = query_args(&words);
        assert!(q.multiline, "argv {words:?}");
        assert!(q.mode().multiline, "argv {words:?}");
        assert!(!q.mode().dotall, "argv {words:?}");
        assert!(q.config(false).multiline, "argv {words:?}");
        assert!(q.json_config().multiline, "argv {words:?}");
    }
    // --multiline-dotall is a regex flag rg only reads under -U, and takes
    // no value in either spelling.
    let q = query_args(&["-U", "--multiline-dotall"]);
    assert_eq!(
        q.mode(),
        MatchMode {
            multiline: true,
            dotall: true,
            ignore_case: false
        }
    );
    let q = query_args(&["--multiline-dotall"]);
    assert_eq!(
        q.mode(),
        MatchMode {
            multiline: false,
            dotall: true,
            ignore_case: false
        }
    );
    // Neither is on by default.
    assert_eq!(query_args(&["--"]).mode(), MatchMode::default());
}

fn parsed(words: &[&str]) -> Command {
    match parse_args(&spec(), &argv(words)).unwrap() {
        Parsed::Cmd(c) => c,
        other => panic!("expected a command for {words:?}, got {other:?}"),
    }
}

#[test]
fn parses_serve_argv() {
    let serve = |path: Option<&str>, listen: Option<&str>| {
        Command::Serve(ServeArgs {
            path: path.map(PathBuf::from),
            listen: listen.map(|a| a.parse().unwrap()),
            no_watch: false,
        })
    };
    assert_eq!(parsed(&["serve"]), serve(None, Some("127.0.0.1:0")));
    assert_eq!(
        parsed(&["serve", "idx", "--listen=0.0.0.0:7070"]),
        serve(Some("idx"), Some("0.0.0.0:7070"))
    );
    assert_eq!(
        parsed(&["serve", "--stdio", "idx"]),
        serve(Some("idx"), None)
    );
    assert_eq!(
        parse_args(&spec(), &argv(&["serve", "-h"])).unwrap(),
        Parsed::Help
    );
}

#[test]
fn parses_server_argv() {
    let client = |action: ClientAction| {
        Command::Client(ClientArgs {
            server: "127.0.0.1:7070".into(),
            action,
        })
    };
    let params = QueryParams {
        terms: vec!["a".into(), "--json".into()],
        expr: None,
        regex: true,
        ignore_case: true,
        multiline: true,
        dotall: true,
        approx_counts: true,
        occurrences: false,
    };
    assert_eq!(
        parsed(&[
            "--server=127.0.0.1:7070",
            "--regex",
            "-i",
            "-U",
            "--multiline-dotall",
            "--json",
            "--approx-counts",
            "a",
            "--",
            "--json",
        ]),
        client(ClientAction::Query {
            params,
            json: true,
            expr_stdin: false
        })
    );
    assert_eq!(
        parsed(&["--server", "127.0.0.1:7070", "x"]),
        client(ClientAction::Query {
            params: QueryParams {
                terms: vec!["x".into()],
                ..QueryParams::default()
            },
            json: false,
            expr_stdin: false,
        })
    );
    assert_eq!(
        parsed(&["--status", "--server", "127.0.0.1:7070"]),
        client(ClientAction::Status)
    );
    assert_eq!(
        parsed(&["--server", "127.0.0.1:7070", "--shutdown"]),
        client(ClientAction::Shutdown)
    );
}

#[test]
fn rejects_bad_serve_and_server_argv() {
    let s = spec();
    let bad = |words: &[&str]| parse_args(&s, &argv(words)).unwrap_err();
    assert!(bad(&["serve", "--listen", "127.0.0.1:0", "--stdio"]).contains("exclusive"));
    assert_eq!(
        bad(&["serve", "--listen", "localhost"]),
        "error: invalid --listen \"localhost\"; expected IP:PORT"
    );
    assert!(bad(&["serve", "--listen"]).contains("--listen requires a value"));
    assert!(bad(&["serve", "a", "b"]).contains("unexpected positional argument"));
    assert!(bad(&["--server"]).contains("--server requires a value"));
    assert!(bad(&["--server", "a:1", "--status", "x"]).contains("take no terms"));
    assert!(bad(&["--server", "a:1", "--status", "--shutdown"]).contains("exclusive"));
    for words in [
        &["serve", "--bogus"][..],
        &["serve", "--index", "idx"],
        &["--server", "a:1", "--matches"],
        &["serve", "a", "b"],
    ] {
        assert!(bad(words).contains(s.usage), "argv {words:?}");
    }
}

#[test]
fn server_refuses_local_only_flags() {
    let s = spec();
    for flag in LOCAL_ONLY {
        let value = match flag {
            "--index" => Some("idx"),
            "--color" => Some("never"),
            "-A" | "-B" | "-C" | "--after-context" | "--before-context" | "--context" => Some("1"),
            _ => None,
        };
        let mut words = vec!["--server", "a:1", flag];
        words.extend(value);
        words.push("x");
        let err = parse_args(&s, &argv(&words)).unwrap_err();
        assert!(
            err.starts_with(&format!("error: {flag} needs a local index")),
            "{words:?}: {err}"
        );
        assert!(err.contains(s.usage), "{words:?}: {err}");
    }
    // The flag is named as spelled, even before --server.
    let err = parse_args(&s, &argv(&["-A3", "--server", "a:1", "x"])).unwrap_err();
    assert!(err.starts_with("error: -A needs a local index"), "{err}");
}

#[test]
fn status_and_shutdown_need_server() {
    let s = spec();
    for flag in ["--status", "--shutdown"] {
        let err = parse_args(&s, &argv(&[flag])).unwrap_err();
        assert!(
            err.starts_with("error: --status and --shutdown need --server ADDR"),
            "{flag}: {err}"
        );
    }
}

#[test]
fn only_the_first_word_selects_a_subcommand() {
    assert!(matches!(parsed(&["serve", "--stdio"]), Command::Serve(_)));
    assert!(matches!(
        parsed(&["index", "--store-docs"]),
        Command::Index(_)
    ));
    for (words, terms) in [
        (&["--", "index"][..], &["index"][..]),
        (&["--", "serve", "x"], &["serve", "x"]),
        (&["indexes", "x"], &["indexes", "x"]),
        (&["Serve", "x"], &["Serve", "x"]),
        (&["x", "index"], &["x", "index"]),
        (&["-i", "serve"], &["serve"]),
    ] {
        assert_eq!(query_args(words).terms, terms, "{words:?}");
    }
    assert_eq!(
        query_args(&["--index", "idx", "x"]).index,
        Some(PathBuf::from("idx"))
    );
}

#[test]
fn version_flag_prints_the_version() {
    for words in [
        &["-V"][..],
        &["--version"],
        &["x", "-V"],
        &["index", "--version"],
        &["serve", "-V"],
    ] {
        assert_eq!(
            parse_args(&spec(), &argv(words)).unwrap(),
            Parsed::Version,
            "{words:?}"
        );
    }
    assert_eq!(query_args(&["--", "-V"]).terms, ["-V"]);
}

#[test]
fn no_watch_parses_for_serve() {
    let Command::Serve(a) = parsed(&["serve", "--no-watch"]) else {
        panic!("expected serve");
    };
    assert!(a.no_watch);
}

const AND_JSON: &str = r#"{"and":[{"term":"a"},{"regex":"b+"}]}"#;

fn and_expr() -> crate::expr::Expr {
    serde_json::from_str(AND_JSON).unwrap()
}

#[test]
fn server_expr_json_sets_the_param() {
    let Parsed::Cmd(Command::Client(a)) =
        parsed_ok(&["--server", "a:1", "--expr-json", AND_JSON, "--json"])
    else {
        panic!("expected client");
    };
    let ClientAction::Query {
        params,
        json,
        expr_stdin,
    } = a.action
    else {
        panic!("expected a query")
    };
    assert!(json && !expr_stdin);
    assert_eq!(params.expr, Some(and_expr()));
    let wire = serde_json::to_string(&params).unwrap();
    assert!(wire.starts_with(r#"{"terms":[],"expr":{"and":"#), "{wire}");
    // Without --expr-json the request carries no `expr` key at all.
    let plain = serde_json::to_string(&QueryParams {
        terms: vec!["x".into()],
        ..QueryParams::default()
    })
    .unwrap();
    assert_eq!(
        plain,
        r#"{"terms":["x"],"regex":false,"ignore_case":false,"multiline":false,"dotall":false,"approx_counts":true}"#
    );
    let Parsed::Cmd(Command::Client(a)) = parsed_ok(&["--server", "a:1", "--expr-json=-"]) else {
        panic!("expected client");
    };
    assert!(matches!(
        a.action,
        ClientAction::Query {
            expr_stdin: true,
            params: QueryParams { expr: None, .. },
            ..
        }
    ));
}

fn parsed_ok(words: &[&str]) -> Parsed {
    parse_args(&spec(), &argv(words)).unwrap()
}

#[test]
fn query_expr_json_parses() {
    assert_eq!(
        query_args(&["--expr-json", AND_JSON, "-i", "-l"]),
        QueryArgs {
            expr: Some(and_expr()),
            ignore_case: true,
            format: Some(Format::FilesWithMatches),
            ..plain_query()
        }
    );
    assert_eq!(
        query_args(&["--expr-json=-"]),
        QueryArgs {
            expr_stdin: true,
            ..plain_query()
        }
    );
}

#[test]
fn expr_json_excludes_terms_regex_and_status() {
    let s = spec();
    let bad = |words: &[&str]| parse_args(&s, &argv(words)).unwrap_err();
    for (words, want) in [
        (
            &["--expr-json", AND_JSON, "x"][..],
            "an expression takes no terms",
        ),
        (
            &["--expr-json", "-", "--", "x"],
            "an expression takes no terms",
        ),
        (
            &["--regex", "--expr-json", AND_JSON],
            "an expression takes no regex flag",
        ),
        (
            &["--server", "a:1", "--expr-json", AND_JSON, "x"],
            "an expression takes no terms",
        ),
        (
            &["--server", "a:1", "--regex", "--expr-json", "-"],
            "an expression takes no regex flag",
        ),
        (
            &["--server", "a:1", "--status", "--expr-json", AND_JSON],
            "--status and --shutdown take no --expr-json",
        ),
    ] {
        let err = bad(words);
        assert!(
            err.contains(want) && err.contains(s.usage),
            "{words:?}: {err}"
        );
    }
    for (json, want) in [
        ("{", "invalid --expr-json: EOF"),
        (
            r#"{"and":[]}"#,
            "invalid --expr-json: an and/or expression needs at least one operand",
        ),
        (
            r#"{"not":{"term":"a"}}"#,
            "invalid --expr-json: unknown variant `not`",
        ),
    ] {
        let err = bad(&["--expr-json", json]);
        assert!(err.starts_with(&format!("error: {want}")), "{json}: {err}");
    }
    assert!(bad(&["--expr-json"]).contains("--expr-json requires a value"));
}

#[test]
fn and_or_flags_equal_their_expr_json() {
    let same = |flags: &[&str], json: &str| {
        assert_eq!(
            query_args(flags),
            query_args(&["--expr-json", json]),
            "{flags:?}"
        );
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
        query_args(&["--and", "a", "/b+/"]),
        query_args(&["--expr", AND_TEXT])
    );
    assert_eq!(
        query_args(&["-i", "-U", "-l", "--and", "a", "/b+/"]),
        QueryArgs {
            multiline: true,
            ..query_args(&["--expr-json", AND_JSON, "-i", "-l"])
        }
    );
}

#[test]
fn server_and_or_flags_send_the_expression() {
    let params =
        |words: &[&str]| match parsed_ok(&[&["--server", "a:1", "--json"][..], words].concat()) {
            Parsed::Cmd(Command::Client(ClientArgs {
                action: ClientAction::Query { params, .. },
                ..
            })) => params,
            other => panic!("expected a client query for {words:?}: {other:?}"),
        };
    for (flags, json) in [
        (&["--and", "a", "/b+/"][..], AND_JSON),
        (
            &["--regex", "--occurrences", "--or", "a", "b+"],
            r#"{"or":[{"regex":"a"},{"regex":"b+"}]}"#,
        ),
    ] {
        let mut want = params(&["--expr-json", json]);
        want.occurrences = flags.contains(&"--occurrences");
        let got = params(flags);
        assert_eq!(got, want, "{flags:?}");
        assert_eq!(
            serde_json::to_string(&got).unwrap(),
            serde_json::to_string(&want).unwrap()
        );
    }
}

#[test]
fn and_or_flag_misuse_is_refused() {
    let s = spec();
    let bad = |words: &[&str]| parse_args(&s, &argv(words)).unwrap_err();
    for (words, want) in [
        (
            &["--and", "--or", "a", "b"][..],
            "error: --and and --or are exclusive",
        ),
        (
            &["--or", "a", "b", "--and"],
            "error: --and and --or are exclusive",
        ),
        (&["--and"], "error: --and needs at least two terms"),
        (&["--or", "a"], "error: --or needs at least two terms"),
        (
            &["--or", "--expr-json", AND_JSON],
            "error: --or and --expr-json are exclusive",
        ),
        (
            &["--and", "--expr-json", "-", "a", "b"],
            "error: --and and --expr-json are exclusive",
        ),
        (
            &["--or", "--expr", AND_TEXT],
            "error: --or and --expr are exclusive",
        ),
        (
            &["--server", "a:1", "--and", "--expr", AND_TEXT],
            "error: --and and --expr are exclusive",
        ),
        (
            &["--and=x", "a", "b"],
            "error: unknown argument \"--and=x\"",
        ),
        (
            &["--server", "a:1", "--or", "a"],
            "error: --or needs at least two terms",
        ),
        (
            &["--server", "a:1", "--and", "--expr-json", AND_JSON],
            "error: --and and --expr-json are exclusive",
        ),
        (
            &["--server", "a:1", "--status", "--and", "a", "b"],
            "error: --status and --shutdown take no terms",
        ),
        (
            &["--server", "a:1", "--shutdown", "--or"],
            "error: --status and --shutdown take no --and/--or",
        ),
    ] {
        let err = bad(words);
        assert!(
            err.starts_with(want) && err.contains(s.usage),
            "{words:?}: {err}"
        );
    }
}

const AND_TEXT: &str = "a AND /b+/";

#[test]
fn query_expr_parses_text() {
    let want = QueryArgs {
        expr: Some(and_expr()),
        ignore_case: true,
        format: Some(Format::FilesWithMatches),
        ..plain_query()
    };
    assert_eq!(query_args(&["--expr", AND_TEXT, "-i", "-l"]), want);
    assert_eq!(
        query_args(&["-i", &format!("--expr={AND_TEXT}"), "-l"]),
        want
    );
    // The last --expr wins; `-` is a term.
    let last = query_args(&["--expr", "x", "--expr", "-"]);
    assert_eq!(last.expr, Some(crate::expr::Expr::Term("-".into())));
}

#[test]
fn server_expr_sends_the_json_expr() {
    let wire = |words: &[&str]| {
        let Parsed::Cmd(Command::Client(a)) = parsed_ok(words) else {
            panic!("expected client")
        };
        let ClientAction::Query {
            params,
            expr_stdin: false,
            ..
        } = a.action
        else {
            panic!("expected a query")
        };
        serde_json::to_string(&params).unwrap()
    };
    assert_eq!(
        wire(&["--server", "a:1", "-i", "--expr", AND_TEXT]),
        wire(&["--server", "a:1", "-i", "--expr-json", AND_JSON])
    );
    assert_eq!(
        wire(&["--server", "a:1", &format!("--expr={AND_TEXT}")]),
        wire(&["--server", "a:1", &format!("--expr-json={AND_JSON}")])
    );
}

#[test]
fn expr_text_excludes_terms_regex_json_and_status() {
    let s = spec();
    let bad = |words: &[&str]| parse_args(&s, &argv(words)).unwrap_err();
    for (words, want) in [
        (
            &["--expr", AND_TEXT, "x"][..],
            "an expression takes no terms",
        ),
        (
            &["--expr", AND_TEXT, "--", "x"],
            "an expression takes no terms",
        ),
        (
            &["--regex", "--expr", AND_TEXT],
            "an expression takes no regex flag",
        ),
        (
            &["--expr", AND_TEXT, "--expr-json", AND_JSON],
            "--expr and --expr-json are exclusive",
        ),
        (
            &["--expr-json", "-", "--expr", AND_TEXT],
            "--expr and --expr-json are exclusive",
        ),
        (
            &["--server", "a:1", "--expr", AND_TEXT, "x"],
            "an expression takes no terms",
        ),
        (
            &["--server", "a:1", "--regex", "--expr", AND_TEXT],
            "an expression takes no regex flag",
        ),
        (
            &[
                "--server",
                "a:1",
                "--expr-json",
                AND_JSON,
                "--expr",
                AND_TEXT,
            ],
            "--expr and --expr-json are exclusive",
        ),
        (
            &["--server", "a:1", "--shutdown", "--expr", AND_TEXT],
            "--status and --shutdown take no --expr (",
        ),
    ] {
        let err = bad(words);
        assert!(
            err.contains(want) && err.contains(s.usage),
            "{words:?}: {err}"
        );
    }
    assert!(bad(&["--expr"]).contains("--expr requires a value"));
    assert!(bad(&["--expr="]).contains("--expr requires a value"));
    assert!(REGEX_REFUSAL.contains("/../"));
}

#[test]
fn expr_text_error_carries_a_caret() {
    let err = parse_args(&spec(), &argv(&["--expr", "a b"])).unwrap_err();
    assert_eq!(
        err,
        "error: invalid --expr: expected AND, OR or ')' (quote a term that holds spaces or parentheses)\n  a b\n    ^"
    );
    let err = parse_args(&spec(), &argv(&["--server", "a:1", "--expr", "   "])).unwrap_err();
    assert_eq!(err, "error: invalid --expr: empty expression\n     \n  ^");
}

use super::*;

fn parse(json: &str) -> Result<QueryParams, String> {
    serde_json::from_str(json).map_err(|e| e.to_string())
}

#[test]
fn mode_defaults_to_counts() {
    for (json, want) in [
        (r#"{"terms":["a"]}"#, Mode::Counts),
        (
            r#"{"terms":["a"],"mode":"json_matches"}"#,
            Mode::JsonMatches,
        ),
        (
            r#"{"terms":["a"],"mode":"count_matches"}"#,
            Mode::CountMatches,
        ),
    ] {
        assert_eq!(parse(json).unwrap().mode, want, "{json}");
    }
    assert!(
        parse(r#"{"terms":["a"],"mode":"bogus"}"#)
            .unwrap_err()
            .contains("unknown variant")
    );
    let p = QueryParams {
        terms: vec!["x".into()],
        ..QueryParams::default()
    };
    assert_eq!(
        serde_json::to_string(&p).unwrap(),
        r#"{"terms":["x"],"regex":false,"ignore_case":false,"multiline":false,"dotall":false,"mode":"counts"}"#
    );
}

#[test]
fn fields_before_mode_are_refused() {
    let err = parse(r#"{"terms":["a"],"approx_counts":true}"#).unwrap_err();
    assert!(err.starts_with("unknown field `approx_counts`"), "{err}");
}

#[test]
fn defaults_are_local_defaults() {
    let p = parse(r#"{"terms":["a"],"mode":"lines"}"#).unwrap();
    assert_eq!(
        (p.context, p.line_numbers, p.heading, p.color),
        (Context::default(), true, true, Color::Never)
    );
    let cfg = p.output_config();
    assert!(cfg.heading && cfg.line_numbers && !cfg.context && !cfg.color && !cfg.banner);
    let p = parse(r#"{"terms":["a","b"],"mode":"only","context":{"after":2},"line_numbers":false,"heading":false,"color":"always"}"#).unwrap();
    assert_eq!(
        p.context,
        Context {
            before: 0,
            after: 2
        }
    );
    let cfg = p.output_config();
    assert!(!cfg.heading && !cfg.line_numbers && cfg.context && cfg.color && cfg.banner);
    assert_eq!(cfg.format, Format::OnlyMatching);
    assert!(parse(r#"{"terms":["a"],"context":{"before":-1}}"#).is_err());
    assert!(parse(r#"{"terms":["a"],"context":{"around":1}}"#).is_err());
}

#[test]
fn color_auto_is_refused() {
    let err = parse(r#"{"terms":["a"],"mode":"lines","color":"auto"}"#).unwrap_err();
    assert!(err.contains("unknown variant `auto`"), "{err}");
}

#[test]
fn text_modes_map_to_query_outputs() {
    let context = Context {
        before: 1,
        after: 2,
    };
    let lines = QueryOutput::Matches {
        before: 1,
        after: 2,
        line_numbers: true,
    };
    for (mode, output, format) in [
        (Mode::Counts, QueryOutput::Counts, Format::Lines),
        (Mode::Lines, lines, Format::Lines),
        (Mode::Only, lines, Format::OnlyMatching),
        (Mode::JsonMatches, lines, Format::Lines),
        (
            Mode::Files,
            QueryOutput::FilesWithMatches,
            Format::FilesWithMatches,
        ),
        (Mode::Count, QueryOutput::CountLines, Format::CountLines),
        (
            Mode::CountMatches,
            QueryOutput::CountMatches,
            Format::CountMatches,
        ),
    ] {
        let p = QueryParams {
            mode,
            context,
            ..QueryParams::default()
        };
        assert_eq!(p.output(), output, "{mode:?}");
        assert_eq!(p.output_config().format, format, "{mode:?}");
    }
    let unnumbered = parse(r#"{"terms":["a"],"mode":"lines","line_numbers":false}"#).unwrap();
    assert_eq!(
        unnumbered.output(),
        QueryOutput::Matches {
            before: 0,
            after: 0,
            line_numbers: false,
        }
    );
    assert!(!unnumbered.output_config().line_numbers && !unnumbered.json_config().line_numbers);
}

#[test]
fn an_expression_is_sent_with_empty_terms() {
    let p = QueryParams {
        expr: Some(serde_json::from_str(r#"{"and":[{"term":"a"},{"regex":"b+"}]}"#).unwrap()),
        ..QueryParams::default()
    };
    let wire = serde_json::to_string(&p).unwrap();
    assert!(wire.starts_with(r#"{"terms":[],"expr":{"and":"#), "{wire}");
    assert_eq!(parse(&wire).unwrap(), p);
}

#[test]
fn filter_fields_are_omitted_when_empty() {
    let p = QueryParams {
        terms: vec!["x".into()],
        ..QueryParams::default()
    };
    assert!(p.filter().unwrap().is_none());
    let json = serde_json::to_string(&p).unwrap();
    assert!(!json.contains("glob") && !json.contains("paths"), "{json}");
    let p = parse(
        r#"{"terms":["a"],"globs":["*.c"],"iglobs":["*.H"],"glob_case_insensitive":true,"paths":["src"]}"#,
    )
    .unwrap();
    assert_eq!(
        (p.globs, p.iglobs, p.glob_case_insensitive, p.paths),
        (
            vec!["*.c".into()],
            vec!["*.H".into()],
            true,
            vec!["src".into()]
        )
    );
}

#[test]
fn a_bad_glob_fails_the_request() {
    let p = parse(r#"{"terms":["a"],"globs":["a[b"]}"#).unwrap();
    let manifest: Manifest = serde_json::from_str(
        r#"{"format":1,"store_docs":true,"num_docs":0,"num_bytes":0,"bom_bytes":0,"index_size_bytes":0,"build_time_us":0,"files":[],"blobs":[],"doc_lens":[],"blob_lens":[]}"#,
    )
    .unwrap();
    let err = p.request(Path::new("idx"), &manifest, None).err().unwrap();
    assert!(err.starts_with("error: invalid glob 'a[b'"), "{err}");
}

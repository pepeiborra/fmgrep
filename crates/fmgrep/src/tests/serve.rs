//! `serve` / `--server` over both layouts: same answers as a local search.

use std::io::Write;
use std::sync::mpsc;

use super::*;

use crate::cli::dispatch_to;

fn staged(tmp: &Path, store_docs: bool) -> PathBuf {
    let folder = stage_corpus(
        tmp,
        &[
            ("a.c", b"printk one\nnothing\nPrintK two printk\n"),
            ("b.c", b"aaaa\ntail printk"),
        ],
    );
    let output = tmp.join(format!("idx-{store_docs}"));
    index_it(&folder, &output, store_docs);
    output
}

/// `<flags> -- <terms>` through the discarding dispatcher.
fn query_with(index: &Path, flags: &[&str], terms: &[&str]) -> OutputReport {
    let mut words = argv(&["--index", &index.display().to_string()]);
    words.extend(argv(flags));
    words.push("--".into());
    words.extend(argv(terms));
    match dispatch(&cli_spec(), &words).unwrap() {
        Outcome::Query(r) => r,
        _ => panic!("expected a query report"),
    }
}

/// Timings zeroed: they are latencies, not answers.
fn timeless(mut r: OutputReport) -> OutputReport {
    r.index_time_us = 0;
    for q in &mut r.queries {
        q.latency_us = 0;
    }
    r
}

/// `serve --stdio` over `requests`; one output line per response.
fn serve_stdio(index: &Path, requests: &[&str]) -> Vec<serde_json::Value> {
    let input = requests
        .iter()
        .map(|r| format!("{r}\n"))
        .collect::<String>();
    let mut out = Vec::new();
    let words = argv(&["serve", "--stdio", &index.display().to_string()]);
    assert!(matches!(
        dispatch_to(&cli_spec(), &words, &mut input.as_bytes(), &mut out, false),
        Ok(Outcome::Done)
    ));
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn result_report(response: &serde_json::Value) -> OutputReport {
    timeless(serde_json::from_value(response["result"].clone()).unwrap())
}

#[test]
fn serve_stdio_matches_query() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let got = serve_stdio(
            &index,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"terms":["printk","aa","/pr[i1]ntk/",""]}}"#,
                r#"{"jsonrpc":"2.0","id":"two","method":"query","params":{"terms":["pr.ntk"],"regex":true}}"#,
                r#"{"jsonrpc":"2.0","id":3,"method":"query","params":{"terms":["printk"],"ignore_case":true,"approx_counts":true}}"#,
                "",
                r#"{"jsonrpc":"2.0","id":4,"method":"status"}"#,
                r#"{"jsonrpc":"2.0","id":5,"method":"shutdown"}"#,
                r#"{"jsonrpc":"2.0","id":6,"method":"status"}"#,
            ],
        );
        assert_eq!(got.len(), 5, "store_docs={store_docs}: {got:?}");
        let want = [
            query_with(&index, &[], &["printk", "aa", "/pr[i1]ntk/", ""]),
            query_with(&index, &["--regex"], &["pr.ntk"]),
            query_with(&index, &["-i"], &["printk"]),
        ];
        for (response, want) in got.iter().zip(want) {
            assert_eq!(response["jsonrpc"], "2.0", "store_docs={store_docs}");
            assert_eq!(
                result_report(response),
                timeless(want),
                "store_docs={store_docs}"
            );
        }
        assert_eq!(got[1]["id"], "two", "store_docs={store_docs}");
        let status = &got[3]["result"];
        assert_eq!(got[3]["id"], 4, "store_docs={store_docs}");
        assert_eq!(status["index_name"], "fmgrep", "store_docs={store_docs}");
        assert_eq!(status["store_docs"], store_docs, "store_docs={store_docs}");
        assert_eq!(status["files"], 2, "store_docs={store_docs}");
        assert_eq!(status["num_bytes"], 53, "store_docs={store_docs}");
        assert_eq!(
            got[4]["result"]["status"], "shutting down",
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn serve_query_occurrences_omits_docs_matched() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let terms = ["printk", "aa", "/pr[i1]ntk/", ""];
        let got = serve_stdio(
            &index,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"terms":["printk","aa","/pr[i1]ntk/",""],"occurrences":true}}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"query","params":{"terms":["printk"],"occurrences":false}}"#,
            ],
        );
        assert_eq!(
            result_report(&got[0]),
            timeless(query_with(&index, &["--occurrences"], &terms)),
            "store_docs={store_docs}"
        );
        for q in got[0]["result"]["queries"].as_array().unwrap() {
            assert!(
                q.get("docs_matched").is_none(),
                "store_docs={store_docs}: {q}"
            );
        }
        assert_eq!(
            got[1]["result"]["queries"][0]["docs_matched"], 2,
            "store_docs={store_docs}"
        );
        let totals = |r: OutputReport| {
            r.queries
                .iter()
                .map(|q| q.total_matches)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            totals(result_report(&got[0])),
            totals(query_with(&index, &[], &terms)),
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn serve_stdio_ends_at_eof() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let got = serve_stdio(
        &index,
        &[r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"terms":["printk"]}}"#],
    );
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["result"]["queries"][0]["total_matches"], 3);
}

#[test]
fn malformed_requests_are_refused_and_serving_continues() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let cases: [(&str, i64, serde_json::Value); 11] = [
        ("{not json", -32700, serde_json::Value::Null),
        (
            r#"[{"jsonrpc":"2.0","id":1,"method":"status"}]"#,
            -32600,
            serde_json::Value::Null,
        ),
        (r#""status""#, -32600, serde_json::Value::Null),
        (
            r#"{"jsonrpc":"1.0","id":2,"method":"status"}"#,
            -32600,
            2.into(),
        ),
        (
            r#"{"jsonrpc":"2.0","method":"status"}"#,
            -32600,
            serde_json::Value::Null,
        ),
        (r#"{"jsonrpc":"2.0","id":3,"method":7}"#, -32600, 3.into()),
        (
            r#"{"jsonrpc":"2.0","id":4,"method":"search"}"#,
            -32601,
            4.into(),
        ),
        (
            r#"{"jsonrpc":"2.0","id":5,"method":"query","params":{"terms":["a"],"bogus":1}}"#,
            -32602,
            5.into(),
        ),
        (
            r#"{"jsonrpc":"2.0","id":6,"method":"query","params":{"terms":"a"}}"#,
            -32602,
            6.into(),
        ),
        (
            r#"{"jsonrpc":"2.0","id":7,"method":"query","params":{"terms":["a"],"approx_counts":false}}"#,
            -32602,
            7.into(),
        ),
        (
            r#"{"jsonrpc":"2.0","id":8,"method":"query","params":{"terms":["("],"regex":true}}"#,
            -32000,
            8.into(),
        ),
    ];
    let mut requests: Vec<&str> = cases.iter().map(|c| c.0).collect();
    requests.push(r#"{"jsonrpc":"2.0","id":9,"method":"query","params":{"terms":["printk"]}}"#);
    let got = serve_stdio(&index, &requests);
    assert_eq!(got.len(), cases.len() + 1, "{got:?}");
    for ((line, code, id), response) in cases.iter().zip(&got) {
        assert_eq!(response["error"]["code"], *code, "{line}: {response}");
        assert_eq!(response["id"], *id, "{line}: {response}");
        assert!(response.get("result").is_none(), "{line}: {response}");
    }
    assert_eq!(
        got[10]["error"]["message"],
        query_it_err(&index, true, &["("]).as_str()
    );
    assert_eq!(
        result_report(&got[11]),
        timeless(query_it(&index, false, &["printk"]))
    );
}

/// Forwards every write to the test thread.
struct Chan(mpsc::Sender<Vec<u8>>);

impl Write for Chan {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(buf.to_vec());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type Server = std::thread::JoinHandle<Result<Outcome, String>>;

/// `serve <flags>` on an ephemeral port; returns its readiness address.
fn spawn_server(index: &Path, flags: &[&str]) -> (String, Server) {
    let (tx, rx) = mpsc::channel();
    let mut words = argv(&["serve", &index.display().to_string()]);
    words.extend(argv(flags));
    let server = std::thread::spawn(move || {
        dispatch_to(&cli_spec(), &words, &mut &b""[..], &mut Chan(tx), false)
    });
    let mut ready = Vec::new();
    while !ready.ends_with(b"\n") {
        ready.extend(rx.recv().expect("readiness line"));
    }
    (
        String::from_utf8(ready).unwrap().trim_end().to_owned(),
        server,
    )
}

fn client(addr: &str, words: &[&str]) -> Result<(Outcome, String), String> {
    let mut all = argv(&["--server", addr]);
    all.extend(argv(words));
    let mut out = Vec::new();
    let outcome = dispatch_to(&cli_spec(), &all, &mut &b""[..], &mut out, false)?;
    Ok((outcome, String::from_utf8(out).unwrap()))
}

#[test]
fn serve_tcp_round_trip() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let (addr, server) = spawn_server(&index, &[]);
        let terms = ["printk", "aa", "/pr[i1]ntk/"];

        let (_, json) = client(&addr, &[&["--json", "--"][..], &terms].concat()).unwrap();
        let report: OutputReport = serde_json::from_str(json.trim_end()).unwrap();
        assert_eq!(
            serde_json::to_string(&report).unwrap(),
            json.trim_end(),
            "store_docs={store_docs}"
        );
        assert_eq!(
            timeless(report),
            timeless(query_it(&index, false, &terms)),
            "store_docs={store_docs}"
        );

        let (_, human) = client(&addr, &["-i", "--", "printk"]).unwrap();
        assert_eq!(
            human, "printk: 4 matches in 2 files\n",
            "store_docs={store_docs}"
        );
        let (_, json) = client(
            &addr,
            &[&["--json", "--occurrences", "--"][..], &terms].concat(),
        )
        .unwrap();
        assert_eq!(
            timeless(serde_json::from_str(json.trim_end()).unwrap()),
            timeless(query_with(&index, &["--occurrences"], &terms)),
            "store_docs={store_docs}"
        );
        let (_, human) = client(&addr, &["--occurrences", "--", "printk"]).unwrap();
        assert_eq!(human, "printk: 3 occurrences\n", "store_docs={store_docs}");

        let (outcome, status) = client(&addr, &["--status"]).unwrap();
        assert!(matches!(outcome, Outcome::Done), "store_docs={store_docs}");
        let status: serde_json::Value = serde_json::from_str(status.trim_end()).unwrap();
        assert_eq!(status["files"], 2, "store_docs={store_docs}");

        let (_, quiet) = client(&addr, &["--shutdown"]).unwrap();
        assert_eq!(quiet, "", "store_docs={store_docs}");
        assert!(
            matches!(server.join().unwrap(), Ok(Outcome::Done)),
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn server_errors_surface_on_the_client() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let (addr, server) = spawn_server(&index, &[]);
    let query = query_it_err(&index, true, &["("]);
    assert_eq!(
        client(&addr, &["--regex", "--", "("]).err().unwrap(),
        format!("error: server: {}", query.strip_prefix("error: ").unwrap())
    );
    // The server survives the error.
    assert!(client(&addr, &["--", "printk"]).is_ok());
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
    assert!(
        client(&addr, &["--status"])
            .err()
            .unwrap()
            .starts_with(&format!("error: {addr}: "))
    );
}

#[test]
fn dispatch_refuses_tcp_serve() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let words = argv(&["serve", &index.display().to_string()]);
    match dispatch(&cli_spec(), &words) {
        Err(e) => assert!(e.contains("serve over TCP needs dispatch_to"), "{e}"),
        Ok(_) => panic!("dispatch must refuse TCP serve"),
    }
}

#[test]
fn serve_load_refusals_match_query() {
    let serve_err = |index: &Path| match dispatch(
        &cli_spec(),
        &argv(&["serve", "--stdio", &index.display().to_string()]),
    ) {
        Err(e) => e,
        Ok(_) => panic!("serve must refuse {}", index.display()),
    };
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let path = index.join("manifest.json");
    let original = std::fs::read_to_string(&path).unwrap();
    for (key, value) in [
        ("engine", serde_json::json!("other")),
        ("num_files_indexed", serde_json::json!(7)),
    ] {
        let mut v: serde_json::Value = serde_json::from_str(&original).unwrap();
        v[key] = value;
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(
            serve_err(&index),
            query_it_err(&index, false, &["printk"]),
            "{key}"
        );
    }
    // A files source whose root is gone refuses at load, as query does.
    let index = staged(tmp.path(), false);
    std::fs::rename(tmp.path().join("corpus"), tmp.path().join("moved")).unwrap();
    let err = serve_err(&index);
    assert!(err.contains("is not a directory"), "got: {err}");
    assert_eq!(err, query_it_err(&index, false, &["printk"]));
}

#[test]
fn serve_reads_a_moved_tree_through_its_dotdir() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let want = timeless(query_with(&index, &["--regex"], &["pr[i1]ntk"]));
    let folder = tmp.path().join("corpus");
    index_it(&folder, &folder.join(INDEX_DIR), false);
    let moved = tmp.path().join("moved");
    std::fs::rename(&folder, &moved).unwrap();
    let words = argv(&["serve", "--stdio", &moved.display().to_string()]);
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"terms":["pr[i1]ntk"],"regex":true}}"#;
    let mut out = Vec::new();
    let served = dispatch_to(
        &cli_spec(),
        &words,
        &mut format!("{request}\n").as_bytes(),
        &mut out,
        false,
    );
    assert!(matches!(served, Ok(Outcome::Done)));
    let response: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(result_report(&response), want);
}

/// `total_matches` per term over `addr`, and the report they came from.
fn served_report(addr: &str, terms: &[&str]) -> (Vec<u64>, String) {
    let mut words = vec!["--json", "--"];
    words.extend(terms);
    let (_, json) = client(addr, &words).unwrap();
    let report: OutputReport = serde_json::from_str(json.trim_end()).unwrap();
    let totals = report.queries.iter().map(|q| q.total_matches).collect();
    (totals, json)
}

fn served_totals(addr: &str, terms: &[&str]) -> Vec<u64> {
    served_report(addr, terms).0
}

/// Apply `change` and poll `addr` until `terms` total `want`, for up to 60 s:
/// a loaded box can hold the watcher's events that long. `change` repeats
/// each poll, as FSEvents can miss changes made just after the watch starts.
fn served_eventually(addr: &str, change: impl Fn(), terms: &[&str], want: &[u64]) {
    let start = std::time::Instant::now();
    change();
    let mut got = served_report(addr, terms);
    while got.0 != want && start.elapsed() < std::time::Duration::from_secs(60) {
        std::thread::sleep(std::time::Duration::from_millis(20));
        change();
        got = served_report(addr, terms);
    }
    assert_eq!(
        got.0,
        want,
        "{terms:?} after {:?}; last report: {}",
        start.elapsed(),
        got.1
    );
}

#[test]
fn serve_rescans_stale_files_per_request() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let corpus = tmp.path().join("corpus");
    std::fs::write(corpus.join("a.c"), b"printk\n").unwrap();
    let (addr, server) = spawn_server(&index, &[]);
    assert_eq!(served_totals(&addr, &["printk", "/pr[i1]ntk/"]), [2, 2]);
    std::fs::write(corpus.join("a.c"), b"printk printk printk\n").unwrap();
    assert_eq!(
        served_totals(&addr, &["printk"]),
        [4],
        "a stale file is read per request"
    );
    // Fresh at load: the watcher sees the change.
    let rewrite_b = || std::fs::write(corpus.join("b.c"), b"aaaa\n").unwrap();
    served_eventually(&addr, rewrite_b, &["printk"], &[3]);
    std::fs::remove_file(corpus.join("a.c")).unwrap();
    served_eventually(&addr, || {}, &["printk", "/pr[i1]ntk/"], &[0, 0]);
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

#[test]
fn serve_no_watch_sees_a_change_only_when_read() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let (addr, server) = spawn_server(&index, &["--no-watch"]);
    std::fs::write(tmp.path().join("corpus").join("b.c"), b"aaaa\n").unwrap();
    // Room for a watcher to act; a slow box only weakens the check.
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(served_totals(&addr, &["printk"]), [3], "the index's count");
    assert_eq!(
        served_totals(&addr, &["/pr[i1]ntk/", "printk"]),
        [2, 2],
        "a read marks the file"
    );
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

#[test]
fn serve_occurrences_locate_a_doc_that_changes_mid_query() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let (addr, server) = spawn_server(&index, &["--no-watch"]);
    std::fs::write(tmp.path().join("corpus").join("b.c"), b"aaaa\n").unwrap();
    // The pattern's read marks b.c stale after the pass counted b.c's
    // indexed printk among the unlocated occurrences.
    let (_, json) = client(
        &addr,
        &["--json", "--occurrences", "--", "/pr[i1]ntk/", "printk"],
    )
    .unwrap();
    let report: OutputReport = serde_json::from_str(json.trim_end()).unwrap();
    let got: Vec<_> = report
        .queries
        .iter()
        .map(|q| (q.total_matches, q.docs_matched))
        .collect();
    assert_eq!(got, [(2, None), (2, None)]);
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

const AND_EXPR: &str = r#"{"and":[{"term":"printk"},{"or":[{"regex":"a{3}"},{"term":"one"}]}]}"#;

/// `--expr-json <expr> <flags>` through the discarding dispatcher.
fn query_expr(index: &Path, flags: &[&str], expr: &str) -> OutputReport {
    let mut words = argv(&["--index", &index.display().to_string(), "--expr-json", expr]);
    words.extend(argv(flags));
    match dispatch(&cli_spec(), &words).unwrap() {
        Outcome::Query(r) => r,
        _ => panic!("expected a query report"),
    }
}

#[test]
fn serve_query_takes_expr() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let got = serve_stdio(
            &index,
            &[
                &format!(
                    r#"{{"jsonrpc":"2.0","id":1,"method":"query","params":{{"expr":{AND_EXPR}}}}}"#
                ),
                &format!(
                    r#"{{"jsonrpc":"2.0","id":2,"method":"query","params":{{"terms":[],"expr":{AND_EXPR},"ignore_case":true}}}}"#
                ),
                r#"{"jsonrpc":"2.0","id":3,"method":"query","params":{"expr":{"or":[{"term":"printk"},{"term":"aa"}]},"occurrences":true}}"#,
            ],
        );
        assert_eq!(
            result_report(&got[0]),
            timeless(query_expr(&index, &[], AND_EXPR)),
            "store_docs={store_docs}"
        );
        assert_eq!(
            result_report(&got[1]),
            timeless(query_expr(&index, &["-i"], AND_EXPR)),
            "store_docs={store_docs}"
        );
        let expr = &got[0]["result"]["queries"][0];
        assert_eq!(
            expr["term"], "printk AND (/a{3}/ OR one)",
            "store_docs={store_docs}"
        );
        assert_eq!(expr["docs_matched"], 2, "store_docs={store_docs}");
        assert_eq!(
            expr["leaves"][0]["term"], "printk",
            "store_docs={store_docs}"
        );
        let occ = &got[2]["result"]["queries"][0];
        assert!(
            occ.get("docs_matched").is_none(),
            "store_docs={store_docs}: {occ}"
        );
        assert_eq!(
            occ["total_matches"],
            4 + 2,
            "store_docs={store_docs}: {occ}"
        );
    }
}

#[test]
fn serve_refuses_bad_expr_params() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let cases: [(&str, i64, &str); 7] = [
        (
            r#"{"terms":["a"],"expr":{"term":"a"}}"#,
            -32602,
            "an expression takes no terms",
        ),
        (
            r#"{"expr":{"term":"a"},"regex":true}"#,
            -32602,
            "an expression takes no regex flag",
        ),
        (r#"{"expr":{"and":[]}}"#, -32602, "at least one operand"),
        (
            r#"{"expr":{"not":{"term":"a"}}}"#,
            -32602,
            "unknown variant",
        ),
        (r#"{}"#, -32602, "missing field `terms`"),
        (
            r#"{"expr":{"and":[{"term":"a"}]},"occurrences":true}"#,
            -32000,
            "cannot answer AND",
        ),
        (r#"{"expr":{"regex":"("}}"#, -32000, "regex parse error"),
    ];
    let mut requests: Vec<String> = cases
        .iter()
        .enumerate()
        .map(|(i, (params, _, _))| {
            format!(r#"{{"jsonrpc":"2.0","id":{i},"method":"query","params":{params}}}"#)
        })
        .collect();
    requests.push(format!(
        r#"{{"jsonrpc":"2.0","id":99,"method":"query","params":{{"expr":{AND_EXPR}}}}}"#
    ));
    let got = serve_stdio(
        &index,
        &requests.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    assert_eq!(got.len(), cases.len() + 1, "{got:?}");
    for ((params, code, want), response) in cases.iter().zip(&got) {
        assert_eq!(response["error"]["code"], *code, "{params}: {response}");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains(want),
            "{params}: {response}"
        );
    }
    assert_eq!(
        result_report(&got[cases.len()]),
        timeless(query_expr(&index, &[], AND_EXPR))
    );
}

#[test]
fn server_expr_json_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let (addr, server) = spawn_server(&index, &[]);
    let want = timeless(query_expr(&index, &[], AND_EXPR));
    let (_, json) = client(&addr, &["--json", "--expr-json", AND_EXPR]).unwrap();
    assert_eq!(
        timeless(serde_json::from_str(json.trim_end()).unwrap()),
        want
    );
    let mut out = Vec::new();
    let words = argv(&["--server", &addr, "--json", "--expr-json", "-"]);
    dispatch_to(
        &cli_spec(),
        &words,
        &mut AND_EXPR.as_bytes(),
        &mut out,
        false,
    )
    .unwrap();
    assert_eq!(timeless(serde_json::from_slice(&out).unwrap()), want);
    let (_, human) = client(&addr, &["--expr-json", AND_EXPR]).unwrap();
    assert_eq!(
        human,
        "printk AND (/a{3}/ OR one): 5 matches in 2 files\n  printk: 3 matches\n  /a{3}/: 1 match\n  one: 1 match\n"
    );
    // A query's stdin expression answers as the inline one.
    let mut out = Vec::new();
    let words = argv(&[
        "--index",
        &index.display().to_string(),
        "--json",
        "--expr-json",
        "-",
    ]);
    dispatch_to(
        &cli_spec(),
        &words,
        &mut AND_EXPR.as_bytes(),
        &mut out,
        false,
    )
    .unwrap();
    assert_eq!(timeless(serde_json::from_slice(&out).unwrap()), want);
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

#[test]
fn server_expr_text_equals_expr_json() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let (addr, server) = spawn_server(&index, &[]);
    let timeless = |s: String| {
        regex::Regex::new(r#""latency_us":\d+"#)
            .unwrap()
            .replace_all(&s, "0")
            .into_owned()
    };
    let or_json = r#"{"or":[{"term":"printk"},{"regex":"a{3}"}]}"#;
    for (flags, text, json) in [
        (&["--json"][..], "printk AND (/a{3}/ OR one)", AND_EXPR),
        (&[], "printk AND (/a{3}/ OR one)", AND_EXPR),
        (&["-i", "--json"], "printk AND (/a{3}/ OR one)", AND_EXPR),
        (&["--occurrences", "--json"], "printk OR /a{3}/", or_json),
    ] {
        let got = client(&addr, &[flags, &["--expr", text]].concat())
            .unwrap()
            .1;
        let want = client(&addr, &[flags, &["--expr-json", json]].concat())
            .unwrap()
            .1;
        assert_eq!(timeless(got), timeless(want), "{flags:?} {text}");
    }
    // The deepest tree the parser takes, two nodes per group, is served.
    let deep = (0..30).fold("a".to_owned(), |e, _| format!("({e} AND b OR c)"));
    client(&addr, &["--expr", &deep]).unwrap();
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

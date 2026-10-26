//! `serve` / `--server` over both layouts: same answers as a local search.

use std::io::Write;
use std::sync::mpsc;

use super::*;

use crate::cli::dispatch_to;

/// `serve --stdio` over `requests`; one output line per response.
fn serve_stdio(index: &Path, requests: &[&str]) -> Vec<serde_json::Value> {
    let input = requests
        .iter()
        .map(|r| format!("{r}\n"))
        .collect::<String>();
    let mut out = Vec::new();
    let words = argv(&["serve", "--stdio", &index.display().to_string()]);
    assert!(matches!(
        dispatch_to(&words, &mut input.as_bytes(), &mut out, false),
        Ok(Outcome::Done)
    ));
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn result_report(response: &serde_json::Value) -> serde_json::Value {
    timeless(serde_json::from_value::<OutputReport>(response["result"].clone()).unwrap())
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
                r#"{"jsonrpc":"2.0","id":3,"method":"query","params":{"terms":["PRINTK"],"ignore_case":true}}"#,
                "",
                r#"{"jsonrpc":"2.0","id":4,"method":"status"}"#,
                r#"{"jsonrpc":"2.0","id":5,"method":"shutdown"}"#,
                r#"{"jsonrpc":"2.0","id":6,"method":"status"}"#,
            ],
        );
        assert_eq!(got.len(), 5, "store_docs={store_docs}: {got:?}");
        let want = [
            query(&index, &[], &["printk", "aa", "/pr[i1]ntk/", ""]).unwrap(),
            query(&index, &["--regex"], &["pr.ntk"]).unwrap(),
            query(&index, &["-i"], &["PRINTK"]).unwrap(),
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
        assert_eq!(status["store_docs"], store_docs, "store_docs={store_docs}");
        assert_eq!(status["files"], 2, "store_docs={store_docs}");
        assert_eq!(status["num_bytes"], 53, "store_docs={store_docs}");
        let mut keys: Vec<&str> = status
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort_unstable();
        let want = [
            "files",
            "index_size_bytes",
            "load_us",
            "num_bytes",
            "root",
            "store_docs",
        ];
        assert_eq!(keys, want, "store_docs={store_docs}");
        let docs = match store_docs {
            true => index.clone(),
            false => PathBuf::from(read_manifest(&index).unwrap().corpus_root.unwrap()),
        };
        let root = std::fs::canonicalize(docs).unwrap().display().to_string();
        assert_eq!(status["root"], root, "store_docs={store_docs}");
        assert_eq!(
            got[4]["result"]["status"], "shutting down",
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
    assert_eq!(got[0]["result"]["queries"][0]["total_matches"], 4);
}

#[test]
fn malformed_requests_are_refused_and_serving_continues() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let cases: [(&str, i64, serde_json::Value); 16] = [
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
            r#"{"jsonrpc":"2.0","id":7,"method":"query","params":{"terms":["a"],"approx_counts":true}}"#,
            -32602,
            7.into(),
        ),
        (
            r#"{"jsonrpc":"2.0","id":8,"method":"query","params":{"terms":["("],"regex":true}}"#,
            -32000,
            8.into(),
        ),
        (
            r#"{"jsonrpc":"2.0","id":10,"method":"query","params":{"terms":["a"],"mode":"lines","color":"auto"}}"#,
            -32602,
            10.into(),
        ),
        (
            r#"{"jsonrpc":"2.0","id":11,"method":"query","params":{"terms":["a"],"mode":"bogus"}}"#,
            -32602,
            11.into(),
        ),
        (
            r#"{"jsonrpc":"2.0","id":12,"method":"query","params":{"terms":["a"],"mode":"lines","context":{"before":-1}}}"#,
            -32602,
            12.into(),
        ),
        (
            r#"{"jsonrpc":"2.0","id":13,"method":"probe","params":{"queries":[]}}"#,
            -32601,
            13.into(),
        ),
        (
            r#"{"jsonrpc":"2.0","id":14,"method":"manifest"}"#,
            -32601,
            14.into(),
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
    let old = got[9]["error"]["message"].as_str().unwrap();
    assert!(
        old.starts_with("invalid params: unknown field `approx_counts`, expected one of"),
        "{old}"
    );
    assert_eq!(
        result_report(&got[cases.len()]),
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
    let server =
        std::thread::spawn(move || dispatch_to(&words, &mut &b""[..], &mut Chan(tx), false));
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
    let outcome = dispatch_to(&all, &mut &b""[..], &mut out, false)?;
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

        let (_, human) = client(&addr, &["-i", "--approx-counts", "--", "PRINTK"]).unwrap();
        assert_eq!(
            human, "PRINTK: 4 matches in 2 files\n",
            "store_docs={store_docs}"
        );
        let (_, lines) = client(&addr, &["-i", "--", "PRINTK"]).unwrap();
        assert_eq!(
            lines, "a.c\n1:printk one\n3:printk two printk\n\nb.c\n2:tail printk\n",
            "store_docs={store_docs}"
        );

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

fn serve_err(index: &Path) -> String {
    match dispatch(argv(&["serve", "--stdio", &index.display().to_string()])) {
        Err(e) => e,
        Ok(_) => panic!("serve must refuse {}", index.display()),
    }
}

#[test]
fn serve_load_refusals_match_query() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let path = index.join("manifest.json");
    let original = std::fs::read_to_string(&path).unwrap();
    for (key, value) in [
        ("format", serde_json::json!(1)),
        ("num_docs", serde_json::json!(7)),
        ("format", serde_json::json!(12)),
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
}

#[test]
fn serve_refuses_an_index_without_its_documents() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    std::fs::remove_file(index.join(DOCS_SHARD)).unwrap();
    let want = format!("error: {} has no docs.shard", index.display());
    assert_eq!(serve_err(&index), want);
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let tree = read_manifest(&index).unwrap().corpus_root.unwrap();
    std::fs::rename(&tree, tmp.path().join("moved")).unwrap();
    assert_eq!(
        serve_err(&index),
        format!("error: {tree} is not a directory")
    );
    std::fs::rename(tmp.path().join("moved"), &tree).unwrap();
    std::fs::remove_file(index.join("docs.stamps")).unwrap();
    let want = format!("error: {} has no docs.stamps", index.display());
    assert_eq!(serve_err(&index), want);
}

#[test]
fn serve_refuses_a_tree_missing_most_files() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let tree = read_manifest(&index).unwrap().corpus_root.unwrap();
    std::fs::remove_file(Path::new(&tree).join("a.c")).unwrap();
    let status = r#"{"jsonrpc":"2.0","id":1,"method":"status"}"#;
    assert_eq!(serve_stdio(&index, &[status])[0]["result"]["files"], 2);
    std::fs::remove_file(Path::new(&tree).join("b.c")).unwrap();
    let root = std::fs::canonicalize(&tree).unwrap();
    let want = format!(
        "error: 2 of 2 indexed files are missing under {}",
        root.display()
    );
    assert_eq!(serve_err(&index), want);
}

#[test]
fn serve_reads_a_moved_tree_through_its_dotdir() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let want = timeless(query(&index, &["--regex"], &["pr[i1]ntk"]).unwrap());
    let folder = tmp.path().join("corpus");
    index_it(&folder, &folder.join(INDEX_DIR), false);
    let moved = tmp.path().join("moved");
    std::fs::rename(&folder, &moved).unwrap();
    let words = argv(&["serve", "--stdio", &moved.display().to_string()]);
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"terms":["pr[i1]ntk"],"regex":true}}"#;
    let mut out = Vec::new();
    let served = dispatch_to(
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
    assert_eq!(served_totals(&addr, &["printk"]), [4], "the index's count");
    assert_eq!(
        served_totals(&addr, &["/pr[i1]ntk/", "printk"]),
        [3, 3],
        "a read marks the file"
    );
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

#[test]
fn serve_finds_files_added_before_and_after_load() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let corpus = tmp.path().join("corpus");
    std::fs::write(corpus.join("ab.c"), b"printk before\n").unwrap();
    let (addr, server) = spawn_server(&index, &[]);
    assert_eq!(served_totals(&addr, &["printk", "/pr[i1]ntk/"]), [5, 5]);
    let add = || {
        std::fs::create_dir_all(corpus.join("new/deep")).unwrap();
        std::fs::write(corpus.join("new/deep/n.c"), b"printk after printk\n").unwrap();
        std::fs::write(corpus.join("bin.dat"), b"printk\0").unwrap();
    };
    served_eventually(&addr, add, &["printk", "/pr[i1]ntk/"], &[7, 7]);
    let (_, listed) = client(&addr, &["-l", "--", "printk"]).unwrap();
    assert_eq!(listed, "a.c\nab.c\nb.c\nnew/deep/n.c\n");
    let remove = || {
        let _ = std::fs::remove_file(corpus.join("ab.c"));
    };
    served_eventually(&addr, remove, &["printk"], &[6]);
    let back = || std::fs::write(corpus.join("ab.c"), b"printk back\n").unwrap();
    served_eventually(&addr, back, &["printk"], &[7]);
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

#[test]
fn serve_no_watch_finds_only_files_added_before_load() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let corpus = tmp.path().join("corpus");
    std::fs::write(corpus.join("ab.c"), b"printk before\n").unwrap();
    let (addr, server) = spawn_server(&index, &["--no-watch"]);
    // The first answer waits for discovery.
    assert_eq!(served_totals(&addr, &["printk"]), [5]);
    std::fs::write(corpus.join("c.c"), b"printk after\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(served_totals(&addr, &["printk"]), [5]);
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

const AND_EXPR: &str = r#"{"and":[{"term":"printk"},{"or":[{"regex":"a{3}"},{"term":"one"}]}]}"#;

fn query_expr(index: &Path, flags: &[&str], expr: &str) -> OutputReport {
    query(index, &[&["--expr-json", expr][..], flags].concat(), &[]).unwrap()
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
    }
}

#[test]
fn serve_refuses_bad_expr_params() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let cases: [(&str, i64, &str); 6] = [
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
        timeless(serde_json::from_str::<OutputReport>(json.trim_end()).unwrap()),
        want
    );
    let mut out = Vec::new();
    let words = argv(&["--server", &addr, "--json", "--expr-json", "-"]);
    dispatch_to(&words, &mut AND_EXPR.as_bytes(), &mut out, false).unwrap();
    assert_eq!(
        timeless(serde_json::from_slice::<OutputReport>(&out).unwrap()),
        want
    );
    let (_, human) = client(&addr, &["--approx-counts", "--expr-json", AND_EXPR]).unwrap();
    assert_eq!(
        human,
        "printk AND (/a{3}/ OR one): 6 matches in 2 files\n  printk: 4 matches\n  /a{3}/: 1 match\n  one: 1 match\n"
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
    dispatch_to(&words, &mut AND_EXPR.as_bytes(), &mut out, false).unwrap();
    assert_eq!(
        timeless(serde_json::from_slice::<OutputReport>(&out).unwrap()),
        want
    );
    // The deepest tree the parser takes, two nodes per group, is served.
    let deep = (0..30).fold("a".to_owned(), |e, _| format!("({e} AND b OR c)"));
    client(&addr, &["--expr", &deep]).unwrap();
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

#[test]
fn served_searches_honour_the_filter() {
    use crate::tests::filter::{FILES, dotted_tree};
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let tree = dotted_tree(tmp.path(), "all", &FILES, store_docs);
        let (addr, server) = spawn_server(&tree.join(INDEX_DIR), &[]);
        let sub = tree.join("sub").display().to_string();
        let b = tree.join("b.h").display().to_string();
        for (flags, terms) in [
            (&["-g", "*.c", "-l"][..], &["printk"][..]),
            (&["--iglob", "*.H", "-n"], &["printk"]),
            (
                &["-g", "!sub", "--json", "--approx-counts"],
                &["printk", "/al[a-z]ha/"],
            ),
            (&["-c"], &["printk", &sub]),
            (&["-g", "!*.h", "-l"], &["printk", &b, &sub]),
            (&["-g", "!z.c", "-l", "--expr", "printk AND alpha"], &[]),
        ] {
            let ctx = format!("store_docs={store_docs} {flags:?} {terms:?}");
            let (_, served) = client(
                &addr,
                &words(&[], flags, terms)
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            assert_eq!(
                untimed(flags, &served),
                untimed(flags, &printed(&tree, flags, terms)),
                "{ctx}"
            );
        }
        let err = client(&addr, &["-g", "*.zz", "--", "printk"])
            .err()
            .unwrap();
        assert_eq!(
            err,
            "error: server: no files were searched: the globs exclude every file"
        );
        client(&addr, &["--shutdown"]).unwrap();
        server.join().unwrap().unwrap();
    }
}

#[test]
fn a_path_needs_the_served_tree() {
    use crate::tests::filter::{FILES, dotted_tree};
    let tmp = tempfile::tempdir().unwrap();
    let tree = dotted_tree(tmp.path(), "all", &FILES, false);
    let index = tmp.path().join("docs");
    index_it(&tree, &index, true);
    let (addr, server) = spawn_server(&index, &[]);
    let sub = tree.join("sub").display().to_string();
    let err = client(&addr, &["--", "printk", &sub]).err().unwrap();
    assert_eq!(
        err,
        "error: a PATH needs the indexed tree; this index records none"
    );
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

mod text;

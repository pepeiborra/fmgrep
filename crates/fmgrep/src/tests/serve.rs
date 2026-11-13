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
    let words = argv(&[
        "serve",
        "--stdio",
        "--no-watch",
        &index.display().to_string(),
    ]);
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
            "format",
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

/// `index`'s manifest, recording served commit `id`.
pub(crate) fn pin(index: &Path, id: &str) {
    let path = index.join("manifest.json");
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    m["commit"] = serde_json::json!({"vcs": "git", "id": id});
    std::fs::write(&path, serde_json::to_string(&m).unwrap()).unwrap();
}

/// A cwd below the served tree scopes a query, unless the query is pinned
/// to the served commit: then only its `paths` scope it.
#[test]
fn a_pinned_query_is_scoped_by_its_paths_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), &TWO_FILES);
    std::fs::create_dir(folder.join("sub")).unwrap();
    std::fs::write(folder.join("sub/c.c"), "printk sub\n").unwrap();
    let index = folder.join(".fmgrep");
    index_it(&folder, &index, true);
    let id = "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d";
    pin(&index, id);
    let cwd = std::fs::canonicalize(folder.join("sub")).unwrap();
    let ask = |extra: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"query","params":{{"terms":["printk"],"mode":"files","cwd":{:?}{extra}}}}}"#,
            cwd.display().to_string()
        )
    };
    let pinned = format!(r#","commit":"{id}""#);
    let sub = format!(r#"{pinned},"paths":[{{"path":"sub","name":""}}]"#);
    let got = serve_stdio(&index, &[&ask(""), &ask(&pinned), &ask(&sub)]);
    let text = |i: usize| got[i]["result"]["text"].as_str().unwrap().to_owned();
    assert_eq!(text(0), "c.c\n");
    assert_eq!(text(1), "a.c\nb.c\nsub/c.c\n");
    assert_eq!(text(2), "c.c\n");
}

/// A query's skip list leaves files out of every mode: of the matches, of
/// the counts and of rg's corpus summary, whose bytes lose the skipped
/// files' less their BOM. The list needs `commit` and `per_file`.
#[test]
fn a_skip_list_leaves_files_out() {
    let tmp = tempfile::tempdir().unwrap();
    let files: [(&str, &[u8]); 4] = [
        ("a.c", b"printk a\n"),
        ("d/b.c", b"\xEF\xBB\xBFprintk b\n"),
        ("d/c.c", b"printk c\n"),
        ("e.c", b"printk e printk\n"),
    ];
    let folder = stage_corpus(tmp.path(), &files[..1]);
    for (rel, bytes) in &files[1..] {
        std::fs::create_dir_all(folder.join(rel).parent().unwrap()).unwrap();
        std::fs::write(folder.join(rel), bytes).unwrap();
    }
    let index = tmp.path().join("idx");
    index_it(&folder, &index, true);
    let id = "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d";
    pin(&index, id);
    let ask = |mode: &str, skip: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"query","params":{{"terms":["printk"],"mode":"{mode}","commit":"{id}","stream":true,"per_file":true,"skip":{skip}}}}}"#
        )
    };
    let skip = r#"["d/","e.c"]"#;
    let mut input = String::new();
    for mode in ["counts", "files", "json_matches"] {
        input.push_str(&ask(mode, skip));
        input.push('\n');
    }
    let mut out = Vec::new();
    let words = argv(&["serve", "--stdio", &index.display().to_string()]);
    dispatch_to(&words, &mut input.as_bytes(), &mut out, false).unwrap();
    let text = String::from_utf8(out).unwrap();
    let counts: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    let q = &counts["result"]["queries"][0];
    assert_eq!(
        (&q["total_matches"], &q["docs_matched"]),
        (&1.into(), &1.into())
    );
    let docs: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix(r#"{"doc":""#))
        .map(|l| l.split('"').next().unwrap())
        .collect();
    assert_eq!(docs, ["a.c", "a.c"], "{text}");
    // One doc of 9 bytes is left of 4 docs of 9, 12 (a BOM), 9 and 16;
    // only rg's JSON summary prints the bytes, so only it reads the BOMs.
    let corpus: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with(r#"{"corpus""#))
        .collect();
    assert_eq!(corpus.last(), Some(&r#"{"corpus":[1,9]}"#), "{text}");

    for (params, why) in [
        (
            r#""skip":["a.c"],"commit":"x","stream":true"#,
            "skip needs commit and per_file",
        ),
        (
            r#""skip":["a.c"],"stream":true,"per_file":true"#,
            "skip needs commit and per_file",
        ),
        (r#""per_file":true"#, "per_file needs stream"),
    ] {
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"query","params":{{"terms":["printk"],{params}}}}}"#
        );
        let got = serve_stdio(&index, &[&line]);
        assert_eq!(got[0]["error"]["code"], -32602, "{params}");
        assert_eq!(
            got[0]["error"]["message"],
            format!("invalid params: {why}"),
            "{params}"
        );
    }
    let many: Vec<String> = (0..10_001).map(|i| format!("f{i}")).collect();
    let line = ask("files", &serde_json::to_string(&many).unwrap());
    let got = serve_stdio(&index, &[&line]);
    assert_eq!(
        got[0]["error"]["message"],
        "invalid params: skip lists 10001 files; the limit is 10000"
    );
}

/// A pinned query whose globs keep it from every file left after its skip
/// list reports so; an unpinned one still fails.
#[test]
fn a_pinned_query_that_searched_no_file_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    let id = "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d";
    pin(&index, id);
    let ask = |params: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"query","params":{{"terms":["printk"],"mode":"files","stream":true,"per_file":true{params}}}}}"#
        )
    };
    let responses = |requests: &[&str]| -> Vec<serde_json::Value> {
        let got = serve_stdio(&index, requests);
        got.into_iter().filter(|v| v.get("id").is_some()).collect()
    };
    let pinned = format!(r#","globs":["a.c"],"commit":"{id}","skip":["a.c"]"#);
    let got = responses(&[&ask(&pinned), &ask(r#","globs":["x"]"#)]);
    assert_eq!(got[0]["result"]["report"]["none_searched"], true, "{got:?}");
    assert_eq!(
        got[1]["error"]["message"],
        "error: no files were searched: the globs exclude every file"
    );
    let got = responses(&[&ask(&format!(r#","globs":["a.c"],"commit":"{id}""#))]);
    assert!(
        got[0]["result"]["report"].get("none_searched").is_none(),
        "{got:?}"
    );
}

/// A query pinned to another format or served commit is refused with
/// -32001; one pinned to this server's is answered.
#[test]
fn a_query_pinned_to_another_index_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let plain = staged(&tmp.path().join("plain"), true);
    let pinned = staged(&tmp.path().join("pinned"), true);
    let id = "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d";
    pin(&pinned, id);
    let ask = |params: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"query","params":{{"terms":["printk"],{params}}}}}"#
        )
    };
    let refused = |index: &Path, params: &str, message: &str| {
        let got = serve_stdio(index, &[&ask(params)]);
        assert_eq!(got[0]["error"]["code"], -32001, "{params}: {got:?}");
        assert_eq!(got[0]["error"]["message"], message, "{params}");
    };
    let ours = format!("this server serves commit {id} of index format 8");
    refused(&pinned, r#""format":7"#, &ours);
    refused(&pinned, r#""commit":"ffff""#, &ours);
    refused(&pinned, &format!(r#""format":7,"commit":"{id}""#), &ours);
    let none = "this server serves no commit, of index format 8";
    refused(&plain, &format!(r#""commit":"{id}""#), none);
    for (index, params) in [
        (&pinned, format!(r#""format":8,"commit":"{id}""#)),
        (&plain, r#""format":8"#.to_owned()),
    ] {
        let got = serve_stdio(index, &[&ask(&params)]);
        assert_eq!(
            got[0]["result"]["queries"][0]["total_matches"], 4,
            "{params}"
        );
    }
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

/// The flags of a server of a tree that does not change while it serves.
const FIXED: &[&str] = &["--no-watch"];

/// `serve <flags>` on an ephemeral port, with this thread's watchman
/// hooks; returns its readiness address.
fn spawn_server(index: &Path, flags: &[&str]) -> (String, Server) {
    let (tx, rx) = mpsc::channel();
    let mut words = argv(&["serve", &index.display().to_string()]);
    words.extend(argv(flags));
    let hooks = Hooks::here();
    let server = std::thread::spawn(move || {
        hooks.apply();
        dispatch_to(&words, &mut &b""[..], &mut Chan(tx), false)
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
    let outcome = dispatch_to(&all, &mut &b""[..], &mut out, false)?;
    Ok((outcome, String::from_utf8(out).unwrap()))
}

#[test]
fn serve_tcp_round_trip() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let (addr, server) = spawn_server(&index, FIXED);
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
            lines, "a.c:printk one\na.c:printk two printk\nb.c:tail printk\n",
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
    let (addr, server) = spawn_server(&index, FIXED);
    let query = query_it_err(&index, true, &["("]);
    assert_eq!(
        client(&addr, &["--regex", "--", "("]).err().unwrap(),
        format!("error: server: {}", query.strip_prefix("error: ").unwrap())
    );
    // The server survives the error.
    assert!(client(&addr, &["--", "printk"]).is_ok());
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
    let gone = client(&addr, &["--status"]).err().unwrap();
    let want = format!("error: cannot reach the server at {addr}: ");
    assert!(gone.starts_with(&want), "{gone}");
}

fn serve_err(index: &Path) -> String {
    match dispatch(argv(&[
        "serve",
        "--stdio",
        "--no-watch",
        &index.display().to_string(),
    ])) {
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
    let words = argv(&[
        "serve",
        "--stdio",
        "--no-watch",
        &moved.display().to_string(),
    ]);
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

/// A private daemon that watches `corpus`, which servers started on this
/// thread ask, keeping their records in `<tmp>/cache`; `None` without
/// watchman.
#[cfg(unix)]
pub(super) fn watching(tmp: &Path, corpus: &Path) -> Option<super::watchman::Daemon> {
    let daemon = super::watchman::Daemon::start()?;
    daemon.watch(corpus);
    WATCHMAN.set(Some(daemon.sock.clone()));
    CACHE.set(Some(tmp.join("cache")));
    Some(daemon)
}

#[test]
fn serve_rescans_stale_files_per_request() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let corpus = tmp.path().join("corpus");
    std::fs::write(corpus.join("a.c"), b"printk\n").unwrap();
    let (addr, server) = spawn_server(&index, FIXED);
    assert_eq!(served_totals(&addr, &["printk", "/pr[i1]ntk/"]), [2, 2]);
    std::fs::write(corpus.join("a.c"), b"printk printk printk\n").unwrap();
    assert_eq!(
        served_totals(&addr, &["printk"]),
        [4],
        "a stale file is read per request"
    );
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

/// Each change is seen by the next query, with no wait: the server asks
/// watchman before it answers.
#[cfg(unix)]
#[test]
fn each_served_query_sees_the_changes_made_before_it() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let corpus = tmp.path().join("corpus");
    std::fs::write(corpus.join("ab.c"), b"printk before\n").unwrap();
    let Some(daemon) = watching(tmp.path(), &corpus) else {
        return;
    };
    let (addr, server) = spawn_server(&index, &[]);
    assert_eq!(served_totals(&addr, &["printk", "/pr[i1]ntk/"]), [5, 5]);
    std::fs::write(corpus.join("b.c"), b"aaaa\n").unwrap();
    assert_eq!(served_totals(&addr, &["printk"]), [4], "an edit");
    std::fs::create_dir_all(corpus.join("new/deep")).unwrap();
    std::fs::write(corpus.join("new/deep/n.c"), b"printk after printk\n").unwrap();
    std::fs::write(corpus.join("bin.dat"), b"printk\0").unwrap();
    assert_eq!(served_totals(&addr, &["printk", "/pr[i1]ntk/"]), [6, 6]);
    let (_, listed) = client(&addr, &["-l", "--", "printk"]).unwrap();
    assert_eq!(listed, "a.c\nab.c\nnew/deep/n.c\n");
    std::fs::remove_file(corpus.join("ab.c")).unwrap();
    std::fs::remove_file(corpus.join("a.c")).unwrap();
    assert_eq!(served_totals(&addr, &["printk"]), [2], "deletes");
    std::fs::write(corpus.join("ab.c"), b"printk back\n").unwrap();
    assert_eq!(served_totals(&addr, &["printk"]), [3], "back");
    assert_eq!(daemon.sent().last().unwrap(), "query");
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

#[test]
fn serve_no_watch_sees_a_change_only_when_read() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let (addr, server) = spawn_server(&index, FIXED);
    std::fs::write(tmp.path().join("corpus").join("b.c"), b"aaaa\n").unwrap();
    assert_eq!(served_totals(&addr, &["printk"]), [4], "the index's count");
    assert_eq!(
        served_totals(&addr, &["/pr[i1]ntk/", "printk"]),
        [3, 3],
        "a read marks the file"
    );
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

/// Where a search asks watchman (here at any size), a `--no-watch` server
/// sends it nothing, at its start or before a query, and leaves the record
/// as the search wrote it.
#[cfg(unix)]
#[test]
fn serve_no_watch_asks_watchman_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let corpus = tmp.path().join("corpus");
    let Some(daemon) = watching(tmp.path(), &corpus) else {
        return;
    };
    ASK_FROM.set(Some(0));
    assert_eq!(printed(&index, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert!(!daemon.sent().is_empty(), "a search asks");
    let record = |dir: &Path| {
        let files = std::fs::read_dir(dir.join("cache")).unwrap();
        files
            .map(|f| std::fs::read(f.unwrap().path()).unwrap())
            .collect::<Vec<_>>()
    };
    let before = record(tmp.path());
    std::fs::write(corpus.join("b.c"), b"aaaa\n").unwrap();
    let (addr, server) = spawn_server(&index, FIXED);
    assert_eq!(
        served_totals(&addr, &["printk"]),
        [3],
        "the stat pass at start"
    );
    std::fs::write(corpus.join("c.c"), b"printk\n").unwrap();
    assert_eq!(served_totals(&addr, &["printk"]), [3]);
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
    assert!(daemon.sent().is_empty(), "the server sent nothing");
    assert_eq!(record(tmp.path()), before);
}

/// A server that cannot use watchman at all refuses, before its readiness
/// line, unless `--no-watch`; a stored-docs index never needs it.
#[cfg(unix)]
#[test]
fn serve_refuses_without_watchman_unless_no_watch() {
    let status = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"status\"}\n";
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(stage_corpus(tmp.path(), &TWO_FILES)).unwrap();
    let (files, docs) = (tmp.path().join("files"), tmp.path().join("docs"));
    index_it(&root, &files, false);
    index_it(&root, &docs, true);
    let dead = tmp.path().join("no-daemon.sock");
    let nfs = tmp.path().join("mountinfo-nfs4");
    let text = format!(
        "22 1 8:1 / / rw - ext4 /dev/sda1 rw\n40 22 0:35 / {} rw - nfs4 s:/x rw\n",
        root.display()
    );
    std::fs::write(&nfs, text).unwrap();
    let unreadable = tmp.path().join("no-mountinfo");
    let causes = [
        (None, None, "cannot run watchman (".to_owned()),
        (
            Some(dead.clone()),
            None,
            format!("no watchman daemon answers on {} (", dead.display()),
        ),
        (
            None,
            Some(nfs),
            format!(
                "{} is on nfs4, a network or shared filesystem",
                root.display()
            ),
        ),
        (
            None,
            Some(unreadable.clone()),
            format!(
                "cannot read {} to tell the tree's filesystem (",
                unreadable.display()
            ),
        ),
    ];
    for (sock, mounts, cause) in causes {
        WATCHMAN.set(sock);
        MOUNTS.set(mounts);
        let serve = |index: &Path, flags: &[&str]| {
            let mut words = argv(&["serve", "--stdio", &index.display().to_string()]);
            words.extend(argv(flags));
            let mut out = Vec::new();
            let got = dispatch_to(&words, &mut &status[..], &mut out, false);
            (got, String::from_utf8(out).unwrap())
        };
        let (got, out) = serve(&files, &[]);
        let e = got.err().unwrap();
        let head = format!("error: cannot use watchman for {} ({cause}", root.display());
        let tail = "; `fmgrep serve` asks it before each query (pass --no-watch to serve a \
                    tree that does not change)";
        assert!(e.starts_with(&head) && e.ends_with(tail), "{e}");
        assert!(out.is_empty(), "{cause}: served nothing");
        for (index, flags) in [(&files, FIXED), (&docs, &[][..])] {
            let (got, out) = serve(index, flags);
            assert!(matches!(got, Ok(Outcome::Done)), "{cause} {flags:?}");
            assert_eq!(out.lines().count(), 1, "{cause} {flags:?}");
        }
    }
    WATCHMAN.set(None);
    MOUNTS.set(None);
}

/// A daemon that hangs at the start is late, not missing: the server
/// starts, checks every file, and asks again before each query.
#[cfg(unix)]
#[test]
fn serve_starts_while_watchman_hangs() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let corpus = tmp.path().join("corpus");
    let Some(daemon) = watching(tmp.path(), &corpus) else {
        return;
    };
    daemon.hang();
    let (addr, server) = spawn_server(&index, &[]);
    std::fs::write(corpus.join("b.c"), b"aaaa\n").unwrap();
    assert_eq!(served_totals(&addr, &["printk"]), [3]);
    daemon.resume();
    let late = "watchman cannot say what changed (watchman did not answer within 100 ms); \
                checked every file";
    assert_eq!(
        super::watched::notes(),
        [late],
        "once: the start and the query"
    );
    std::fs::write(corpus.join("c.c"), b"printk\n").unwrap();
    assert_eq!(served_totals(&addr, &["printk"]), [4]);
    assert_eq!(daemon.sent().last().unwrap(), "clock");
    std::fs::write(corpus.join("d.c"), b"printk\n").unwrap();
    assert_eq!(served_totals(&addr, &["printk"]), [5]);
    assert_eq!(daemon.sent().last().unwrap(), "query");
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

#[test]
fn serve_no_watch_finds_only_files_added_before_load() {
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), false);
    let corpus = tmp.path().join("corpus");
    std::fs::write(corpus.join("ab.c"), b"printk before\n").unwrap();
    let (addr, server) = spawn_server(&index, FIXED);
    assert_eq!(served_totals(&addr, &["printk"]), [5]);
    std::fs::write(corpus.join("c.c"), b"printk after\n").unwrap();
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
    let (addr, server) = spawn_server(&index, FIXED);
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
        let (addr, server) = spawn_server(&tree.join(INDEX_DIR), FIXED);
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
    let (addr, server) = spawn_server(&index, FIXED);
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

//! Rendering modes: `--server` prints exactly what a local search prints.

use super::*;

const FILES: [(&str, &[u8]); 4] = [
    (
        "a.c",
        b"printk one\nnothing\nPrintK two printk\nctx\nprintk\n",
    ),
    ("b.c", b"aaaa\ntail printk"),
    ("c.c", b"first\nprintk in c\nx\ny\none printk again\n"),
    ("d.c", b"\x80\x81 printk \xe9\nplain\n"),
];

fn stage(tmp: &Path, store_docs: bool) -> PathBuf {
    let folder = stage_corpus(tmp, &FILES);
    let output = tmp.join(format!("idx-{store_docs}"));
    index_it(&folder, &output, store_docs);
    output
}

fn local(index: &Path, flags: &[&str], terms: &[&str]) -> Vec<String> {
    words(&["--index", &index.display().to_string()], flags, terms)
}

/// Local and served stdout for the same flags; JSON with wall clocks dropped.
fn both(index: &Path, addr: &str, flags: &[&str], terms: &[&str]) -> (Vec<u8>, Vec<u8>) {
    let here = output(&local(index, flags, terms), false).unwrap();
    let served = output(&words(&["--server", addr], flags, terms), false).unwrap();
    if !flags.contains(&"--json") {
        return (here, served);
    }
    let json = |b: Vec<u8>| untimed(flags, &String::from_utf8(b).unwrap()).into_bytes();
    (json(here), json(served))
}

const EXPR: &str = r#"{"and":[{"term":"printk"},{"or":[{"regex":"a{3}"},{"term":"one"}]}]}"#;

const CASES: &[(&[&str], &[&str])] = &[
    (&[], &["printk"]),
    (&[], &["printk", "aaaa", "/pr[i1]ntk/", "absent"]),
    (&["-l"], &["printk"]),
    (&["-c"], &["printk"]),
    (&["--count-matches"], &["printk"]),
    (&["-o"], &["printk", "/p.i/"]),
    (&["-A1"], &["printk"]),
    (&["-B1"], &["printk"]),
    (&["-C1"], &["printk"]),
    (&["-C0"], &["printk"]),
    (&["-N"], &["printk"]),
    (&["--no-heading"], &["printk"]),
    (&["--no-heading", "-N", "-C1"], &["printk"]),
    (&["--color", "always"], &["printk", "/p.i/"]),
    (&["--color", "always", "-l"], &["printk"]),
    (&["--color", "never"], &["printk"]),
    (&["-i"], &["printk"]),
    (&["-i", "-c"], &["PRINTK"]),
    (&["--regex", "-U"], &["one\nnothing", "k\\n"]),
    (&["--regex", "-U", "--multiline-dotall"], &["one.nothing"]),
    (&["--expr", "printk AND (/a{3}/ OR one)"], &[]),
    (&["--expr-json", EXPR, "-l"], &[]),
    (&["--and"], &["printk", "one"]),
    (&["--or", "-c"], &["aaaa", "first"]),
    (&["--json", "--matches"], &["printk", "aaaa"]),
    (&["--json", "-o"], &["printk"]),
    (&["--json", "--matches", "-C1", "-N"], &["printk"]),
    (&["--json", "-l"], &["printk"]),
    (&["--json", "-c"], &["printk"]),
    (
        &["--json", "--count-matches", "--expr", "printk OR aaaa"],
        &[],
    ),
    (&["--json"], &["printk", "/pr[i1]ntk/"]),
    (&["--approx-counts"], &["printk"]),
];

#[test]
fn served_text_equals_local_stdout_in_every_mode() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = stage(tmp.path(), store_docs);
        let (addr, server) = spawn_server(&index, &[]);
        for (flags, terms) in CASES {
            let (here, served) = both(&index, &addr, flags, terms);
            assert!(!here.is_empty(), "store_docs={store_docs} {flags:?}");
            assert_eq!(
                String::from_utf8_lossy(&served),
                String::from_utf8_lossy(&here),
                "store_docs={store_docs} {flags:?} {terms:?}"
            );
            assert_eq!(served, here, "store_docs={store_docs} {flags:?} {terms:?}");
        }
        client(&addr, &["--shutdown"]).unwrap();
        server.join().unwrap().unwrap();
    }
}

#[test]
fn client_color_auto_follows_its_own_tty() {
    let tmp = tempfile::tempdir().unwrap();
    let index = stage(tmp.path(), true);
    let (addr, server) = spawn_server(&index, &[]);
    let search = local(&index, &[], &["printk"]);
    let served = argv(&["--server", &addr, "printk"]);
    for tty in [false, true] {
        assert_eq!(output(&served, tty), output(&search, tty), "tty={tty}");
    }
    assert_ne!(output(&served, true), output(&served, false));
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

#[test]
fn served_non_utf8_output_is_base64_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let index = stage(tmp.path(), true);
    let got = serve_stdio(
        &index,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"terms":["printk"],"mode":"lines"}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"query","params":{"terms":["aaaa"],"mode":"lines","heading":false}}"#,
        ],
    );
    let result = &got[0]["result"];
    assert!(result.get("text").is_none(), "{result}");
    let bytes = crate::base64::decode(result["bytes"].as_str().unwrap()).unwrap();
    assert_eq!(
        bytes,
        output(&local(&index, &[], &["printk"]), false).unwrap()
    );
    assert!(std::str::from_utf8(&bytes).is_err());
    assert_eq!(result["report"]["queries"][0]["term"], "printk");
    assert_eq!(got[1]["result"]["text"], "b.c:1:aaaa\n");
    assert!(got[1]["result"].get("bytes").is_none());
}

#[test]
fn stdio_request_without_mode_keeps_the_bare_report() {
    let tmp = tempfile::tempdir().unwrap();
    let index = stage(tmp.path(), true);
    let got = serve_stdio(
        &index,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"terms":["printk"],"context":{"before":2},"color":"always"}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"query","params":{"terms":["printk"],"mode":"counts"}}"#,
        ],
    );
    for response in &got {
        let result = &response["result"];
        assert!(
            result.get("text").is_none() && result.get("report").is_none(),
            "{result}"
        );
        assert_eq!(
            result_report(response),
            timeless(query_it(&index, false, &["printk"]))
        );
    }
}

#[test]
fn served_stale_file_equals_local() {
    let tmp = tempfile::tempdir().unwrap();
    let index = stage(tmp.path(), false);
    let corpus = tmp.path().join("corpus");
    std::fs::write(corpus.join("a.c"), b"printk changed\nprintk\n").unwrap();
    let (addr, server) = spawn_server(&index, &[]);
    let modes: [&[&str]; 5] = [&[], &["-l"], &["-c"], &["-C1"], &["--json", "--matches"]];
    for flags in modes {
        let (here, served) = both(&index, &addr, flags, &["printk"]);
        assert_eq!(served, here, "stale at load {flags:?}");
    }
    // Rewritten each poll, as FSEvents can miss a change made just after the
    // watch starts.
    let change = || std::fs::write(corpus.join("b.c"), b"printk printk\nnew\n").unwrap();
    let start = std::time::Instant::now();
    change();
    let converged = || {
        let (here, served) = both(&index, &addr, &["-c"], &["printk"]);
        here == served
    };
    while !converged() && start.elapsed() < std::time::Duration::from_secs(60) {
        std::thread::sleep(std::time::Duration::from_millis(20));
        change();
    }
    for flags in modes {
        let (here, served) = both(&index, &addr, flags, &["printk"]);
        assert_eq!(served, here, "changed while serving {flags:?}");
    }
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

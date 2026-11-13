//! Rendering modes: `--server` prints exactly what a local search prints.

use std::io::BufRead;

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
        let (addr, server) = spawn_server(&index, FIXED);
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
    let (addr, server) = spawn_server(&index, FIXED);
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
    let here = output(&local(&index, &["-n", "--heading"], &["printk"]), false).unwrap();
    assert!(std::str::from_utf8(&here).is_err());
    assert_eq!(result["bytes"], crate::base64::encode(&here));
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
    let (addr, server) = spawn_server(&index, FIXED);
    for flags in MODES {
        let (here, served) = both(&index, &addr, flags, &["printk"]);
        assert_eq!(served, here, "stale at load {flags:?}");
    }
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

const MODES: [&[&str]; 5] = [&[], &["-l"], &["-c"], &["-C1"], &["--json", "--matches"]];

#[cfg(unix)]
#[test]
fn a_file_changed_while_serving_equals_local() {
    let tmp = tempfile::tempdir().unwrap();
    let index = stage(tmp.path(), false);
    let corpus = tmp.path().join("corpus");
    let Some(_daemon) = watching(tmp.path(), &corpus) else {
        return;
    };
    let (addr, server) = spawn_server(&index, &[]);
    std::fs::write(corpus.join("b.c"), b"printk printk\nnew\n").unwrap();
    for flags in MODES {
        let (here, served) = both(&index, &addr, flags, &["printk"]);
        assert_eq!(served, here, "changed while serving {flags:?}");
    }
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

/// `serve --stdio` over `requests`: each response with the frames before it.
fn serve_frames(index: &Path, requests: &[&str]) -> Vec<(Vec<Vec<u8>>, serde_json::Value)> {
    let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
    let mut out = Vec::new();
    let words = argv(&[
        "serve",
        "--stdio",
        "--no-watch",
        &index.display().to_string(),
    ]);
    dispatch_to(&words, &mut input.as_bytes(), &mut out, false).unwrap();
    let (mut rest, mut got, mut frames) = (&out[..], Vec::new(), Vec::new());
    while let Some(eol) = rest.iter().position(|&b| b == b'\n') {
        let line = std::str::from_utf8(&rest[..eol]).unwrap();
        rest = &rest[eol + 1..];
        match line.parse::<usize>() {
            Ok(len) => {
                frames.push(rest[..len].to_vec());
                rest = &rest[len..];
            }
            Err(_) => got.push((
                std::mem::take(&mut frames),
                serde_json::from_str(line).unwrap(),
            )),
        }
    }
    assert!(rest.is_empty() && frames.is_empty());
    got
}

fn streamed(params: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":1,"method":"query","params":{{{params},"stream":true}}}}"#)
}

#[test]
fn streamed_output_is_framed_before_the_response() {
    let tmp = tempfile::tempdir().unwrap();
    let index = stage(tmp.path(), true);
    let cases: [(&str, &[&str]); 3] = [
        ("lines", &["-n", "--heading"]),
        ("count", &["-c"]),
        ("json_matches", &["--json", "--matches"]),
    ];
    let requests: Vec<String> = cases
        .iter()
        .map(|(mode, _)| streamed(&format!(r#""terms":["printk"],"mode":"{mode}""#)))
        .chain([streamed(r#""terms":["printk"]"#)])
        .collect();
    let got = serve_frames(
        &index,
        &requests.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    for ((frames, response), (mode, flags)) in got.iter().zip(cases) {
        let here = output(&local(&index, flags, &["printk"]), false).unwrap();
        let served = frames.concat();
        assert_eq!(
            untimed(flags, &String::from_utf8_lossy(&served)),
            untimed(flags, &String::from_utf8_lossy(&here)),
            "{mode}"
        );
        if mode != "json_matches" {
            assert_eq!(served, here, "{mode}");
        }
        let result = response["result"].as_object().unwrap();
        assert_eq!(result.keys().collect::<Vec<_>>(), ["report"], "{mode}");
    }
    assert!(std::str::from_utf8(&got[0].0.concat()).is_err());
    let (frames, response) = &got[3];
    assert!(frames.is_empty());
    assert_eq!(
        result_report(response),
        timeless(query_it(&index, false, &["printk"]))
    );
}

/// One file of `n` matched lines, 64 bytes each.
fn stage_big(tmp: &Path, n: usize) -> PathBuf {
    let big: Vec<u8> = (0..n)
        .flat_map(|i| format!("printk {i:08} {}\n", "x".repeat(47)).into_bytes())
        .collect();
    let folder = stage_corpus(tmp, &[("big.c", &big)]);
    let output = tmp.join("idx");
    index_it(&folder, &output, true);
    output
}

#[test]
fn streamed_output_splits_into_bounded_frames() {
    let tmp = tempfile::tempdir().unwrap();
    let index = stage_big(tmp.path(), 16_000);
    let here = output(&local(&index, &["-n", "--heading"], &["printk"]), false).unwrap();
    let got = serve_frames(&index, &[&streamed(r#""terms":["printk"],"mode":"lines""#)]);
    let frames = &got[0].0;
    assert!(frames.len() > 10, "{}", frames.len());
    assert!(frames.iter().all(|f| f.len() <= 64 << 10));
    assert_eq!(frames.concat(), here);
    let (addr, server) = spawn_server(&index, FIXED);
    let served = output(&words(&["--server", &addr], &[], &["printk"]), false).unwrap();
    assert_eq!(
        served,
        output(&local(&index, &[], &["printk"]), false).unwrap()
    );
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

/// A one-connection server that answers its `n`th request with
/// `replies[n]`, then closes.
fn fake_server(replies: &[&[u8]]) -> String {
    let replies: Vec<Vec<u8>> = replies.iter().map(|r| r.to_vec()).collect();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut input = std::io::BufReader::new(&stream);
        for reply in replies {
            let mut line = String::new();
            input.read_line(&mut line).unwrap();
            (&stream).write_all(&reply).unwrap();
        }
    });
    addr
}

/// A `status` response of index format `format`, or none.
fn status_reply(format: Option<u32>) -> String {
    let format = format
        .map(|f| format!("\"format\":{f},"))
        .unwrap_or_default();
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{{format}\"store_docs\":true,\"files\":1,\
         \"num_bytes\":1,\"index_size_bytes\":1,\"load_us\":1,\"root\":\"/x\"}}}}\n"
    )
}

/// A client refuses a server of another index format, or of none; a
/// query refused with -32001 after the status means the server's index
/// changed.
#[test]
fn a_client_refuses_a_server_of_another_index() {
    let error = "{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32001,\"message\":\"m\"}}\n";
    let ours = status_reply(Some(crate::manifest::MANIFEST_FORMAT));
    for (replies, want) in [
        (
            vec![status_reply(Some(7))],
            "serves an index of format 7; this fmgrep reads format 8 (use one fmgrep version \
             on both sides)",
        ),
        (
            vec![status_reply(None)],
            "reports no index format; it is older than this fmgrep (use one fmgrep version on \
             both sides)",
        ),
        (
            vec![ours, error.to_owned()],
            "changed its index during this search; run the search again",
        ),
    ] {
        let replies: Vec<&[u8]> = replies.iter().map(|r| r.as_bytes()).collect();
        let addr = fake_server(&replies);
        let words = argv(&["--server", &addr, "printk"]);
        let got = dispatch_to(&words, &mut &b""[..], &mut Vec::new(), false)
            .err()
            .unwrap();
        assert_eq!(got, format!("error: the server at {addr} {want}"));
    }
}

/// `payload` as one frame.
fn frame(payload: &str) -> String {
    format!("{}\n{payload}", payload.len())
}

#[test]
fn a_server_error_after_output_keeps_the_output() {
    let error =
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32000,\"message\":\"error: boom\"}}\n";
    let sent = [
        frame("{\"term\":\"printk\"}\n"),
        frame("{\"doc\":\"a\",\"path\":\"a\",\"stats\":[1,1,6]}\nhello!"),
        error.to_owned(),
    ]
    .concat();
    let cut = [frame("{\"term\":\"printk\"}\n"), "60\n{\"doc\"".to_owned()].concat();
    for (reply, printed, error) in [
        (sent.as_bytes(), &b"hello!"[..], "server: boom"),
        (cut.as_bytes(), b"", "{addr}: connection closed mid-frame"),
    ] {
        let status = status_reply(Some(crate::manifest::MANIFEST_FORMAT));
        let addr = fake_server(&[status.as_bytes(), reply]);
        let mut out = Vec::new();
        let words = argv(&["--server", &addr, "printk"]);
        let got = dispatch_to(&words, &mut &b""[..], &mut out, false)
            .err()
            .unwrap();
        assert_eq!(got, format!("error: {}", error.replace("{addr}", &addr)));
        assert_eq!(out, printed);
    }
}

#[test]
fn a_client_leaving_mid_stream_leaves_the_server_serving() {
    let tmp = tempfile::tempdir().unwrap();
    let index = stage_big(tmp.path(), 16_000);
    let (addr, server) = spawn_server(&index, FIXED);
    let stream = std::net::TcpStream::connect(&addr).unwrap();
    let request = streamed(r#""terms":["printk"],"mode":"lines""#);
    (&stream)
        .write_all(format!("{request}\n").as_bytes())
        .unwrap();
    let mut header = String::new();
    std::io::BufReader::new(&stream)
        .read_line(&mut header)
        .unwrap();
    assert_eq!(header, format!("{}\n", 64 << 10));
    drop(stream);
    let (_, counted) = client(&addr, &["-c", "--", "printk"]).unwrap();
    assert_eq!(counted, printed(&index, &["-c"], &["printk"]));
    client(&addr, &["--shutdown"]).unwrap();
    server.join().unwrap().unwrap();
}

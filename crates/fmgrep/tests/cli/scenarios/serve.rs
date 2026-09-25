//! `serve` + `--server` through the shipped binary: the same answers, byte
//! for byte modulo timings, as a local `--json --approx-counts`.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use crate::{Fmgrep, index_argv};

const DOCS: [(&str, &[u8]); 3] = [
    ("a.bin", b"printk hello printk\n"),
    ("b.bin", b"\x80\x81 non-utf8 \xe9 blob printk"),
    ("c.bin", b"aaaa nothing here"),
];
const TERMS: [&str; 5] = ["printk", "aa", "missing", "/pr[i1]ntk/", "/a+/"];

fn run(e: &Fmgrep, args: &[&str]) -> Output {
    let out = e.command().args(args).output().expect("spawn");
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stage(e: &Fmgrep, store_docs: bool) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let docs = tmp.path().join("docs");
    std::fs::create_dir(&docs).unwrap();
    for (name, bytes) in DOCS {
        std::fs::write(docs.join(name), bytes).unwrap();
    }
    let index = tmp.path().join("idx");
    let argv = index_argv(&docs, &index, store_docs, &[]);
    run(e, &argv.iter().map(String::as_str).collect::<Vec<_>>());
    (tmp, index.display().to_string())
}

/// Stdout with every timing zeroed.
fn timeless(stdout: &[u8]) -> String {
    let re = regex::Regex::new(r#""(latency_us|index_time_us|load_us)":\d+"#).unwrap();
    re.replace_all(std::str::from_utf8(stdout).unwrap(), "\"$1\":0")
        .into_owned()
}

fn query(e: &Fmgrep, index: &str, terms: &[&str]) -> String {
    let mut args = vec!["--index", index, "--json", "--approx-counts", "--"];
    args.extend(terms);
    timeless(&run(e, &args).stdout)
}

fn client(e: &Fmgrep, addr: &str, words: &[&str]) -> String {
    let mut args = vec!["--server", addr];
    args.extend(words);
    timeless(&run(e, &args).stdout)
}

/// Killed on drop, so a failing test leaves no server behind.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Server {
    fn spawn(e: &Fmgrep, args: &[&str]) -> Self {
        Server(
            e.command()
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn serve"),
        )
    }

    fn exits_cleanly(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "serve exited {status}");
                return;
            }
            assert!(
                Instant::now() < deadline,
                "serve did not exit after shutdown"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

pub fn tcp_server_flag_equals_local_search(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        let (_tmp, index) = stage(e, store_docs);
        let mut server = Server::spawn(e, &["serve", &index]);
        let mut stdout = BufReader::new(server.0.stdout.take().unwrap());
        let mut addr = String::new();
        stdout.read_line(&mut addr).unwrap();
        let addr = addr.trim_end();
        for term in TERMS {
            assert_eq!(
                client(e, addr, &["--json", "--", term]),
                query(e, &index, &[term]),
                "{store_docs} {term}"
            );
        }
        let mut all = vec!["--json", "--"];
        all.extend(TERMS);
        assert_eq!(
            client(e, addr, &all),
            query(e, &index, &TERMS),
            "{store_docs}"
        );

        let status: serde_json::Value =
            serde_json::from_str(&client(e, addr, &["--status"])).unwrap();
        let bytes: usize = DOCS.iter().map(|(_, b)| b.len()).sum();
        assert_eq!(status["files"], DOCS.len(), "{store_docs}");
        assert_eq!(status["num_bytes"], bytes, "{store_docs}");
        assert_eq!(status["store_docs"], store_docs, "{store_docs}");

        assert_eq!(client(e, addr, &["--shutdown"]), "", "{store_docs}");
        server.exits_cleanly();
        let mut rest = String::new();
        stdout.read_to_string(&mut rest).unwrap();
        assert_eq!(
            rest, "",
            "{store_docs}: stdout carries the readiness line only"
        );
    }
}

/// `serve PATH` finds the `.fmgrep` at or above PATH, as a search does.
pub fn serve_path_finds_the_dotdir(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        let (tmp, index) = stage(e, store_docs);
        let docs = tmp.path().join("docs");
        let docs_arg = docs.display().to_string();
        let flags: &[&str] = if store_docs { &["--store-docs"] } else { &[] };
        run(e, &[&["index", docs_arg.as_str()][..], flags].concat());
        let sub = docs.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut server = Server::spawn(e, &["serve", &sub.display().to_string()]);
        let mut stdout = BufReader::new(server.0.stdout.take().unwrap());
        let mut addr = String::new();
        stdout.read_line(&mut addr).unwrap();
        let addr = addr.trim_end();
        let mut all = vec!["--json", "--"];
        all.extend(TERMS);
        assert_eq!(
            client(e, addr, &all),
            query(e, &index, &TERMS),
            "{store_docs}"
        );
        assert_eq!(client(e, addr, &["--shutdown"]), "", "{store_docs}");
        server.exits_cleanly();
    }
}

pub fn stdio_round_trip(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        let (_tmp, index) = stage(e, store_docs);
        let mut server = Server::spawn(e, &["serve", "--stdio", &index]);
        let requests = [
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"terms":["printk","/a+/"]}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"query","params":{"terms":["("],"regex":true}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"shutdown"}"#,
        ];
        let mut stdin = server.0.stdin.take().unwrap();
        for r in requests {
            writeln!(stdin, "{r}").unwrap();
        }
        let mut out = String::new();
        server
            .0
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        server.exits_cleanly();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{store_docs}: {out}");
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let want: serde_json::Value =
            serde_json::from_str(&query(e, &index, &["printk", "/a+/"])).unwrap();
        let mut got = first["result"].clone();
        got["index_time_us"] = 0.into();
        for q in got["queries"].as_array_mut().unwrap() {
            q["latency_us"] = 0.into();
        }
        assert_eq!(got, want, "{store_docs}");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["error"]["code"], -32000, "{store_docs}");
        assert_eq!(second["id"], 2, "{store_docs}");
        assert!(lines[2].contains("shutting down"), "{store_docs}");
    }
}

/// An expression through the binary: `--server --expr-json`, inline and on
/// stdin, and `--expr` text equal a local `--expr-json`; `-l` lists the
/// members.
pub fn expr_round_trip(e: &Fmgrep) {
    const EXPR: &str = r#"{"or":[{"and":[{"term":"printk"},{"term":"hello"}]},{"regex":"a{3}"}]}"#;
    const TEXT: &str = "printk AND hello OR /a{3}/";
    for &store_docs in e.store_docs {
        let (_tmp, index) = stage(e, store_docs);
        let want = timeless(&run(e, &["--index", &index, "--json", "--expr-json", EXPR]).stdout);
        let report: serde_json::Value = serde_json::from_str(&want).unwrap();
        assert_eq!(
            report["queries"][0]["term"], "printk AND hello OR /a{3}/",
            "{store_docs}"
        );
        assert_eq!(report["queries"][0]["docs_matched"], 2, "{store_docs}");
        let listed = run(e, &["--index", &index, "-l", "--expr-json", EXPR]).stdout;
        assert_eq!(listed, b"a.bin\nc.bin\n", "{store_docs}");
        assert_eq!(
            timeless(&run(e, &["--index", &index, "--json", "--expr", TEXT]).stdout),
            want,
            "{store_docs}"
        );
        assert_eq!(
            run(e, &["--index", &index, "-l", "--expr", TEXT]).stdout,
            listed,
            "{store_docs}"
        );
        let mut server = Server::spawn(e, &["serve", &index]);
        let mut stdout = BufReader::new(server.0.stdout.take().unwrap());
        let mut addr = String::new();
        stdout.read_line(&mut addr).unwrap();
        let addr = addr.trim_end();
        assert_eq!(
            client(e, addr, &["--json", "--expr-json", EXPR]),
            want,
            "{store_docs}"
        );
        assert_eq!(
            client(e, addr, &["--json", "--expr", TEXT]),
            want,
            "{store_docs}"
        );
        let mut piped = e
            .command()
            .args(["--server", addr, "--json", "--expr-json", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn --server");
        piped
            .stdin
            .take()
            .unwrap()
            .write_all(EXPR.as_bytes())
            .unwrap();
        let out = piped.wait_with_output().unwrap();
        assert!(out.status.success(), "{store_docs}");
        assert_eq!(timeless(&out.stdout), want, "{store_docs}");
        assert_eq!(client(e, addr, &["--shutdown"]), "", "{store_docs}");
        server.exits_cleanly();
    }
}

/// `--and` / `--or` through the binary equal their `--expr-json` form, for
/// a local search and `--server`; one term is refused.
pub fn flags_round_trip(e: &Fmgrep) {
    const AND: &str = r#"{"and":[{"term":"printk"},{"term":"hello"}]}"#;
    const OR: &str = r#"{"or":[{"term":"printk"},{"regex":"a{3}"}]}"#;
    for &store_docs in e.store_docs {
        let (_tmp, index) = stage(e, store_docs);
        let q = |words: &[&str]| run(e, &[&["--index", &index][..], words].concat()).stdout;
        let and_json = timeless(&q(&["--json", "--expr-json", AND]));
        assert_eq!(
            timeless(&q(&["--json", "--and", "printk", "hello"])),
            and_json,
            "{store_docs}"
        );
        assert_eq!(
            q(&["-l", "--or", "--", "printk", "/a{3}/"]),
            q(&["-l", "--expr-json", OR]),
            "{store_docs}"
        );
        let mut server = Server::spawn(e, &["serve", &index]);
        let mut stdout = BufReader::new(server.0.stdout.take().unwrap());
        let mut addr = String::new();
        stdout.read_line(&mut addr).unwrap();
        let addr = addr.trim_end();
        let or_json = timeless(&q(&["--json", "--expr-json", OR]));
        assert_eq!(
            client(e, addr, &["--json", "--or", "printk", "/a{3}/"]),
            or_json,
            "{store_docs}"
        );
        assert_eq!(
            client(e, addr, &["--json", "--and", "printk", "hello"]),
            and_json,
            "{store_docs}"
        );
        assert_eq!(client(e, addr, &["--shutdown"]), "", "{store_docs}");
        server.exits_cleanly();
        let one = e
            .command()
            .args(["--index", &index, "--and", "printk"])
            .output()
            .expect("spawn");
        assert_eq!(one.status.code(), Some(2), "{store_docs}");
        assert!(
            String::from_utf8_lossy(&one.stderr).contains("--and needs at least two terms"),
            "{store_docs}"
        );
    }
}

/// `total_matches` per term, asked of the server at `addr`.
fn served_totals(e: &Fmgrep, addr: &str, regex: bool, terms: &[&str]) -> Vec<serde_json::Value> {
    let mut words = vec!["--json"];
    if regex {
        words.push("--regex");
    }
    words.push("--");
    words.extend(terms);
    let report: serde_json::Value = serde_json::from_str(&client(e, addr, &words)).unwrap();
    report["queries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| q["total_matches"].clone())
        .collect()
}

/// The server's readiness address, a watch-side change made by `change`,
/// then the totals polled for up to 60 s until they equal `want`. `change`
/// repeats until the first total matches, as FSEvents can miss changes made
/// just after the watch starts.
fn served_after(
    e: &Fmgrep,
    store_docs: bool,
    change: impl Fn(&std::path::Path),
    want: &[(bool, &str, u64)],
) -> String {
    let (tmp, index) = stage(e, store_docs);
    let mut server = Server(
        e.command()
            .args(["serve", &index])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn serve"),
    );
    let mut stdout = BufReader::new(server.0.stdout.take().unwrap());
    let mut addr = String::new();
    stdout.read_line(&mut addr).unwrap();
    let addr = addr.trim_end();
    assert_eq!(
        served_totals(e, addr, false, &["printk"]),
        [3],
        "{store_docs}"
    );
    let docs = tmp.path().join("docs");
    change(&docs);
    let start = Instant::now();
    for (i, &(regex, term, total)) in want.iter().enumerate() {
        let mut got = served_totals(e, addr, regex, &[term]);
        while got != [total] && start.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(20));
            if i == 0 {
                change(&docs);
            }
            got = served_totals(e, addr, regex, &[term]);
        }
        assert_eq!(
            got,
            [total],
            "{store_docs} {term} after {:?}",
            start.elapsed()
        );
    }
    assert_eq!(client(e, addr, &["--shutdown"]), "", "{store_docs}");
    server.exits_cleanly();
    let mut stderr = String::new();
    server
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    stderr
}

/// Files source: the server's watcher turns an indexed file changed after
/// load stale, so a literal the index answered alone counts the new bytes.
pub fn serve_watch_scans_a_changed_file(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        let change = |docs: &std::path::Path| {
            std::fs::write(docs.join("c.bin"), b"printk printk aa").unwrap()
        };
        let stderr = served_after(
            e,
            store_docs,
            change,
            &[
                (false, "printk", 5),
                (false, "aa", 1),
                (true, "pr[i1]ntk", 5),
            ],
        );
        assert!(!stderr.contains("warning"), "{store_docs}: {stderr}");
    }
}

/// Files source: a file deleted under the server is excluded, one warning.
pub fn serve_watch_excludes_a_deleted_file(e: &Fmgrep) {
    for &store_docs in e.store_docs {
        // Put back first, so a repeat is a fresh event.
        let change = |docs: &std::path::Path| {
            std::fs::write(docs.join(DOCS[0].0), DOCS[0].1).unwrap();
            std::fs::remove_file(docs.join(DOCS[0].0)).unwrap();
        };
        let stderr = served_after(
            e,
            store_docs,
            change,
            &[
                (false, "printk", 1),
                (false, "hello", 0),
                (true, "pr[i1]ntk", 1),
            ],
        );
        assert_eq!(
            stderr.matches("warning:").count(),
            1,
            "{store_docs}: {stderr}"
        );
        assert!(
            stderr.contains("warning: excluding") && stderr.contains("a.bin"),
            "{store_docs}: {stderr}"
        );
    }
}

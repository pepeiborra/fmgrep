use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use crate::*;

const TERMS: [&str; 5] = ["printk", "aa", "missing", "/pr[i1]ntk/", "/a+/"];

/// Runs to exit 0, or 1 for a search with no match.
fn ok(args: &[&str]) -> Output {
    let out = run(args);
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "{args:?}: {}",
        stderr(&out)
    );
    out
}

/// Stdout with every timing zeroed.
fn timeless(stdout: &[u8]) -> String {
    let re = regex::Regex::new(
        r#""(latency_us|index_time_us|load_us|elapsed|elapsed_total)":(\d+|\{[^}]*\})"#,
    )
    .unwrap();
    re.replace_all(std::str::from_utf8(stdout).unwrap(), "\"$1\":0")
        .into_owned()
}

fn query(index: &str, terms: &[&str]) -> String {
    let mut args = vec!["--index", index, "--json", "--approx-counts", "--"];
    args.extend(terms);
    timeless(&ok(&args).stdout)
}

/// `--server addr <words>`: stdout, timeless; nothing on stderr.
fn client(addr: &str, words: &[&str]) -> String {
    let out = ok(&[&["--server", addr][..], words].concat());
    assert!(out.stderr.is_empty(), "{words:?}: {}", stderr(&out));
    timeless(&out.stdout)
}

fn shutdown(server: &mut Server, addr: &str) {
    assert_eq!(client(addr, &["--shutdown"]), "");
    server.exits_cleanly();
}

fn served_stderr(server: &mut Server) -> String {
    let mut stderr = String::new();
    let mut pipe = server.0.stderr.take().unwrap();
    pipe.read_to_string(&mut stderr).unwrap();
    stderr
}

#[test]
fn tcp_server_flag_equals_local_search() {
    for store_docs in [false, true] {
        let (_tmp, docs) = stage();
        let index = do_index(&docs, store_docs).display().to_string();
        let mut server = Server::spawn(&["serve", &index]);
        let mut stdout = BufReader::new(server.0.stdout.take().unwrap());
        let mut addr = String::new();
        stdout.read_line(&mut addr).unwrap();
        let addr = addr.trim_end();
        assert!(addr.starts_with("127.0.0.1:"), "{addr}");
        for term in TERMS {
            assert_eq!(
                client(addr, &["--json", "--", term]),
                query(&index, &[term]),
                "{store_docs} {term}"
            );
            let want = Some(if term == "missing" { 1 } else { 0 });
            let code = |args: &[&str]| run(args).status.code();
            assert_eq!(code(&["--server", addr, "--", term]), want, "{term}");
            assert_eq!(code(&["--index", &index, "--", term]), want, "{term}");
        }
        let all = [&["--json", "--"][..], &TERMS].concat();
        assert_eq!(client(addr, &all), query(&index, &TERMS), "{store_docs}");
        let status: serde_json::Value = serde_json::from_str(&client(addr, &["--status"])).unwrap();
        let bytes: usize = DOCS.iter().map(|(_, b)| b.len()).sum();
        assert_eq!(status["files"], DOCS.len(), "{store_docs}");
        assert_eq!(status["num_bytes"], bytes, "{store_docs}");
        assert_eq!(status["store_docs"], store_docs, "{store_docs}");
        shutdown(&mut server, addr);
        let mut rest = String::new();
        stdout.read_to_string(&mut rest).unwrap();
        assert_eq!(
            rest, "",
            "{store_docs}: stdout carries the readiness line only"
        );
    }
}

/// `total_matches` of `printk`, asked of the server at `addr`.
fn served_printk(addr: &str) -> u64 {
    let report: serde_json::Value =
        serde_json::from_str(&client(addr, &["--json", "--", "printk"])).unwrap();
    report["queries"][0]["total_matches"].as_u64().unwrap()
}

/// Files source: an added file and an indexed one, each deleted and then
/// recreated under the server, come back; only the indexed one warns.
#[test]
fn serve_watch_handles_delete_then_recreate() {
    let (_tmp, docs) = stage();
    let index = do_index(&docs, false).display().to_string();
    std::fs::write(docs.join("d.bin"), b"zebra printk\n").unwrap();
    let mut server = Server::spawn(&["serve", &index]);
    let addr = server.addr();
    // `change` repeats each poll, as FSEvents can miss a change.
    let poll = |change: &dyn Fn(), want: u64| {
        let deadline = Instant::now() + Duration::from_secs(60);
        change();
        while served_printk(&addr) != want && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            change();
        }
        assert_eq!(served_printk(&addr), want);
    };
    poll(&|| {}, 4);
    let delete = || {
        let _ = std::fs::remove_file(docs.join("d.bin"));
        let _ = std::fs::remove_file(docs.join("a.bin"));
    };
    poll(&delete, 1);
    let recreate = || {
        std::fs::write(docs.join("d.bin"), b"zebra printk\n").unwrap();
        std::fs::write(docs.join("a.bin"), b"printk back\n").unwrap();
    };
    poll(&recreate, 3);
    shutdown(&mut server, &addr);
    let stderr = served_stderr(&mut server);
    assert_eq!(stderr.matches("warning:").count(), 1, "{stderr}");
    assert!(stderr.contains("a.bin"), "{stderr}");
}

#[test]
fn serve_is_ready_only_once_added_files_are_found() {
    let (tmp, docs) = stage();
    let index = do_index(&docs, false).display().to_string();
    std::fs::write(docs.join("d.bin"), b"printk added\n").unwrap();
    for flags in [&[][..], &["--no-watch"]] {
        // One file for both streams keeps their writes in order.
        let log = tmp.path().join("serve.log");
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let mut server = Server(
            fmgrep()
                .args([&["serve", &index][..], flags].concat())
                .env("FMGREP_VERBOSE", "1")
                .stdout(out)
                .stderr(err)
                .spawn()
                .expect("spawn serve"),
        );
        let deadline = Instant::now() + Duration::from_secs(60);
        let (text, addr) = loop {
            let text = std::fs::read_to_string(&log).unwrap();
            let mut lines = text.split_inclusive('\n');
            let addr = lines.find(|l| l.starts_with("127.0.0.1:") && l.ends_with('\n'));
            if let Some(addr) = addr.map(|a| a.trim_end().to_owned()) {
                break (text, addr);
            }
            assert!(Instant::now() < deadline, "no readiness line: {text}");
            std::thread::sleep(Duration::from_millis(20));
        };
        let note = text.find("1 file(s) added since indexing");
        assert!(
            note.is_some_and(|n| n < text.find(&addr).unwrap()),
            "{flags:?}: {text}"
        );
        assert_eq!(served_printk(&addr), 4, "{flags:?}");
        shutdown(&mut server, &addr);
    }
}

/// `--server` reads no local index: the `--no-watch` server's answer, which
/// misses a file added after its first search, from anywhere.
#[test]
fn a_client_ignores_local_indexes() {
    let (tmp, docs) = stage();
    ok(&["index", &docs.display().to_string()]);
    let index = docs.join(".fmgrep");
    let mut server = Server::spawn(&["serve", &docs.display().to_string(), "--no-watch"]);
    let addr = server.addr();
    let files = |cwd: &Path, args: &[&str]| {
        let out = run_in(
            cwd,
            &[&["--server", &addr, "-l", "printk"][..], args].concat(),
        );
        assert!(out.status.success(), "{}: {}", cwd.display(), stderr(&out));
        assert!(out.stderr.is_empty(), "{}: {}", cwd.display(), stderr(&out));
        String::from_utf8(out.stdout).unwrap()
    };
    let want = "a.bin\nb.bin\n";
    assert_eq!(files(tmp.path(), &[]), want);
    std::fs::write(docs.join("d.bin"), b"printk added\n").unwrap();
    assert_eq!(files(tmp.path(), &[]), want);
    assert_eq!(files(&docs, &[]), want);
    assert_eq!(files(&index, &[".."]), "../a.bin\n../b.bin\n");
    let other = tmp.path().join("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("e.bin"), b"printk elsewhere\n").unwrap();
    ok(&["index", &other.display().to_string()]);
    let manifest = other.join(".fmgrep/manifest.json");
    let before = std::fs::read(&manifest).unwrap();
    assert_eq!(files(&other, &[]), want);
    assert_eq!(std::fs::read(&manifest).unwrap(), before);
    shutdown(&mut server, &addr);
}

/// A `.fmgrep` copied away from its tree, or without `docs.stamps`, is not served.
#[test]
fn serve_without_its_documents_is_refused() {
    let (tmp, docs) = stage();
    ok(&["index", &docs.display().to_string()]);
    let copy = tmp.path().join("copy");
    std::fs::create_dir_all(copy.join(".fmgrep")).unwrap();
    for f in std::fs::read_dir(docs.join(".fmgrep")).unwrap() {
        let f = f.unwrap();
        std::fs::copy(f.path(), copy.join(".fmgrep").join(f.file_name())).unwrap();
    }
    let out = run_in(&copy, &["serve", "--stdio"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let copy = std::fs::canonicalize(&copy).unwrap();
    let want = format!(
        "warning: excluding 3 of 3 indexed files, missing under {0}\n\
         error: 3 of 3 indexed files are missing under {0}\n",
        copy.display()
    );
    assert_eq!(stderr(&out), want);
    std::fs::remove_file(copy.join(".fmgrep/docs.stamps")).unwrap();
    let out = run_in(&copy, &["serve", "--stdio"]);
    assert_eq!(out.status.code(), Some(2));
    let want = format!("error: {}/.fmgrep has no docs.stamps\n", copy.display());
    assert_eq!(stderr(&out), want);
}

#[test]
fn served_paths_print_as_the_local_search() {
    let (_tmp, docs) = stage_files(&[("a.c", b"printk a\n"), ("b.h", b"printk b\n")]);
    std::fs::create_dir(docs.join("sub")).unwrap();
    std::fs::write(docs.join("sub/c.c"), "printk c\n").unwrap();
    ok(&["index", &docs.display().to_string()]);
    let mut server = Server::spawn(&["serve", &docs.display().to_string(), "--no-watch"]);
    let addr = server.addr();
    let sub = docs.join("sub");
    for args in [
        &["-l", "printk", ".", "../a.c"][..],
        &["-g", "!*.h", "printk", ".."],
        &["-c", "printk", "../a.c"],
        &["-l", "printk"],
        &["-g", "!*.h", "printk"],
    ] {
        let local = run_in(&sub, args);
        assert_eq!(local.status.code(), Some(0), "{args:?}");
        let served = run_in(&sub, &[&["--server", &addr][..], args].concat());
        assert_eq!(
            (served.status.code(), served.stdout),
            (local.status.code(), local.stdout),
            "{args:?}"
        );
    }
    shutdown(&mut server, &addr);
}

/// The server scopes a search to the client's cwd.
#[test]
fn a_client_below_the_served_tree_searches_its_subtree() {
    let (tmp, docs) = stage_files(&[("a.c", b"printk a\n"), ("b.h", b"printk b\n")]);
    std::fs::create_dir(docs.join("sub")).unwrap();
    std::fs::write(docs.join("sub/c.c"), "printk c\n").unwrap();
    std::fs::write(docs.join("sub/d.h"), "printk d\n").unwrap();
    let index = do_index(&docs, false).display().to_string();
    let mut server = Server::spawn(&["serve", &index, "--no-watch"]);
    let addr = server.addr();
    let sub = docs.join("sub");
    let found = |cwd: &Path, args: &[&str]| {
        let out = run_in(cwd, args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(
        found(&sub, &["--server", &addr, "-l", "printk"]),
        "c.c\nd.h\n"
    );
    assert_eq!(
        found(&sub, &["--index", &index, "-l", "printk"]),
        "c.c\nd.h\n"
    );
    assert_eq!(
        found(&sub, &["--server", &addr, "-l", "-g", "*.c", "printk"]),
        "c.c\n"
    );
    assert_eq!(
        found(tmp.path(), &["--server", &addr, "-l", "printk"]),
        "a.c\nb.h\nsub/c.c\nsub/d.h\n"
    );
    shutdown(&mut server, &addr);
}

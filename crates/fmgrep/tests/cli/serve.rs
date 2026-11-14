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
pub(crate) fn timeless(stdout: &[u8]) -> String {
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
        let mut serve = fmgrep();
        serve.args(["serve", &index, "--no-watch"]);
        let mut server = Server::spawn_cmd(serve.env("FMGREP_VERBOSE", "1"));
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
        let verbose = fmgrep()
            .args(["--server", addr, "printk"])
            .env("FMGREP_VERBOSE", "1")
            .output()
            .expect("spawn fmgrep");
        assert_eq!(stderr(&verbose), "", "the client prints no spans");
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
        let err = served_stderr(&mut server);
        assert!(!err.contains("note: span"), "{err}");
    }
}

/// `total_matches` of `printk`, asked of the server at `addr`.
fn served_printk(addr: &str) -> u64 {
    let report: serde_json::Value =
        serde_json::from_str(&client(addr, &["--json", "--", "printk"])).unwrap();
    report["queries"][0]["total_matches"].as_u64().unwrap()
}

/// Files source: an added file and an indexed one, each deleted and then
/// recreated under a server that asks a private watchman daemon, come
/// back at the next query; only the indexed one warns.
#[cfg(unix)]
#[test]
fn serve_sees_a_delete_then_recreate() {
    let (_tmp, docs) = stage();
    let index = do_index(&docs, false).display().to_string();
    std::fs::write(docs.join("d.bin"), b"zebra printk\n").unwrap();
    let Some(daemon) = crate::watchman::Daemon::start() else {
        return;
    };
    daemon.watch(&docs);
    let mut cmd = fmgrep();
    let mut server = Server::spawn_cmd(
        cmd.args(["serve", &index])
            .env("WATCHMAN_SOCK", &daemon.sock),
    );
    let addr = server.addr();
    assert_eq!(served_printk(&addr), 4);
    std::fs::remove_file(docs.join("d.bin")).unwrap();
    std::fs::remove_file(docs.join("a.bin")).unwrap();
    assert_eq!(served_printk(&addr), 1);
    std::fs::write(docs.join("d.bin"), b"zebra printk\n").unwrap();
    std::fs::write(docs.join("a.bin"), b"printk back\n").unwrap();
    assert_eq!(served_printk(&addr), 3);
    shutdown(&mut server, &addr);
    let stderr = served_stderr(&mut server);
    let excluding: Vec<&str> = stderr.lines().filter(|l| l.contains("excluding")).collect();
    assert!(
        excluding.len() == 1 && excluding[0].contains("a.bin"),
        "{stderr}"
    );
    let first = "warning: checked every file once (no record of an earlier search)";
    assert!(stderr.starts_with(first), "{stderr}");
    assert_eq!(stderr.matches("warning:").count(), 2, "{stderr}");
}

#[test]
fn serve_is_ready_only_once_added_files_are_found() {
    let (tmp, docs) = stage();
    let index = do_index(&docs, false).display().to_string();
    std::fs::write(docs.join("d.bin"), b"printk added\n").unwrap();
    // A server that asks watchman, when a private daemon can start.
    #[cfg(unix)]
    let daemon = crate::watchman::Daemon::start();
    let mut cases: Vec<(&[&str], Option<&Path>)> = vec![(&["--no-watch"], None)];
    #[cfg(unix)]
    if let Some(daemon) = &daemon {
        daemon.watch(&docs);
        cases.push((&[], Some(&daemon.sock)));
    }
    for (flags, sock) in cases {
        // One file for both streams keeps their writes in order.
        let log = tmp.path().join("serve.log");
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let mut cmd = fmgrep();
        if let Some(sock) = sock {
            cmd.env("WATCHMAN_SOCK", sock);
        }
        let mut server = Server(
            cmd.args([&["serve", &index][..], flags].concat())
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
    let manifest = other.join(".fmgrep/manifest.bin");
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
    let out = run_in(&copy, &["serve", "--stdio", "--no-watch"]);
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

/// `argv` from `cwd` with `FMGREP_SERVER` set to `server`.
fn run_env(cwd: &Path, server: &str, argv: &[&str]) -> Output {
    fmgrep()
        .current_dir(cwd)
        .env("FMGREP_SERVER", server)
        .args(argv)
        .output()
        .expect("spawn fmgrep")
}

/// An address where no server listens: a port that was just free.
fn closed_port() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().to_string()
}

/// `FMGREP_SERVER` stands for `--server` where no index is found by walking
/// up; an index found wins, with a warning; `--index` and an empty value
/// leave the variable out; `--status` reads it and `--shutdown` does not.
#[test]
fn fmgrep_server_gives_the_server() {
    let (tmp, docs) = stage();
    let index = do_index(&docs, false).display().to_string();
    let mut server = Server::spawn(&["serve", &index, "--no-watch"]);
    let addr = server.addr();
    let away = tmp.path().join("away");
    std::fs::create_dir(&away).unwrap();
    let words = ["-l", "printk"];
    let flag = run_in(&away, &[&["--server", &addr][..], &words].concat());
    assert_eq!(flag.status.code(), Some(0), "{}", stderr(&flag));
    let env = run_env(&away, &addr, &words);
    assert_eq!((env.status.code(), &env.stdout), (Some(0), &flag.stdout));
    assert_eq!(stderr(&env), "");

    // An index found by walking up wins: it sees a file the server misses.
    std::fs::write(docs.join("d.bin"), b"printk added\n").unwrap();
    ok(&["index", &docs.display().to_string()]);
    let local = run_in(&docs, &words);
    assert_eq!(
        String::from_utf8_lossy(&local.stdout),
        "a.bin\nb.bin\nd.bin\n"
    );
    let env = run_env(&docs, &addr, &words);
    assert_eq!((env.status.code(), &env.stdout), (Some(0), &local.stdout));
    let lines: Vec<String> = stderr(&env).lines().map(str::to_owned).collect();
    let dotdir = std::fs::canonicalize(&docs).unwrap().join(".fmgrep");
    let want = format!(
        "warning: Using local index {}, not the server at {addr} (FMGREP_SERVER)",
        dotdir.display()
    );
    assert_eq!(lines, [want]);

    // `--index` searches locally and says nothing; empty is unset.
    let env = run_env(&away, &addr, &[&["--index", &index][..], &words].concat());
    assert_eq!(
        String::from_utf8_lossy(&env.stdout),
        "a.bin\nb.bin\nd.bin\n"
    );
    assert_eq!(stderr(&env), "");
    let empty = run_env(&away, "", &words);
    assert_eq!(empty.status.code(), Some(2));
    assert!(
        stderr(&empty).contains("no .fmgrep index"),
        "{}",
        stderr(&empty)
    );

    let status = run_env(&away, &addr, &["--status"]);
    assert_eq!(status.status.code(), Some(0), "{}", stderr(&status));
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["files"], DOCS.len());
    let stop = run_env(&away, &addr, &["--shutdown"]);
    assert_eq!(stop.status.code(), Some(2));
    assert_eq!(
        stderr(&stop),
        "error: --shutdown needs --server ADDR; FMGREP_SERVER does not stop a server\n"
    );
    let status = run_in(&away, &["--status"]);
    assert_eq!(status.status.code(), Some(2));
    assert_eq!(
        stderr(&status),
        "error: --status needs --server ADDR or FMGREP_SERVER\n"
    );
    shutdown(&mut server, &addr);

    let closed = closed_port();
    let out = run_env(&away, &closed, &words);
    assert_eq!(out.status.code(), Some(2));
    let want = format!("error: cannot reach the server at {closed} (FMGREP_SERVER): ");
    assert!(stderr(&out).starts_with(&want), "{}", stderr(&out));
}

/// Bare `fmgrep` prints the help with `FMGREP_SERVER` set too: the
/// variable is not an argument.
#[test]
fn fmgrep_server_alone_prints_the_help() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_env(tmp.path(), &closed_port(), &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("Usage: fmgrep [OPTIONS] [TERM]..."),
        "{}",
        stderr(&out)
    );
}

/// An `FMGREP_SERVER` that is not UTF-8 fails only a search that would use
/// it, and names the variable. Unix only: the value is raw bytes.
#[cfg(unix)]
#[test]
fn a_malformed_fmgrep_server_fails_only_a_search_that_uses_it() {
    use std::os::unix::ffi::OsStrExt;
    let run_bad = |cwd: &Path, argv: &[&str]| {
        (fmgrep().current_dir(cwd))
            .env("FMGREP_SERVER", OsStr::from_bytes(b"\xff\xfe"))
            .args(argv)
            .output()
            .expect("spawn fmgrep")
    };
    let (tmp, docs) = stage();
    let index = do_index(&docs, false).display().to_string();
    let away = tmp.path().join("away");
    std::fs::create_dir(&away).unwrap();
    for argv in [
        &["--help"][..],
        &["--version"],
        &["serve", "--help"],
        &["status", "--help"],
    ] {
        let out = run_bad(&away, argv);
        assert_eq!(out.status.code(), Some(0), "{argv:?}: {}", stderr(&out));
    }
    let bare = run_bad(&away, &[]);
    assert_eq!(bare.status.code(), Some(2));
    assert!(stderr(&bare).contains("Usage: fmgrep"), "{}", stderr(&bare));

    // `--server` and `--index` win; a subcommand runs.
    let mut server = Server::spawn(&["serve", &index, "--no-watch"]);
    let addr = server.addr();
    let words = ["-l", "printk"];
    let hits = "a.bin\nb.bin\n";
    for flag in [["--server", &addr], ["--index", &index]] {
        let out = run_bad(&away, &[&flag[..], &words].concat());
        assert_eq!(out.status.code(), Some(0), "{flag:?}: {}", stderr(&out));
        assert_eq!(
            (String::from_utf8_lossy(&out.stdout), stderr(&out)),
            (hits.into(), "".into())
        );
    }
    shutdown(&mut server, &addr);
    let out = run_bad(&away, &["index", &docs.display().to_string()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    // An index found by walking up answers; the value is not looked at.
    let out = run_bad(&docs, &words);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        (String::from_utf8_lossy(&out.stdout), stderr(&out)),
        (hits.into(), "".into())
    );

    // `status` of a search that an index found by walking up answers.
    let local = run_in(&docs, &["status", "--json"]);
    assert_eq!(local.status.code(), Some(0), "{}", stderr(&local));
    let out = run_bad(&docs, &["status", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!((&out.stdout, stderr(&out)), (&local.stdout, "".into()));

    // A search, `status` or `--status` that would reach the server fails.
    for argv in [&words[..], &["--status"], &["status"]] {
        let out = run_bad(&away, argv);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert_eq!(
            stderr(&out),
            "error: FMGREP_SERVER=\"\\xFF\\xFE\" is not UTF-8\n"
        );
    }
}

/// A served `idx` of the staged docs, and a `.fmgrep` in them that also
/// holds `s.txt`, written once the server checked the tree: `-l status`
/// finds it only in the local index. Then `<tmp>/away`, below no index.
fn served_and_local() -> (tempfile::TempDir, std::path::PathBuf, Server, String) {
    let (tmp, docs) = stage();
    let index = do_index(&docs, false).display().to_string();
    let mut server = Server::spawn(&["serve", &index, "--no-watch"]);
    let addr = server.addr();
    std::fs::write(docs.join("s.txt"), b"status\n").unwrap();
    ok(&["index", &docs.display().to_string()]);
    std::fs::create_dir(tmp.path().join("away")).unwrap();
    (tmp, docs, server, addr)
}

/// What a search, or `status`, read: the local index with the note a
/// search warns, the server, or the error that both print.
#[derive(Debug, PartialEq)]
enum Source {
    Local(String),
    Served,
    Failed(String),
}

/// `fmgrep status` names what a search started in PATH (else the cwd)
/// reads: the index found by walking up, which wins over `FMGREP_SERVER`;
/// else that server; with `--server`, that server, else the index it
/// falls back to. `--status` prints the server's own reply, which
/// `status --json` holds under `.server.status`; `-- status` searches.
#[test]
fn status_names_what_a_search_reads() {
    let (tmp, docs, mut server, addr) = served_and_local();
    let away = tmp.path().join("away");
    let dotdir = std::fs::canonicalize(docs.join(".fmgrep")).unwrap();
    let tree = std::fs::canonicalize(&docs).unwrap();
    let lines = |out: &Output| -> Vec<String> {
        assert_eq!(out.status.code(), Some(0), "{}", stderr(out));
        assert_eq!(stderr(out), "");
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines().map(str::to_owned).collect()
    };

    let local = lines(&run_in(&docs, &["status"]));
    let first = format!("a search here uses the local index {}", dotdir.display());
    assert_eq!(local[0], first);
    let passed = lines(&run_env(&docs, &addr, &["status"]));
    let note = format!(", not the server at {addr} (FMGREP_SERVER)");
    assert_eq!(passed[0], format!("{first}{note}"));
    assert_eq!(passed[1..], local[1..], "the server is not asked");
    let sub = lines(&run_in(&away, &["status", &docs.display().to_string()]));
    let first = format!(
        "a search in {} uses the local index {}",
        tree.display(),
        dotdir.display()
    );
    assert_eq!(sub[0], first);

    let served = lines(&run_env(&away, &addr, &["status"]));
    assert_eq!(
        served[0],
        format!("a search here uses the server at {addr} (FMGREP_SERVER)")
    );
    let want = regex::Regex::new(&format!(
        r"^server: tree {}, 3 files, \d+ B of text, [\d.]+ KiB of index, loaded in \d+ ms$",
        regex::escape(&tree.display().to_string())
    ))
    .unwrap();
    assert!(want.is_match(&served[1]), "{served:?}");
    let index = regex::Regex::new(r"^index: format 8, built \d+ s ago$").unwrap();
    assert!(index.is_match(&served[2]), "{served:?}");
    let once = "changes: the server checked the tree once at its start and asks watchman nothing \
                (--no-watch)";
    assert_eq!(served[3..], [once]);
    let flag = lines(&run_in(&docs, &["status", "--server", &addr]));
    assert_eq!(flag[0], format!("a search here uses the server at {addr}"));

    let out = run_env(&away, &addr, &["status", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["uses"], "server");
    assert_eq!(report["server"]["addr"], addr.as_str());
    assert_eq!(report["server"]["env"], true);
    assert!(report["index"].is_null() && report["note"].is_null());
    let out = run_env(&docs, &addr, &["--status"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let raw: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(raw["files"], DOCS.len(), "the served index has no s.txt");
    assert_eq!(report["server"]["status"], raw);

    // Down: FMGREP_SERVER fails; --server falls back to the index above.
    let closed = closed_port();
    let out = run_env(&away, &closed, &["status"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let want = format!("error: cannot reach the server at {closed} (FMGREP_SERVER): ");
    assert!(stderr(&out).starts_with(&want), "{}", stderr(&out));
    let fell = lines(&run_in(&docs, &["status", "--server", &closed]));
    let want = format!(
        "a search here uses the local index {}: cannot reach the server at {closed} (",
        dotdir.display()
    );
    assert!(fell[0].starts_with(&want), "{fell:?}");
    assert_eq!(fell[1..], local[1..]);

    // `-- status` is a search: through the server from away, of the local
    // index (with the warning) under docs.
    let out = run_env(&away, &addr, &["-l", "--", "status"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!((out.stdout.len(), stderr(&out)), (0, "".into()));
    let out = run_env(&docs, &addr, &["-l", "--", "status"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "s.txt\n");
    assert!(stderr(&out).starts_with("warning: Using local index"));
    shutdown(&mut server, &addr);
}

/// What `status --json` names, in `Source`'s terms.
fn reported(out: &Output, dotdir: &Path, live: &str) -> Source {
    if out.status.code() != Some(0) {
        return Source::Failed(stderr(out));
    }
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    match r["uses"].as_str() {
        Some("index") => {
            assert_eq!(r["index"]["dir"], dotdir.to_str().unwrap());
            Source::Local(r["note"].as_str().unwrap_or_default().to_owned())
        }
        Some("server") => {
            assert_eq!(r["server"]["addr"], live);
            Source::Served
        }
        _ => panic!("{r}"),
    }
}

/// In every row of the choice (cwd below an index or not; FMGREP_SERVER
/// unset, up or down; `--server` absent, up or down) `status` names the
/// source that a search reads, with the search's note or error.
#[test]
fn status_and_a_search_agree_on_the_source() {
    let (tmp, docs, mut server, live) = served_and_local();
    let away = tmp.path().join("away");
    let closed = closed_port();
    let dotdir = std::fs::canonicalize(docs.join(".fmgrep")).unwrap();
    let warned = format!("warning: Using local index {}", dotdir.display());
    let mut rows = Vec::new();
    for cwd in [&docs, &away] {
        for env in [None, Some(&live), Some(&closed)] {
            for flag in [None, Some(&live), Some(&closed)] {
                let run = |head: &[&str], tail: &[&str]| {
                    let mut cmd = fmgrep();
                    cmd.current_dir(cwd).args(head);
                    if let Some(addr) = env {
                        cmd.env("FMGREP_SERVER", addr);
                    }
                    if let Some(addr) = flag {
                        cmd.args(["--server", addr]);
                    }
                    cmd.args(tail).output().expect("spawn fmgrep")
                };
                let what = format!("{cwd:?} {env:?} {flag:?}");
                let out = run(&[], &["-l", "status"]);
                let err = stderr(&out);
                let searched = match (out.status.code(), &out.stdout[..]) {
                    (Some(0), b"s.txt\n") => match err.strip_prefix(&warned) {
                        Some(note) => Source::Local(note.trim_end().to_owned()),
                        None if err.is_empty() => Source::Local(String::new()),
                        None => panic!("{what}: {err}"),
                    },
                    (Some(1), b"") => Source::Served,
                    (Some(2), b"") => Source::Failed(err),
                    _ => panic!("{what}: {out:?}"),
                };
                let status = run(&["status"], &["--json"]);
                assert_eq!(reported(&status, &dotdir, &live), searched, "{what}");
                let kind = match &searched {
                    Source::Served => "server",
                    Source::Failed(_) => "error",
                    Source::Local(n) if n.is_empty() => "index",
                    Source::Local(n) if n.starts_with(": cannot reach") => "fallback",
                    Source::Local(_) => "passed over",
                };
                let up = |a: Option<&String>| a == Some(&live);
                let want = match (cwd == &docs, env, flag) {
                    (_, _, Some(_)) if up(flag) => "server",
                    (true, _, Some(_)) => "fallback",
                    (true, None, None) => "index",
                    (true, Some(_), None) => "passed over",
                    (false, _, None) if up(env) => "server",
                    (false, _, _) => "error",
                };
                assert_eq!(kind, want, "{what}");
                rows.push(kind);
            }
        }
    }
    assert_eq!(rows.len(), 18);
    shutdown(&mut server, &live);
}

/// `--server` that cannot be reached: an index found by walking up answers,
/// with one warning; with none, the search fails.
#[test]
fn an_unreachable_server_falls_back_to_a_local_index() {
    let (tmp, docs) = stage();
    ok(&["index", &docs.display().to_string()]);
    let closed = closed_port();
    let words = ["--server", &closed, "-l", "printk"];
    let out = run_in(&docs, &words);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.bin\nb.bin\n");
    let lines: Vec<String> = stderr(&out).lines().map(str::to_owned).collect();
    let dotdir = std::fs::canonicalize(&docs).unwrap().join(".fmgrep");
    let want = format!(
        "warning: Using local index {}: cannot reach the server at {closed} (",
        dotdir.display()
    );
    assert!(lines.len() == 1 && lines[0].starts_with(&want), "{lines:?}");
    let out = run_in(tmp.path(), &words);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let want = format!("error: cannot reach the server at {closed}: ");
    assert!(stderr(&out).starts_with(&want), "{}", stderr(&out));
}

/// Under `FMGREP_VERBOSE=1` a search that a local index answers ends with
/// its spans on stderr, after the warning that the server was passed over;
/// a search that a server answers prints none. Without the variable no
/// search prints spans.
#[test]
fn spans_print_only_when_a_local_index_answers() {
    let (tmp, docs) = stage();
    let index = do_index(&docs, false).display().to_string();
    ok(&["index", &docs.display().to_string()]);
    let mut server = Server::spawn(&["serve", &index, "--no-watch"]);
    let (live, closed) = (server.addr(), closed_port());
    let away = tmp.path().join("away");
    std::fs::create_dir(&away).unwrap();
    let dotdir = std::fs::canonicalize(&docs).unwrap().join(".fmgrep");
    let dotdir = dotdir.display();
    let unreachable =
        format!("warning: Using local index {dotdir}: cannot reach the server at {closed} (");
    let passed_over =
        format!("warning: Using local index {dotdir}, not the server at {live} (FMGREP_SERVER)");
    // (cwd, FMGREP_SERVER, --server, the warning when a local index answers)
    let cases = [
        (&docs, None, Some(&closed), Some(&unreachable)),
        (&docs, Some(&live), None, Some(&passed_over)),
        (&away, None, Some(&live), None),
        (&away, Some(&live), None, None),
    ];
    for (cwd, env, flag, warning) in cases {
        for verbose in [false, true] {
            let mut cmd = fmgrep();
            cmd.current_dir(cwd).env_remove("FMGREP_VERBOSE");
            if verbose {
                cmd.env("FMGREP_VERBOSE", "1");
            }
            if let Some(addr) = env {
                cmd.env("FMGREP_SERVER", addr);
            }
            if let Some(addr) = flag {
                cmd.args(["--server", addr]);
            }
            let start = Instant::now();
            let out = cmd.args(["-l", "printk"]).output().expect("spawn fmgrep");
            let wall = start.elapsed();
            let what = format!("{cwd:?} {env:?} {flag:?} {verbose}");
            let err = stderr(&out);
            assert_eq!(out.status.code(), Some(0), "{what}: {err}");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "a.bin\nb.bin\n");
            // The warning comes first, and the spans last.
            let spans = match warning {
                Some(want) => {
                    let (first, rest) = err.split_once('\n').expect(&err);
                    assert!(first.starts_with(want.as_str()), "{what}: {err}");
                    rest
                }
                None => &err,
            };
            match verbose && warning.is_some() {
                true => crate::roundtrip::check_spans(spans, false, wall),
                false => assert_eq!(spans, "", "{what}"),
            }
        }
    }
    shutdown(&mut server, &live);
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

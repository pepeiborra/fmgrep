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
        let reply = ok(&["status", "--server", addr, "--json"]);
        let status: serde_json::Value = serde_json::from_slice(&reply.stdout).unwrap();
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
/// leave the variable out; `status` reads it and `--shutdown` does not.
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

    let status = run_env(&away, &addr, &["status", "--json"]);
    assert_eq!(status.status.code(), Some(0), "{}", stderr(&status));
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["files"], DOCS.len());
    let stop = run_env(&away, &addr, &["--shutdown"]);
    assert_eq!(stop.status.code(), Some(2));
    assert_eq!(
        stderr(&stop),
        "error: --shutdown needs --server ADDR; FMGREP_SERVER does not stop a server\n"
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

    // `status` reports on that index; the value is not looked at.
    let local = run_in(&docs, &["status", "--json"]);
    assert_eq!(local.status.code(), Some(0), "{}", stderr(&local));
    let out = run_bad(&docs, &["status", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!((&out.stdout, stderr(&out)), (&local.stdout, "".into()));

    // A search or `status` that would reach the server fails.
    for argv in [&words[..], &["status"]] {
        let out = run_bad(&away, argv);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert_eq!(
            stderr(&out),
            "error: FMGREP_SERVER=\"\\xFF\\xFE\" is not UTF-8\n"
        );
    }
}

/// `fmgrep status` names the index or server a search in the same place
/// uses, with the note that search warns, and fails where it fails, in
/// its words; `status --server` is a search's `--server`. `fmgrep --
/// status` searches for the word.
#[test]
fn status_names_what_a_search_uses() {
    let (tmp, docs) = stage();
    let index = do_index(&docs, false).display().to_string();
    let mut server = Server::spawn(&["serve", &index, "--no-watch"]);
    let addr = server.addr();
    std::fs::write(docs.join("s.txt"), b"status\n").unwrap();
    ok(&["index", &docs.display().to_string()]);
    let away = tmp.path().join("away");
    std::fs::create_dir(&away).unwrap();
    let dotdir = std::fs::canonicalize(&docs).unwrap().join(".fmgrep");
    let closed = closed_port();
    let first = |out: &Output| {
        assert_eq!(out.status.code(), Some(0), "{}", stderr(out));
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.lines().next().unwrap_or_default().to_owned()
    };

    let local = run_in(&docs, &["status"]);
    let uses = format!("a search here uses the local index {}", dotdir.display());
    assert_eq!(first(&local), uses);
    let out = run_env(&docs, &addr, &["status"]);
    let not = format!("{uses}, not the server at {addr} (FMGREP_SERVER)");
    assert_eq!((first(&out), stderr(&out)), (not, "".into()));
    let out = run_in(&docs, &["status", "--server", &closed]);
    let down = format!("{uses}: cannot reach the server at {closed} (");
    assert!(first(&out).starts_with(&down), "{}", first(&out));
    let out = run_in(&away, &["status", &docs.display().to_string()]);
    let uses_in = format!("a search in {} uses the local index", docs.display());
    assert!(first(&out).starts_with(&uses_in), "{}", first(&out));

    // `--json` prints the manifest, whatever the variable holds, and the
    // note.
    let json = run_in(&docs, &["status", "--json"]);
    let m: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        (&m["num_docs"], &m["note"]),
        (&(DOCS.len() + 1).into(), &().into())
    );
    for value in [addr.as_str(), "not an address", ""] {
        let out = run_env(&docs, value, &["status", "--json"]);
        assert_eq!(out.status.code(), Some(0), "{value}: {}", stderr(&out));
        let mut got: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let note = got["note"].take();
        assert_eq!((got, stderr(&out)), (m.clone(), "".into()));
        let want = format!(", not the server at {value} (FMGREP_SERVER)");
        let want = (!value.is_empty()).then_some(want);
        assert_eq!(note.as_str(), want.as_deref(), "{value}");
    }

    // With no index above, the server answers, which serves the index
    // without s.txt; under `--json` its reply.
    let reply = run_in(&docs, &["status", "--server", &addr, "--json"]);
    let served: serde_json::Value = serde_json::from_slice(&reply.stdout).unwrap();
    assert_eq!(served["files"], DOCS.len());
    let out = run_env(&away, &addr, &["status"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    let uses = format!("a search here uses the server at {addr} (FMGREP_SERVER)");
    assert_eq!(lines[..3], [&uses, "store_docs: false", "files: 3"]);
    let out = run_env(&away, &addr, &["status", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(out.stdout, reply.stdout);

    // Where a search fails before it searches, status fails with its words.
    for value in [closed.as_str(), "not an address", ""] {
        let search = run_env(&away, value, &["printk"]);
        let out = run_env(&away, value, &["status"]);
        assert_eq!(out.status.code(), Some(2), "{value}");
        assert!(out.stdout.is_empty(), "{value}");
        assert_eq!(stderr(&out), stderr(&search), "{value}");
    }

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

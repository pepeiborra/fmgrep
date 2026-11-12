use serde_json::json;

use super::*;

fn token(clock: &str) -> Token {
    Token {
        sock: "/run/watchman/sock".into(),
        watch: "/src".into(),
        relative: "linux".into(),
        clock: clock.into(),
    }
}

fn index(dir: &str, started_ns: i64) -> Index {
    Index {
        dir: dir.into(),
        started_ns,
    }
}

#[test]
fn loads_ask_from_50_000_docs() {
    assert_eq!(MIN_DOCS, 50_000);
    crate::tests::ASK_FROM.set(Some(10));
    assert!(!due(9) && due(10));
    crate::tests::ASK_FROM.set(None);
    assert!(!due(usize::MAX), "tests ask only when told");
}

#[test]
fn the_cap_is_where_the_stat_pass_gets_cheaper() {
    assert_eq!(named_cap(1_250_000), 156_250);
    assert_eq!(named_cap(10), 64, "a small tree's floor");
}

#[test]
fn a_record_is_named_by_its_tree_and_index() {
    let tmp = tempfile::tempdir().unwrap();
    crate::tests::CACHE.set(Some(tmp.path().into()));
    let one = path("/src/linux", "/src/linux/.fmgrep").unwrap();
    assert_eq!(one.parent(), Some(tmp.path()));
    let name = one.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("linux-") && name.ends_with(".json") && name.len() == 27,
        "{name}"
    );
    assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325, "FNV-1a's offset basis");
    assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
    let other_index = path("/src/linux", "/idx/linux").unwrap();
    let other_tree = path("/src/linux2", "/src/linux/.fmgrep").unwrap();
    assert!(one != other_index && one != other_tree && other_index != other_tree);
    assert_eq!(path("/", "/idx").unwrap().file_name().unwrap().len(), 26);
    crate::tests::CACHE.set(None);
    assert_eq!(path("/src/linux", "/src/linux/.fmgrep"), None);
}

#[test]
fn a_record_is_used_only_by_its_index_build() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    crate::tests::CACHE.set(Some(cache.clone()));
    let idx = index("/src/linux/.fmgrep", 7);
    let baseline = vec!["a.c".to_owned(), "new".to_owned()];
    save(
        Path::new("/src/linux"),
        &idx,
        token("c:1:2"),
        baseline.clone(),
        64,
    );
    let record = Record::read("/src/linux", &idx).unwrap();
    let want = Record {
        v: 1,
        root: "/src/linux".into(),
        index: "/src/linux/.fmgrep".into(),
        index_started_ns: 7,
        token: token("c:1:2"),
        baseline: Some(baseline),
    };
    assert_eq!(record, want);
    let file = path("/src/linux", "/src/linux/.fmgrep").unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.starts_with(r#"{"v":1,"root":"/src/linux","index":"#),
        "{text}"
    );
    assert_eq!(
        std::fs::read_dir(&cache).unwrap().count(),
        1,
        "no temp left"
    );
    let rebuilt = Record::read("/src/linux", &index("/src/linux/.fmgrep", 8));
    assert!(
        rebuilt
            .unwrap_err()
            .ends_with(" was written for another index build")
    );
    std::fs::remove_file(&file).unwrap();
    let none = Record::read("/src/linux", &idx).unwrap_err();
    assert_eq!(none, "no record of an earlier search");
    for bad in [&b"{"[..], br#"{"v":1}"#, b""] {
        std::fs::write(&file, bad).unwrap();
        let err = Record::read("/src/linux", &idx).unwrap_err();
        assert!(err.contains("malformed"), "{err}");
    }
    let mut other = serde_json::to_value(&want).unwrap();
    other["v"] = json!(2);
    std::fs::write(&file, other.to_string()).unwrap();
    assert!(Record::read("/src/linux", &idx).is_err(), "another version");
    let many: Vec<String> = (0..65).map(|i| format!("n{i}.c")).collect();
    save(Path::new("/src/linux"), &idx, token("c:1:3"), many, 64);
    let over = Record::read("/src/linux", &idx).unwrap();
    assert_eq!(over.baseline, None, "past the cap");
}

#[test]
fn a_record_that_cannot_be_saved_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = || crate::tests::NOTES.with_borrow(|n| std::mem::take(&mut *n.lock().unwrap()));
    crate::tests::CACHE.set(None);
    notes();
    save(
        Path::new("/src"),
        &index("/i", 1),
        token("c:1:2"),
        vec![],
        64,
    );
    let none = "cannot save the watchman token (no cache directory: set XDG_CACHE_HOME or \
                HOME); the next search checks again";
    assert_eq!(notes(), [none]);
    let file = tmp.path().join("file");
    std::fs::write(&file, b"").unwrap();
    crate::tests::CACHE.set(Some(file.join("fmgrep")));
    save(
        Path::new("/src"),
        &index("/i", 1),
        token("c:1:2"),
        vec![],
        64,
    );
    let got = notes();
    assert!(
        got.len() == 1
            && got[0].starts_with("cannot save ")
            && got[0].contains("/file/fmgrep/src-"),
        "{got:?}"
    );
}

#[cfg(unix)]
mod watchman_tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::source::journal::watchman::{Watchman, hides, ticked};
    use crate::tests::watchman::Daemon;

    /// Under load.
    const SLACK: Duration = Duration::from_millis(1500);

    /// Replies that a real daemon gives only in states a test cannot make
    /// it hold: a query error text, two texts of a sync that timed out (the
    /// second came from a real daemon), the error of a sync during a crawl,
    /// to a query or to a clock as well, and the clocks of a crawl's tail,
    /// which share one tick.
    const ERROR: u8 = 0;
    const SYNC_FAILED: u8 = 1;
    const NO_COOKIE: u8 = 2;
    const CRAWL: u8 = 3;
    const CRAWL_CLOCK: u8 = 4;
    const STUCK: u8 = 5;

    const CRAWL_TEXT: &str = "synchronization failed: syncToNow: timed out waiting for cookie \
                              file to be observed by watcher within 74 milliseconds: Timer \
                              expired. (performing initial crawl)";

    /// A daemon on a socket in `dir` that watches `/w` and answers as
    /// `mode` says.
    fn fake(dir: &Path, mode: Arc<AtomicU8>) -> PathBuf {
        let sock = dir.join("sock");
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            for (tick, conn) in listener.incoming().enumerate() {
                let mut conn = conn.unwrap();
                let mut line = String::new();
                BufReader::new(&conn).read_line(&mut line).unwrap();
                let cmd: serde_json::Value = serde_json::from_str(&line).unwrap();
                let mode = mode.load(Ordering::Relaxed);
                let reply = match (cmd[0].as_str().unwrap(), mode) {
                    ("watch-list", _) => json!({"roots": ["/w"]}),
                    ("get-config", _) => json!({"config": {}}),
                    ("clock", CRAWL_CLOCK) => json!({"error": CRAWL_TEXT}),
                    ("clock", STUCK) => json!({"clock": "c:1:2"}),
                    ("clock", _) => json!({"clock": format!("c:1:{tick}")}),
                    ("query", ERROR) => json!({"error": "bad"}),
                    ("query", SYNC_FAILED) => json!({"error": "synchronization failed: \
                        syncToNow: timed out waiting for cookie file to be observed by \
                        watcher within 74 milliseconds: Timer expired."}),
                    ("query", NO_COOKIE) => json!({"error": "std::system_error: syncToNow: \
                        timed out waiting for cookie file to be observed by watcher within 74 \
                        milliseconds: Connection timed out"}),
                    ("query", CRAWL | CRAWL_CLOCK) => json!({"error": CRAWL_TEXT}),
                    ("query", _) => json!({"error": "bad"}),
                    (other, _) => panic!("unexpected {other}"),
                };
                writeln!(conn, "{}", json!({"unilateral": true, "log": "noise"})).unwrap();
                writeln!(conn, "{reply}").unwrap();
            }
        });
        sock
    }

    /// A token of the fake on `sock`, of the tree `/w/sub`.
    fn faked(sock: &Path) -> Token {
        Token {
            sock: sock.to_str().unwrap().into(),
            watch: "/w".into(),
            relative: "sub".into(),
            clock: "c:1:1".into(),
        }
    }

    /// A private daemon of this test, which this thread finds; `None`
    /// without watchman.
    fn live() -> Option<Daemon> {
        let daemon = Daemon::start()?;
        crate::tests::WATCHMAN.set(Some(daemon.sock.clone()));
        Some(daemon)
    }

    fn at(daemon: &Daemon) -> Watchman {
        Watchman::at(daemon.sock.clone(), Duration::from_secs(5))
    }

    fn canonical(path: &Path) -> PathBuf {
        path.canonicalize().unwrap()
    }

    fn put(root: &Path, rel: &str) {
        std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
        std::fs::write(root.join(rel), rel).unwrap();
    }

    /// A token of the watch `w` for the tree `w/sub`, then the paths
    /// changed since it, relative to the tree; a restart and a watch gone
    /// are refused.
    #[test]
    fn watchman_client_speaks_the_protocol() {
        let Some(mut daemon) = live() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let (watch, root) = (canonical(tmp.path()), canonical(tmp.path()).join("sub"));
        std::fs::create_dir(&root).unwrap();
        daemon.watch(&watch);
        let marked = at(&daemon).mark(&root).expect("a clock");
        let w = watch.to_str().unwrap();
        assert_eq!(
            (marked.watch.as_str(), marked.relative.as_str()),
            (w, "sub")
        );
        assert_eq!(Path::new(&marked.sock), daemon.sock);
        daemon.sent();
        put(&root, "a.c");
        put(&root, "d/x.c");
        put(&watch, "beside.c");
        let since = |root: &Path| at(&daemon).since(&marked, root);
        // Watchman lists the paths in no set order.
        let mut paths = since(&root).unwrap();
        paths.sort();
        assert_eq!(paths, ["a.c", "d", "d/x.c"]);
        assert_eq!(daemon.sent(), ["query"]);
        let another = since(&watch).unwrap_err();
        assert_eq!(another, "the token watches another root");
        daemon.unwatch(&watch);
        let gone = format!("watchman is not watching {}", root.display());
        assert_eq!(since(&root), Err(gone), "the watch is gone");
        daemon.watch(&watch);
        let restarted = since(&root).unwrap_err();
        assert!(restarted.contains("restarted"), "{restarted}");
        let marked = at(&daemon).mark(&root).unwrap();
        daemon.restart();
        daemon.watch(&watch);
        let restarted = at(&daemon).since(&marked, &root).unwrap_err();
        assert!(restarted.contains("restarted"), "{restarted}");
        daemon.stop();
        let none = at(&daemon);
        let gone = none.mark(&root).unwrap_err();
        assert!(
            gone.starts_with("no watchman daemon answers on ") && gone.ends_with(')'),
            "{gone}"
        );
        assert!(!none.late(), "a daemon gone is not late");
    }

    /// An error text, and the texts of a sync that timed out, which mean
    /// it did not answer in time, so the record is kept.
    #[test]
    fn an_error_text_is_the_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let mode = Arc::new(AtomicU8::new(ERROR));
        let sock = fake(tmp.path(), Arc::clone(&mode));
        let root = Path::new("/w/sub");
        for (m, want, late) in [
            (ERROR, "watchman: bad", false),
            (SYNC_FAILED, "watchman did not answer within 5000 ms", true),
            (NO_COOKIE, "watchman did not answer within 5000 ms", true),
            (CRAWL, "watchman is still crawling /w/sub", true),
        ] {
            mode.store(m, Ordering::Relaxed);
            let w = Watchman::at(sock.clone(), Duration::from_secs(5));
            assert_eq!(w.since(&faked(&sock), root), Err(want.into()), "mode {m}");
            assert_eq!(w.late(), late, "mode {m}");
        }
    }

    /// Where no watch covers the tree, watchman is asked to watch it; else
    /// the token belongs to the deepest watch above it, `w`, not the
    /// tree's parent nor `w/subway`; no token where ignore_dirs hide part
    /// of the tree.
    #[test]
    fn watchman_takes_a_token_of_a_watch_already_there() {
        let Some(daemon) = live() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let top = canonical(tmp.path());
        let root = top.join("w/sub");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(top.join("w/subway")).unwrap();
        let watched = |t: &Token| (t.watch.clone(), t.relative.clone());
        let mark = || at(&daemon).mark(&root);
        let marked = mark().expect("asked to watch it");
        let tree = root.to_str().unwrap().to_owned();
        assert_eq!(
            watched(&marked),
            (tree, String::new()),
            "the tree is the watch"
        );
        let asked = [
            "watch-list",
            "watch-project",
            "get-config",
            "clock",
            "clock",
        ];
        assert_eq!(daemon.sent(), asked);
        assert!(at(&daemon).since(&marked, &root).is_ok());
        daemon.unwatch(&root);
        // Inner first: watch-project takes a watch above, when there is one.
        for dir in [&top.join("w/subway"), &top.join("w"), &top] {
            daemon.watch(dir);
        }
        assert_eq!(daemon.roots().len(), 3);
        let marked = mark().expect("a token of the watch above");
        let w = top.join("w");
        let above = (w.to_str().unwrap().to_owned(), "sub".to_owned());
        assert_eq!(watched(&marked), above);
        let two_clocks = ["watch-list", "get-config", "clock", "clock"];
        assert_eq!(daemon.sent(), two_clocks, "no watch-project");
        daemon.unwatch(&top);
        // Watchman reads ignore_dirs when it adds the watch.
        let configured = |config: &str| {
            daemon.unwatch(&w);
            std::fs::write(w.join(".watchmanconfig"), config).unwrap();
            daemon.watch(&w);
        };
        configured(r#"{"ignore_dirs": ["build", "Sub2/x"]}"#);
        assert!(mark().is_ok(), "ignore_dirs beside the tree");
        configured(r#"{"ignore_dirs": ["build", "sub/out"]}"#);
        let why = format!("watchman's ignore_dirs hide part of {}", root.display());
        assert_eq!(mark(), Err(why));
    }

    #[test]
    fn ignore_dirs_hide_at_under_or_above_the_tree() {
        for (dirs, relative, hidden) in [
            (json!([]), "", false),
            (json!(["build"]), "", true),
            (json!(["build"]), "sub", false),
            (json!(["subway", "a/sub"]), "sub", false),
            (json!(["sub"]), "sub", true),
            (json!(["sub/out/"]), "sub", true),
            (json!(["a"]), "a/b", true),
            (json!(["A/B/c"]), "a/b", true),
            (json!(["./sub"]), "x", true),
            (json!(["../x"]), "x", true),
            (json!([1]), "x", true),
            (json!("build"), "x", true),
        ] {
            assert_eq!(hides(&dirs, relative), hidden, "{dirs} {relative}");
        }
    }

    /// A hung daemon (stopped in place) costs a search at most
    /// [`ASK_WAIT`], and gives no token.
    #[test]
    fn a_search_never_waits_long_for_watchman() {
        let Some(daemon) = live() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path());
        let old = at(&daemon).mark(&root).expect("a token");
        daemon.hang();
        let start = Instant::now();
        let w = Watchman::at(daemon.sock.clone(), ASK_WAIT);
        let got = w.mark(&root).and_then(|_| w.since(&old, &root));
        let took = start.elapsed();
        daemon.resume();
        assert!(took < ASK_WAIT + SLACK, "took {took:?}");
        assert_eq!(got, Err("watchman did not answer within 100 ms".into()));
        assert!(w.late());
    }

    /// Late or crawling, as watchman's error texts say, to the token
    /// question or to the clocks.
    #[test]
    fn a_search_says_why_watchman_was_late() {
        let tmp = tempfile::tempdir().unwrap();
        let mode = Arc::new(AtomicU8::new(SYNC_FAILED));
        let sock = fake(tmp.path(), Arc::clone(&mode));
        let root = Path::new("/w/sub");
        for (m, want) in [
            (SYNC_FAILED, "watchman did not answer within 100 ms"),
            (CRAWL, "watchman is still crawling /w/sub"),
            (CRAWL_CLOCK, "watchman is still crawling /w/sub"),
        ] {
            mode.store(m, Ordering::Relaxed);
            let w = Watchman::at(sock.clone(), ASK_WAIT);
            let got = w.mark(root).and_then(|_| w.since(&faked(&sock), root));
            assert_eq!(got, Err(want.into()), "mode {m}");
            assert!(w.late(), "mode {m}");
        }
    }

    /// The new token came and only the token question was late: the search
    /// keeps the new token, to save with what its stat pass finds.
    #[test]
    fn a_late_answer_keeps_the_new_token() {
        let tmp = tempfile::tempdir().unwrap();
        crate::tests::CACHE.set(Some(tmp.path().join("cache")));
        let sock = fake(tmp.path(), Arc::new(AtomicU8::new(SYNC_FAILED)));
        let (root, idx) = (Path::new("/w/sub"), index("/i", 1));
        save(root, &idx, faked(&sock), vec!["b.c".into()], 64);
        let asked = ask(root, &idx, 1000);
        let token = asked.token.expect("the new token");
        assert_ne!(token.clock, faked(&sock).clock);
        assert!(asked.paths.is_none(), "every file is checked");
        let notes = crate::tests::NOTES.with_borrow(|n| std::mem::take(&mut *n.lock().unwrap()));
        let late = "watchman cannot say what changed (watchman did not answer within 100 ms); \
                    checked every file";
        assert_eq!(notes, [late]);
    }

    /// A crawl's tail answers syncs at once, all with the crawl's tick, which
    /// changes made then get too: no token.
    #[test]
    fn a_clock_from_a_crawls_tail_is_no_token() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = fake(tmp.path(), Arc::new(AtomicU8::new(STUCK)));
        let w = Watchman::at(sock, Duration::from_secs(5));
        assert_eq!(
            w.mark(Path::new("/w/sub")),
            Err("watchman is still crawling /w/sub".into())
        );
        assert!(w.late());
    }

    #[test]
    fn a_token_is_a_clock_ticked_past_the_first() {
        for (a, b, later) in [
            ("c:1731333968:123:1:41", "c:1731333968:123:1:42", true),
            ("c:1731333968:123:1:42", "c:1731333968:123:1:42", false),
            ("c:1731333968:123:1:42", "c:1731333968:123:1:41", false),
            ("c:1731333968:123:1:41", "c:1731333968:123:2:42", false),
            ("c:1731333968:123:1:41", "c:1731339999:456:1:42", false),
            ("c:1:x", "c:1:2", false),
            ("n:foo", "c:1:2", false),
        ] {
            assert_eq!(ticked(a, b), later, "{a} {b}");
        }
    }

    #[test]
    fn watchman_gives_up_on_a_socket_that_never_accepts() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("sock");
        let _listener = UnixListener::bind(&sock).unwrap();
        let start = Instant::now();
        let w = Watchman::at(sock, Duration::from_secs(1));
        let why = w.mark(Path::new("/w/sub")).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(1) + SLACK);
        assert_eq!(why, "watchman did not answer within 1000 ms");
        assert!(w.late());
    }

    #[test]
    fn watchman_detection_kills_a_stalled_child() {
        use std::os::unix::fs::PermissionsExt as _;
        const BUDGET: Duration = Duration::from_secs(1);
        let tmp = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let path = tmp.path().join(name);
            let body = format!("#!/bin/sh\n[ -n \"$PROBE\" ] && exit 0\n{body}\n");
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            // A thread forking meanwhile may hold the file open for writing
            // until its child execs: wait until it runs.
            let probe = || std::process::Command::new(&path).env("PROBE", "1").status();
            while probe().is_err() {
                std::thread::sleep(Duration::from_millis(5));
            }
            path
        };
        let start = Instant::now();
        let stalled = Watchman::detect(&script("stalls", "exec sleep 30"), BUDGET);
        assert!(
            start.elapsed() < BUDGET + SLACK,
            "took {:?}",
            start.elapsed()
        );
        assert_eq!(
            stalled,
            Err("watchman get-sockname did not answer within 1000 ms".into())
        );
        let args = tmp.path().join("args");
        let answers = script(
            "answers",
            &format!(
                r#"echo "$@" > {}; echo '{{"sockname": "/s"}}'"#,
                args.display()
            ),
        );
        assert_eq!(Watchman::detect(&answers, BUDGET * 5), Ok("/s".into()));
        let args = std::fs::read_to_string(&args).unwrap();
        assert_eq!(args, "--no-pretty get-sockname\n", "may start the daemon");
        // A daemon started by the client may keep its stdout open.
        let holds = script("holds", r#"echo '{"sockname": "/s"}'; sleep 5 &"#);
        let start = Instant::now();
        assert_eq!(Watchman::detect(&holds, BUDGET * 5), Ok("/s".into()));
        assert!(start.elapsed() < BUDGET, "took {:?}", start.elapsed());
        let failed = Watchman::detect(&script("fails", "exit 1"), BUDGET).unwrap_err();
        assert!(
            failed.starts_with("watchman get-sockname failed (exit status: 1"),
            "{failed}"
        );
        let missing = Watchman::detect(&tmp.path().join("none"), BUDGET).unwrap_err();
        assert!(
            missing.starts_with("cannot run watchman (") && missing.ends_with("; install watchman"),
            "{missing}"
        );
    }

    #[test]
    fn watchman_live_round_trip() {
        let Some(daemon) = live() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path());
        let token = at(&daemon).mark(&root).expect("a token of the tree");
        std::fs::write(root.join("new.c"), b"new").unwrap();
        let named = |token: &Token| at(&daemon).since(token, &root).unwrap();
        assert!(named(&token).iter().any(|p| p == "new.c"));
        for i in 0..10 {
            let token = at(&daemon).mark(&root).unwrap();
            let name = format!("r{i}.c");
            std::fs::write(root.join(&name), b"raced").unwrap();
            assert!(named(&token).contains(&name), "rep {i}");
        }
    }
}

use std::path::PathBuf;

use serde_json::json;

use super::*;

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

/// `<tmp>/root` and `<tmp>/idx`, canonical.
fn dirs(tmp: &Path) -> (PathBuf, PathBuf) {
    let (root, index) = (canonical(tmp).join("root"), canonical(tmp).join("idx"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    (root, index)
}

#[test]
fn journal_round_trips_and_malformed_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (root, index) = dirs(tmp.path());
    let root_str = root.to_str().unwrap();
    Journal::write_new(&index, root_str, Vec::new()).unwrap();
    assert_eq!(
        Journal::open(&index, Knob::Auto).unwrap(),
        None,
        "no tokens"
    );
    let tokens = vec![Token {
        kind: "elsewhere".into(),
        token: json!(1),
    }];
    Journal::write_new(&index, root_str, tokens.clone()).unwrap();
    let journal = Journal::open(&index, Knob::Auto).unwrap().expect("written");
    let want = Journal {
        v: 1,
        root: root_str.into(),
        tokens,
        named: Vec::new(),
    };
    assert_eq!(journal, want);
    let (marks, replay) = journal.ask(&root, 1);
    assert!(marks.is_empty() && replay.is_err(), "an unknown kind");
    assert!(journal.ask(&index, 1).1.is_err(), "another root");
    std::fs::write(index.join(DOCS_JOURNAL), br#"{"v":2}"#).unwrap();
    assert_eq!(
        Journal::open(&index, Knob::Auto).unwrap(),
        None,
        "another v"
    );
    for bad in [&b"{"[..], br#"{"v":1}"#, br#"{"v":1,"root":0}"#] {
        std::fs::write(index.join(DOCS_JOURNAL), bad).unwrap();
        let err = Journal::open(&index, Knob::Auto).unwrap_err();
        assert!(
            err.contains(DOCS_JOURNAL) && err.contains("tampered"),
            "got: {err}"
        );
    }
}

#[test]
fn loads_ask_from_50_000_docs() {
    assert_eq!(MIN_DOCS, 50_000);
    crate::tests::ASK_FROM.set(Some(10));
    assert!(!Policy::here().due(9) && Policy::here().due(10));
    crate::tests::ASK_FROM.set(None);
    assert!(!Policy::here().due(usize::MAX), "tests ask only when told");
}

#[cfg(unix)]
mod watchman_tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
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
                let cmd: Value = serde_json::from_str(&line).unwrap();
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

    /// A journal of `/w/sub` holding a token of the fake on `sock`.
    fn faked(sock: &Path) -> Journal {
        let token = json!({"sock": sock, "watch": "/w", "relative": "sub", "clock": "c:1:1"});
        Journal {
            v: VERSION,
            root: "/w/sub".into(),
            tokens: vec![Token {
                kind: "watchman".into(),
                token,
            }],
            named: Vec::new(),
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

    fn put(root: &Path, rel: &str) {
        std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
        std::fs::write(root.join(rel), rel).unwrap();
    }

    fn why(since: Since) -> String {
        match since {
            Since::Unknown(why) => why,
            Since::Paths(p) => panic!("{p:?}"),
        }
    }

    /// A token of the watch `w` for the tree `w/sub`, then the paths
    /// changed since it, relative to the tree; a restart is refused.
    #[test]
    fn watchman_client_speaks_the_protocol() {
        let Some(mut daemon) = live() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let (watch, root) = (canonical(tmp.path()), canonical(tmp.path()).join("sub"));
        std::fs::create_dir(&root).unwrap();
        daemon.watch(&watch);
        let marked = ChangeSource::mark(&at(&daemon), &root).expect("a clock");
        assert_eq!(
            (&marked["watch"], &marked["relative"]),
            (&json!(watch), &json!("sub"))
        );
        assert_eq!(marked["sock"], json!(daemon.sock));
        daemon.sent();
        put(&root, "a.c");
        put(&root, "d/x.c");
        put(&watch, "beside.c");
        let w = Watchman::of(&marked, Duration::from_secs(5)).expect("the token's daemon");
        let ask = |root: &Path| {
            ChangeSource::since(
                &w,
                &Ask {
                    root,
                    token: &marked,
                },
            )
        };
        // Watchman lists the paths in no set order.
        let Since::Paths(mut paths) = ask(&root) else {
            panic!("no paths");
        };
        paths.sort();
        assert_eq!(paths, ["a.c", "d", "d/x.c"]);
        assert_eq!(daemon.sent(), ["query"]);
        assert!(why(ask(&watch)).contains("another root"));
        daemon.restart();
        daemon.watch(&watch);
        let w = Watchman::of(&marked, Duration::from_secs(5)).unwrap();
        let ask = Ask {
            root: &root,
            token: &marked,
        };
        assert!(why(ChangeSource::since(&w, &ask)).contains("restarted"));
        daemon.stop();
        let gone = ChangeSource::mark(&at(&daemon), &root).unwrap_err();
        assert!(
            gone.starts_with("no watchman daemon answers on ")
                && gone.ends_with("; start watchman"),
            "{gone}"
        );
        assert!(Watchman::of(&json!({"watch": "/w"}), Duration::from_secs(5)).is_none());
    }

    /// An error text, and both ways watchman words a sync that timed out,
    /// which mean it did not answer in time.
    #[test]
    fn an_error_text_is_the_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let mode = Arc::new(AtomicU8::new(ERROR));
        let sock = fake(tmp.path(), Arc::clone(&mode));
        let journal = faked(&sock);
        let w = Watchman::of(&journal.tokens[0].token, Duration::from_secs(5)).unwrap();
        let ask = Ask {
            root: Path::new("/w/sub"),
            token: &journal.tokens[0].token,
        };
        for (m, want) in [
            (ERROR, "watchman: bad"),
            (SYNC_FAILED, "watchman did not answer within 5000 ms"),
            (NO_COOKIE, "watchman did not answer within 5000 ms"),
            (CRAWL, "watchman is still crawling /w/sub"),
        ] {
            mode.store(m, Ordering::Relaxed);
            assert_eq!(why(ChangeSource::since(&w, &ask)), want, "mode {m}");
        }
    }

    /// Where no watch covers the tree, watchman is asked to watch it; else
    /// the token belongs to the deepest watch above it, `w`, not the
    /// tree's parent nor `w/subway`; no token where ignore_dirs are set.
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
        let watched = |t: &Value| (t["watch"].clone(), t["relative"].clone());
        let marked = ChangeSource::mark(&at(&daemon), &root).expect("asked to watch it");
        assert_eq!(
            watched(&marked),
            (json!(root), json!("")),
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
        daemon.unwatch(&root);
        // Inner first: watch-project takes a watch above, when there is one.
        for dir in [&top.join("w/subway"), &top.join("w"), &top] {
            daemon.watch(dir);
        }
        assert_eq!(daemon.roots().len(), 3);
        let marked = ChangeSource::mark(&at(&daemon), &root).expect("a token of the watch above");
        assert_eq!(watched(&marked), (json!(top.join("w")), json!("sub")));
        let two_clocks = ["watch-list", "get-config", "clock", "clock"];
        assert_eq!(daemon.sent(), two_clocks, "no watch-project");
        let w = top.join("w");
        daemon.unwatch(&top);
        // Watchman reads ignore_dirs when it adds the watch.
        let configured = |config: &str| {
            daemon.unwatch(&w);
            std::fs::write(w.join(".watchmanconfig"), config).unwrap();
            daemon.watch(&w);
        };
        configured(r#"{"ignore_dirs": ["build", "Sub2/x"]}"#);
        let mark = || ChangeSource::mark(&at(&daemon), &root);
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

    /// A hung daemon (stopped in place) costs a load of any size at most
    /// [`ASK_WAIT`], and gives no token.
    #[test]
    fn a_load_never_waits_long_for_watchman() {
        let Some(daemon) = live() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path());
        let journal = Journal {
            v: VERSION,
            root: root.to_str().unwrap().into(),
            tokens: vec![Token {
                kind: "watchman".into(),
                token: ChangeSource::mark(&at(&daemon), &root).expect("a token"),
            }],
            named: Vec::new(),
        };
        daemon.hang();
        let start = Instant::now();
        let (marks, replay) = journal.ask(&root, 2_000_000);
        let took = start.elapsed();
        daemon.resume();
        assert!(took < ASK_WAIT + SLACK / 3, "took {took:?}");
        let why = replay.err().unwrap();
        assert_eq!(why, "watchman did not answer within 100 ms");
        assert!(marks.is_empty(), "no token");
    }

    /// Late or crawling, as watchman's error texts say: the new token is
    /// kept when the clock answered.
    #[test]
    fn a_load_says_why_watchman_was_late() {
        let tmp = tempfile::tempdir().unwrap();
        let mode = Arc::new(AtomicU8::new(SYNC_FAILED));
        let sock = fake(tmp.path(), Arc::clone(&mode));
        for (m, marked, want) in [
            (SYNC_FAILED, 1, "watchman did not answer within 100 ms"),
            (CRAWL, 1, "watchman is still crawling /w/sub"),
            (CRAWL_CLOCK, 0, "watchman is still crawling /w/sub"),
        ] {
            mode.store(m, Ordering::Relaxed);
            let (marks, replay) = faked(&sock).ask(Path::new("/w/sub"), 1_000_000);
            assert_eq!(replay.err().unwrap(), want, "mode {m}");
            assert_eq!(marks.len(), marked, "mode {m}");
        }
    }

    /// A crawl's tail answers syncs at once, all with the crawl's tick, which
    /// changes made then get too: no token, at index or at load.
    #[test]
    fn a_clock_from_a_crawls_tail_is_no_token() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = fake(tmp.path(), Arc::new(AtomicU8::new(STUCK)));
        let (_, index) = dirs(tmp.path());
        let root = Path::new("/w/sub");
        let w = Watchman::at(sock.clone(), MARK_WAIT);
        let why = token(&w, root).unwrap_err();
        assert_eq!(why, "watchman is still crawling /w/sub");
        faked(&sock).write(&index).unwrap();
        let before = std::fs::read(index.join(DOCS_JOURNAL)).unwrap();
        let journal = Journal::open(&index, Knob::Auto).unwrap().unwrap();
        let (marks, replay) = journal.ask(root, 1_000_000);
        assert!(marks.is_empty());
        let named = replay.map(|r| r.paths).unwrap_or_default();
        journal.checkpoint(&index, marks, named);
        let after = std::fs::read(index.join(DOCS_JOURNAL)).unwrap();
        assert_eq!(after, before, "a load checkpoints nothing");
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

    /// After a restart the old clock is from another instance: a load
    /// takes a new token, which is kept with what the stat pass found, and
    /// the next load asks watchman again.
    #[test]
    fn a_restarted_watchman_is_trusted_once_it_syncs() {
        let Some(mut daemon) = live() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let (root, index) = dirs(tmp.path());
        let journal = Journal {
            v: VERSION,
            root: root.to_str().unwrap().into(),
            tokens: vec![Token {
                kind: "watchman".into(),
                token: ChangeSource::mark(&at(&daemon), &root).expect("a token"),
            }],
            named: Vec::new(),
        };
        journal.write(&index).unwrap();
        let docs = 1_000_000;
        let stat_pass_found = || vec!["e1.c".to_owned()];
        daemon.restart();
        daemon.watch(&root);
        let journal = Journal::open(&index, Knob::Auto).unwrap().unwrap();
        let (marks, replay) = journal.ask(&root, docs);
        let why = replay.err().unwrap();
        assert!(
            why.contains("restarted"),
            "the old clock cannot vouch: {why}"
        );
        assert_eq!(marks.len(), 1);
        journal.checkpoint(&index, marks, stat_pass_found());
        put(&root, "a.c");
        put(&root, "d/x.c");
        let journal = Journal::open(&index, Knob::Auto).unwrap().unwrap();
        let (_, replay) = journal.ask(&root, docs);
        let paths = replay.unwrap_or_else(|why| panic!("{why}")).paths;
        assert_eq!(
            paths,
            ["a.c", "d", "d/x.c", "e1.c"],
            "and keeps what the stat pass found"
        );
    }

    #[test]
    fn watchman_gives_up_on_a_socket_that_never_accepts() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("sock");
        let _listener = UnixListener::bind(&sock).unwrap();
        let start = Instant::now();
        let w = Watchman::at(sock, Duration::from_secs(1));
        let why = ChangeSource::mark(&w, Path::new("/w/sub")).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(1) + SLACK);
        assert_eq!(why, "watchman did not answer within 1000 ms");
    }

    #[test]
    fn watchman_detection_kills_a_stalled_child() {
        use std::os::unix::fs::PermissionsExt as _;
        const BUDGET: Duration = Duration::from_secs(1);
        let tmp = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let path = tmp.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        let answers = script("answers", r#"echo '{"sockname": "/s"}'"#);
        assert_eq!(Watchman::detect(&answers, BUDGET * 5), Ok("/s".into()));
        let failed = Watchman::detect(&script("fails", "exit 1"), BUDGET).unwrap_err();
        assert!(failed.starts_with("watchman get-sockname found no watchman ("));
        let missing = Watchman::detect(&tmp.path().join("none"), BUDGET).unwrap_err();
        assert!(
            missing.starts_with("watchman get-sockname found no watchman ("),
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
        let token = ChangeSource::mark(&at(&daemon), &root).expect("a token of the tree");
        std::fs::write(root.join("new.c"), b"new").unwrap();
        let named = |token: &Value| {
            let w = Watchman::of(token, Duration::from_secs(5)).unwrap();
            match ChangeSource::since(&w, &Ask { root: &root, token }) {
                Since::Paths(p) => p,
                Since::Unknown(why) => panic!("cannot vouch: {why}"),
            }
        };
        assert!(named(&token).iter().any(|p| p == "new.c"));
        for i in 0..10 {
            let token = ChangeSource::mark(&at(&daemon), &root).unwrap();
            let name = format!("r{i}.c");
            std::fs::write(root.join(&name), b"raced").unwrap();
            assert!(named(&token).contains(&name), "rep {i}");
        }
    }

    #[test]
    fn watchman_alone_is_marked_unless_off() {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path());
        let Some(_daemon) = live() else {
            return;
        };
        let kinds =
            |knob| -> Vec<String> { marks(&root, knob).into_iter().map(|t| t.kind).collect() };
        assert_eq!(kinds(Knob::Auto), ["watchman"]);
        assert_eq!(kinds(Knob::On), ["watchman"]);
        assert!(kinds(Knob::Off).is_empty());
    }
}

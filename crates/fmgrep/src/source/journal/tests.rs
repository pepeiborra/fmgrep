use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::Stamp;

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
    let stamp = Stamp {
        len: 1,
        mtime_ns: 1,
    };
    let walk = WalkNotes {
        links: vec!["l.c".into()],
        dirs: vec![(String::new(), Some(stamp)), ("d".into(), None)],
    };
    let root_str = root.to_str().unwrap();
    Journal::write_new(&index, root_str, Vec::new(), &walk).unwrap();
    assert_eq!(Journal::open(&index, Knob::On).unwrap(), None, "no marks");
    let tokens = vec![Token {
        kind: "elsewhere".into(),
        token: json!(1),
    }];
    Journal::write_new(&index, root_str, tokens.clone(), &walk).unwrap();
    assert_eq!(Journal::open(&index, Knob::Off).unwrap(), None, "off");
    let journal = Journal::open(&index, Knob::On).unwrap().expect("written");
    let want = Journal {
        v: 1,
        root: root_str.into(),
        tokens,
        recheck: vec!["l.c".into()],
        relist: vec!["d".into()],
        named: Vec::new(),
        knob: Knob::On,
    };
    assert_eq!(journal, want);
    let (marks, replay) = journal.ask(&root, 1);
    assert!(marks.is_empty() && replay.is_none(), "an unknown kind");
    assert!(journal.ask(&index, 1).1.is_none(), "another root");
    std::fs::write(index.join(DOCS_JOURNAL), br#"{"v":2}"#).unwrap();
    assert_eq!(Journal::open(&index, Knob::On).unwrap(), None, "another v");
    for bad in [&b"{"[..], br#"{"v":1}"#, br#"{"v":1,"root":0}"#] {
        std::fs::write(index.join(DOCS_JOURNAL), bad).unwrap();
        let err = Journal::open(&index, Knob::On).unwrap_err();
        assert!(
            err.contains(DOCS_JOURNAL) && err.contains("tampered"),
            "got: {err}"
        );
    }
}

#[test]
fn the_knob_reads_off_and_on() {
    assert_eq!(Knob::of(None), Knob::Auto);
    assert_eq!(Knob::of(Some("off")), Knob::Off);
    assert_eq!(Knob::of(Some("on")), Knob::On);
    assert!(available(Knob::Off).is_empty());
}

#[test]
fn a_late_source_names_the_knob_that_skips_it() {
    assert_eq!(
        late("watchman"),
        "warning: watchman did not answer in time; set FMGREP_JOURNAL=off to skip it"
    );
}

#[cfg(unix)]
mod watchman_tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::source::journal::watchman::Watchman;

    const NORMAL: u8 = 0;
    const FRESH: u8 = 1;
    const ERROR: u8 = 2;
    const IGNORES: u8 = 3;

    /// A daemon on a socket in `dir` answering as `mode` says.
    fn fake(dir: &Path, mode: Arc<AtomicU8>) -> PathBuf {
        let sock = dir.join("sock");
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let mut conn = conn.unwrap();
                let mut line = String::new();
                BufReader::new(&conn).read_line(&mut line).unwrap();
                let cmd: Value = serde_json::from_str(&line).unwrap();
                let mode = mode.load(Ordering::Relaxed);
                let reply = match (cmd[0].as_str().unwrap(), mode) {
                    ("watch-project", _) => json!({"watch": "/w", "relative_path": "sub"}),
                    ("get-config", IGNORES) => json!({"config": {"ignore_dirs": ["build"]}}),
                    ("get-config", _) => json!({"config": {}}),
                    ("clock", _) => json!({"clock": "c:1:2"}),
                    ("query", ERROR) => json!({"error": "bad"}),
                    ("query", _) => {
                        let asked = (&cmd[1], &cmd[2]["since"], &cmd[2]["relative_root"]);
                        assert_eq!(asked, (&json!("/w"), &json!("c:1:2"), &json!("sub")));
                        json!({"files": ["a.c", "d/x.c"], "is_fresh_instance": mode == FRESH})
                    }
                    (other, _) => panic!("unexpected {other}"),
                };
                writeln!(conn, "{}", json!({"unilateral": true, "log": "noise"})).unwrap();
                writeln!(conn, "{reply}").unwrap();
            }
        });
        sock
    }

    fn token(sock: &Path) -> Value {
        json!({"sock": sock, "watch": "/w", "relative": "sub", "clock": "c:1:2"})
    }

    #[test]
    fn watchman_client_speaks_the_protocol() {
        let tmp = tempfile::tempdir().unwrap();
        let mode = Arc::new(AtomicU8::new(NORMAL));
        let sock = fake(tmp.path(), Arc::clone(&mode));
        let root = Path::new("/w/sub");
        let marked = Watchman::at(sock.clone(), Duration::from_secs(5))
            .mark(root)
            .expect("a clock");
        assert_eq!(marked, token(&sock));
        let w = Watchman::of(&marked, Duration::from_secs(5)).expect("the token's daemon");
        let ask = |root: &Path| {
            w.since(&Ask {
                root,
                token: &marked,
            })
        };
        assert!(matches!(ask(root), Since::Paths(p) if p == ["a.c", "d/x.c"]));
        assert!(
            matches!(ask(Path::new("/w")), Since::Unknown(_)),
            "another root"
        );
        for m in [FRESH, ERROR] {
            mode.store(m, Ordering::Relaxed);
            assert!(matches!(ask(root), Since::Unknown(_)), "mode {m}");
        }
        mode.store(IGNORES, Ordering::Relaxed);
        assert!(w.mark(root).is_none(), "ignore_dirs hides indexed files");
        let none = Watchman::at(tmp.path().join("none"), Duration::from_secs(5));
        assert!(none.mark(root).is_none(), "no daemon");
        assert!(Watchman::of(&json!({"watch": "/w"}), Duration::from_secs(5)).is_none());
    }

    const BUDGET: Duration = Duration::from_secs(1);
    /// Under load.
    const SLACK: Duration = Duration::from_millis(1500);

    /// The late warnings on this thread since the last call.
    fn warned() -> Vec<String> {
        WARNED.take()
    }

    /// Mark then ask on one budget: no mark, no vouching, and within it,
    /// with one warning.
    fn gives_up_in_time(sock: PathBuf) {
        warned();
        let start = Instant::now();
        let token = token(&sock);
        let w = Watchman::at(sock, BUDGET);
        let root = Path::new("/w/sub");
        assert!(w.mark(root).is_none());
        let ask = Ask {
            root,
            token: &token,
        };
        assert!(matches!(w.since(&ask), Since::Unknown(_)));
        assert!(
            start.elapsed() < BUDGET + SLACK,
            "took {:?}",
            start.elapsed()
        );
        assert_eq!(warned(), [late("watchman")]);
    }

    #[test]
    fn watchman_gives_up_on_a_daemon_that_never_answers() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("sock");
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            let held: Vec<_> = listener.incoming().map_while(Result::ok).collect();
            drop(held);
        });
        gives_up_in_time(sock);
    }

    #[test]
    fn watchman_gives_up_on_a_socket_that_never_accepts() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("sock");
        let _listener = UnixListener::bind(&sock).unwrap();
        gives_up_in_time(sock);
    }

    #[test]
    fn watchman_detection_kills_a_stalled_child() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let path = tmp.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        warned();
        let start = Instant::now();
        assert!(Watchman::detect(&script("stalls", "exec sleep 30"), BUDGET).is_none());
        assert!(
            start.elapsed() < BUDGET + SLACK,
            "took {:?}",
            start.elapsed()
        );
        assert_eq!(warned(), [late("watchman")]);
        let answers = script("answers", r#"echo '{"sockname": "/s"}'"#);
        assert!(Watchman::detect(&answers, BUDGET * 5).is_some());
        assert!(Watchman::detect(&script("fails", "exit 1"), BUDGET).is_none());
        assert!(
            warned().is_empty(),
            "a quick answer or failure warns nothing"
        );
    }

    /// Live tests add watches, so they run only against an explicit `WATCHMAN_SOCK`.
    fn live() -> Option<Watchman> {
        let w =
            std::env::var_os("WATCHMAN_SOCK").and_then(|_| Watchman::find(Duration::from_secs(5)));
        if w.is_none() {
            eprintln!("skipped: WATCHMAN_SOCK is unset");
        }
        w
    }

    #[test]
    fn watchman_live_round_trip() {
        let Some(w) = live() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path());
        let Some(token) = w.mark(&root) else {
            eprintln!("skipped: watchman does not cover {}", root.display());
            return;
        };
        std::fs::write(root.join("new.c"), b"new").unwrap();
        let w = Watchman::of(&token, Duration::from_secs(5)).unwrap();
        let ask = Ask {
            root: &root,
            token: &token,
        };
        let named = w.since(&ask);
        assert!(matches!(named, Since::Paths(p) if p.iter().any(|p| p == "new.c")));
    }

    #[test]
    fn watchman_alone_is_marked_unless_off() {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path());
        let Some(w) = live() else {
            return;
        };
        if w.mark(&root).is_none() {
            eprintln!("skipped: watchman does not cover {}", root.display());
            return;
        }
        let kinds =
            |knob| -> Vec<String> { marks(&root, knob).into_iter().map(|t| t.kind).collect() };
        assert_eq!(kinds(Knob::Auto), ["watchman"]);
        assert_eq!(kinds(Knob::On), ["watchman"]);
        assert!(kinds(Knob::Off).is_empty());
    }
}

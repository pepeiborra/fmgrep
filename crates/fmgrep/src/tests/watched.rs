//! Searches that ask watchman: each test starts a private daemon of its
//! own, and is skipped without watchman. Records go to a temporary cache
//! directory.

use super::drive::answers_like;
use super::watchman::Daemon;
use super::*;

/// Whether a load of `index` re-checks only what watchman and the
/// baseline name.
fn replays(index: &Path) -> bool {
    let manifest = read_manifest(index).unwrap();
    let docs =
        crate::fetch::DocStore::open(index, &manifest, None, crate::fetch::Ask::Load).unwrap();
    docs.replay(&manifest.files)
}

/// The warnings given on this thread since the last call.
pub(super) fn notes() -> Vec<String> {
    NOTES.with_borrow(|n| std::mem::take(&mut *n.lock().unwrap()))
}

pub(super) fn put(root: &Path, rel: &str, bytes: &[u8]) {
    std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
    std::fs::write(root.join(rel), bytes).unwrap();
}

/// The records in `<tmp>/cache`.
pub(super) fn records(tmp: &Path) -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(tmp.join("cache")) else {
        return Vec::new();
    };
    let mut all: Vec<PathBuf> = dir.map(|e| e.unwrap().path()).collect();
    all.sort();
    all
}

/// The one record in `<tmp>/cache`.
pub(super) fn record(tmp: &Path) -> serde_json::Value {
    let all = records(tmp);
    assert_eq!(all.len(), 1, "{all:?}");
    serde_json::from_slice(&std::fs::read(&all[0]).unwrap()).unwrap()
}

pub(super) fn clock(tmp: &Path) -> serde_json::Value {
    record(tmp)["token"]["clock"].clone()
}

fn baseline(tmp: &Path) -> serde_json::Value {
    record(tmp)["baseline"].clone()
}

/// A private daemon that watches `folder`, which this thread's indexes and
/// searches ask, at any size, keeping their records in `<tmp>/cache`;
/// `None` without watchman.
fn live(tmp: &Path, folder: &Path) -> Option<Daemon> {
    let daemon = Daemon::start()?;
    daemon.watch(folder);
    WATCHMAN.set(Some(daemon.sock.clone()));
    ASK_FROM.set(Some(0));
    CACHE.set(Some(tmp.join("cache")));
    Some(daemon)
}

/// `files` under `<tmp>/corpus`, backdated, indexed at `<tmp>/idx` while
/// [`live`] watches the tree, which the index only checks for.
pub(super) fn indexed(tmp: &Path, files: &[(&str, &[u8])]) -> Option<(PathBuf, PathBuf, Daemon)> {
    let folder = tmp.join("corpus");
    for (rel, bytes) in files {
        put(&folder, rel, bytes);
    }
    backdate(&folder);
    let daemon = live(tmp, &folder)?;
    let output = tmp.join("idx");
    index_it(&folder, &output, false);
    assert_eq!(daemon.sent(), ["watch-list"], "index takes no token");
    Some((folder, output, daemon))
}

/// [`indexed`], then the first search, which took a token.
fn watched(tmp: &Path, files: &[(&str, &[u8])]) -> Option<(PathBuf, PathBuf, Daemon)> {
    let (folder, output, daemon) = indexed(tmp, files)?;
    query_it(&output, false, &["printk"]);
    assert_eq!(notes(), [ONCE]);
    assert_eq!(records(tmp).len(), 1, "the first token");
    daemon.sent();
    Some((folder, output, daemon))
}

/// What the first search after an index says.
pub(super) const ONCE: &str = "checked every file once (no record of an earlier search); later searches \
                    ask watchman";

pub(super) const AB: &[(&str, &[u8])] = &[("a.c", b"printk\n"), ("b.c", b"none\n")];

/// The requests of a search that takes a token and has none to ask with.
pub(super) const TAKES: [&str; 4] = ["watch-list", "get-config", "clock", "clock"];

pub(super) fn cannot(why: &str) -> String {
    format!("watchman cannot say what changed ({why}); checked every file")
}

#[test]
fn a_first_search_takes_a_token_then_checks_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    assert!(records(tmp.path()).is_empty());
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert_eq!(daemon.sent(), TAKES);
    assert_eq!(notes(), [ONCE]);
    let record = record(tmp.path());
    let root = std::fs::canonicalize(&folder).unwrap();
    let index = std::fs::canonicalize(&output).unwrap();
    assert_eq!(record["root"], root.to_str().unwrap());
    let started = read_manifest(&output).unwrap().index_started_ns;
    let of = format!("index {}@{started}", index.to_str().unwrap());
    assert_eq!(record["of"], of);
    let token = &record["token"];
    assert_eq!(
        token["sock"],
        daemon.sock.to_str().unwrap(),
        "the socket, kept"
    );
    assert_eq!(token["watch"], record["root"]);
    assert_eq!(record["baseline"], serde_json::json!(["b.c"]));
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    let asked = ["watch-list", "get-config", "clock", "clock", "query"];
    assert_eq!(daemon.sent(), asked, "the token question");
    assert!(notes().is_empty(), "the second search asks watchman");
}

#[test]
fn a_later_search_uses_the_socket_in_its_record() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, _daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    let before = clock(tmp.path());
    WATCHMAN.set(None);
    std::fs::write(folder.join("c.c"), b"printk\n").unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nc.c\n");
    assert!(notes().is_empty(), "found through the record alone");
    assert_ne!(clock(tmp.path()), before, "a newer token");
}

#[test]
fn watchman_sock_set_to_another_socket_wins_over_the_record() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    let kept = record(tmp.path());
    let dead = tmp.path().join("no-daemon.sock");
    WATCHMAN.set(Some(dead.clone()));
    std::fs::write(folder.join("c.c"), b"printk\n").unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nc.c\n");
    assert!(daemon.sent().is_empty(), "the record's daemon is not asked");
    let why = notes();
    let line = format!(
        "watchman cannot say what changed (no watchman daemon answers on {} (",
        dead.display()
    );
    assert!(why.len() == 1 && why[0].starts_with(&line), "{why:?}");
    assert_eq!(record(tmp.path()), kept, "the record is kept as it was");
    WATCHMAN.set(Some(daemon.sock.clone()));
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nc.c\n");
    assert!(notes().is_empty(), "the record serves again");
}

/// A daemon that hangs (stopped in place) gives no token: the first search
/// checks every file and saves nothing.
#[test]
fn a_first_search_that_gets_no_token_saves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    daemon.hang();
    let answer = printed(&output, &["-l"], &["printk"]);
    daemon.resume();
    assert_eq!(answer, "a.c\nb.c\n");
    assert_eq!(notes(), [cannot("watchman did not answer within 100 ms")]);
    assert!(records(tmp.path()).is_empty());
}

/// A stand-in watchman client at `<tmp>/bin/watchman`: it logs each run's
/// arguments to `<tmp>/runs` and gives `sock`, as the real client does
/// once the daemon it started listens.
fn stand_in(tmp: &Path, sock: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = tmp.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let program = bin.join("watchman");
    let reply = serde_json::json!({"version": "stand-in", "sockname": sock});
    let runs = tmp.join("runs");
    let body = format!(
        "#!/bin/sh\n[ -n \"$PROBE\" ] && exit 0\necho \"$@\" >> '{}'\necho '{reply}'\n",
        runs.display()
    );
    std::fs::write(&program, body).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A test thread forking meanwhile may hold the file open for writing
    // until its child execs: wait until it runs.
    while std::process::Command::new(&program)
        .env("PROBE", "1")
        .status()
        .is_err()
    {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    program
}

fn runs(tmp: &Path) -> String {
    std::fs::read_to_string(tmp.join("runs")).unwrap_or_default()
}

#[test]
fn without_watchman_installed_a_search_says_so_and_checks_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    WATCHMAN.set(None);
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    let got = notes();
    let missing = "watchman cannot say what changed (cannot run watchman (";
    let fix = "; install watchman); checked every file";
    assert!(
        got.len() == 1 && got[0].starts_with(missing) && got[0].ends_with(fix),
        "{got:?}"
    );
    assert!(daemon.sent().is_empty() && records(tmp.path()).is_empty());
}

/// No socket is given: `index` runs the client for one, which may start
/// the daemon (here a stand-in gives the test's daemon), and asks the
/// daemon to watch the tree without waiting for its read. A search runs
/// the client too and takes the token; the next one finds the daemon
/// through its record.
#[test]
fn index_and_a_search_start_watchman_and_ask_it_to_watch_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    WATCHMAN.set(None);
    WATCHMAN_PROGRAM.set(stand_in(tmp.path(), &daemon.sock));
    daemon.unwatch(&folder);
    let start = std::time::Instant::now();
    index_it(&folder, &tmp.path().join("idx2"), false);
    let took = start.elapsed();
    assert!(took < std::time::Duration::from_millis(1600), "{took:?}");
    assert_eq!(runs(tmp.path()), "--no-pretty get-sockname\n", "may start");
    assert_eq!(daemon.sent(), ["watch-list", "watch-project"]);
    let root = std::fs::canonicalize(&folder).unwrap();
    assert_eq!(daemon.roots(), [root.to_str().unwrap()], "the watch");
    assert!(notes().is_empty() && records(tmp.path()).is_empty());
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert_eq!(
        runs(tmp.path()).lines().count(),
        2,
        "the search runs it too"
    );
    assert_eq!(notes(), [ONCE]);
    assert_eq!(
        record(tmp.path())["token"]["sock"],
        daemon.sock.to_str().unwrap()
    );
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert!(notes().is_empty(), "the token question");
    assert_eq!(
        runs(tmp.path()).lines().count(),
        2,
        "the record has the socket"
    );
}

/// `index` asks watchman to watch the tree, does not wait for the reply
/// beyond its 100 ms, takes no token and writes no record; the first
/// search takes one.
#[test]
fn index_asks_for_a_watch_without_waiting_and_a_search_takes_the_token() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), AB);
    let Some(daemon) = Daemon::start() else {
        return;
    };
    WATCHMAN.set(Some(daemon.sock.clone()));
    ASK_FROM.set(Some(0));
    CACHE.set(Some(tmp.path().join("cache")));
    let output = tmp.path().join("idx");
    let start = Instant::now();
    index_it(&folder, &output, false);
    let took = start.elapsed();
    assert!(took < Duration::from_millis(1600), "took {took:?}");
    assert_eq!(daemon.sent(), ["watch-list", "watch-project"]);
    let root = std::fs::canonicalize(&folder).unwrap();
    assert_eq!(daemon.roots(), [root.to_str().unwrap()], "the watch");
    assert!(records(tmp.path()).is_empty(), "no token");
    assert!(notes().is_empty(), "index prints nothing");
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\n");
    assert_eq!(notes(), [ONCE]);
    assert_eq!(records(tmp.path()).len(), 1, "the first token");
    put(&folder, "b.c", b"printk now\n");
    daemon.sent();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert_eq!(daemon.sent().last().unwrap(), "query");
    assert!(notes().is_empty());
}

/// A hung daemon (stopped in place), then none: each search says why,
/// checks every file and keeps the record.
#[test]
fn a_search_watchman_cannot_answer_says_why_and_checks_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, mut daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    let kept = record(tmp.path());
    daemon.hang();
    let start = std::time::Instant::now();
    assert!(!replays(&output));
    let took = start.elapsed();
    daemon.resume();
    assert!(
        took < std::time::Duration::from_millis(1600),
        "took {took:?}"
    );
    assert_eq!(notes(), [cannot("watchman did not answer within 100 ms")]);
    assert_eq!(record(tmp.path()), kept);
    daemon.stop();
    assert!(!replays(&output));
    let why = notes();
    let gone = "watchman cannot say what changed (no watchman daemon answers on ";
    assert!(why.len() == 1 && why[0].starts_with(gone), "{why:?}");
    assert_eq!(record(tmp.path()), kept);
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
}

/// After a restart, and after the watch is removed (the search adds it
/// again), the old token cannot say what changed: the search takes a new
/// one, checks every file once and says so; the next search asks.
#[test]
fn a_restarted_watchman_or_a_lost_watch_makes_a_new_baseline() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, mut daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    let once = "checked every file once (watchman restarted, added the watch again or pruned \
                deleted files since the last search); later searches ask watchman";
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    let before = clock(tmp.path());
    daemon.restart();
    daemon.watch(&folder);
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert_eq!(notes(), [once], "restarted");
    let asked = ["watch-list", "get-config", "clock", "clock", "query"];
    assert_eq!(daemon.sent(), asked, "the token just taken is kept");
    assert_ne!(clock(tmp.path()), before);
    assert_eq!(baseline(tmp.path()), serde_json::json!(["b.c"]));
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert!(notes().is_empty(), "watched again: the record is used");
    daemon.unwatch(&folder);
    let before = clock(tmp.path());
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert_eq!(notes(), [once], "the watch is gone: added again");
    let asked = [
        "watch-list",
        "watch-project",
        "get-config",
        "clock",
        "clock",
        "query",
    ];
    assert_eq!(daemon.sent(), asked);
    assert_ne!(clock(tmp.path()), before);
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert!(notes().is_empty(), "watched again: the record is used");
}

#[test]
fn a_rebuilt_index_does_not_use_the_old_record() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, _daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    let first = records(tmp.path());
    std::fs::remove_dir_all(&output).unwrap();
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    index_it(&folder, &output, false);
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    let once = format!(
        "checked every file once ({} was written for another index build); later searches ask \
         watchman",
        first[0].display()
    );
    assert_eq!(notes(), [once]);
    assert_eq!(records(tmp.path()), first, "the same file, rewritten");
    assert_eq!(baseline(tmp.path()), serde_json::json!([]));
    let started = read_manifest(&output).unwrap().index_started_ns;
    let of = record(tmp.path())["of"].as_str().unwrap().to_owned();
    assert!(of.ends_with(&format!("@{started}")), "{of}");
}

#[test]
fn two_trees_or_two_indexes_keep_two_records() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, _daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    let other = tmp.path().join("idx2");
    index_it(&folder, &other, false);
    query_it(&other, false, &["printk"]);
    query_it(&output, false, &["printk"]);
    assert_eq!(records(tmp.path()).len(), 2);
}

#[test]
fn past_the_limit_the_record_holds_no_baseline() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    let before = clock(tmp.path());
    for i in 0..65 {
        put(&folder, &format!("n{i}.c"), b"printk\n");
    }
    let found = printed(&output, &["-l"], &["printk"]);
    assert_eq!(found.lines().count(), 66);
    let over = "more than 64 paths may differ from the index; checked every file (re-run index)";
    assert_eq!(notes(), [over]);
    assert_eq!(baseline(tmp.path()), serde_json::Value::Null);
    let after = clock(tmp.path());
    assert_ne!(after, before);
    daemon.sent();
    assert_eq!(printed(&output, &["-l"], &["printk"]), found);
    assert_eq!(notes(), [over], "every search, until re-index");
    assert_eq!(daemon.sent(), TAKES, "no token question");
    assert_ne!(clock(tmp.path()), after);
}

#[test]
fn an_unwritable_cache_says_so_and_still_answers() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, _daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    let cache = tmp.path().join("cache");
    std::fs::create_dir(&cache).unwrap();
    let mode = |m: u32| std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(m));
    mode(0o500).unwrap();
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    let answer = printed(&output, &["-l"], &["printk"]);
    mode(0o755).unwrap();
    assert_eq!(answer, "a.c\nb.c\n");
    let got = notes();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(
        got[1].starts_with(&format!("cannot save {}/corpus-", cache.display()))
            && got[1].ends_with("; the next search checks again"),
        "{got:?}"
    );
    assert!(records(tmp.path()).is_empty());
}

#[test]
fn a_search_writes_nothing_into_the_index() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, _daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    let listing = |dir: &Path| {
        let mut all: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let meta = e.metadata().unwrap();
                (e.file_name(), meta.len(), meta.modified().unwrap())
            })
            .collect();
        all.sort();
        all
    };
    let before = listing(&output);
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert_eq!(listing(&output), before, "the index directory is as built");
    assert_eq!(records(tmp.path()).len(), 1);
}

#[test]
fn searches_at_once_leave_one_whole_record() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, _daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let hooks = Hooks::here();
            let output = &output;
            scope.spawn(move || {
                hooks.apply();
                assert_eq!(printed(output, &["-l"], &["printk"]), "a.c\nb.c\n");
            });
        }
    });
    assert_eq!(records(tmp.path()).len(), 1, "no temporary file is left");
    // Eight searches at once can each wait past their 100 ms; a search
    // that took no token keeps the record as it was.
    let late = cannot("watchman did not answer within 100 ms");
    let got = notes();
    assert!(got.iter().all(|n| *n == late), "{got:?}");
    let all_late = got.len() == 8 && baseline(tmp.path()) == serde_json::json!([]);
    assert!(
        all_late || baseline(tmp.path()) == serde_json::json!(["b.c"]),
        "{got:?} {}",
        record(tmp.path())
    );
}

/// A mountinfo file in `tmp` that puts canonical `root` on a filesystem of
/// type `fs`.
fn mounted(tmp: &Path, root: &Path, fs: &str) -> PathBuf {
    let file = tmp.join(format!("mountinfo-{fs}"));
    let root = root.display();
    let text =
        format!("22 1 8:1 / / rw - ext4 /dev/sda1 rw\n40 22 0:35 / {root} rw - {fs} srv:/x rw\n");
    std::fs::write(&file, text).unwrap();
    file
}

#[test]
fn a_tree_on_a_network_filesystem_takes_no_token_and_checks_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    let root = std::fs::canonicalize(&folder).unwrap();
    MOUNTS.set(Some(mounted(tmp.path(), &root, "nfs4")));
    index_it(&folder, &tmp.path().join("idx2"), false);
    assert!(daemon.sent().is_empty(), "index asks for no watch");
    let why = format!(
        "{} is on nfs4, a network or shared filesystem: watchman does not see changes made \
         there from other machines",
        root.display()
    );
    for (i, text) in ["printk now\n", "printk again\n"].iter().enumerate() {
        std::fs::write(folder.join("b.c"), text).unwrap();
        assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n", "{i}");
        assert_eq!(notes(), [cannot(&why)], "{i}");
        assert!(daemon.sent().is_empty(), "{i}: no token");
        assert!(records(tmp.path()).is_empty(), "{i}: no record");
    }
    // FUSE (bindfs, EdenFS) passes events on: a token.
    MOUNTS.set(Some(mounted(tmp.path(), &root, "fuse.bindfs")));
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert_eq!(notes(), [ONCE]);
    assert_eq!(daemon.sent(), TAKES);
    assert_eq!(baseline(tmp.path()), serde_json::json!(["b.c"]));
}

#[test]
fn unreadable_mountinfo_takes_no_token_and_checks_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    let file = tmp.path().join("no-mountinfo");
    MOUNTS.set(Some(file.clone()));
    index_it(&folder, &tmp.path().join("idx2"), false);
    assert!(daemon.sent().is_empty(), "index asks for no watch");
    let err = std::fs::read_to_string(&file).unwrap_err();
    let why = format!(
        "cannot read {} to tell the tree's filesystem ({err})",
        file.display()
    );
    for (i, text) in ["printk now\n", "printk again\n"].iter().enumerate() {
        std::fs::write(folder.join("b.c"), text).unwrap();
        assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n", "{i}");
        assert_eq!(notes(), [cannot(&why)], "{i}");
        assert!(daemon.sent().is_empty(), "{i}: no token");
        assert!(records(tmp.path()).is_empty(), "{i}: no record");
    }
}

#[test]
fn a_small_tree_asks_nothing_and_says_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    ASK_FROM.set(Some(3));
    WATCHMAN.set(None);
    WATCHMAN_PROGRAM.set(stand_in(tmp.path(), &daemon.sock));
    index_it(&folder, &tmp.path().join("idx2"), false);
    assert!(daemon.sent().is_empty(), "nor does index");
    std::fs::write(folder.join("c.c"), b"printk\n").unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nc.c\n");
    assert!(!replays(&output));
    assert!(daemon.sent().is_empty());
    assert_eq!(runs(tmp.path()), "", "no watchman client either");
    assert!(notes().is_empty(), "no line");
    assert!(records(tmp.path()).is_empty(), "no record");
}

#[test]
fn the_baseline_carries_what_differs_between_searches() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    std::fs::write(folder.join("a.c"), b"printk edited\n").unwrap();
    std::fs::write(folder.join("n.c"), b"printk new\n").unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nn.c\n");
    let asked = ["watch-list", "get-config", "clock", "clock", "query"];
    assert_eq!(daemon.sent(), asked);
    assert_eq!(baseline(tmp.path()), serde_json::json!(["a.c", "n.c"]));
    std::fs::remove_file(folder.join("n.c")).unwrap();
    assert_eq!(printed(&output, &["-c"], &["printk"]), "a.c:1\n");
    assert_eq!(baseline(tmp.path()), serde_json::json!(["a.c"]));
    assert_eq!(printed(&output, &["-c"], &["printk"]), "a.c:1\n", "carried");
    assert_eq!(baseline(tmp.path()), serde_json::json!(["a.c"]));
    assert!(notes().is_empty());
}

#[test]
fn a_new_ignore_file_deep_in_the_tree_hides_its_files_from_every_later_search() {
    let tmp = tempfile::tempdir().unwrap();
    let docs: &[(&str, &[u8])] = &[("a.c", b"printk\n"), ("x/y/z/k.c", b"printk\n")];
    let Some((folder, output, _daemon)) = watched(tmp.path(), docs) else {
        return;
    };
    let hidden = serde_json::json!(["x/y/z/.ignore", "x/y/z/k.c"]);
    put(&folder, "x/y/z/.ignore", b"k.c\n");
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\n");
    assert_eq!(baseline(tmp.path()), hidden, "the ignore file is kept");
    for _ in 0..2 {
        assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\n");
    }
    // Before the first search: the stat pass finds it in its listed dir.
    for record in records(tmp.path()) {
        std::fs::remove_file(record).unwrap();
    }
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\n");
    assert_eq!(baseline(tmp.path()), hidden);
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\n");
    std::fs::remove_file(folder.join("x/y/z/.ignore")).unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nx/y/z/k.c\n");
    assert_eq!(baseline(tmp.path()), serde_json::json!([]));
    notes();
}

/// What a load says while `n` ignore files differ from the index.
fn ignores_changed(n: usize) -> String {
    let (files, dirs) = match n {
        1 => ("file", "its directory"),
        _ => ("files", "their directories"),
    };
    format!(
        "{n} ignore {files} changed since the index; each search reads {dirs} again (re-run \
         index to make searches fast again)"
    )
}

/// An edited ignore file and a new one: each load that asks watchman says
/// so, until neither differs from the index.
#[test]
fn a_changed_ignore_file_warns_at_every_load() {
    let tmp = tempfile::tempdir().unwrap();
    let docs: &[(&str, &[u8])] = &[
        (".ignore", b"*.o\n"),
        ("a.c", b"printk\n"),
        ("b.c", b"printk\n"),
        ("x/y/z/k.c", b"printk\n"),
    ];
    let Some((folder, output, _daemon)) = watched(tmp.path(), docs) else {
        return;
    };
    assert!(replays(&output));
    assert!(notes().is_empty(), "no ignore file changed");
    put(&folder, ".ignore", b"*.o\nb.c\n");
    put(&folder, "x/y/z/.ignore", b"k.c\n");
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\n");
    notes();
    for _ in 0..2 {
        assert!(replays(&output));
        assert_eq!(notes(), [ignores_changed(2)]);
    }
    std::fs::remove_file(folder.join("x/y/z/.ignore")).unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nx/y/z/k.c\n");
    notes();
    assert!(replays(&output));
    assert_eq!(notes(), [ignores_changed(1)], "the edited one");
    put(&folder, ".ignore", b"*.o\n");
    backdate(&folder);
    assert_eq!(
        printed(&output, &["-l"], &["printk"]),
        "a.c\nb.c\nx/y/z/k.c\n"
    );
    notes();
    assert!(replays(&output));
    assert!(notes().is_empty(), "both as indexed");
}

/// a/sub keeps the stamp b/sub had; watchman reports the files under both,
/// as for any directory moved into the tree.
#[test]
fn a_dir_swapped_in_is_listed_down_to_its_files() {
    let tmp = tempfile::tempdir().unwrap();
    let docs: &[(&str, &[u8])] = &[
        ("a/sub/f.c", b"printk old\n"),
        ("b/sub/f.c", b"printk newer\n"),
    ];
    let Some((folder, output, _daemon)) = watched(tmp.path(), docs) else {
        return;
    };
    std::fs::remove_dir_all(folder.join("a")).unwrap();
    std::fs::rename(folder.join("b"), folder.join("a")).unwrap();
    assert_eq!(printed(&output, &["-l"], &["newer"]), "a/sub/f.c\n");
    assert_eq!(
        baseline(tmp.path()),
        serde_json::json!(["a/sub/f.c", "b/sub/f.c"])
    );
    assert!(notes().is_empty());
}

#[test]
fn named_changes_answer_like_a_fresh_index() {
    let tmp = tempfile::tempdir().unwrap();
    let docs: &[(&str, &[u8])] = &[
        (".ignore", b"ig/\n"),
        ("a.c", b"printk one\n"),
        ("ig/x.c", b"printk ignored\n"),
        ("kb/k.dat", b"printk\0binary"),
        ("r.c", b"printk renamed\n"),
        ("sub/keep.c", b"printk kept\n"),
        ("sub/mw/m.c", b"printk moved\n"),
        ("sub/s.c", b"printk deleted\n"),
    ];
    let Some((folder, stale, _daemon)) = watched(tmp.path(), docs) else {
        return;
    };
    let at = |rel: &str| folder.join(rel);
    put(&folder, "a.c", b"printk one\nprintk printk\n");
    put(&folder, "b.c", b"printk added\naa\n");
    std::fs::remove_file(at("sub/s.c")).unwrap();
    std::fs::rename(at("sub/mw"), at("sub/mv2")).unwrap();
    put(&folder, "new/deep/n.c", b"deep printk\n");
    put(&folder, "kb/k.dat", b"printk text now\n");
    put(&folder, "ig/y.c", b"printk still ignored\n");
    std::fs::rename(at("r.c"), at("r2.c")).unwrap();
    assert!(replays(&stale), "{:?}", notes());
    let want = "a.c\nb.c\nkb/k.dat\nnew/deep/n.c\nr2.c\nsub/keep.c\nsub/mv2/m.c\n";
    assert_eq!(printed(&stale, &["-l"], &["printk"]), want);
    put(&folder, "kb/k.dat", b"printk\0binary");
    let fresh = tmp.path().join("fresh");
    index_it(&folder, &fresh, false);
    let terms = ["printk", "/pr[i1]ntk/", "aa", "deep", "now"];
    answers_like(&stale, &fresh, &terms, &["printk", "aa", "deep"]);
}

#[test]
fn edits_before_the_first_search_and_between_searches_answer_like_a_fresh_index() {
    let tmp = tempfile::tempdir().unwrap();
    let docs: &[(&str, &[u8])] = &[
        (".ignore", b"*.gen\n"),
        ("a.c", b"printk one\n"),
        ("d.c", b"printk gone\n"),
        ("g.gen", b"printk generated\n"),
        ("mw/m.c", b"printk moved\n"),
        ("p/cp/r.c", b"printk newer\n"),
        ("p/q/r.c", b"printk old\n"),
        ("sub/s.c", b"printk sub\n"),
        ("sub/t.c", b"printk sub too\n"),
        ("x/y/z/k.c", b"printk hidden\n"),
    ];
    let Some((folder, stale, daemon)) = indexed(tmp.path(), docs) else {
        return;
    };
    let at = |rel: &str| folder.join(rel);
    let fresh = |name: &str| {
        let fresh = tmp.path().join(name);
        index_it(&folder, &fresh, false);
        fresh
    };
    let terms = ["printk", "/pr[i1]ntk/", "now", "deep"];
    // Before the first search, which takes a token, then checks every file.
    put(&folder, "a.c", b"printk one\nprintk now\n");
    put(&folder, "b.c", b"printk added\n");
    put(&folder, "new/deep/n.c", b"deep printk\n");
    std::fs::remove_file(at("d.c")).unwrap();
    std::fs::rename(at("mw"), at("mv2")).unwrap();
    put(&folder, ".ignore", b"*.tmp\n");
    // p/q replaced by a copy made before the index: it keeps the stamp.
    std::fs::remove_dir_all(at("p/q")).unwrap();
    std::fs::rename(at("p/cp"), at("p/q")).unwrap();
    put(&folder, "x/y/z/.ignore", b"k.c\n");
    let want = "a.c\nb.c\ng.gen\nmv2/m.c\nnew/deep/n.c\np/q/r.c\nsub/s.c\nsub/t.c\n";
    assert_eq!(printed(&stale, &["-l"], &["printk"]), want);
    assert_eq!(notes(), [ONCE]);
    let differ = [
        "a.c",
        "b.c",
        "d.c",
        "g.gen",
        "mv2",
        "mw/m.c",
        "new",
        "p/cp/r.c",
        "p/q/r.c",
        "x/y/z/.ignore",
        "x/y/z/k.c",
    ];
    assert_eq!(baseline(tmp.path()), serde_json::json!(differ));
    // The second search asks the token question; watchman reports what
    // it saw after the token, which the stat pass found already.
    daemon.sent();
    assert!(replays(&stale), "{:?}", notes());
    assert_eq!(daemon.sent().last().unwrap(), "query");
    assert_eq!(baseline(tmp.path()), serde_json::json!(differ));
    answers_like(&stale, &fresh("fresh1"), &terms, &["printk", "now"]);
    // Between searches, as watchman reports them.
    put(&folder, "sub/s.c", b"printk sub now\n");
    std::fs::remove_file(at("sub/t.c")).unwrap();
    put(&folder, "new/deep/m.c", b"deep printk two\n");
    std::fs::rename(at("mv2"), at("mv3")).unwrap();
    put(&folder, ".ignore", b"*.gen\nb.c\n");
    assert!(replays(&stale), "{:?}", notes());
    answers_like(&stale, &fresh("fresh2"), &terms, &["printk", "now", "deep"]);
    let want = "a.c\nmv3/m.c\nnew/deep/m.c\nnew/deep/n.c\np/q/r.c\nsub/s.c\n";
    assert_eq!(
        printed(&stale, &["-l"], &["printk"]),
        want,
        "nothing changed since"
    );
}

#[test]
fn watchman_answers_like_the_stat_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    let outside = tmp.path().join("outside");
    for (rel, bytes) in [
        (".ignore", &b"ig.c\n"[..]),
        ("a.c", b"printk one\n"),
        ("a2.c", b"printk soon ignored\n"),
        ("d.c", b"printk gone\n"),
        ("ig.c", b"printk soon searched\n"),
        ("kb/k.dat", b"printk\0binary"),
        ("mw/m.c", b"printk moved\n"),
        ("r.c", b"printk renamed\n"),
        ("tb.c", b"printk text\n"),
    ] {
        put(&folder, rel, bytes);
    }
    put(&outside, "t/deep/x.c", b"deep printk\n");
    let Some(_daemon) = live(tmp.path(), &folder) else {
        return;
    };
    let stale = tmp.path().join("idx");
    index_it(&folder, &stale, false);
    query_it(&stale, false, &["printk"]);
    assert_eq!(notes(), [ONCE], "the first search takes the token");
    put(&folder, ".ignore", b"a2.c\n");
    put(&folder, "a.c", b"printk one\nprintk printk\n");
    put(&folder, "b.c", b"printk added\naa\n");
    put(&folder, "new/deep/n.c", b"deep printk\n");
    std::fs::rename(outside.join("t"), folder.join("t")).unwrap();
    put(&folder, "kb/k.dat", b"printk text now\n");
    put(&folder, "tb.c", b"printk\0now binary");
    std::fs::rename(folder.join("mw"), folder.join("mv2")).unwrap();
    std::fs::rename(folder.join("r.c"), folder.join("r2.c")).unwrap();
    std::fs::remove_file(folder.join("d.c")).unwrap();
    assert!(replays(&stale), "{:?}", notes());
    let want = "a.c\nb.c\nig.c\nkb/k.dat\nmv2/m.c\nnew/deep/n.c\nr2.c\nt/deep/x.c\n";
    assert_eq!(printed(&stale, &["-l"], &["printk"]), want);
    put(&folder, "kb/k.dat", b"printk\0binary");
    let fresh = tmp.path().join("fresh");
    index_it(&folder, &fresh, false);
    assert!(replays(&stale));
    let terms = ["printk", "aa", "deep", "now"];
    answers_like(
        &stale,
        &fresh,
        &["printk", "/pr[i1]ntk/", "aa", "deep", "now"],
        &terms,
    );
}

#[test]
fn watchman_skips_the_stat_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(
        tmp.path(),
        &[("a.c", b"printk hello printk"), ("b.c", b"nothing here")],
    );
    let Some(_daemon) = live(tmp.path(), &folder) else {
        return;
    };
    let output = tmp.path().join("idx");
    index_it(&folder, &output, false);
    query_it(&output, false, &["printk"]);
    assert_eq!(notes(), [ONCE], "the first search takes the token");
    let mut stamps = std::fs::read(output.join("docs.stamps")).unwrap();
    stamps[0] ^= 1;
    std::fs::write(output.join("docs.stamps"), stamps).unwrap();
    let manifest = read_manifest(&output).unwrap();
    let mut docs =
        crate::fetch::DocStore::open(&output, &manifest, None, crate::fetch::Ask::Load).unwrap();
    assert!(docs.replay(&manifest.files), "{:?}", notes());
    docs.begin();
    assert_eq!(docs.stale(), [0usize; 0], "watchman: no doc is named");
    docs.stat_all(&manifest.files);
    docs.begin();
    assert_eq!(docs.stale(), [0], "the stat pass: the tampered stamp");
}

#[test]
fn a_private_daemon_watches_logs_restarts_and_hangs() {
    use serde_json::json;
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::time::{Duration, Instant};
    let Some(mut daemon) = Daemon::start() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::write(root.join("a.c"), b"a").unwrap();
    daemon.watch(&root);
    assert_eq!(daemon.roots(), [root.to_str().unwrap()]);
    let clock = json!(["clock", root, {"sync_timeout": 10_000}]);
    let first = daemon.call(&clock).unwrap()["clock"].clone();
    assert_eq!(daemon.sent(), ["clock"]);
    daemon.restart();
    assert_eq!(daemon.roots(), [root.to_str().unwrap()], "watched again");
    let again = daemon.call(&clock).unwrap()["clock"].clone();
    assert_ne!(first, again, "another instance");
    daemon.hang();
    let start = Instant::now();
    let mut conn = std::os::unix::net::UnixStream::connect(&daemon.sock).unwrap();
    conn.set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let command = format!("{}\n", json!(["watch-list"]));
    conn.write_all(command.as_bytes()).unwrap();
    let mut line = String::new();
    assert!(
        BufReader::new(&conn).read_line(&mut line).is_err(),
        "no reply while it hangs: {line}"
    );
    assert!(start.elapsed() >= Duration::from_millis(300));
    daemon.resume();
    daemon.unwatch(&root);
    assert!(daemon.roots().is_empty());
}

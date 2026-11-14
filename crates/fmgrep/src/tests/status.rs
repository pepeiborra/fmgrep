//! `fmgrep status` against a private watchman daemon: it says what the
//! next search would learn from watchman, and changes nothing: no token,
//! no record, no watch, no daemon.

use super::watched::{AB, indexed, notes, records, watched};
use super::watchman::Daemon;
use super::*;
use crate::cli::dispatch_to;

/// `status --json` of `index`.
fn status(index: &Path) -> serde_json::Value {
    let mut out = Vec::new();
    let args = ["status", "--json", index.to_str().unwrap()];
    dispatch_to(args, &mut std::io::empty(), &mut out, false).unwrap();
    serde_json::from_slice(&out).unwrap()
}

/// What `status` must leave as it found it: the records in `<tmp>/cache`,
/// byte for byte, and what the daemon watches.
fn state(tmp: &Path, daemon: &Daemon) -> (Vec<(PathBuf, Vec<u8>)>, Vec<String>) {
    let records = records(tmp)
        .into_iter()
        .map(|p| (p.clone(), std::fs::read(&p).unwrap()))
        .collect();
    (records, daemon.roots())
}

/// The changes object, and the requests the daemon got for it.
fn changes(index: &Path, daemon: &Daemon) -> (serde_json::Value, Vec<String>) {
    daemon.sent();
    let report = status(index);
    assert!(notes().is_empty(), "status says nothing on stderr");
    (report["changes"].clone(), daemon.sent())
}

fn why(changes: &serde_json::Value) -> &str {
    assert_eq!(changes["watchman"], false, "{changes}");
    changes["why"].as_str().unwrap()
}

#[test]
fn without_a_record_status_names_the_first_search_and_takes_no_token() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((_folder, output, daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    let before = state(tmp.path(), &daemon);
    let (got, sent) = changes(&output, &daemon);
    assert_eq!(
        why(&got),
        "no record of an earlier search; the next search checks every file and takes a token, \
         later ones ask watchman"
    );
    assert!(got["record"].is_null());
    assert_eq!(sent, ["watch-list", "get-config"], "no clock");
    assert_eq!(state(tmp.path(), &daemon), before);
}

#[test]
fn with_a_record_status_asks_since_its_token_and_keeps_it() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    let before = state(tmp.path(), &daemon);
    let (got, sent) = changes(&output, &daemon);
    assert_eq!(got["watchman"], true, "{got}");
    assert!(got["why"].is_null());
    assert_eq!(got["record"], before.0[0].0.to_str().unwrap());
    assert_eq!(sent, ["watch-list", "get-config", "query"], "no clock");
    assert_eq!(state(tmp.path(), &daemon), before);
    // The next search still asks since the token the first one took.
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    assert!(notes().is_empty(), "the record serves");
    let asked = ["watch-list", "get-config", "clock", "clock", "query"];
    assert_eq!(daemon.sent(), asked);
}

#[test]
fn status_says_why_a_record_cannot_serve_and_mends_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, mut daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    let root = std::fs::canonicalize(&folder).unwrap();

    // The watch removed: status does not add it again.
    daemon.unwatch(&folder);
    let before = state(tmp.path(), &daemon);
    let (got, sent) = changes(&output, &daemon);
    let gone = format!(
        "watchman no longer watches {}; the next search asks it to and checks every file",
        root.display()
    );
    assert_eq!(why(&got), gone);
    assert_eq!(sent, ["watch-list"]);
    assert_eq!(state(tmp.path(), &daemon), before);
    assert!(before.1.is_empty(), "still no watch");

    // Restarted: the record's token is of the old instance.
    daemon.restart();
    daemon.watch(&folder);
    let before = state(tmp.path(), &daemon);
    let (got, sent) = changes(&output, &daemon);
    let restarted = "watchman cannot say what changed (watchman restarted, added the watch again \
                     or pruned deleted files since the last search); the next search checks \
                     every file";
    assert_eq!(why(&got), restarted);
    assert_eq!(sent, ["watch-list", "get-config", "query"]);
    assert_eq!(state(tmp.path(), &daemon), before);

    // Gone, with WATCHMAN_SOCK naming its socket: no search starts one.
    let records = before.0;
    daemon.stop();
    let got = status(&output)["changes"].clone();
    let line = format!(
        "watchman cannot say what changed (no watchman daemon answers on {} (",
        daemon.sock.display()
    );
    assert!(why(&got).starts_with(&line), "{got}");
    assert!(
        why(&got).ends_with("); every search checks every file"),
        "{got}"
    );
    assert!(notes().is_empty());
    let kept: Vec<_> = super::watched::records(tmp.path())
        .into_iter()
        .map(|p| (p.clone(), std::fs::read(&p).unwrap()))
        .collect();
    assert_eq!(kept, records);
}

/// In each state of watchman and the record, status says that watchman
/// can say exactly when the search that follows checks only what watchman
/// names, and so warns nothing of checking every file.
#[test]
fn status_says_watchman_can_say_exactly_when_the_next_search_uses_it() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, mut daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    let root = std::fs::canonicalize(&folder).unwrap();
    let agree = |state: &str| -> String {
        let got = status(&output)["changes"].clone();
        assert!(notes().is_empty(), "{state}: status says nothing on stderr");
        printed(&output, &["-l"], &["printk"]);
        let warned = notes();
        let checked = warned.iter().any(|w| w.contains("checked every file"));
        assert_eq!(
            got["watchman"] == true,
            !checked,
            "{state}: {got} {warned:?}"
        );
        got["why"].as_str().unwrap_or_default().to_owned()
    };
    daemon.unwatch(&folder);
    let unwatched = format!(
        "watchman does not watch {}; the next search asks it to and checks every file",
        root.display()
    );
    assert_eq!(agree("no record, no watch"), unwatched);
    agree("the first record");
    daemon.unwatch(&folder);
    agree("the watch removed");
    agree("watched again");
    daemon.restart();
    daemon.watch(&folder);
    agree("restarted");
    agree("a record of the new daemon");
    for i in 0..65 {
        super::watched::put(&folder, &format!("n{i}.c"), b"printk\n");
    }
    let over = "more than 64 paths differ from the index; the next search checks every file \
                (re-run index)";
    assert_eq!(agree("over the cap"), over);
    agree("a record without a baseline");
    daemon.stop();
    agree("no daemon");
}

/// Without `WATCHMAN_SOCK`, status runs the client with `--no-spawn`, so
/// that it starts no daemon, and says that the next search starts one.
#[test]
fn status_finds_the_daemon_without_starting_one() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((_folder, output, mut daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    WATCHMAN.set(None);
    WATCHMAN_PROGRAM.set(super::watched::stand_in(tmp.path(), &daemon.sock));
    let got = status(&output)["changes"].clone();
    assert_eq!(got["watchman"], true, "the record's socket: {got}");
    assert_eq!(super::watched::runs(tmp.path()), "", "the record names it");
    daemon.stop();
    let got = status(&output)["changes"].clone();
    assert_eq!(
        why(&got),
        "watchman is not running; the next search starts it and checks every file, later ones \
         ask watchman"
    );
    let runs = super::watched::runs(tmp.path());
    assert_eq!(runs, "--no-spawn --no-pretty get-sockname\n");
    assert!(notes().is_empty() && records(tmp.path()).len() == 1);
}

/// `AB` indexed at `<tmp>/idx`, asked about at any size, with records in
/// `<tmp>/cache`, and no daemon: the folder and the index.
fn unwatched(tmp: &Path) -> (PathBuf, PathBuf) {
    let folder = stage_corpus(tmp, AB);
    let output = tmp.join("idx");
    index_it(&folder, &output, false);
    ASK_FROM.set(Some(0));
    CACHE.set(Some(tmp.join("cache")));
    (folder, output)
}

#[test]
fn without_watchman_installed_status_says_every_search_checks_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (_folder, output) = unwatched(tmp.path());
    let got = status(&output)["changes"].clone();
    let why = why(&got);
    assert!(
        why.starts_with("watchman cannot say what changed (cannot run watchman (")
            && why.ends_with("; install watchman); every search checks every file"),
        "{why}"
    );
    assert!(notes().is_empty() && records(tmp.path()).is_empty());
}

#[test]
fn on_a_network_filesystem_status_asks_no_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let (folder, output) = unwatched(tmp.path());
    let root = std::fs::canonicalize(&folder).unwrap();
    MOUNTS.set(Some(super::watched::mounted(tmp.path(), &root, "nfs4")));
    let got = status(&output)["changes"].clone();
    let want = format!(
        "watchman cannot say what changed ({} is on nfs4, a network or shared filesystem: \
         watchman does not see changes made there from other machines); every search checks \
         every file",
        root.display()
    );
    assert_eq!(why(&got), want);
    assert!(notes().is_empty() && records(tmp.path()).is_empty());
}

/// A server that asks watchman before each query says so in its status
/// reply, and that watchman answered its last ask.
#[test]
fn a_watching_server_reports_watchmans_last_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((_folder, output, _daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"terms":["printk"]}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"status"}"#,
        "\n"
    );
    let mut out = Vec::new();
    let args = ["serve", "--stdio", output.to_str().unwrap()];
    dispatch_to(args, &mut input.as_bytes(), &mut out, false).unwrap();
    let text = String::from_utf8(out).unwrap();
    let reply: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
    let status = &reply["result"];
    assert_eq!(
        (&status["watch"], &status["format"]),
        (&json(true), &json(8))
    );
    assert!(status["watchman"].is_null(), "{status}");
    notes();
}

fn json(v: impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(v).unwrap()
}

/// With a record, `status` counts what the next search re-checks: the
/// same stale, added and gone files, from what watchman names.
#[test]
fn status_counts_what_the_next_search_scans() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    super::watched::put(&folder, "new/c.c", b"printk\n");
    std::fs::remove_file(folder.join("a.c")).unwrap();
    let (got, _) = changes(&output, &daemon);
    assert_eq!(got["watchman"], true, "{got}");
    let counts = [&got["stale"], &got["added"], &got["gone"]];
    assert_eq!(counts, [1, 1, 1], "{got}");
    assert_eq!(got["ignores"], serde_json::json!([]));
    assert_eq!(got["ignores_all"], true, "new ignore files were looked for");
    let manifest = read_manifest(&output).unwrap();
    let ask = crate::fetch::Ask::Load;
    let mut docs = crate::fetch::DocStore::open(&output, &manifest, None, ask).unwrap();
    assert!(docs.replay(&manifest.files), "the search asks watchman");
    docs.begin();
    let n = manifest.files.len();
    let stale = docs.stale().partition_point(|&d| d < n);
    let scanned = (stale, docs.stale().len() - stale, docs.gone());
    assert_eq!(scanned, (1, 1, 1));
}

/// On a tree that a search asks watchman about, when watchman cannot say,
/// `status` runs no stat pass: the counts are not known, and only the
/// recorded ignore inputs are statted.
#[test]
fn a_large_tree_watchman_cannot_cover_is_not_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let (folder, output) = unwatched(tmp.path());
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    let got = status(&output)["changes"].clone();
    for key in ["stale", "added", "gone"] {
        assert!(got[key].is_null(), "{got}");
    }
    assert_eq!(got["ignores"], serde_json::json!([]));
    assert_eq!(got["ignores_all"], false, "only the recorded inputs: {got}");
    let mut out = Vec::new();
    let args = ["status", output.to_str().unwrap()];
    dispatch_to(args, &mut std::io::empty(), &mut out, false).unwrap();
    let text = String::from_utf8(out).unwrap();
    let tail: Vec<&str> = text.lines().skip(4).collect();
    assert_eq!(
        tail,
        [
            "since the index: not known without checking every file; a search checks",
            "ignore files: none changed since the index (of those the index recorded)",
        ]
    );
    assert!(notes().is_empty() && records(tmp.path()).is_empty());
}

//! Searches that ask watchman: each test starts a private daemon of its
//! own, and is skipped without watchman.

use super::drive::answers_like;
use super::watchman::Daemon;
use super::*;

/// Whether a load of `index` re-checks only what watchman names.
fn replays(index: &Path) -> bool {
    let manifest = read_manifest(index).unwrap();
    let docs = crate::fetch::DocStore::open(index, &manifest, None).unwrap();
    docs.replay(&manifest.files)
}

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
    std::fs::write(root.join(rel), bytes).unwrap();
}

fn journal_json(index: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(index.join("docs.journal")).unwrap()).unwrap()
}

/// A private daemon that watches `folder`, which this thread's indexes and
/// searches ask, at any size; `None` without watchman.
fn live(folder: &Path) -> Option<Daemon> {
    let daemon = Daemon::start()?;
    daemon.watch(folder);
    WATCHMAN.set(Some(daemon.sock.clone()));
    JOURNAL.set(Some("on"));
    Some(daemon)
}

/// `files` under `<tmp>/corpus`, backdated, indexed at `<tmp>/idx` while
/// [`live`] watches the tree.
fn watched(tmp: &Path, files: &[(&str, &[u8])]) -> Option<(PathBuf, PathBuf, Daemon)> {
    let folder = tmp.join("corpus");
    for (rel, bytes) in files {
        put(&folder, rel, bytes);
    }
    backdate(&folder);
    let daemon = live(&folder)?;
    let output = tmp.join("idx");
    index_it(&folder, &output, false);
    daemon.sent();
    Some((folder, output, daemon))
}

const AB: &[(&str, &[u8])] = &[("a.c", b"printk\n"), ("b.c", b"none\n")];

#[test]
fn a_small_tree_asks_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    let journal = std::fs::read(output.join("docs.journal")).unwrap();
    JOURNAL.set(None);
    std::fs::write(folder.join("c.c"), b"printk\n").unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nc.c\n");
    assert!(!replays(&output));
    assert!(daemon.sent().is_empty());
    assert_eq!(std::fs::read(output.join("docs.journal")).unwrap(), journal);
}

#[test]
fn journal_answers_like_the_sweep() {
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
    let Some(_daemon) = live(&folder) else {
        return;
    };
    let stale = tmp.path().join("idx");
    index_it(&folder, &stale, false);
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
    assert!(replays(&stale));
    let want = "a.c\nb.c\nig.c\nkb/k.dat\nmv2/m.c\nnew/deep/n.c\nr2.c\nt/deep/x.c\n";
    let got = printed(&stale, &["-l"], &["printk"]);
    assert_eq!(got, want, "kb/k.dat, named, is found");
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
fn journal_replays_edits_made_after_its_mark() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), &[("a.c", b"printk\n"), ("b.c", b"none\n")]);
    let Some(_daemon) = live(&folder) else {
        return;
    };
    let early = crate::source::marks(&folder, crate::source::Knob::On);
    std::fs::write(folder.join("a.c"), b"printk printk during\n").unwrap();
    let stale = tmp.path().join("idx");
    index_it(&folder, &stale, false);
    let mut journal = journal_json(&stale);
    journal["tokens"] = serde_json::to_value(early).unwrap();
    std::fs::write(stale.join("docs.journal"), journal.to_string()).unwrap();
    std::fs::write(folder.join("b.c"), b"printk after\n").unwrap();
    let fresh = tmp.path().join("fresh");
    index_it(&folder, &fresh, false);
    assert!(replays(&stale));
    answers_like(&stale, &fresh, &["printk", "during", "after"], &["printk"]);
}

#[test]
fn journal_skips_the_stat_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(
        tmp.path(),
        &[("a.c", b"printk hello printk"), ("b.c", b"nothing here")],
    );
    let Some(_daemon) = live(&folder) else {
        return;
    };
    let output = tmp.path().join("idx");
    index_it(&folder, &output, false);
    let mut stamps = std::fs::read(output.join("docs.stamps")).unwrap();
    stamps[0] ^= 1;
    std::fs::write(output.join("docs.stamps"), stamps).unwrap();
    let manifest = read_manifest(&output).unwrap();
    let mut docs = crate::fetch::DocStore::open(&output, &manifest, None).unwrap();
    assert!(docs.replay(&manifest.files));
    docs.begin();
    assert_eq!(docs.stale(), [0usize; 0], "journal: no doc is named");
    docs.stat_all(&manifest.files);
    docs.begin();
    assert_eq!(docs.stale(), [0], "sweep: the tampered stamp");
}

#[test]
fn checkpoint_advances_the_token_and_keeps_changed_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    let clock = |j: &serde_json::Value| j["tokens"][0]["token"]["clock"].clone();
    let before = journal_json(&output);
    std::fs::write(folder.join("a.c"), b"printk edited\n").unwrap();
    std::fs::write(folder.join("n.c"), b"printk new\n").unwrap();
    // Watchman sees the edits before the search's clock, which does not
    // sync.
    daemon.watch(&folder);
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nn.c\n");
    let asked = ["watch-list", "get-config", "clock", "query"];
    assert_eq!(daemon.sent(), asked);
    let after = journal_json(&output);
    assert_ne!(clock(&after), clock(&before), "{after}");
    assert_eq!(after["named"], serde_json::json!(["a.c", "n.c"]));
    assert_eq!(after["root"], before["root"]);
    std::fs::remove_file(folder.join("n.c")).unwrap();
    assert_eq!(printed(&output, &["-c"], &["printk"]), "a.c:1\n");
    assert_eq!(journal_json(&output)["named"], serde_json::json!(["a.c"]));
}

#[test]
fn read_only_index_dir_skips_the_checkpoint() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, _daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    let journal = std::fs::read(output.join("docs.journal")).unwrap();
    let mode = |m: u32| std::fs::set_permissions(&output, std::fs::Permissions::from_mode(m));
    mode(0o555).unwrap();
    let answer = printed(&output, &["-c"], &["printk"]);
    mode(0o755).unwrap();
    assert_eq!(answer, "a.c:1\nb.c:1\n");
    let now = std::fs::read(output.join("docs.journal")).unwrap();
    assert_eq!(now, journal, "no checkpoint");
}

#[test]
fn knob_off_reads_no_journal_and_writes_none() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    let journal = std::fs::read(output.join("docs.journal")).unwrap();
    JOURNAL.set(Some("off"));
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    let now = std::fs::read(output.join("docs.journal")).unwrap();
    assert_eq!(now, journal, "no checkpoint");
    let off = tmp.path().join("off");
    index_it(&folder, &off, false);
    assert!(!off.join("docs.journal").exists());
    assert!(daemon.sent().is_empty(), "nothing asked");
}

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

/// The warnings given on this thread since the last call.
fn notes() -> Vec<String> {
    crate::source::NOTES.take()
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
    ASK_FROM.set(Some(0));
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

fn cannot(why: &str) -> String {
    format!("watchman cannot say what changed ({why}); checked every file")
}

/// A hung daemon (stopped in place), a watch removed and added again, and
/// no daemon: each search says why, and checks every file.
#[test]
fn a_load_watchman_cannot_answer_says_why_and_stats_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, mut daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    std::fs::write(folder.join("b.c"), b"printk now\n").unwrap();
    let journal = std::fs::read(output.join("docs.journal")).unwrap();
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
    assert_eq!(std::fs::read(output.join("docs.journal")).unwrap(), journal);
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    notes();
    daemon.unwatch(&folder);
    assert!(!replays(&output));
    let again = "watchman restarted, added the watch again or pruned deleted files since the \
                 last search";
    assert_eq!(notes(), [cannot(again)]);
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nb.c\n");
    let now = journal_json(&output);
    assert_ne!(now.to_string().into_bytes(), journal, "a later token");
    assert_eq!(now["named"], serde_json::json!(["b.c"]), "the stat pass's");
    notes();
    daemon.stop();
    assert!(!replays(&output));
    let why = notes();
    let gone = "watchman cannot say what changed (no watchman daemon answers on ";
    assert!(why.len() == 1 && why[0].starts_with(gone), "{why:?}");
}

#[test]
fn an_index_without_a_token_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), AB);
    let output = tmp.path().join("idx");
    index_it(&folder, &output, false);
    assert!(!output.join("docs.journal").exists());
    ASK_FROM.set(Some(0));
    assert!(!replays(&output));
    let root = std::fs::canonicalize(&folder).unwrap();
    let r = root.display();
    let why = format!(
        "watchman cannot say what changed (the index holds no watchman token; run 'watchman \
         watch-project {r}', then re-index); checked every file"
    );
    assert_eq!(notes(), [why]);
}

#[test]
fn a_small_tree_asks_nothing_and_says_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    let journal = std::fs::read(output.join("docs.journal")).unwrap();
    ASK_FROM.set(Some(3));
    std::fs::write(folder.join("c.c"), b"printk\n").unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nc.c\n");
    assert!(!replays(&output));
    assert!(daemon.sent().is_empty());
    assert!(notes().is_empty(), "no line");
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
    assert!(replays(&stale), "{:?}", notes());
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
    assert!(docs.replay(&manifest.files), "{:?}", notes());
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
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nn.c\n");
    let asked = ["watch-list", "get-config", "clock", "clock", "query"];
    assert_eq!(daemon.sent(), asked);
    let after = journal_json(&output);
    assert_ne!(clock(&after), clock(&before), "{after}");
    assert_eq!(after["named"], serde_json::json!(["a.c", "n.c"]));
    assert_eq!(after["root"], before["root"]);
    std::fs::remove_file(folder.join("n.c")).unwrap();
    assert_eq!(printed(&output, &["-c"], &["printk"]), "a.c:1\n");
    assert_eq!(journal_json(&output)["named"], serde_json::json!(["a.c"]));
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
    assert!(notes().is_empty(), "{:?}", notes());
}

#[test]
fn a_file_added_under_a_new_dir_is_found_again_from_the_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, _daemon)) = watched(tmp.path(), AB) else {
        return;
    };
    put(&folder, "new/sub/n.c", b"printk new\n");
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nnew/sub/n.c\n");
    assert_eq!(journal_json(&output)["named"], serde_json::json!(["new"]));
    let again = "a.c\nnew/sub/n.c\n";
    assert_eq!(
        printed(&output, &["-l"], &["printk"]),
        again,
        "nothing changed since"
    );
    assert_eq!(journal_json(&output)["named"], serde_json::json!(["new"]));
    std::fs::remove_dir_all(folder.join("new")).unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\n");
    assert_eq!(journal_json(&output)["named"], serde_json::json!([]));
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
    let named = || journal_json(&output)["named"].clone();
    assert_eq!(named(), hidden, "the ignore file is kept");
    for _ in 0..2 {
        assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\n");
    }
    assert_eq!(named(), hidden);
    std::fs::remove_file(folder.join("x/y/z/.ignore")).unwrap();
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\nx/y/z/k.c\n");
    assert_eq!(named(), serde_json::json!([]));
    assert!(notes().is_empty());
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
        journal_json(&output)["named"],
        serde_json::json!(["a/sub/f.c", "b/sub/f.c"])
    );
    assert!(notes().is_empty());
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

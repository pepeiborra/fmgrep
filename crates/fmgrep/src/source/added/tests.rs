use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[cfg(unix)]
use super::super::tests::Fix;
use super::super::tests::{StopOnDrop, eventually, open_files, stage_files};
use super::super::{Files, WARNED};
#[cfg(unix)]
use crate::tests::support::{T0, backdate, set_mtime};

/// Live added paths, ascending by slot.
fn live(files: &Files) -> Vec<String> {
    let snap = files.snapshot();
    let n = files.known.state.len();
    snap.added
        .iter()
        .map(|&id| snap.names[id - n].clone())
        .collect()
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn discovery_registers_text_files_missing_from_the_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk", b"zz"]);
    let root = &fix.path;
    for (rel, bytes) in [
        ("b.c", &b"printk"[..]),
        ("new/deep/n.c", b"printk"),
        ("bin.dat", b"printk\0"),
        (".dot.c", b"printk"),
        (".hdir/x.c", b"printk"),
        ("idx/manifest.json", b"{\"printk\":1}"),
    ] {
        write(root, rel, bytes);
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("new"), root.join("linkdir")).unwrap();
        std::os::unix::fs::symlink(root.join("b.c"), root.join("alias.c")).unwrap();
    }
    let mut files = open_files(&fix);
    files.exclude(&root.join("idx"));
    files.discovery().run(&fix.files, &AtomicBool::new(false));
    let want: &[&str] = if cfg!(unix) {
        &["alias.c", "b.c", "new/deep/n.c"]
    } else {
        &["b.c", "new/deep/n.c"]
    };
    assert_eq!(live(&files), want);
    assert_eq!(
        files.snapshot().added[0],
        fix.files.len(),
        "added ids follow the manifest's"
    );
    assert_eq!(
        files.read_added(fix.files.len() + 1).as_deref(),
        Some(&b"printk"[..])
    );
}

#[test]
fn watch_registers_created_renamed_and_new_dir_files() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk"]);
    let root = fix.path.clone();
    let files = open_files(&fix);
    let watch = files.watch().unwrap();
    let discovery = files.discovery();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| watch.run(&fix.files, &fix.lens, &stop));
        let _stop = StopOnDrop(&stop);
        discovery.run(&fix.files, &stop);
        // FSEvents can miss changes made just after the watch starts.
        let created = || {
            write(&root, "x.c", b"printk");
            live(&files) == ["x.c"]
        };
        assert!(eventually(created), "{:?}", live(&files));
        std::fs::rename(root.join("x.c"), root.join("y.c")).unwrap();
        assert!(eventually(|| live(&files) == ["y.c"]), "{:?}", live(&files));
        write(&root, "nd/sub/z.c", b"printk");
        write(&root, "nd/bin.dat", b"\0");
        let want = ["y.c", "nd/sub/z.c"];
        assert!(eventually(|| live(&files) == want), "{:?}", live(&files));
        std::fs::remove_dir_all(root.join("nd")).unwrap();
        assert!(eventually(|| live(&files) == ["y.c"]), "{:?}", live(&files));
    });
}

#[test]
fn watch_drops_a_deleted_added_file_and_readds_it_when_recreated() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk"]);
    let root = fix.path.clone();
    write(&root, "x.c", b"printk");
    let files = open_files(&fix);
    let watch = files.watch().unwrap();
    let discovery = files.discovery();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| watch.run(&fix.files, &fix.lens, &stop));
        let _stop = StopOnDrop(&stop);
        discovery.run(&fix.files, &stop);
        assert_eq!(files.snapshot().added, [1]);
        let deleted = || {
            let _ = std::fs::remove_file(root.join("x.c"));
            files.snapshot().added.is_empty()
        };
        assert!(eventually(deleted), "{:?}", live(&files));
        write(&root, "x.c", b"printk again");
        assert!(
            eventually(|| files.snapshot().added == [1]),
            "the slot is kept: {:?}",
            files.snapshot()
        );
    });
}

#[test]
fn watch_waits_for_discovery_before_applying_events() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk"]);
    let files = open_files(&fix);
    let watch = files.watch().unwrap();
    let discovery = files.discovery();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| watch.run(&fix.files, &fix.lens, &stop));
        let _stop = StopOnDrop(&stop);
        write(&fix.path, "x.c", b"printk");
        std::fs::write(fix.path.join(&fix.files[0]), b"changed").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            files.snapshot().stale.is_empty(),
            "no event is applied before discovery"
        );
        // Unrun, it only releases; a rewrite covers events FSEvents dropped.
        files.known.discovering.store(true, Ordering::Release);
        drop(discovery);
        let seen = || {
            write(&fix.path, "x.c", b"printk");
            std::fs::write(fix.path.join(&fix.files[0]), b"changed").unwrap();
            live(&files) == ["x.c"] && files.snapshot().stale == [0]
        };
        assert!(eventually(seen), "{:?}", files.snapshot());
    });
}

#[test]
fn read_of_an_added_file_turned_binary_drops_it_silently() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk"]);
    write(&fix.path, "x.c", b"printk");
    write(&fix.path, "y.c", b"printk");
    let files = open_files(&fix);
    files.discovery().run(&fix.files, &AtomicBool::new(false));
    assert_eq!(files.snapshot().added, [1, 2]);
    let epoch = files.epoch();
    std::fs::write(fix.path.join("x.c"), b"printk\0").unwrap();
    std::fs::remove_file(fix.path.join("y.c")).unwrap();
    assert_eq!(files.read_added(1), None);
    assert_eq!(files.read_added(2), None);
    assert_eq!(files.read_added(3), None, "no such slot");
    assert!(files.snapshot().added.is_empty() && files.epoch() != epoch);
    assert_eq!(files.known.state[0].load(Ordering::Acquire) & WARNED, 0);
}

/// `stage_files` of `docs` plus the unlisted `extra` files, backdated, with
/// the walk recorded as `index` records it, the walk having begun `started`.
#[cfg(unix)]
fn stage_recorded(
    dir: &Path,
    docs: &[&[u8]],
    extra: &[(&str, &[u8])],
    started: std::time::SystemTime,
) -> Fix {
    let fix = stage_files(dir, docs);
    for (rel, bytes) in extra {
        write(&fix.path, rel, bytes);
    }
    backdate(&fix.path);
    let first = std::fs::metadata(fix.path.join(&fix.files[0])).unwrap();
    let t0 = crate::Stamp::of(&first).mtime_ns;
    super::super::write_stamps(dir, &vec![t0; docs.len()]).unwrap();
    let walk = crate::for_each_index_doc(&fix.path, None, |_, _, _| Ok(())).unwrap();
    super::super::write_walk(dir, &walk, started).unwrap();
    fix
}

/// The fixture's files, with its record unless `walk_all`, after discovery.
#[cfg(unix)]
fn discovered(fix: &Fix, manifest: &[String], walk_all: bool) -> Files {
    let mut files = open_files(fix);
    if !walk_all {
        let index = fix.path.parent().unwrap();
        files.record(super::super::Recorded::open(index).unwrap().unwrap());
    }
    files.discovery().run(manifest, &AtomicBool::new(false));
    files
}

#[cfg(unix)]
#[test]
fn recorded_discovery_lists_only_changed_and_new_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let extra: &[(&str, &[u8])] = &[
        ("old/o.bin", b"\0"),
        ("keep/k.bin", b"\0"),
        ("keep/deep/x.bin", b"\0"),
    ];
    let now = std::time::SystemTime::now();
    let fix = stage_recorded(tmp.path(), &[b"printk", b"zz"], extra, now);
    let root = &fix.path;
    write(root, "old/new.c", b"printk");
    write(root, "top.c", b"printk");
    write(root, "fresh/n/n.c", b"printk");
    // Same entry count, so with its mtime restored `keep` keeps its stamp.
    std::fs::rename(root.join("keep/k.bin"), root.join("keep/planted.c")).unwrap();
    write(root, "keep/planted.c", b"printk");
    set_mtime(&root.join("keep"), T0);
    std::fs::write(root.join(&fix.files[1]), b"z\0").unwrap();
    assert_eq!(
        live(&discovered(&fix, &fix.files, false)),
        ["fresh/n/n.c", "old/new.c", "top.c"]
    );
    let all = ["fresh/n/n.c", "keep/planted.c", "old/new.c", "top.c"];
    assert_eq!(
        live(&discovered(&fix, &fix.files, true)),
        all,
        "the full walk finds the planted file"
    );
}

#[cfg(unix)]
#[test]
fn recorded_discovery_walks_a_tree_copied_in_with_preserved_mtimes() {
    let tmp = tempfile::tempdir().unwrap();
    let now = std::time::SystemTime::now();
    let fix = stage_recorded(tmp.path(), &[b"printk"], &[("sub/s.bin", b"\0")], now);
    let tree = tmp.path().join("tree");
    write(&tree, "t/deep/x.c", b"printk");
    write(&tree, "t/y.c", b"printk");
    backdate(&tree);
    std::fs::rename(tree.join("t"), fix.path.join("sub/t")).unwrap();
    assert_eq!(
        live(&discovered(&fix, &fix.files, false)),
        ["sub/t/deep/x.c", "sub/t/y.c"]
    );
}

#[cfg(unix)]
#[test]
fn recorded_discovery_follows_a_renamed_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let extra: &[(&str, &[u8])] = &[("mw/m.c", b"printk"), ("mw/in/i.c", b"printk")];
    let now = std::time::SystemTime::now();
    let fix = stage_recorded(tmp.path(), &[b"printk"], extra, now);
    let manifest: Vec<String> = [fix.files[0].as_str(), "mw/in/i.c", "mw/m.c"]
        .map(String::from)
        .to_vec();
    assert!(live(&discovered(&fix, &manifest, false)).is_empty());
    std::fs::rename(fix.path.join("mw"), fix.path.join("mv2")).unwrap();
    assert_eq!(
        live(&discovered(&fix, &manifest, false)),
        ["mv2/in/i.c", "mv2/m.c"]
    );
}

#[cfg(unix)]
#[test]
fn skipped_file_turned_text_is_found_once_named_or_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let now = std::time::SystemTime::now();
    let fix = stage_recorded(tmp.path(), &[b"printk"], &[("kb/k.bin", b"printk\0")], now);
    let (root, k) = (&fix.path, fix.path.join("kb/k.bin"));
    std::fs::write(&k, b"printk now text").unwrap();
    let files = discovered(&fix, &fix.files, false);
    assert!(live(&files).is_empty(), "its unchanged dir is not listed");
    files.known.reconcile_path(root, &k, &fix.files);
    assert_eq!(live(&files), ["kb/k.bin"], "named by the watcher");
    set_mtime(&root.join("kb"), T0 + Duration::from_secs(1));
    assert_eq!(
        live(&discovered(&fix, &fix.files, false)),
        ["kb/k.bin"],
        "in a listed dir"
    );
    assert_eq!(
        live(&discovered(&fix, &fix.files, true)),
        ["kb/k.bin"],
        "by the full walk"
    );
}

#[cfg(unix)]
#[test]
fn racy_dir_is_listed_on_every_load() {
    let tmp = tempfile::tempdir().unwrap();
    let started = T0 + Duration::from_secs(1);
    let fix = stage_recorded(tmp.path(), &[b"printk"], &[("sub/s.bin", b"\0")], started);
    std::fs::rename(fix.path.join("sub/s.bin"), fix.path.join("sub/p.c")).unwrap();
    write(&fix.path, "sub/p.c", b"printk");
    set_mtime(&fix.path.join("sub"), T0);
    assert_eq!(live(&discovered(&fix, &fix.files, false)), ["sub/p.c"]);
}

#[cfg(unix)]
#[test]
fn dir_turned_symlink_is_not_followed() {
    let tmp = tempfile::tempdir().unwrap();
    let now = std::time::SystemTime::now();
    let fix = stage_recorded(tmp.path(), &[b"printk"], &[("a/b/k.bin", b"\0")], now);
    let away = tmp.path().join("away");
    std::fs::rename(fix.path.join("a"), &away).unwrap();
    std::os::unix::fs::symlink(&away, fix.path.join("a")).unwrap();
    std::fs::write(away.join("b/k.bin"), b"printk").unwrap();
    assert!(live(&discovered(&fix, &fix.files, false)).is_empty());
    assert!(live(&discovered(&fix, &fix.files, true)).is_empty());
}

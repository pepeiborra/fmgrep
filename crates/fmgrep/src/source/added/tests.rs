use std::path::Path;
use std::sync::atomic::Ordering;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use super::super::tests::Fix;
use super::super::tests::{open_files, stage_files};
use super::super::{Files, IGNORED, WARNED};
#[cfg(unix)]
use crate::manifest::FileTable;
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
        (".ignore", b"*.gen\nig/\n"),
        ("x.gen", b"printk"),
        ("ig/y.c", b"printk"),
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
    files.discover(&fix.files, &fix.lens);
    assert_eq!(live(&files), ["b.c", "new/deep/n.c"]);
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
fn read_of_an_added_file_turned_binary_drops_it_silently() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk"]);
    write(&fix.path, "x.c", b"printk");
    write(&fix.path, "y.c", b"printk");
    let files = open_files(&fix);
    files.discover(&fix.files, &fix.lens);
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

#[test]
fn added_utf16_file_is_found_and_read_as_utf8() {
    let tmp = tempfile::tempdir().unwrap();
    let fix = stage_files(tmp.path(), &[b"printk"]);
    write(&fix.path, "u.c", &crate::binary::utf16le("printk ü\r\n"));
    write(&fix.path, "z.c", &crate::binary::utf16le("printk\0"));
    let files = open_files(&fix);
    files.discover(&fix.files, &fix.lens);
    assert_eq!(live(&files), ["u.c"]);
    assert_eq!(
        files.read_added(1).as_deref(),
        Some("printk ü\r\n".as_bytes())
    );
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
    super::super::write_stamps(dir, &vec![t0; docs.len()], &[]).unwrap();
    let walk = crate::for_each_index_doc(&fix.path, None, |_, _, _, _| Ok(())).unwrap();
    super::super::write_walk(dir, &walk, started).unwrap();
    fix
}

/// The fixture's files, with its record unless `walk_all`, after discovery.
#[cfg(unix)]
fn discovered(fix: &Fix, manifest: &FileTable, walk_all: bool) -> Files {
    let mut files = open_files(fix);
    if !walk_all {
        let index = fix.path.parent().unwrap();
        files.record(super::super::Recorded::open(index).unwrap().unwrap());
    }
    files.discover(manifest, &vec![0; manifest.len()]);
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
    let manifest: FileTable = [&fix.files[0], "mw/in/i.c", "mw/m.c"].into_iter().collect();
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
    let recorded = files.known.recorded.clone().unwrap();
    let named = ["kb/k.bin".to_owned()];
    assert_eq!(
        files.known.replay(&recorded, &named, &fix.files, &fix.lens),
        Ok(1)
    );
    assert_eq!(live(&files), ["kb/k.bin"], "reported by watchman");
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

#[cfg(unix)]
#[test]
fn an_ignore_file_edited_in_place_rewalks_its_dir_at_load() {
    let tmp = tempfile::tempdir().unwrap();
    let now = std::time::SystemTime::now();
    let extra: &[(&str, &[u8])] = &[(".ignore", b"u.c\n"), ("sub/u.c", b"printk")];
    let fix = stage_recorded(tmp.path(), &[b"printk", b"zz"], extra, now);
    std::fs::write(fix.path.join(".ignore"), b"d0001.c\n").unwrap();
    let files = discovered(&fix, &fix.files, false);
    assert_eq!(live(&files), ["sub/u.c"]);
    assert_eq!(files.snapshot().gone, [1]);
    assert_eq!(files.known.state[1].load(Ordering::Acquire) & WARNED, 0);
}

#[cfg(unix)]
#[test]
fn discovery_keeps_a_doc_in_a_dir_it_cannot_list() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let mut fix = stage_files(tmp.path(), &[b"printk", b"zz"]);
    let sub = fix.path.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::rename(fix.path.join(&fix.files[1]), sub.join("x.c")).unwrap();
    fix.files = [&fix.files[0], "sub/x.c"].into_iter().collect();
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o311)).unwrap();
    let files = discovered(&fix, &fix.files, true);
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(files.known.kind(1), IGNORED);
    assert!(files.snapshot().gone.is_empty());
}

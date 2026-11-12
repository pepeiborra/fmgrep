use std::time::{Duration, UNIX_EPOCH};

use super::*;

fn at(secs: i64) -> Stamp {
    Stamp {
        len: 64,
        mtime_ns: secs * 1_000_000_000,
    }
}

fn rows(t: &Table) -> Vec<(String, Stamp)> {
    (0..t.rows.len())
        .map(|i| (t.path(i).to_owned(), t.rows[i].1))
        .collect()
}

fn owned(rows: &[(&str, Stamp)]) -> Vec<(String, Stamp)> {
    rows.iter().map(|&(p, s)| (p.to_owned(), s)).collect()
}

#[test]
fn walk_record_round_trips_and_racy_or_unstatted_dirs_never_match() {
    let tmp = tempfile::tempdir().unwrap();
    let walk = WalkNotes {
        dirs: vec![
            (String::new(), Some(at(1000))),
            ("a".into(), Some(at(2000))),
            ("a/b".into(), None),
            ("c".into(), Some(at(9_999))),
            ("d".into(), Some(at(3000))),
            ("d".into(), Some(at(3000))),
            ("e".into(), Some(at(20_000))),
        ],
        ignores: vec![
            ("../.gitignore".into(), None),
            (".gitignore".into(), Some(at(1000))),
            ("a/.ignore".into(), Some(at(20_000))),
        ],
    };
    write_walk(tmp.path(), &walk, UNIX_EPOCH + Duration::from_secs(10_000)).unwrap();
    let recorded = Recorded::open(tmp.path()).unwrap().expect("written");
    let dirs = [
        ("", at(1000)),
        ("a", at(2000)),
        ("a/b", UNKNOWN),
        ("c", UNKNOWN),
        ("d", UNKNOWN),
        ("e", UNKNOWN),
    ];
    assert_eq!(rows(&recorded.dirs), owned(&dirs));
    let ignores = [
        ("../.gitignore", ABSENT),
        (".gitignore", at(1000)),
        ("a/.ignore", UNKNOWN),
    ];
    assert_eq!(rows(&recorded.ignores), owned(&ignores));
    assert!(recorded.is_dir("a/b") && recorded.is_dir("") && !recorded.is_dir("a/x.bin"));
}

/// One record's bytes.
fn record(stamp: Stamp, rel: &[u8]) -> Vec<u8> {
    let mut out = stamp.len.to_le_bytes().to_vec();
    out.extend_from_slice(&stamp.mtime_ns.to_le_bytes());
    out.extend_from_slice(&(rel.len() as u32).to_le_bytes());
    out.extend_from_slice(rel);
    out
}

#[test]
fn malformed_record_is_refused_and_absent_or_foreign_one_walks_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let dirs = |body: &[&[u8]]| [&[&DIRS_MAGIC[..]], body].concat().concat();
    let (root, a, b) = (record(at(1), b""), record(at(1), b"a"), record(at(1), b"b"));
    let good = dirs(&[&root, &a, &b]);
    for (why, bytes) in [
        ("truncated", good[..good.len() - 1].to_vec()),
        ("truncated head", dirs(&[&root, &a[..10]])),
        ("short magic", DIRS_MAGIC[..5].to_vec()),
        ("not ascending", dirs(&[&root, &b, &a])),
        ("repeated", dirs(&[&root, &a, &a])),
        ("bad UTF-8", dirs(&[&root, &record(at(1), b"\xff")])),
        ("no root", dirs(&[&a, &b])),
        ("empty", dirs(&[])),
    ] {
        std::fs::write(dir.join(DIRS_STAMPS), bytes).unwrap();
        let err = Recorded::open(dir)
            .err()
            .unwrap_or_else(|| panic!("{why} must be refused"));
        assert!(
            err.contains(DIRS_STAMPS) && err.contains("tampered"),
            "{why}: {err}"
        );
    }
    std::fs::write(dir.join(DIRS_STAMPS), &good).unwrap();
    assert!(Recorded::open(dir).unwrap().is_none(), "no ignores.stamps");
    let ignores = [&IGNORES_MAGIC[..], &record(ABSENT, b".gitignore")].concat();
    for (why, bytes) in [
        ("truncated", ignores[..ignores.len() - 1].to_vec()),
        (
            "not ascending",
            [&ignores[..], &record(ABSENT, b"..")].concat(),
        ),
    ] {
        std::fs::write(dir.join(IGNORES_STAMPS), bytes).unwrap();
        let err = Recorded::open(dir).err().unwrap_or_else(|| panic!("{why}"));
        assert!(
            err.contains(IGNORES_STAMPS) && err.contains("tampered"),
            "{why}: {err}"
        );
    }
    let mut foreign = ignores.clone();
    foreign[7] = 2;
    std::fs::write(dir.join(IGNORES_STAMPS), foreign).unwrap();
    assert!(
        Recorded::open(dir).unwrap().is_none(),
        "another ignores.stamps"
    );
    std::fs::write(dir.join(IGNORES_STAMPS), &ignores).unwrap();
    assert!(Recorded::open(dir).unwrap().is_some());
    let mut foreign = good.clone();
    foreign[7] = 2;
    std::fs::write(dir.join(DIRS_STAMPS), foreign).unwrap();
    assert!(Recorded::open(dir).unwrap().is_none(), "another version");
    std::fs::remove_file(dir.join(DIRS_STAMPS)).unwrap();
    assert!(Recorded::open(dir).unwrap().is_none(), "no dirs.stamps");
}

/// `<tmp>/outer/root` holding `files`, backdated, recorded as `index` records it.
#[cfg(unix)]
fn recorded_tree(tmp: &Path, files: &[&str]) -> (PathBuf, Recorded) {
    let root = tmp.join("outer/root");
    for rel in files {
        std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
        std::fs::write(root.join(rel), b"x\n").unwrap();
    }
    crate::tests::support::backdate(&tmp.join("outer"));
    let walk = crate::for_each_index_doc(&root, None, |_, _, _, _| Ok(())).unwrap();
    write_walk(tmp, &walk, std::time::SystemTime::now()).unwrap();
    (root, Recorded::open(tmp).unwrap().unwrap())
}

#[cfg(unix)]
#[test]
fn ignore_scopes_name_the_dir_of_a_moved_row_and_the_root_above_it() {
    let tmp = tempfile::tempdir().unwrap();
    let files = [
        "a.c",
        ".gitignore",
        "sub/.ignore",
        "a/b/.rgignore",
        "a/b/c.c",
    ];
    let (root, recorded) = recorded_tree(tmp.path(), &files);
    let scopes = || {
        let got = recorded.ignore_scopes(&root, &[]);
        (got.scopes, got.moved)
    };
    assert_eq!(scopes(), (vec![], 0));
    std::fs::write(root.join("sub/.ignore"), b"x\ny\n").unwrap();
    assert_eq!(scopes(), (vec!["sub".to_owned()], 1));
    std::fs::remove_file(root.join("a/b/.rgignore")).unwrap();
    assert_eq!(scopes(), (vec!["a/b".to_owned(), "sub".to_owned()], 2));
    std::fs::write(root.join("../.ignore"), b"*.c\n").unwrap();
    assert_eq!(scopes(), (vec![String::new()], 3));
}

#[test]
fn direct_skips_each_subdirectory_run() {
    let paths = ["a", "a-b", "a.c", "a/b", "a/b/c", "a/d", "a0", "b"];
    let seek = |k: &str| paths.partition_point(|p| *p < k);
    let at = |dir| -> Vec<&str> {
        let ids = direct(dir, paths.len(), |i| paths[i], seek);
        ids.into_iter().map(|i| paths[i]).collect()
    };
    assert_eq!(at(""), ["a", "a-b", "a.c", "a0", "b"]);
    assert_eq!(at("a"), ["a/b", "a/d"]);
    assert_eq!(at("a/b"), ["a/b/c"]);
    assert!(at("z").is_empty());
}

#[cfg(unix)]
#[test]
fn child_dirs_are_the_recorded_dirs_one_level_down() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, recorded) = recorded_tree(tmp.path(), &["a.c", "c/d.c", "c/e/f.c", "c-x/g.c"]);
    let names = |rel| -> Vec<&str> {
        recorded
            .child_dirs(rel)
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    };
    assert_eq!(names(""), ["c", "c-x"]);
    assert_eq!(names("c"), ["c/e"]);
    assert!(names("c/e").is_empty());
}

#[cfg(unix)]
#[test]
fn a_new_ignore_file_in_a_changed_dir_is_a_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let (root, recorded) = recorded_tree(tmp.path(), &["a.c", "c/d.c", "e/f.c"]);
    std::fs::write(root.join("c/.gitignore"), b"d.c\n").unwrap();
    std::fs::write(root.join("e/g.c"), b"x\n").unwrap();
    let changed = recorded.changes(&root, &|_| false);
    assert_eq!(changed, [root.join("c"), root.join("e")]);
    let got = recorded.ignore_scopes(&root, &changed);
    assert_eq!(
        (got.scopes, got.found, got.moved),
        (vec!["c".to_owned()], vec!["c/.gitignore".to_owned()], 0)
    );
    assert!(
        recorded.ignore_scopes(&root, &[]).scopes.is_empty(),
        "only a changed dir is checked"
    );
}

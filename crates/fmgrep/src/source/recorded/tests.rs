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
    assert!(Recorded::open(dir).unwrap().is_some());
    let mut foreign = good.clone();
    foreign[7] = 2;
    std::fs::write(dir.join(DIRS_STAMPS), foreign).unwrap();
    assert!(Recorded::open(dir).unwrap().is_none(), "another version");
    std::fs::remove_file(dir.join(DIRS_STAMPS)).unwrap();
    assert!(Recorded::open(dir).unwrap().is_none(), "no dirs.stamps");
}

//! AND demotion never changes output, whatever K.

use super::*;

use crate::tests::{RATIO, SHARD_BYTES};

/// Indexed at 16 shard bytes: one shard per file.
const SHARDS: [(&str, &[u8]); 5] = [
    ("s1.c", b"rare zz qwvbnmjkl\n"),
    ("s2.c", b"common rare cdfghiptuy\ncommon\n"),
    ("s3.c", b"zz only QWERTY\n"),
    ("s4.c", b"common and more common XJ 89#%&@\n"),
    ("s5.c", b"\xef\xbb\xbfcommon rare zz 01234567\n"),
];

fn rare_trees() -> Vec<Expr> {
    vec![
        and(vec![t("a"), t("printk")]),
        and(vec![t("tail"), t("a")]),
        or(vec![and(vec![t("a"), t("tail")]), t("beta")]),
        and(vec![t("a"), t("l"), or(vec![t("one"), t("tail")])]),
        and(vec![t("a"), t("e"), t("printk")]),
        and(vec![t("\u{feff}bom"), t("a")]),
        and(vec![t("a"), re("ta[i]l")]),
        and(vec![t("alpha"), re("be+ta"), t("a")]),
        and(vec![t("a"), t("zzz")]),
    ]
}

fn shard_trees() -> Vec<Expr> {
    vec![
        or(vec![and(vec![t("common"), t("rare")]), t("zz")]),
        and(vec![t("common"), t("rare")]),
        and(vec![t("rare"), t("common"), t("e")]),
        or(vec![
            and(vec![t("common"), t("rare"), t("m")]),
            t("zz"),
            and(vec![t("o"), t("QWERTY")]),
        ]),
        and(vec![t("common"), or(vec![t("rare"), t("zz")]), t("co")]),
    ]
}

const MODES: [&[&str]; 9] = [
    &["--json"],
    &["--approx-counts"],
    &["-l"],
    &["-c"],
    &["--count-matches"],
    &["--no-heading"],
    &["--no-heading", "-C", "1"],
    &["-o", "-n"],
    &["--matches", "--json"],
];

fn at<T>(k: u64, f: impl FnOnce() -> T) -> T {
    RATIO.set(Some(k));
    let out = f();
    RATIO.set(None);
    out
}

/// Printed output with wall clocks dropped.
fn out_at(k: u64, index: &Path, flags: &[&str], expr: &Expr) -> Result<String, String> {
    at(k, || printed_expr(index, flags, expr)).map(|s| untimed(flags, &s))
}

fn check(index: &Path, ctx: &str, trees: &[Expr]) {
    for expr in trees {
        for ci in [&[][..], &["-i"]] {
            for mode in MODES {
                let flags = [ci, mode].concat();
                let want = out_at(0, index, &flags, expr);
                for k in [1, 8] {
                    assert_eq!(
                        out_at(k, index, &flags, expr),
                        want,
                        "{ctx} K={k} {expr} {flags:?}"
                    );
                }
            }
        }
    }
}

fn shard_index(tmp: &Path, store_docs: bool) -> PathBuf {
    let folder = stage_corpus(tmp, &SHARDS);
    let index = tmp.join("idx");
    SHARD_BYTES.set(Some(16));
    index_it(&folder, &index, store_docs);
    SHARD_BYTES.set(None);
    index
}

#[test]
fn and_locate_ratio_never_changes_output() {
    for store_docs in LAYOUTS {
        let ctx = format!("store_docs={store_docs}");
        let tmp = tempfile::tempdir().unwrap();
        let index = stage(tmp.path(), store_docs);
        check(&index, &ctx, &[trees(), rare_trees()].concat());
        let tmp = tempfile::tempdir().unwrap();
        check(&shard_index(tmp.path(), store_docs), &ctx, &shard_trees());
    }
}

#[test]
fn shard_fixture_cuts_one_shard_per_file() {
    let tmp = tempfile::tempdir().unwrap();
    let index = shard_index(tmp.path(), false);
    assert_eq!(read_manifest(&index).unwrap().blobs.len(), SHARDS.len());
    // The rare side's per-leaf total includes a member it is not located in.
    let expr = or(vec![and(vec![t("common"), t("rare")]), t("zz")]);
    let r = at(1, || report(&index, &[], &expr)).unwrap();
    let totals: Vec<u64> = r.leaves.iter().map(|l| l.total_matches).collect();
    assert_eq!((r.docs_matched, totals), (4, vec![3, 3, 3]));
}

#[test]
fn demoted_leaf_is_decided_by_reading() {
    let files: [(&str, &[u8]); 3] = [
        ("a.c", b"alpha x printk"),
        ("b.c", b"alpha"),
        ("c.c", b"printk alpha alpha"),
    ];
    let expr = and(vec![t("alpha"), t("x")]);
    let tmp = tempfile::tempdir().unwrap();
    let blind = blind_index(tmp.path(), &files);
    assert_eq!(
        at(0, || printed_expr(&blind, &["-l"], &expr)).unwrap(),
        "a.c\n"
    );
    assert_eq!(
        at(0, || report(&blind, &[], &expr)).unwrap().docs_matched,
        1
    );
    for flags in [&["-l"][..], &[]] {
        let err = at(1, || printed_expr(&blind, flags, &expr)).unwrap_err();
        assert!(err.contains("cannot read"), "{flags:?}: {err}");
    }
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(tmp.path(), &files);
    let index = tmp.path().join("idx");
    index_it(&folder, &index, true);
    check(&index, "readable", &[expr]);
}

#[test]
fn demotion_over_stale_docs_equals_a_fresh_index() {
    let tmp = tempfile::tempdir().unwrap();
    let files: [(&str, &[u8]); 5] = [
        ("a.c", b"printk only\n"),
        ("b.c", b"printk alpha a a a\nbeta\n"),
        ("c.c", b"alpha printk\n"),
        ("d.c", b"alpha beta a\n"),
        ("e.c", b"a a a a a a a x\n"),
    ];
    let folder = stage_corpus(tmp.path(), &files);
    let stale = tmp.path().join("idx");
    index_it(&folder, &stale, false);
    std::fs::write(folder.join("a.c"), b"printk only a\nalpha, beeta x\n").unwrap();
    std::fs::write(folder.join("b.c"), b"printk\n").unwrap();
    std::fs::remove_file(folder.join("c.c")).unwrap();
    std::fs::write(folder.join("e.c"), b"nothing\n").unwrap();
    let fresh = tmp.path().join("fresh");
    index_it(&folder, &fresh, false);
    for expr in [
        and(vec![t("a"), t("printk")]),
        and(vec![t("a"), t("x")]),
        or(vec![and(vec![t("a"), t("x")]), t("beta")]),
        and(vec![t("a"), re("be+ta")]),
    ] {
        for flags in [&[][..], &["-i"]] {
            let (s, f) = (
                at(1, || report(&stale, flags, &expr)).unwrap(),
                at(0, || report(&fresh, flags, &expr)).unwrap(),
            );
            assert_eq!(
                (s.docs_matched, s.total_matches, &s.leaves),
                (f.docs_matched, f.total_matches, &f.leaves),
                "{expr} {flags:?}"
            );
            for mode in [
                &["-l"][..],
                &["-c"],
                &["--count-matches"],
                &["--no-heading"],
            ] {
                let flags = [flags, mode].concat();
                assert_eq!(
                    out_at(1, &stale, &flags, &expr),
                    out_at(0, &fresh, &flags, &expr),
                    "{expr} {flags:?}"
                );
            }
        }
        check(&stale, "stale", std::slice::from_ref(&expr));
    }
}

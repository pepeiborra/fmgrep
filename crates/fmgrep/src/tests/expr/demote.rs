//! AND demotion never changes output, against a plan that demotes nothing.

use super::*;

use crate::tests::{BAND, NO_DEMOTION, SHARD_BYTES};

/// Over 8 times `rare`: an AND of the two demotes `common`, even under an OR.
const COMMON: &[u8] = b"common common common common common common common common common\n\
    common common common common common common common common common common\n\
    common common common common common common common and more common XJ 89#%&@\n";

/// Indexed at 16 shard bytes: one shard per file.
const SHARDS: [(&str, &[u8]); 5] = [
    ("s1.c", b"rare zz qwvbnmjkl\n"),
    ("s2.c", b"common rare cdfghiptuy\ncommon\n"),
    ("s3.c", b"zz only QWERTY\n"),
    ("s4.c", COMMON),
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

#[derive(Clone, Copy, Debug)]
enum Demotion {
    Off,
    Cost { min_us: f64, max_us: f64 },
}

const OFF: Demotion = Demotion::Off;

const fn cost(min_us: f64, max_us: f64) -> Demotion {
    Demotion::Cost { min_us, max_us }
}

/// The cost rule demoting every candidate, locating every one, and sampling
/// once.
const COSTS: [Demotion; 3] = [
    cost(0.0, 0.0),
    cost(f64::INFINITY, f64::INFINITY),
    cost(0.0, f64::INFINITY),
];

fn at<T>(rule: Demotion, f: impl FnOnce() -> T) -> T {
    match rule {
        Demotion::Off => NO_DEMOTION.set(true),
        Demotion::Cost { min_us, max_us } => BAND.set(Some((min_us, max_us))),
    }
    let out = f();
    NO_DEMOTION.set(false);
    BAND.set(None);
    out
}

/// Printed output with wall clocks dropped.
fn out_at(rule: Demotion, index: &Path, flags: &[&str], expr: &Expr) -> Result<String, String> {
    at(rule, || printed_expr(index, flags, expr)).map(|s| untimed(flags, &s))
}

fn check(index: &Path, ctx: &str, trees: &[Expr]) {
    for expr in trees {
        for ci in [&[][..], &["-i"]] {
            for mode in MODES {
                let flags = [ci, mode].concat();
                let want = out_at(OFF, index, &flags, expr);
                for rule in COSTS {
                    assert_eq!(
                        out_at(rule, index, &flags, expr),
                        want,
                        "{ctx} {rule:?} {expr} {flags:?}"
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
fn and_demotion_never_changes_output() {
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
    // `rare` is located only where `common` occurs, yet its total includes
    // s1.c, a member through `zz`.
    let expr = or(vec![and(vec![t("common"), t("rare")]), t("zz")]);
    let r = report(&index, &[], &expr).unwrap();
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
        at(OFF, || printed_expr(&blind, &["-l"], &expr)).unwrap(),
        "a.c\n"
    );
    assert_eq!(
        at(OFF, || report(&blind, &[], &expr)).unwrap().docs_matched,
        1
    );
    for flags in [&["-l"][..], &[]] {
        let err = at(COSTS[0], || printed_expr(&blind, flags, &expr)).unwrap_err();
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
        for (rule, flags) in [COSTS[0], COSTS[2]]
            .into_iter()
            .flat_map(|r| [(r, &[][..]), (r, &["-i"])])
        {
            let (s, f) = (
                at(rule, || report(&stale, flags, &expr)).unwrap(),
                at(OFF, || report(&fresh, flags, &expr)).unwrap(),
            );
            let ctx = format!("{rule:?} {expr} {flags:?}");
            assert_eq!(
                (s.docs_matched, s.total_matches, &s.leaves),
                (f.docs_matched, f.total_matches, &f.leaves),
                "{ctx}"
            );
            for mode in [
                &["-l"][..],
                &["-c"],
                &["--count-matches"],
                &["--no-heading"],
            ] {
                let flags = [flags, mode].concat();
                assert_eq!(
                    out_at(rule, &stale, &flags, &expr),
                    out_at(OFF, &fresh, &flags, &expr),
                    "{ctx} {mode:?}"
                );
            }
        }
        check(&stale, "stale", std::slice::from_ref(&expr));
    }
}

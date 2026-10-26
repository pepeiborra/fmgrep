//! `-g`, `--iglob` and PATHs: a filtered search prints what a search of an
//! index over the kept files alone prints.

use super::*;

pub(crate) const FILES: [(&str, &[u8]); 5] = [
    ("a.c", b"printk one\nalpha\n"),
    ("b.h", b"printk two\n"),
    ("sub/c.c", b"first\nprintk three alpha\n"),
    ("sub/d.h", b"alpha printk\n"),
    ("z.c", b"printk four alpha\n"),
];

const FILTER: [&str; 4] = ["-g", "*.c", "-g", "!z.c"];
const KEPT: [usize; 2] = [0, 2];

/// `files` in `tmp/name`, indexed into its `.fmgrep`; returns the tree.
pub(crate) fn dotted_tree(
    tmp: &Path,
    name: &str,
    files: &[(&str, &[u8])],
    store_docs: bool,
) -> PathBuf {
    let folder = tmp.join(name);
    for (f, bytes) in files {
        let path = folder.join(f);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    index_it(&folder, &folder.join(INDEX_DIR), store_docs);
    std::fs::canonicalize(folder).unwrap()
}

/// [`FILES`] and its [`KEPT`] files alone.
fn trees(tmp: &Path, store_docs: bool) -> (PathBuf, PathBuf) {
    let kept: Vec<_> = KEPT.iter().map(|&i| FILES[i]).collect();
    (
        dotted_tree(tmp, "all", &FILES, store_docs),
        dotted_tree(tmp, "kept", &kept, store_docs),
    )
}

/// `out` without what covers the whole index: rg's summary event, the
/// report's index fields and wall clocks.
fn comparable(flags: &[&str], out: &str) -> String {
    if !flags.contains(&"--json") {
        return out.to_owned();
    }
    let lines: Vec<_> = timeless_lines(out)
        .into_iter()
        .filter(|v| v["type"] != "summary")
        .map(|v| match v.get("queries") {
            Some(q) => q.clone(),
            None => v,
        })
        .collect();
    format!("{lines:?}")
}

fn same(all: &Path, kept: &Path, filter: &[&str], flags: &[&str], terms: &[&str]) {
    let got = printed(all, &[filter, flags].concat(), terms);
    let want = printed(kept, flags, terms);
    assert_eq!(
        comparable(flags, &got),
        comparable(flags, &want),
        "{filter:?} {flags:?} {terms:?}"
    );
}

const MODES: [&[&str]; 10] = [
    &[],
    &["-l"],
    &["-c"],
    &["--count-matches"],
    &["-o"],
    &["-C", "1"],
    &["-i", "--no-heading"],
    &["--json", "--matches"],
    &["--json", "--approx-counts"],
    &["--json"],
];

#[test]
fn every_mode_sees_only_filtered_documents() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let (all, kept) = trees(tmp.path(), store_docs);
        for flags in MODES {
            for terms in [
                &["printk"][..],
                &["/al[a-z]ha/"],
                &["printk", "alpha"],
                &["/^/"],
            ] {
                same(&all, &kept, &FILTER, flags, terms);
            }
        }
        for flags in [
            &["--iglob", "*.C", "--iglob", "!Z.C"][..],
            &["-g", "!*.h", "-g", "!/z.*"],
        ] {
            same(&all, &kept, flags, &["-l"], &["printk"]);
        }
    }
}

#[test]
fn a_regex_without_candidates_reads_only_kept_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (all, _) = trees(tmp.path(), true);
    // z.c, the last document, cut off the shard: reading it fails.
    let index = all.join(INDEX_DIR);
    let manifest = read_manifest(&index).unwrap();
    let offsets = crate::manifest::shard_offsets(&manifest.doc_lens).unwrap();
    let shard = std::fs::OpenOptions::new()
        .write(true)
        .open(index.join(DOCS_SHARD))
        .unwrap();
    shard.set_len(offsets[manifest.files.len() - 1]).unwrap();
    assert_eq!(
        printed(&all, &[&FILTER[..], &["-c"]].concat(), &["/^/"]),
        "a.c:2\nsub/c.c:2\n"
    );
    assert!(query(&all, &["-c"], &["/^/"]).is_err());
}

#[cfg(unix)]
#[test]
fn stale_and_added_files_are_filtered() {
    let tmp = tempfile::tempdir().unwrap();
    let (all, _) = trees(tmp.path(), false);
    std::fs::write(all.join("a.c"), "printk changed\n").unwrap();
    std::fs::write(all.join("b.h"), "printk changed\n").unwrap();
    std::fs::create_dir_all(all.join("new")).unwrap();
    std::fs::write(all.join("new/n.c"), "printk added\n").unwrap();
    std::fs::write(all.join("new/n.h"), "printk added\n").unwrap();
    for flags in [&[][..], &["--regex"]] {
        assert_eq!(
            printed(&all, &[&FILTER[..], flags, &["-l"]].concat(), &["printk"]),
            "a.c\nnew/n.c\nsub/c.c\n",
            "{flags:?}"
        );
    }
    let counts = query(
        &all,
        &[&FILTER[..], &["--approx-counts"]].concat(),
        &["printk"],
    )
    .unwrap();
    assert_eq!(counts.queries[0].total_matches, 3);
}

#[test]
fn expressions_are_filtered() {
    for (store_docs, band) in [(false, None), (true, None), (false, Some((0.0, 0.0)))] {
        crate::tests::BAND.set(band);
        let tmp = tempfile::tempdir().unwrap();
        let (all, kept) = trees(tmp.path(), store_docs);
        for expr in [
            "printk AND alpha",
            "one OR /al[a-z]ha/",
            "printk AND (alpha OR first)",
        ] {
            for flags in [&[][..], &["-l"], &["-c"], &["--json", "--approx-counts"]] {
                same(
                    &all,
                    &kept,
                    &FILTER,
                    &[flags, &["--expr", expr]].concat(),
                    &[],
                );
            }
        }
        same(&all, &kept, &FILTER, &["--and"], &["printk", "alpha"]);
        crate::tests::BAND.set(None);
    }
}

#[test]
fn a_filter_excluding_every_file_is_an_error_without_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let (all, _) = trees(tmp.path(), false);
    let none = "error: no files were searched: the globs exclude every file";
    for (flags, terms) in [
        (&["-g", "*.zz"][..], &["printk"][..]),
        (&["-g", "!*", "--expr", "printk"], &[]),
    ] {
        assert_eq!(query(&all, flags, terms).err().unwrap(), none, "{flags:?}");
    }
    let sub = all.join("sub").display().to_string();
    let r = query(&all, &["-g", "*.zz"], &["printk", &sub]).unwrap();
    assert_eq!(r.queries[0].total_matches, 0);
    let r = query(&all, &["-g", "*.c"], &["missing"]).unwrap();
    assert_eq!(r.queries[0].total_matches, 0);
}

#[test]
fn gone_files_are_not_searched() {
    let tmp = tempfile::tempdir().unwrap();
    let (all, _) = trees(tmp.path(), false);
    std::fs::remove_file(all.join("z.c")).unwrap();
    assert_eq!(
        query(&all, &["-g", "z.c"], &["printk"]).err().unwrap(),
        "error: no files were searched: the globs exclude every file"
    );
}

#[test]
fn absolute_paths_print_as_typed() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let (all, _) = trees(tmp.path(), store_docs);
        let at = |p: &str| all.join(p).display().to_string();
        let lists = |flags: &[&str], paths: &[String]| {
            let terms: Vec<&str> = ["printk"]
                .into_iter()
                .chain(paths.iter().map(String::as_str))
                .collect();
            printed(&all, &[flags, &["-l"]].concat(), &terms)
        };
        let lines = |paths: &[&str]| paths.iter().map(|p| at(p) + "\n").collect::<String>();
        assert_eq!(lists(&[], &[at("sub")]), lines(&["sub/c.c", "sub/d.h"]));
        assert_eq!(
            lists(&["-g", "!*.h"], &[at("sub"), at("b.h")]),
            lines(&["b.h", "sub/c.c"])
        );
        assert_eq!(
            lists(&["-g", "!sub"], &[at("sub")]),
            lines(&["sub/c.c", "sub/d.h"])
        );
        assert_eq!(
            lists(&[], &[at(".")]),
            lines(&["./a.c", "./b.h", "./sub/c.c", "./sub/d.h", "./z.c"])
        );
        let expr = printed(&all, &["-l", "--expr", "alpha", &at("sub")], &[]);
        assert_eq!(expr, lines(&["sub/c.c", "sub/d.h"]));
    }
}

#[test]
fn a_single_file_path_prints_no_file_name() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let (all, _) = trees(tmp.path(), store_docs);
        let c = all.join("sub/c.c").display().to_string();
        let one = |flags: &[&str]| printed(&all, flags, &["printk", &c]);
        assert_eq!(one(&[]), "2:printk three alpha\n");
        assert_eq!(one(&["-c"]), "1\n");
        assert_eq!(one(&["-l"]), format!("{c}\n"));
        let begin = timeless_lines(&one(&["--json", "--matches"]))[0].clone();
        assert_eq!(begin["data"]["path"]["text"], c.as_str());
    }
}

#[test]
fn a_path_outside_the_tree_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (all, kept) = trees(tmp.path(), false);
    let err = query(&all, &[], &["printk", &kept.display().to_string()])
        .err()
        .unwrap();
    assert_eq!(
        err,
        format!(
            "error: {} is outside the indexed tree at {}",
            kept.display(),
            all.display()
        )
    );
}

//! Expectations are `rg --files --no-ignore` over the same tree.

use super::*;

const FILES: [&str; 11] = [
    "!n.txt",
    "#c.txt",
    "H.TXT",
    "a.txt",
    "b.rs",
    "mw/m.txt",
    "sub/c.txt",
    "sub/deep/d.txt",
    "sub/deep/e.rs",
    "sub2/g.txt",
    "x/sub/f.txt",
];

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn table(v: &[&str]) -> FileTable {
    v.iter().collect()
}

/// `(path, name)` pairs.
fn given(paths: &[(&str, &str)]) -> Vec<SearchPath> {
    paths
        .iter()
        .map(|&(path, name)| SearchPath {
            path: path.into(),
            name: name.into(),
        })
        .collect()
}

/// `paths` typed as their root-relative paths, from the root.
fn filter(globs: &[&str], iglobs: &[&str], fold: bool, paths: &[&str]) -> PathFilter {
    let paths: Vec<_> = paths.iter().map(|&p| (p, p)).collect();
    let paths = given(&paths);
    PathFilter::new(&strings(globs), &strings(iglobs), fold, &paths, "/r")
        .unwrap()
        .unwrap()
}

/// `paths` typed in `cwd`.
fn from(cwd: &str, globs: &[&str], paths: &[(&str, &str)]) -> PathFilter {
    PathFilter::new(&strings(globs), &[], false, &given(paths), cwd)
        .unwrap()
        .unwrap()
}

fn kept(f: &PathFilter) -> Vec<&'static str> {
    let mut keep = f.keeper();
    FILES.into_iter().filter(|p| keep(p)).collect()
}

fn globbed(globs: &[&str]) -> Vec<&'static str> {
    kept(&filter(globs, &[], false, &[]))
}

#[test]
fn an_unanchored_glob_matches_the_basename_at_any_depth() {
    assert_eq!(
        globbed(&["*.txt"]),
        [
            "!n.txt",
            "#c.txt",
            "a.txt",
            "mw/m.txt",
            "sub/c.txt",
            "sub/deep/d.txt",
            "sub2/g.txt",
            "x/sub/f.txt"
        ]
    );
}

#[test]
fn a_slash_anchors_the_glob_at_the_root() {
    assert_eq!(globbed(&["/*.txt"]), ["!n.txt", "#c.txt", "a.txt"]);
    assert_eq!(globbed(&["sub/*.txt"]), ["sub/c.txt"]);
}

#[test]
fn the_last_matching_glob_wins() {
    assert_eq!(
        globbed(&["*.txt", "!sub/**"]),
        [
            "!n.txt",
            "#c.txt",
            "a.txt",
            "mw/m.txt",
            "sub2/g.txt",
            "x/sub/f.txt"
        ]
    );
}

#[test]
fn a_whitelist_drops_unmatched_files_but_not_directories() {
    assert!(globbed(&["sub"]).is_empty());
    assert_eq!(globbed(&["*.rs"]), ["b.rs", "sub/deep/e.rs"]);
}

#[test]
fn an_excluded_directory_excludes_everything_below() {
    let rest = [
        "!n.txt",
        "#c.txt",
        "H.TXT",
        "a.txt",
        "b.rs",
        "mw/m.txt",
        "sub2/g.txt",
    ];
    assert_eq!(globbed(&["!sub"]), rest);
    // `sub/deep` matches `sub/**`, so a later `*.txt` cannot reach d.txt.
    assert_eq!(
        globbed(&["!sub/**", "*.txt"]),
        [
            "!n.txt",
            "#c.txt",
            "a.txt",
            "mw/m.txt",
            "sub/c.txt",
            "sub2/g.txt",
            "x/sub/f.txt"
        ]
    );
}

#[test]
fn a_trailing_slash_matches_directories_only() {
    assert_eq!(
        globbed(&["!sub/"]),
        [
            "!n.txt",
            "#c.txt",
            "H.TXT",
            "a.txt",
            "b.rs",
            "mw/m.txt",
            "sub2/g.txt"
        ]
    );
    assert!(globbed(&["sub/"]).is_empty());
    assert_eq!(globbed(&["!*.rs/"]), FILES);
}

#[test]
fn double_star_forms_follow_gitignore() {
    assert_eq!(
        globbed(&["sub/**"]),
        ["sub/c.txt", "sub/deep/d.txt", "sub/deep/e.rs"]
    );
    assert_eq!(globbed(&["**/deep/*"]), ["sub/deep/d.txt", "sub/deep/e.rs"]);
    assert_eq!(globbed(&["**"]), FILES);
    assert_eq!(globbed(&["sub/**/d.txt"]), ["sub/deep/d.txt"]);
}

#[test]
fn iglobs_follow_globs_whatever_the_argv_order() {
    let f = filter(&["!*.txt"], &["*.TXT"], false, &[]);
    assert_eq!(
        kept(&f),
        [
            "!n.txt",
            "#c.txt",
            "H.TXT",
            "a.txt",
            "mw/m.txt",
            "sub/c.txt",
            "sub/deep/d.txt",
            "sub2/g.txt",
            "x/sub/f.txt"
        ]
    );
    assert_eq!(kept(&filter(&[], &["h.txt"], false, &[])), ["H.TXT"]);
}

#[test]
fn glob_case_insensitive_folds_every_glob() {
    assert_eq!(
        kept(&filter(&["SUB/**"], &[], true, &[])),
        ["sub/c.txt", "sub/deep/d.txt", "sub/deep/e.rs"]
    );
    assert!(kept(&filter(&["SUB/**"], &[], false, &[])).is_empty());
}

#[test]
fn comments_and_blank_globs_are_ignored() {
    assert_eq!(globbed(&["#c.txt", "*.rs"]), ["b.rs", "sub/deep/e.rs"]);
    assert_eq!(globbed(&["", " ", "*.rs"]), ["b.rs", "sub/deep/e.rs"]);
    assert_eq!(globbed(&["\\#c.txt"]), ["#c.txt"]);
    assert_eq!(globbed(&["\\!n.txt"]), ["!n.txt"]);
    assert!(
        PathFilter::new(&strings(&["#x", ""]), &[], false, &[], "")
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_invalid_glob_is_refused_with_its_text() {
    let err = PathFilter::new(&strings(&["["]), &[], false, &[], "")
        .err()
        .unwrap();
    assert_eq!(
        err,
        "error: invalid glob '[': unclosed character class; missing ']'"
    );
}

#[test]
fn paths_limit_the_search_to_their_subtrees() {
    assert_eq!(
        kept(&filter(&[], &[], false, &["sub", "x"])),
        [
            "sub/c.txt",
            "sub/deep/d.txt",
            "sub/deep/e.rs",
            "x/sub/f.txt"
        ]
    );
    assert_eq!(
        kept(&filter(&["*.rs"], &[], false, &["sub/deep/"])),
        ["sub/deep/e.rs"]
    );
    assert_eq!(kept(&filter(&[], &[], false, &[""])), FILES);
}

#[test]
fn a_file_path_is_searched_whatever_the_globs() {
    let f = filter(&["!*.txt"], &[], false, &["a.txt", "sub/c.txt"]);
    assert_eq!(kept(&f), ["a.txt", "sub/c.txt"]);
}

#[test]
fn directories_above_a_path_are_not_filtered() {
    let f = filter(&["!sub"], &[], false, &["sub"]);
    assert_eq!(kept(&f), ["sub/c.txt", "sub/deep/d.txt", "sub/deep/e.rs"]);
    assert_eq!(
        kept(&filter(&["!deep"], &[], false, &["sub"])),
        ["sub/c.txt"]
    );
}

// rg 15.2.0 run in /r/sub, the tree at /r.

#[test]
fn globs_see_the_path_as_printed_below_the_cwd() {
    let dot = [("sub", ".")];
    assert_eq!(
        kept(&from("/r/sub", &["deep/*"], &dot)),
        ["sub/deep/d.txt", "sub/deep/e.rs"]
    );
    assert!(kept(&from("/r/sub", &["sub/**"], &dot)).is_empty());
    assert_eq!(kept(&from("/r/sub", &["/c.txt"], &dot)), ["sub/c.txt"]);
    let abs = [("sub", "/r/sub")];
    assert_eq!(kept(&from("/r/sub", &["/c.txt"], &abs)), ["sub/c.txt"]);
}

#[test]
fn globs_see_a_path_above_the_cwd_through_dot_dot() {
    let up = [("", "..")];
    assert!(kept(&from("/r/sub", &["sub/**"], &up)).is_empty());
    assert_eq!(
        kept(&from("/r/sub", &["*.rs"], &up)),
        ["b.rs", "sub/deep/e.rs"]
    );
    assert_eq!(
        kept(&from("/r/sub", &["!sub"], &up)),
        [
            "!n.txt",
            "#c.txt",
            "H.TXT",
            "a.txt",
            "b.rs",
            "mw/m.txt",
            "sub2/g.txt"
        ]
    );
}

#[test]
fn the_cwd_is_stripped_from_an_absolute_path_as_bytes() {
    let abs = [("", "/r")];
    assert_eq!(kept(&from("/r/sub", &["/c.txt"], &abs)), ["sub/c.txt"]);
    assert!(kept(&from("/r/sub", &["/a.txt"], &abs)).is_empty());
    assert_eq!(kept(&from("/r/sub", &["/2/*"], &abs)), ["sub2/g.txt"]);
    assert!(kept(&from("/r/sub", &["/r/a.txt"], &abs)).is_empty());
    assert_eq!(kept(&from("/r/sub", &["**/r/a.txt"], &abs)), ["a.txt"]);
}

#[test]
fn files_print_under_their_path_as_typed() {
    let print =
        |paths: &[(&str, &str)], path: &str| from("/r", &[], paths).printed(path).into_owned();
    let none = PathFilter::new(&strings(&["*"]), &[], false, &[], "").unwrap();
    assert_eq!(none.unwrap().printed("sub/c.txt"), "sub/c.txt");
    assert_eq!(print(&[("sub", ".")], "sub/deep/d.txt"), "./deep/d.txt");
    assert_eq!(print(&[("sub", "./")], "sub/c.txt"), "./c.txt");
    assert_eq!(print(&[("sub", "sub/")], "sub/c.txt"), "sub/c.txt");
    assert_eq!(print(&[("sub", "sub//")], "sub/c.txt"), "sub//c.txt");
    assert_eq!(print(&[("", "..")], "a.txt"), "../a.txt");
    assert_eq!(print(&[("a.txt", "../a.txt")], "a.txt"), "../a.txt");
    assert_eq!(print(&[("", "/r/.")], "b.rs"), "/r/./b.rs");
    assert_eq!(
        print(&[("sub/deep", "deep"), ("sub", ".")], "sub/deep/e.rs"),
        "deep/e.rs"
    );
}

#[test]
fn files_print_under_the_first_path_that_keeps_them() {
    let both = [("", ".."), ("sub", ".")];
    let print =
        |globs: &[&str], path: &str| from("/r/sub", globs, &both).printed(path).into_owned();
    assert_eq!(print(&[], "sub/c.txt"), "../sub/c.txt");
    assert_eq!(print(&["!sub"], "sub/c.txt"), "./c.txt");
    assert_eq!(print(&["!../sub/deep"], "sub/deep/d.txt"), "./deep/d.txt");
    let file = [("", ".."), ("sub/c.txt", "c.txt")];
    let f = from("/r/sub", &["!*.txt"], &file);
    assert_eq!(f.printed("sub/c.txt"), "c.txt");
}

/// `path`'s names under `paths`, typed from the root, with `globs`.
fn names(globs: &[&str], paths: &[(&str, &str)], path: &str) -> Vec<String> {
    let f = from("", globs, paths);
    f.names(path).into_iter().map(Cow::into_owned).collect()
}

#[test]
fn a_file_prints_once_under_each_path_that_keeps_it() {
    let all = |paths: &[&str], path: &str| {
        let paths: Vec<_> = paths.iter().map(|p| (*p, *p)).collect();
        names(&[], &paths, path)
    };
    // A file and the directory over it, in PATH order.
    assert_eq!(
        all(&["sub/c.txt", "sub"], "sub/c.txt"),
        ["sub/c.txt", "sub/c.txt"]
    );
    assert_eq!(
        names(&[], &[("sub", "sub"), ("sub/c.txt", "c")], "sub/c.txt"),
        ["sub/c.txt", "c"]
    );
    // A directory twice, and a directory and one inside it.
    assert_eq!(
        all(&["sub", "sub"], "sub/c.txt"),
        ["sub/c.txt", "sub/c.txt"]
    );
    assert_eq!(
        names(&[], &[("sub", "sub"), ("sub/deep", "d")], "sub/deep/d.txt"),
        ["sub/deep/d.txt", "d/d.txt"]
    );
    // The root and a directory, in either order.
    assert_eq!(
        names(&[], &[("", "."), ("sub", "sub")], "sub/c.txt"),
        ["./sub/c.txt", "sub/c.txt"]
    );
    assert_eq!(
        names(&[], &[("sub", "sub"), ("", ".")], "sub/c.txt"),
        ["sub/c.txt", "./sub/c.txt"]
    );
    // Two spellings of one directory, and an absolute path with a relative one.
    assert_eq!(
        names(&[], &[("sub", "sub"), ("sub/", "sub/")], "sub/c.txt"),
        ["sub/c.txt", "sub/c.txt"]
    );
    assert_eq!(
        names(&[], &[("sub", "sub"), ("sub", "/r/sub")], "sub/c.txt"),
        ["sub/c.txt", "/r/sub/c.txt"]
    );
}

#[test]
fn a_glob_drops_a_file_from_the_paths_that_walk_it_only() {
    let both = [("sub/c.txt", "sub/c.txt"), ("sub", "sub")];
    // The file PATH keeps it whatever the globs; the walk of `sub` drops it.
    assert_eq!(names(&["!c.txt"], &both, "sub/c.txt"), ["sub/c.txt"]);
    assert_eq!(names(&["!*.txt"], &both, "sub/c.txt"), ["sub/c.txt"]);
    // A directory excluded for one PATH only: the other holds the file.
    let nested = [("sub", "sub"), ("sub/deep", "sub/deep")];
    assert_eq!(
        names(&["!sub/deep"], &nested, "sub/deep/d.txt"),
        ["sub/deep/d.txt"]
    );
    assert_eq!(
        names(&["!deep"], &nested, "sub/deep/d.txt"),
        ["sub/deep/d.txt"]
    );
    assert_eq!(names(&["!deep"], &nested, "sub/c.txt"), ["sub/c.txt"]);
}

#[test]
fn without_overlap_a_file_has_the_one_printed_name() {
    let apart = [("sub", "sub"), ("sub2", "sub2"), ("a.txt", "a.txt")];
    let f = from("", &[], &apart);
    for path in ["sub/c.txt", "sub2/g.txt", "a.txt", "b.rs"] {
        assert_eq!(f.names(path), [f.printed(path)], "{path}");
        assert_eq!(f.copies(path), 1, "{path}");
    }
    let f = from("", &[], &[("sub", "sub"), ("sub/deep", "d")]);
    assert_eq!(f.copies("sub/deep/e.rs"), 2);
    assert_eq!(f.copies("sub/c.txt"), 1);
}

#[test]
fn the_cwd_of_a_search_without_paths_is_the_default_path() {
    let cwd = |globs: &[&str]| from("/r/sub", globs, &[("sub", "")]);
    assert_eq!(
        kept(&cwd(&[])),
        ["sub/c.txt", "sub/deep/d.txt", "sub/deep/e.rs"]
    );
    assert_eq!(cwd(&[]).printed("sub/deep/d.txt"), "deep/d.txt");
    assert_eq!(kept(&cwd(&["/c.txt"])), ["sub/c.txt"]);
    assert_eq!(kept(&cwd(&["deep/*"])), ["sub/deep/d.txt", "sub/deep/e.rs"]);
    assert!(kept(&cwd(&["sub/**"])).is_empty());
    assert!(!cwd(&[]).globs() && cwd(&["*"]).globs());
}

fn cover(globs: &[&str], fold: bool, paths: &[&str], files: &[&str]) -> Option<Vec<Range<usize>>> {
    filter(globs, &[], fold, paths)
        .cover(&table(files))
        .unwrap()
}

const ORDERED: [&str; 8] = [
    "a.txt", "p", "p-x/a", "p.x", "p/a", "p/b/c.rs", "p/c.rs", "q/p/a",
];

#[test]
fn prefix_globs_cover_only_their_subtrees() {
    let at = |globs: &[&str], paths: &[&str]| -> Vec<(usize, usize)> {
        let cover = cover(globs, false, paths, &ORDERED).unwrap();
        cover.into_iter().map(|r| (r.start, r.end)).collect()
    };
    assert_eq!(at(&["p/**"], &[]), [(4, 7)]);
    assert_eq!(at(&["/p/*.rs"], &[]), [(4, 7)]);
    assert_eq!(at(&["p/c*", "/a*"], &[]), [(0, 1), (6, 7)]);
    assert_eq!(at(&["p\\-x/*"], &[]), [(2, 3)]);
    assert_eq!(at(&["p/{a,c.rs}"], &[]), [(4, 7)]);
    assert_eq!(at(&["p/**", "!p/b/**"], &[]), [(4, 7)]);
    assert_eq!(at(&[], &["p"]), [(1, 2), (4, 7)]);
    assert_eq!(at(&[], &["p/c.rs", "a.txt"]), [(0, 1), (6, 7)]);
    assert_eq!(at(&["p/b/**"], &["p"]), [(1, 2), (5, 6)]);
    assert_eq!(at(&["/p*"], &["p/b"]), [(5, 6)]);
    assert_eq!(at(&["q/**"], &["p"]), [(1, 2)]);
    assert!(at(&["z/**"], &[]).is_empty());
}

#[test]
fn unprefixed_folded_or_exclude_only_filters_are_unbounded() {
    for globs in [
        &["*.rs"][..],
        &["**/p"],
        &["p/"],
        &["!p/**"],
        &["p/**", "*.txt"],
    ] {
        assert_eq!(cover(globs, false, &[], &ORDERED), None, "{globs:?}");
    }
    assert_eq!(cover(&["p/**"], true, &[], &ORDERED), None);
    assert_eq!(cover(&["*.rs"], false, &["", "p"], &ORDERED), None);
    let iglob = PathFilter::new(&[], &strings(&["p/**"]), false, &[], "");
    assert_eq!(iglob.unwrap().unwrap().cover(&table(&ORDERED)), Ok(None));
}

#[test]
fn the_cover_holds_every_kept_file() {
    const PARTS: [&str; 7] = ["a", "a-b", "a.b", "b", "GHC", "x.rs", "y.txt"];
    let mut seed = 0x9e37_79b9_u32;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as usize % n
    };
    let mut files: Vec<String> = (0..400)
        .map(|_| {
            let depth = 1 + next(4);
            let parts: Vec<&str> = (0..depth).map(|_| PARTS[next(PARTS.len())]).collect();
            parts.join("/")
        })
        .collect();
    files.sort();
    files.dedup();
    let files: FileTable = files.iter().collect();
    let sets: [&[&str]; 17] = [
        &["a/**"],
        &["/a*"],
        &["a/b/*.rs"],
        &["a.b/**", "!**/x.rs"],
        &["a-b/**", "b/GHC/*"],
        &["a/**", "!a/b/"],
        &["GHC/**/*.txt"],
        &["b/{a,b}/**"],
        &["a/[ab]*"],
        &["b/?/x.rs"],
        &["*.rs"],
        &["!b/**"],
        &["GHC/**"],
        &["../b/**"],
        &["/x.rs"],
        &[".b/**"],
        &["b/**", "a/a*"],
    ];
    let roots: [(&str, &[(&str, &str)]); 13] = [
        ("/r", &[]),
        ("/r/a", &[("a", "")]),
        ("/r", &[("a", "a")]),
        ("/r", &[("a/b", "a/b"), ("b", "./b")]),
        ("/r", &[("a/x.rs", "a/x.rs"), ("", ".")]),
        ("/r/a", &[("a", ".")]),
        ("/r/a", &[("", "..")]),
        ("/r/a", &[("b", "../b"), ("a/b", "b/")]),
        ("/r/a", &[("a", "/r/a")]),
        ("/r/a", &[("", "/r")]),
        ("/r/a", &[("a/GHC", "/r/a/GHC"), ("a.b", "/r/a.b")]),
        ("/r/a/b", &[("a", "..")]),
        ("/r/a/b", &[("", "/r")]),
    ];
    for globs in sets {
        for (cwd, paths) in roots {
            let f = PathFilter::new(&strings(globs), &[], false, &given(paths), cwd);
            let f = f.unwrap().unwrap();
            let mut keep = f.keeper();
            let ctx = format!("{globs:?} {cwd} {paths:?}");
            let Some(cover) = f.cover(&files).unwrap() else {
                continue;
            };
            for (id, path) in files.checked().unwrap().iter().enumerate() {
                if keep(path) {
                    assert!(cover.iter().any(|r| r.contains(&id)), "{ctx}: {path}");
                }
            }
        }
    }
}

#[test]
fn a_cover_follows_the_path_the_globs_see() {
    let files = table(&FILES);
    let at = |cwd: &str, globs: &[&str], paths: &[(&str, &str)]| {
        let cover = from(cwd, globs, paths).cover(&files).unwrap();
        cover.map(|c| c.into_iter().map(|r| (r.start, r.end)).collect::<Vec<_>>())
    };
    assert_eq!(
        at("/r/sub", &["deep/*"], &[("sub", ".")]),
        Some(vec![(7, 9)])
    );
    assert_eq!(
        at("/r/sub", &["deep/*"], &[("sub", "/r/sub")]),
        Some(vec![(7, 9)])
    );
    assert_eq!(at("/r/sub", &["sub/**"], &[("sub", ".")]), Some(vec![]));
    assert_eq!(
        at("/r/sub", &["../sub2/*"], &[("", "..")]),
        Some(vec![(9, 10)])
    );
    assert_eq!(at("/r/sub", &["/2/*"], &[("", "/r")]), None);
    assert_eq!(
        at("/r/sub", &["deep/*"], &[("sub", "")]),
        Some(vec![(7, 9)])
    );
    assert_eq!(at("/r/sub", &[], &[("sub", "")]), Some(vec![(6, 9)]));
}

/// A path that cannot be read stops the filter that reads it, in the
/// manifest's words: it is never counted as excluded.
#[test]
fn a_damaged_path_stops_the_filter() {
    // "a.txt", "sub/\xFF.txt", "sub/c.txt".
    let files = crate::manifest::damaged(&[5, 14, 23], b"a.txtsub/\xFF.txtsub/c.txt");
    let why = "error: idx/manifest.bin holds a path that is not UTF-8; re-run `index`";
    let paths = crate::fetch::Paths::of(&files);
    let txt = filter(&["*.txt"], &[], false, &[]);
    // Each binary search of the cover reads path 1 first.
    let sub = filter(&[], &[], false, &["sub"]);
    assert_eq!(sub.cover(&files), Err(why.to_owned()));
    let sub = filter(&["sub/**"], &[], false, &[]);
    assert_eq!(sub.cover(&files), Err(why.to_owned()));
    let mut ids = vec![0, 1, 2];
    assert_eq!(txt.retain(&paths, &mut ids, |&id| id), Err(why.to_owned()));
    assert_eq!(ids, [0, 1, 2], "nothing dropped");
    let mut ids = vec![0, 2];
    assert_eq!(txt.retain(&paths, &mut ids, |&id| id), Ok(()));
    assert_eq!(ids, [0, 2]);
    let mut failed = None;
    let kept: Vec<usize> = txt.kept(&paths, 0..3, &mut failed).collect();
    assert_eq!((kept, failed.as_deref()), (vec![0], Some(why)));
    let rs = filter(&["*.rs"], &[], false, &[]);
    assert_eq!(rs.excludes_all(&paths, |_| false), Err(why.to_owned()));
}

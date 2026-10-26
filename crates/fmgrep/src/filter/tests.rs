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

fn filter(globs: &[&str], iglobs: &[&str], fold: bool, paths: &[&str]) -> PathFilter {
    PathFilter::new(&strings(globs), &strings(iglobs), fold, &strings(paths))
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
        PathFilter::new(&strings(&["#x", ""]), &[], false, &[])
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_invalid_glob_is_refused_with_its_text() {
    let err = PathFilter::new(&strings(&["["]), &[], false, &[])
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

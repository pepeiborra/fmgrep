//! Which commit a `--store-docs` index records as served, on real git
//! repositories.

use crate::git::{Sandbox, commit_of};

const FILES: [(&str, &str); 3] = [
    ("a.c", "printk a\n"),
    ("dir/b.c", "printk b\n"),
    ("dir/c.h", "printk c\n"),
];

/// The one `note:` line of an index build's stderr.
fn note(stderr: &str) -> String {
    let notes: Vec<&str> = stderr.lines().filter(|l| l.starts_with("note:")).collect();
    assert_eq!(notes.len(), 1, "{stderr}");
    notes[0].to_owned()
}

#[test]
fn a_clean_checkout_records_its_head() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let (index, stderr) = s.index(&repo, "idx");
    let head = s.head(&repo);
    assert_eq!(
        note(&stderr),
        format!("note: the index holds commit {head}")
    );
    assert_eq!(commit_of(&index), Some(head));
}

/// A change made to a checkout.
type Change = fn(&Sandbox, &std::path::Path);

#[test]
fn a_changed_checkout_records_no_commit() {
    let s = Sandbox::new();
    let cases: [(&str, Change, &str); 4] = [
        (
            "edited",
            |s, repo| s.write(repo, &[("a.c", "printk edited\n")]),
            "a.c differs from HEAD",
        ),
        (
            "untracked",
            |s, repo| s.write(repo, &[("new.c", "printk new\n")]),
            "new.c is not tracked by git",
        ),
        (
            "staged",
            |s, repo| {
                s.write(repo, &[("a.c", "printk staged\n")]);
                s.git(repo, &["add", "a.c"]);
            },
            "a.c differs from HEAD",
        ),
        (
            "gitlink",
            |s, repo| {
                let head = s.head(repo);
                let info = format!("160000,{head},sub");
                s.git(repo, &["update-index", "--add", "--cacheinfo", &info]);
                s.git(repo, &["commit", "-q", "-m", "sub"]);
                // A submodule not checked out: an empty directory.
                std::fs::create_dir(repo.join("sub")).unwrap();
            },
            "the checkout holds a submodule (sub)",
        ),
    ];
    for (name, change, why) in cases {
        let repo = s.repo(name, &FILES);
        change(&s, &repo);
        let (index, stderr) = s.index(&repo, &format!("idx-{name}"));
        let want = format!("note: the index records no served commit ({why})");
        assert_eq!(note(&stderr), want, "{name}");
        assert_eq!(commit_of(&index), None, "{name}");
    }
}

#[test]
fn only_the_top_of_a_checkout_records_a_commit() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let (index, stderr) = s.index(&repo.join("dir"), "idx-dir");
    let want = format!(
        "note: the index records no served commit ({} is not the top of its git checkout, {})",
        repo.join("dir").display(),
        repo.display()
    );
    assert_eq!(note(&stderr), want);
    assert_eq!(commit_of(&index), None);

    let plain = s.path("plain");
    s.write(&plain, &FILES);
    let (index, stderr) = s.index(&plain, "idx-plain");
    let plain = std::fs::canonicalize(plain).unwrap();
    let want = format!(
        "note: the index records no served commit ({} is in no git checkout)",
        plain.display()
    );
    assert_eq!(note(&stderr), want);
    assert_eq!(commit_of(&index), None);

    let unborn = s.path("unborn");
    s.write(&unborn, &FILES);
    s.git(&unborn, &["init", "-q", "-b", "main"]);
    let (_, stderr) = s.index(&unborn, "idx-unborn");
    let want = "note: the index records no served commit (the checkout has no commit yet)";
    assert_eq!(note(&stderr), want);
}

/// A file that git converts on checkout differs from its blob, though git
/// status shows no change: its stored text decides.
#[test]
fn a_converted_file_records_no_commit() {
    let s = Sandbox::new();
    let set: [(&str, &[&[&str]]); 2] = [
        ("crlf", &[&["config", "core.autocrlf", "true"]]),
        (
            "filter",
            &[
                &["config", "filter.up.smudge", "tr a-z A-Z"],
                &["config", "filter.up.clean", "tr A-Z a-z"],
            ],
        ),
    ];
    for (name, config) in set {
        let repo = s.repo(name, &FILES);
        s.write(&repo, &[(".gitattributes", "*.c filter=up\n")]);
        s.commit(&repo, "attributes");
        for args in config {
            s.git(&repo, args);
        }
        std::fs::remove_file(repo.join("a.c")).unwrap();
        s.git(&repo, &["checkout", "--", "a.c"]);
        assert_eq!(s.git(&repo, &["status", "--porcelain"]), "", "{name}");
        let (index, stderr) = s.index(&repo, &format!("idx-{name}"));
        let want = "note: the index records no served commit (a.c differs from its blob in HEAD)";
        assert_eq!(note(&stderr), want, "{name}");
        assert_eq!(commit_of(&index), None, "{name}");
    }
}

/// An index that reads the tree records no commit and says nothing of one.
#[test]
fn a_plain_index_records_no_commit() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let output = s.path("idx").display().to_string();
    let out = s.run(&repo, &["index", ".", "--output", &output]);
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("note:"));
    assert_eq!(commit_of(&s.path("idx")), None);
}

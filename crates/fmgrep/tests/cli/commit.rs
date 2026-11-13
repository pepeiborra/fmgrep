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

/// An ignore rule that a checkout of the commit would not have: an
/// untracked ignore file in the tree, or a pattern above it, in the global
/// excludes file, or in `info/exclude` (of a linked worktree too). Comments
/// alone are no rule.
#[test]
fn an_ignore_rule_from_outside_records_no_commit() {
    let s = Sandbox::new();
    let no = |repo: &std::path::Path, name: &str, why: String| {
        let (index, stderr) = s.index(repo, &format!("idx-{name}"));
        let want = format!("note: the index records no served commit ({why})");
        assert_eq!(note(&stderr), want, "{name}");
        assert_eq!(commit_of(&index), None, "{name}");
    };
    let outside = |path: &std::path::Path| {
        format!(
            "the ignore file {} outside the checkout has a pattern",
            path.display()
        )
    };

    let repo = s.repo("untracked", &FILES);
    s.write(&repo, &[("dir/.ignore", "*.h\n")]);
    no(
        &repo,
        "untracked",
        "the ignore file dir/.ignore is not tracked by git".into(),
    );

    let repo = s.repo("parent/repo", &FILES);
    let parent = std::fs::canonicalize(s.path("parent")).unwrap();
    s.write(&parent, &[(".ignore", "# a comment\n\n")]);
    let (index, _) = s.index(&repo, "idx-comment");
    assert_eq!(commit_of(&index), Some(s.head(&repo)));
    s.write(&parent, &[(".ignore", "*.h\n")]);
    no(&repo, "parent", outside(&repo.join("../.ignore")));

    let repo = s.repo("exclude", &FILES);
    s.write(&repo, &[(".git/info/exclude", "*.h\n")]);
    no(&repo, "exclude", outside(&repo.join(".git/info/exclude")));

    let repo = s.repo("main", &FILES);
    let tree = s.path("linked");
    s.git(
        &repo,
        &["worktree", "add", "-q", &tree.display().to_string()],
    );
    let tree = std::fs::canonicalize(tree).unwrap();
    let (index, _) = s.index(&tree, "idx-linked-clean");
    assert_eq!(commit_of(&index), Some(s.head(&repo)));
    s.write(&repo, &[(".git/info/exclude", "*.h\n")]);
    no(&tree, "linked", outside(&repo.join(".git/info/exclude")));

    let repo = s.repo("global", &FILES);
    s.write(&s.path("config"), &[("git/ignore", "*.h\n")]);
    no(&repo, "global", outside(&s.path("config/git/ignore")));
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

/// `status` reports the served commit of a `--store-docs` index, and none
/// for an index that reads the tree.
#[test]
fn status_reports_the_served_commit() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let (index, _) = s.index(&repo, "idx");
    let (_server, addr) = s.serve(&index, &[]);
    let status = s.status(&addr);
    assert_eq!(status["commit"]["vcs"], "git");
    assert_eq!(status["commit"]["id"], s.head(&repo));
    let plain = s.path("plain").display().to_string();
    let out = s.run(&repo, &["index", ".", "--output", &plain]);
    assert!(out.status.success());
    let (_server, addr) = s.serve(&s.path("plain"), &["--no-watch"]);
    assert!(s.status(&addr).get("commit").is_none());
}

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
    let cases: [(&str, Change, &str); 7] = [
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
        // Files that git status does not see, absent from the tree.
        (
            "sparse",
            |s, repo| {
                s.git(repo, &["sparse-checkout", "set", "--no-cone", "/a.c"]);
            },
            "git hides changes to 2 files (assume-unchanged or skip-worktree)",
        ),
        (
            "skip-worktree",
            |s, repo| {
                s.git(repo, &["update-index", "--skip-worktree", "dir/b.c"]);
                std::fs::remove_file(repo.join("dir/b.c")).unwrap();
            },
            "git hides changes to 1 file (assume-unchanged or skip-worktree)",
        ),
        (
            "assume-unchanged",
            |s, repo| {
                s.git(repo, &["update-index", "--assume-unchanged", "dir/b.c"]);
                std::fs::remove_file(repo.join("dir/b.c")).unwrap();
            },
            "git hides changes to 1 file (assume-unchanged or skip-worktree)",
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

/// The build time that `index` reports counts the check of the served
/// commit: with a git that waits a second before it reads the blobs, it
/// is a second or more.
#[test]
fn the_build_time_counts_the_check_of_the_commit() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let path = wrapped_git(&s, "[ \"$3\" = cat-file ] && sleep 1\n");
    let output = s.path("idx").display().to_string();
    let out = s
        .fmgrep(&repo)
        .env("PATH", path)
        .args(["index", "--store-docs", "--json", ".", "--output", &output])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let summary: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let us = summary["build_time_us"].as_u64().unwrap();
    assert!(us >= 1_000_000, "{us}");
    assert_eq!(commit_of(&s.path("idx")), Some(s.head(&repo)));
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

const REQUIRE: &str = "--require-served-commit";

/// `index --store-docs` of `dir` into the sandbox's `name`, with `flags`,
/// and with `PATH` set to `path` if given: its exit code and stderr.
fn build(
    s: &Sandbox,
    dir: &std::path::Path,
    name: &str,
    flags: &[&str],
    path: Option<&str>,
) -> (Option<i32>, String) {
    let output = s.path(name).display().to_string();
    let mut cmd = s.fmgrep(dir);
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    cmd.args(["index", "--store-docs", ".", "--output", &output]);
    let out = cmd.args(flags).output().unwrap();
    (out.status.code(), String::from_utf8(out.stderr).unwrap())
}

/// The reason why the index of `dir` records no served commit, as the
/// note of a build without [`REQUIRE`] gives it. With [`REQUIRE`], the
/// build fails with the same reason in an error, exits 2 and leaves no
/// index. Where `early`, it fails before the walk reads a file: an
/// unreadable file in the tree, which stops any walk, changes nothing,
/// and the error is all it prints.
fn required(
    s: &Sandbox,
    dir: &std::path::Path,
    name: &str,
    path: Option<&str>,
    early: bool,
) -> String {
    // The state of a wrapped git.
    let reset = || {
        let _ = std::fs::remove_file(s.path("seen"));
    };
    reset();
    let (code, stderr) = build(s, dir, &format!("idx-{name}"), &[], path);
    assert_eq!(code, Some(0), "{name}: {stderr}");
    let note = note(&stderr);
    let why = note
        .strip_prefix("note: the index records no served commit (")
        .and_then(|w| w.strip_suffix(')'))
        .unwrap_or_else(|| panic!("{name}: {note}"))
        .to_owned();
    let error = format!("error: the index cannot record its served commit: {why}\n");
    let mut runs = vec![format!("req-{name}")];
    if early {
        let unread = dir.join("unread.c");
        std::fs::write(&unread, "printk unread\n").unwrap();
        let mode = std::os::unix::fs::PermissionsExt::from_mode(0o000);
        std::fs::set_permissions(&unread, mode).unwrap();
        runs.push(format!("req-unread-{name}"));
    }
    for run in runs {
        reset();
        let (code, stderr) = build(s, dir, &run, &[REQUIRE], path);
        assert_eq!(code, Some(2), "{run}: {stderr}");
        match early {
            true => assert_eq!(stderr, error, "{run}"),
            false => {
                assert!(stderr.ends_with(&error), "{run}: {stderr}");
                assert!(!stderr.contains("note:"), "{run}: {stderr}");
            }
        }
        assert!(!s.path(&run).exists(), "{run}");
    }
    why
}

/// Every reason that git alone can tell, before the walk: with
/// `--require-served-commit` the build fails on it before it reads a file.
#[test]
fn a_required_commit_fails_before_the_walk() {
    let s = Sandbox::new();
    let outside = |path: &std::path::Path| {
        format!(
            "the ignore file {} outside the checkout has a pattern",
            path.display()
        )
    };

    let plain = s.path("plain");
    s.write(&plain, &FILES);
    let plain = std::fs::canonicalize(plain).unwrap();
    let why = format!("{} is in no git checkout", plain.display());
    assert_eq!(required(&s, &plain, "plain", None, true), why);

    let repo = s.repo("nogit", &FILES);
    std::fs::create_dir(s.path("empty")).unwrap();
    let empty = s.path("empty").display().to_string();
    let enoent = std::io::Error::from_raw_os_error(2);
    let why = format!("cannot run git ({enoent})");
    assert_eq!(required(&s, &repo, "nogit", Some(&empty), true), why);

    let repo = s.repo("sub", &FILES);
    let dir = repo.join("dir");
    let why = format!(
        "{} is not the top of its git checkout, {}",
        dir.display(),
        repo.display()
    );
    assert_eq!(required(&s, &dir, "dir", None, true), why);

    let unborn = s.path("unborn");
    s.write(&unborn, &FILES);
    s.git(&unborn, &["init", "-q", "-b", "main"]);
    let why = "the checkout has no commit yet";
    assert_eq!(required(&s, &unborn, "unborn", None, true), why);

    let repo = s.repo("edited", &FILES);
    s.write(&repo, &[("a.c", "printk edited\n")]);
    let why = "a.c differs from HEAD";
    assert_eq!(required(&s, &repo, "edited", None, true), why);

    let repo = s.repo("nohead", &FILES);
    let path = wrapped_git(&s, "[ \"$3\" = status ] && exit 0\n");
    let why = "git status gave no HEAD";
    assert_eq!(required(&s, &repo, "nohead", Some(&path), true), why);

    let repo = s.repo("sparse", &FILES);
    s.git(&repo, &["sparse-checkout", "set", "--no-cone", "/a.c"]);
    let why = "git hides changes to 2 files (assume-unchanged or skip-worktree)";
    assert_eq!(required(&s, &repo, "sparse", None, true), why);

    let repo = s.repo("gitlink", &FILES);
    let info = format!("160000,{},sub", s.head(&repo));
    s.git(&repo, &["update-index", "--add", "--cacheinfo", &info]);
    s.git(&repo, &["commit", "-q", "-m", "sub"]);
    std::fs::create_dir(repo.join("sub")).unwrap();
    let why = "the checkout holds a submodule (sub)";
    assert_eq!(required(&s, &repo, "gitlink", None, true), why);

    let repo = s.repo("parent/repo", &FILES);
    s.write(&s.path("parent"), &[(".ignore", "*.h\n")]);
    let why = outside(&repo.join("../.ignore"));
    assert_eq!(required(&s, &repo, "parent", None, true), why);

    let repo = s.repo("exclude", &FILES);
    s.write(&repo, &[(".git/info/exclude", "*.h\n")]);
    let why = outside(&repo.join(".git/info/exclude"));
    assert_eq!(required(&s, &repo, "exclude", None, true), why);

    let repo = s.repo("global", &FILES);
    s.write(&s.path("config"), &[("git/ignore", "*.h\n")]);
    let why = outside(&s.path("config/git/ignore"));
    assert_eq!(required(&s, &repo, "global", None, true), why);
}

/// Every reason that only the walk or the built index shows: with
/// `--require-served-commit` the build fails on it after the build, and
/// removes what it wrote.
#[test]
fn a_required_commit_fails_after_the_build() {
    let s = Sandbox::new();

    let repo = s.repo("untracked", &FILES);
    s.write(&repo, &[("new.c", "printk new\n")]);
    let why = "new.c is not tracked by git";
    assert_eq!(required(&s, &repo, "untracked", None, false), why);

    // Without symlinks, git checks a link out as a file of its target.
    let repo = s.repo("link", &FILES);
    std::os::unix::fs::symlink("a.c", repo.join("link.c")).unwrap();
    s.commit(&repo, "link");
    s.git(&repo, &["config", "core.symlinks", "false"]);
    std::fs::remove_file(repo.join("link.c")).unwrap();
    s.git(&repo, &["checkout", "--", "link.c"]);
    assert_eq!(s.git(&repo, &["status", "--porcelain"]), "");
    let why = "link.c is not a regular file of HEAD";
    assert_eq!(required(&s, &repo, "link", None, false), why);

    let repo = s.repo("crlf", &FILES);
    s.git(&repo, &["config", "core.autocrlf", "true"]);
    std::fs::remove_file(repo.join("a.c")).unwrap();
    s.git(&repo, &["checkout", "--", "a.c"]);
    let why = "a.c differs from its blob in HEAD";
    assert_eq!(required(&s, &repo, "crlf", None, false), why);

    let extra = [
        ("nul.c", "printk Z\n"),
        (".gitattributes", "nul.c filter=nul\n"),
    ];
    let repo = s.repo("nul", &[&FILES[..], &extra].concat());
    s.git(&repo, &["config", "filter.nul.smudge", "tr Z '\\000'"]);
    s.git(&repo, &["config", "filter.nul.clean", "tr '\\000' Z"]);
    std::fs::remove_file(repo.join("nul.c")).unwrap();
    s.git(&repo, &["checkout", "--", "nul.c"]);
    let why = "nul.c differs from its blob in HEAD";
    assert_eq!(required(&s, &repo, "nul", None, false), why);

    let repo = s.repo("ignore", &FILES);
    s.write(&repo, &[("dir/.ignore", "*.h\n")]);
    let why = "the ignore file dir/.ignore is not tracked by git";
    assert_eq!(required(&s, &repo, "ignore", None, false), why);

    let extra = [
        ("dir/.ignore", "*.x\n"),
        (".gitattributes", ".ignore filter=up\n"),
    ];
    let repo = s.repo("converted", &[&FILES[..], &extra].concat());
    s.git(&repo, &["config", "filter.up.smudge", "tr x h"]);
    s.git(&repo, &["config", "filter.up.clean", "tr h x"]);
    std::fs::remove_file(repo.join("dir/.ignore")).unwrap();
    s.git(&repo, &["checkout", "--", "dir/.ignore"]);
    let why = "the ignore file dir/.ignore differs from its blob in HEAD";
    assert_eq!(required(&s, &repo, "converted", None, false), why);

    // The first git status edits dir/.ignore after it ran; the second
    // restores it before it runs.
    let repo = s.repo("rules", &[&FILES[..], &[("dir/.ignore", "*.x\n")]].concat());
    let body = format!(
        "if [ \"$3\" = status ]; then\n\
           [ -e '{seen}' ] && printf '*.x\\n' > \"$2/dir/.ignore\" && exec \"$real\" \"$@\"\n\
           \"$real\" \"$@\"; code=$?; touch '{seen}'; printf '*.h\\n' > \"$2/dir/.ignore\"; exit $code\n\
         fi\n",
        seen = s.path("seen").display()
    );
    let path = wrapped_git(&s, &body);
    let why = "the ignore file dir/.ignore changed during the build";
    assert_eq!(required(&s, &repo, "rules", Some(&path), false), why);

    // The first git status moves dir/b.c away after it ran; the second
    // puts it back before it runs.
    let repo = s.repo("moved", &FILES);
    let body = format!(
        "if [ \"$3\" = status ]; then\n\
           [ -e '{seen}' ] && mv '{away}' \"$2/dir/b.c\" && exec \"$real\" \"$@\"\n\
           \"$real\" \"$@\"; code=$?; touch '{seen}'; mv \"$2/dir/b.c\" '{away}'; exit $code\n\
         fi\n",
        seen = s.path("seen").display(),
        away = s.path("away.c").display()
    );
    let path = wrapped_git(&s, &body);
    let why = "the folder dir changed during the build";
    assert_eq!(required(&s, &repo, "moved", Some(&path), false), why);

    // The second git status commits first.
    let repo = s.repo("head", &FILES);
    let body = format!(
        "if [ \"$3\" = status ]; then\n\
           [ -e '{seen}' ] && \"$real\" -C \"$2\" commit -q --allow-empty -m moved\n\
           touch '{seen}'\n\
         fi\n",
        seen = s.path("seen").display()
    );
    let path = wrapped_git(&s, &body);
    let why = "HEAD moved during the build";
    assert_eq!(required(&s, &repo, "head", Some(&path), false), why);

    let repo = s.repo("corrupt", &FILES);
    let oid = s.git(&repo, &["rev-parse", "HEAD:a.c"]);
    let object = repo
        .join(".git/objects")
        .join(&oid[..2])
        .join(oid[2..].trim());
    let mode = std::os::unix::fs::PermissionsExt::from_mode(0o644);
    std::fs::set_permissions(&object, mode).unwrap();
    std::fs::write(&object, "not an object").unwrap();
    let why = required(&s, &repo, "corrupt", None, false);
    assert!(why.starts_with("git failed: "), "{why}");
}

/// A clean checkout with `--require-served-commit` records its commit and
/// prints what it prints without the flag.
#[test]
fn a_required_commit_of_a_clean_checkout_is_recorded() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let (code, plain) = build(&s, &repo, "idx-plain", &[], None);
    assert_eq!(code, Some(0), "{plain}");
    let (code, required) = build(&s, &repo, "idx", &[REQUIRE], None);
    assert_eq!(code, Some(0), "{required}");
    assert_eq!(required, plain);
    assert_eq!(
        required,
        format!("note: the index holds commit {}\n", s.head(&repo))
    );
    assert_eq!(commit_of(&s.path("idx")), Some(s.head(&repo)));
}

/// Only a `--store-docs` index records a served commit, so the flag
/// without it is a usage error, and nothing is built.
#[test]
fn a_required_commit_needs_store_docs() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let output = s.path("idx").display().to_string();
    let out = s.run(&repo, &["index", REQUIRE, ".", "--output", &output]);
    assert_eq!(out.status.code(), Some(2));
    let want = "error: --require-served-commit needs --store-docs: only a --store-docs \
                index records a served commit\n";
    assert_eq!(String::from_utf8(out.stderr).unwrap(), want);
    assert!(!s.path("idx").exists());
}

/// A build that `--require-served-commit` stops before the walk leaves an
/// older index at its output as it was.
#[test]
fn a_required_commit_keeps_an_older_index() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let (_, stderr) = s.index(&repo, "idx");
    let head = s.head(&repo);
    assert_eq!(
        note(&stderr),
        format!("note: the index holds commit {head}")
    );
    let manifest = std::fs::read(s.path("idx/manifest.bin")).unwrap();
    s.write(&repo, &[("a.c", "printk edited\n")]);
    let (code, stderr) = build(&s, &repo, "idx", &[REQUIRE], None);
    assert_eq!(code, Some(2), "{stderr}");
    let want = "error: the index cannot record its served commit: a.c differs from HEAD\n";
    assert_eq!(stderr, want);
    assert_eq!(std::fs::read(s.path("idx/manifest.bin")).unwrap(), manifest);
    assert_eq!(commit_of(&s.path("idx")), Some(head));
}

/// A `PATH` whose first `git` is a shell script that runs `body`, then the
/// real git, `$real`, with the same arguments: `-C`, the directory, then
/// git's own.
fn wrapped_git(s: &Sandbox, body: &str) -> String {
    let path = std::env::var("PATH").unwrap();
    let real = std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|git| git.is_file())
        .expect("git on PATH");
    let script = format!(
        "#!/bin/sh\nreal='{}'\n{body}exec \"$real\" \"$@\"\n",
        real.display()
    );
    s.write(&s.path("wrapped"), &[("git", &script)]);
    let wrapped = s.path("wrapped/git");
    let mut mode = std::fs::metadata(&wrapped).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&wrapped, mode).unwrap();
    format!("{}:{path}", s.path("wrapped").display())
}

/// The index of `repo` built with `PATH` set to `path`: its stderr.
fn index_with(s: &Sandbox, repo: &std::path::Path, path: &str) -> String {
    let output = s.path("idx").display().to_string();
    let out = s
        .fmgrep(repo)
        .env("PATH", path)
        .args(["index", "--store-docs", ".", "--output", &output])
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    stderr
}

/// The stored files are compared with the blobs of HEAD's tree, not with
/// git's index, which can change after git status: a file edited for the
/// walk, restored for git status, then staged as the walk read it.
#[test]
fn a_file_staged_after_git_status_records_no_commit() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    // The first git status edits a.c after it ran, and the walk reads the
    // edit; the second restores a.c before it runs; git ls-files -v, the
    // next git, stages the edit.
    let body = format!(
        "edit() {{ printf 'printk staged\\n' > \"$2/a.c\"; }}\n\
         case \"$3 $4\" in\n\
         'status '*)\n\
           [ -e '{seen}' ] && printf 'printk a\\n' > \"$2/a.c\" && exec \"$real\" \"$@\"\n\
           \"$real\" \"$@\"; code=$?; touch '{seen}'; edit \"$@\"; exit $code ;;\n\
         'ls-files -v') edit \"$@\"; \"$real\" -C \"$2\" add a.c ;;\n\
         esac\n",
        seen = s.path("seen").display()
    );
    let stderr = index_with(&s, &repo, &wrapped_git(&s, &body));
    assert_eq!(s.git(&repo, &["diff", "--cached", "--name-only"]), "a.c\n");
    let want = "note: the index records no served commit (a.c differs from its blob in HEAD)";
    assert_eq!(note(&stderr), want);
    assert_eq!(commit_of(&s.path("idx")), None);
}

/// A file moved away from the walk and put back before the second git
/// status is missing from the index, though git status shows no change:
/// its folder changed during the build.
#[test]
fn a_file_moved_away_from_the_walk_records_no_commit() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let away = s.path("away.c").display().to_string();
    // The first git status moves dir/b.c away after it ran; the second
    // puts it back before it runs.
    let body = format!(
        "if [ \"$3\" = status ]; then\n\
           [ -e '{seen}' ] && mv '{away}' \"$2/dir/b.c\" && exec \"$real\" \"$@\"\n\
           \"$real\" \"$@\"; code=$?; touch '{seen}'; mv \"$2/dir/b.c\" '{away}'; exit $code\n\
         fi\n",
        seen = s.path("seen").display()
    );
    let stderr = index_with(&s, &repo, &wrapped_git(&s, &body));
    assert_eq!(s.git(&repo, &["status", "--porcelain"]), "");
    let want = "note: the index records no served commit (the folder dir changed during the build)";
    assert_eq!(note(&stderr), want);
    assert_eq!(commit_of(&s.path("idx")), None);
}

/// Git's replace refs are not followed: a checkout reset to a replaced
/// HEAD holds the replacement's files, which another checkout of HEAD
/// does not have.
#[test]
fn a_replaced_head_records_no_commit() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let head = s.head(&repo);
    s.write(&repo, &[("a.c", "printk replaced\n")]);
    let other = s.commit(&repo, "other");
    s.git(&repo, &["replace", &head, &other]);
    s.git(&repo, &["reset", "-q", "--hard", &head]);
    assert_eq!(s.head(&repo), head);
    assert_eq!(s.git(&repo, &["status", "--porcelain"]), "");
    let text = std::fs::read_to_string(repo.join("a.c")).unwrap();
    assert_eq!(text, "printk replaced\n");
    let (index, stderr) = s.index(&repo, "idx");
    let want = "note: the index records no served commit (a.c differs from HEAD)";
    assert_eq!(note(&stderr), want);
    assert_eq!(commit_of(&index), None);
}

/// A file that git turns binary on checkout is left out of the index,
/// though its blob is text that a checkout without the filter searches.
#[test]
fn a_file_made_binary_on_checkout_records_no_commit() {
    let s = Sandbox::new();
    let extra = [
        ("nul.c", "printk Z\n"),
        (".gitattributes", "nul.c filter=nul\n"),
    ];
    let repo = s.repo("repo", &[&FILES[..], &extra].concat());
    s.git(&repo, &["config", "filter.nul.smudge", "tr Z '\\000'"]);
    s.git(&repo, &["config", "filter.nul.clean", "tr '\\000' Z"]);
    std::fs::remove_file(repo.join("nul.c")).unwrap();
    s.git(&repo, &["checkout", "--", "nul.c"]);
    assert_eq!(std::fs::read(repo.join("nul.c")).unwrap(), b"printk \0\n");
    assert_eq!(s.git(&repo, &["status", "--porcelain"]), "");
    let (index, stderr) = s.index(&repo, "idx");
    assert!(stderr.contains("skipped 1 non-text file(s)"), "{stderr}");
    let want = "note: the index records no served commit (nul.c differs from its blob in HEAD)";
    assert_eq!(note(&stderr), want);
    assert_eq!(commit_of(&index), None);
}

/// A blob that git cannot read gives git's own reason.
#[test]
fn an_unreadable_blob_says_why_git_failed() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let oid = s.git(&repo, &["rev-parse", "HEAD:a.c"]);
    let object = repo
        .join(".git/objects")
        .join(&oid[..2])
        .join(oid[2..].trim());
    let mut mode = std::fs::metadata(&object).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o644);
    std::fs::set_permissions(&object, mode).unwrap();
    std::fs::write(&object, "not an object").unwrap();
    assert_eq!(s.git(&repo, &["status", "--porcelain"]), "");
    let (index, stderr) = s.index(&repo, "idx");
    let note = note(&stderr);
    assert!(
        note.starts_with("note: the index records no served commit (git failed: "),
        "{note}"
    );
    assert!(!note.contains("has no blob"), "{note}");
    assert_eq!(commit_of(&index), None);
}

/// A tracked ignore file that a filter changes on checkout holds rules
/// that a checkout of HEAD without the filter does not have, while git
/// status shows no change: here it leaves `dir/c.h` out.
#[test]
fn an_ignore_file_converted_on_checkout_records_no_commit() {
    let s = Sandbox::new();
    let extra = [
        ("dir/.ignore", "*.x\n"),
        (".gitattributes", ".ignore filter=up\n"),
    ];
    let repo = s.repo("repo", &[&FILES[..], &extra].concat());
    s.git(&repo, &["config", "filter.up.smudge", "tr x h"]);
    s.git(&repo, &["config", "filter.up.clean", "tr h x"]);
    std::fs::remove_file(repo.join("dir/.ignore")).unwrap();
    s.git(&repo, &["checkout", "--", "dir/.ignore"]);
    let text = std::fs::read_to_string(repo.join("dir/.ignore")).unwrap();
    assert_eq!(text, "*.h\n");
    assert_eq!(s.git(&repo, &["status", "--porcelain"]), "");
    let (index, stderr) = s.index(&repo, "idx");
    let why = "the ignore file dir/.ignore differs from its blob in HEAD";
    assert_eq!(
        note(&stderr),
        format!("note: the index records no served commit ({why})")
    );
    assert_eq!(commit_of(&index), None);
}

/// An ignore file edited for the walk and restored for git status gave the
/// walk other rules than HEAD's.
#[test]
fn an_ignore_file_edited_during_the_build_records_no_commit() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &[&FILES[..], &[("dir/.ignore", "*.x\n")]].concat());
    let body = format!(
        "if [ \"$3\" = status ]; then\n\
           [ -e '{seen}' ] && printf '*.x\\n' > \"$2/dir/.ignore\" && exec \"$real\" \"$@\"\n\
           \"$real\" \"$@\"; code=$?; touch '{seen}'; printf '*.h\\n' > \"$2/dir/.ignore\"; exit $code\n\
         fi\n",
        seen = s.path("seen").display()
    );
    let stderr = index_with(&s, &repo, &wrapped_git(&s, &body));
    let why = "the ignore file dir/.ignore changed during the build";
    assert_eq!(
        note(&stderr),
        format!("note: the index records no served commit ({why})")
    );
    assert_eq!(commit_of(&s.path("idx")), None);
}

/// Leading blanks belong to a gitignore pattern: ` #v.c` is a rule for the
/// file ` #v.c`, not a comment.
#[test]
fn a_pattern_after_blanks_outside_records_no_commit() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &[&FILES[..], &[(" #v.c", "printk v\n")]].concat());
    s.write(&s.path("config"), &[("git/ignore", " #v.c\n")]);
    let (index, stderr) = s.index(&repo, "idx");
    let why = format!(
        "the ignore file {} outside the checkout has a pattern",
        s.path("config/git/ignore").display()
    );
    assert_eq!(
        note(&stderr),
        format!("note: the index records no served commit ({why})")
    );
    assert_eq!(commit_of(&index), None);
}

/// A server's `status` reports the format and the served commit of a
/// `--store-docs` index, and no commit for an index that reads the tree;
/// `fmgrep status --server` prints both, one line each.
#[test]
fn status_reports_the_served_commit() {
    let s = Sandbox::new();
    let repo = s.repo("repo", &FILES);
    let (index, _) = s.index(&repo, "idx");
    let (_server, addr) = s.serve(&index, &[]);
    let status = s.status(&addr);
    assert_eq!(status["format"], 8);
    assert_eq!(status["commit"]["vcs"], "git");
    assert_eq!(status["commit"]["id"], s.head(&repo));
    let lines = s.status_lines(&addr);
    let commit = format!("commit: git {}", s.head(&repo));
    assert_eq!(lines[1], "format: 8", "{lines:?}");
    assert_eq!(lines.last(), Some(&commit), "{lines:?}");
    let plain = s.path("plain").display().to_string();
    let out = s.run(&repo, &["index", ".", "--output", &plain]);
    assert!(out.status.success());
    let (_server, addr) = s.serve(&s.path("plain"), &["--no-watch"]);
    assert!(s.status(&addr).get("commit").is_none());
    let lines = s.status_lines(&addr);
    assert_eq!(lines.last().map(String::as_str), Some("commit: none"));
}

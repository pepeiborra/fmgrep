//! A search from a checkout of the served commit that the developer
//! changed: the client searches the files that differ, the server the
//! rest, and the output is what a local index of the checkout prints.

use std::path::Path;

use crate::remote::{Remote, stderr, stdout};

const FILES: [(&str, &str); 8] = [
    (".gitignore", "*.log\n"),
    (".hidden.c", "printk hidden\n"),
    ("a.c", "printk a\nother\n"),
    ("bin.dat", "printk\0binary\n"),
    ("dir/b.c", "printk b\nother b\n"),
    ("dir/sub/c.h", "printk c\nPrintK C\n"),
    ("e.c", "printk e printk\n"),
    ("z.c", "zzz other\n"),
];

/// The client's checkout back at the served commit, with nothing else. A
/// second reset restores what the clean removed: the first one leaves a
/// symlink that stands in place of a tracked directory, and the clean
/// deletes it.
fn reset(r: &Remote) {
    let c = &r.client;
    r.s.git(c, &["reset", "-q", "--hard", &r.commit]);
    r.s.git(c, &["clean", "-q", "-f", "-d", "-x"]);
    r.s.git(c, &["reset", "-q", "--hard", &r.commit]);
}

type Change = fn(&Remote, &Path);

/// Each kind of uncommitted change, alone, searched through the server and
/// by a local index of the checkout: the same output.
#[test]
fn each_uncommitted_change_answers_as_a_local_index() {
    let r = Remote::new(&FILES);
    let cases: [(&str, Change); 18] = [
        ("match added", |r, c| r.s.write(c, &[("z.c", "printk z\n")])),
        ("match removed", |r, c| r.s.write(c, &[("a.c", "none\n")])),
        ("deleted", |_, c| {
            std::fs::remove_file(c.join("dir/b.c")).unwrap()
        }),
        ("staged deletion", |r, c| {
            r.s.git(c, &["rm", "-q", "e.c"]);
        }),
        ("untracked", |r, c| {
            r.s.write(c, &[("dir/n.c", "printk n\n")])
        }),
        ("untracked dir", |r, c| {
            r.s.write(c, &[("new/x.c", "printk x\n"), ("new/y/z.c", "printk z\n")])
        }),
        ("untracked dir with its .gitignore", |r, c| {
            r.s.write(
                c,
                &[
                    ("new/.gitignore", "skip.c\n"),
                    ("new/skip.c", "printk s\n"),
                    ("new/k.c", "printk k\n"),
                ],
            )
        }),
        ("staged new", |r, c| {
            r.s.write(c, &[("b2.c", "printk b2\n")]);
            r.s.git(c, &["add", "b2.c"]);
        }),
        ("renamed", |r, c| {
            r.s.git(c, &["mv", "a.c", "dir/a2.c"]);
        }),
        ("turned binary", |r, c| {
            r.s.write(c, &[("e.c", "printk\0now\n")])
        }),
        ("turned text", |r, c| {
            r.s.write(c, &[("bin.dat", "printk text\n")])
        }),
        ("a symlink in place of a file", |_, c| {
            std::fs::remove_file(c.join("a.c")).unwrap();
            std::os::unix::fs::symlink("e.c", c.join("a.c")).unwrap();
        }),
        ("a symlink in place of a directory", |_, c| {
            std::fs::rename(c.join("dir/sub"), c.join("sub.real")).unwrap();
            std::os::unix::fs::symlink("../sub.real", c.join("dir/sub")).unwrap();
        }),
        (
            "a symlink to a directory moved out of the checkout",
            |r, c| {
                let away = r.s.path("away");
                let _ = std::fs::remove_dir_all(&away);
                std::fs::rename(c.join("dir/sub"), &away).unwrap();
                std::os::unix::fs::symlink(&away, c.join("dir/sub")).unwrap();
            },
        ),
        ("a directory in place of a file", |r, c| {
            std::fs::remove_file(c.join("z.c")).unwrap();
            r.s.write(c, &[("z.c/inner.c", "printk inner\n")]);
        }),
        ("mode only", |r, c| {
            r.s.git(c, &["update-index", "--chmod=+x", "e.c"]);
            let path = c.join("e.c");
            let mut mode = std::fs::metadata(&path).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
            std::fs::set_permissions(&path, mode).unwrap();
        }),
        ("a hidden file edited", |r, c| {
            r.s.write(c, &[(".hidden.c", "printk more\n")])
        }),
        ("an ignored file added", |r, c| {
            r.s.write(c, &[("x.log", "printk log\n")])
        }),
    ];
    for (name, change) in cases {
        reset(&r);
        change(&r, &r.client);
        for args in [
            &["printk"][..],
            &["-c", "printk"],
            &["--json", "--matches", "printk"],
        ] {
            r.same("", args);
        }
        let _ = name;
    }
}

/// A checkout whose HEAD moved from the served commit: commits on top, a
/// branch, a stash, a rebase, a merge conflict, HEAD behind the commit,
/// a soft reset. The paths where the two commits differ are searched
/// here too.
#[test]
fn a_checkout_whose_head_moved_answers_as_a_local_index() {
    let first = [
        (".gitignore", "*.log\n"),
        ("a.c", "printk old a\n"),
        ("old.c", "printk old\n"),
        ("z.c", "zzz other\n"),
    ];
    let r = Remote::after(&first, &FILES);
    let back = format!("{}~1", r.commit);
    let cases: [(&str, Change); 9] = [
        ("a commit on top", |r, c| {
            r.s.write(c, &[("a.c", "none\n"), ("dir/n.c", "printk n\n")]);
            r.s.commit(c, "top");
        }),
        ("commits on top", |r, c| {
            r.s.write(c, &[("z.c", "printk z\n")]);
            r.s.commit(c, "one");
            std::fs::remove_file(c.join("e.c")).unwrap();
            r.s.commit(c, "two");
            r.s.write(c, &[("dir/b.c", "uncommitted\n")]);
        }),
        ("another branch", |r, c| {
            r.s.git(c, &["checkout", "-q", "-b", "other"]);
            r.s.write(c, &[("a.c", "printk other branch\n")]);
            r.s.commit(c, "other");
        }),
        ("another branch and back", |r, c| {
            r.s.git(c, &["checkout", "-q", "-b", "other"]);
            r.s.write(c, &[("a.c", "printk other branch\n")]);
            r.s.commit(c, "other");
            r.s.git(c, &["checkout", "-q", "main"]);
        }),
        ("stash and pop", |r, c| {
            r.s.write(c, &[("a.c", "printk stashed\n")]);
            r.s.git(c, &["stash", "-q"]);
            r.s.write(c, &[("z.c", "printk between\n")]);
            r.s.git(c, &["stash", "pop", "-q"]);
        }),
        ("rebase", |r, c| {
            r.s.git(c, &["checkout", "-q", "-b", "base"]);
            r.s.write(c, &[("dir/sub/c.h", "printk rebased base\n")]);
            r.s.commit(c, "base");
            r.s.git(c, &["checkout", "-q", "main"]);
            r.s.write(c, &[("a.c", "printk mine\n")]);
            r.s.commit(c, "mine");
            r.s.git(c, &["rebase", "-q", "base"]);
        }),
        ("a merge conflict", |r, c| {
            r.s.git(c, &["checkout", "-q", "-b", "theirs"]);
            r.s.write(c, &[("a.c", "printk theirs\n")]);
            r.s.commit(c, "theirs");
            r.s.git(c, &["checkout", "-q", "main"]);
            r.s.write(c, &[("a.c", "printk ours\n")]);
            r.s.commit(c, "ours");
            let merged = r.s.git_output(c, &["merge", "-q", "theirs"]);
            assert!(!merged.status.success(), "a conflict");
        }),
        ("HEAD behind", |r, c| {
            let back = format!("{}~1", r.commit);
            r.s.git(c, &["checkout", "-q", &back]);
        }),
        ("a soft reset", |r, c| {
            let back = format!("{}~1", r.commit);
            r.s.git(c, &["reset", "-q", "--soft", &back]);
        }),
    ];
    let _ = back;
    for (name, change) in cases {
        reset(&r);
        r.s.git(&r.client, &["checkout", "-q", "-f", "main"]);
        for branch in ["other", "base", "theirs"] {
            let _ = r.s.git_output(&r.client, &["branch", "-q", "-D", branch]);
        }
        reset(&r);
        change(&r, &r.client);
        for args in [
            &["printk"][..],
            &["-l", "printk", "dir"],
            &["--json", "--matches", "printk"],
        ] {
            r.same("", args);
        }
        let _ = name;
    }
}

/// A partial clone (`--filter=blob:none`) of a later commit lacks the
/// served commit's blobs: the search answers as a local index of the
/// checkout, and no git that it runs fetches (git's trace2 log of every
/// git run holds no fetch).
#[test]
fn a_partial_clone_answers_and_fetches_nothing() {
    let mut r = Remote::new(&FILES);
    r.s.git(&r.origin, &["config", "uploadpack.allowFilter", "true"]);
    r.s.write(
        &r.origin,
        &[("a.c", "printk a2\n"), ("dir/n.c", "printk n\n")],
    );
    r.s.commit(&r.origin, "second");
    let url = format!("file://{}", r.origin.display());
    let clone = ["clone", "-q", "--filter=blob:none", &url, "partial"];
    r.s.git(r.s.tmp.path(), &clone);
    r.client = std::fs::canonicalize(r.s.path("partial")).unwrap();
    let missing = r.s.git(
        &r.client,
        &["rev-list", "--objects", "--missing=print", "--all"],
    );
    assert!(missing.lines().any(|l| l.starts_with('?')), "{missing}");
    r.s.write(&r.client, &[("e.c", "printk e edited\n")]);
    let log = r.s.path("trace2.json");
    let config = format!("[trace2]\n\teventTarget = {}\n", log.display());
    std::fs::write(r.s.global_config(), config).unwrap();
    for args in [&["printk"][..], &["-l", "printk", "dir"]] {
        r.same("", args);
    }
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(
        log.contains(r#""status""#),
        "the log holds the client's git runs"
    );
    assert!(!log.contains(r#""fetch""#), "{log}");
}

/// One change set that interleaves the client's files with the server's,
/// in every output mode.
#[test]
fn an_interleaved_change_set_answers_as_a_local_index_in_every_mode() {
    let r = Remote::new(&FILES);
    let c = &r.client;
    r.s.write(
        c,
        &[
            ("a.c", "printk a changed\nother\nprintk again\n"),
            ("dir/n.c", "PRINTK n\nother n\n"),
            ("dir/sub/c.h", "printk c\n"),
            ("y.c", "x\nprintk y\nz\n"),
        ],
    );
    std::fs::remove_file(c.join("e.c")).unwrap();
    for (rel, args) in [
        ("", &["printk"][..]),
        ("", &["--heading", "printk"]),
        ("", &["--no-heading", "-n", "printk"]),
        ("", &["--heading", "-N", "printk"]),
        ("", &["-C2", "printk"]),
        ("", &["-o", "pr.ntk"]),
        ("", &["-i", "printk"]),
        ("", &["/oth[a-z]r/"]),
        ("", &["-U", "/printk.*\\n.*other/"]),
        ("", &["-l", "printk"]),
        ("", &["-c", "printk"]),
        ("", &["--count-matches", "printk"]),
        ("", &["--json", "--matches", "printk"]),
        ("", &["--json", "printk", "other"]),
        ("", &["--approx-counts", "printk"]),
        ("", &["--and", "printk", "other"]),
        ("", &["--or", "printk", "zzz"]),
        ("", &["--expr", "printk AND (other OR again)"]),
        ("", &["printk", "other"]),
        ("", &["-l", "printk", "a.c", "dir"]),
        ("dir", &["printk"]),
        ("dir", &["-l", "printk", "../y.c"]),
        ("", &["-l", "-g", "*.c", "printk"]),
        ("", &["-l", "--iglob", "DIR/**", "printk"]),
        ("", &["--color", "always", "--heading", "printk"]),
        ("", &["nothing-matches"]),
        ("", &["-g", "*.none", "printk"]),
        ("", &["/(/"]),
    ] {
        r.same(rel, args);
    }
}

/// More files differ than a query may list: the server's answer alone,
/// and the reason.
#[test]
fn past_the_limit_the_answer_is_the_servers_alone() {
    let r = Remote::new(&FILES);
    let many: Vec<(String, String)> = (0..10_001)
        .map(|i| (format!("many/{i:05}.c"), "printk\n".to_owned()))
        .collect();
    let many: Vec<(&str, &str)> = many.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    r.s.write(&r.client, &many);
    r.s.git(&r.client, &["add", "many"]);
    let out = r.search(&r.client, &["-l", "printk"]);
    let why = format!(
        "10001 files differ from the served commit {}; the limit is 10000",
        &r.commit[..7]
    );
    assert_eq!(stderr(&out), crate::remote::ignored(&why));
    assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n");
}

/// A change, and the reason it gives for ignoring the checkout's changes;
/// an empty reason: none.
type Unsound = (Change, &'static str);

/// Each checkout whose changes git cannot list soundly: the server's
/// answer alone, with an edit of `a.c` that it ignores, and the reason.
#[test]
fn each_unsound_checkout_gets_the_servers_answer_and_the_reason() {
    let r = Remote::new(&FILES);
    let served = "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n";
    let cases: [Unsound; 9] = [
        (
            |r, c| {
                r.s.git(c, &["update-index", "--assume-unchanged", "z.c"]);
            },
            "git hides changes to 1 file (assume-unchanged or skip-worktree)",
        ),
        (
            |r, c| {
                r.s.git(c, &["update-index", "--skip-worktree", "z.c"]);
            },
            "git hides changes to 1 file (assume-unchanged or skip-worktree)",
        ),
        (
            |r, c| {
                r.s.git(c, &["config", "core.sparseCheckout", "true"]);
            },
            "the checkout is sparse (core.sparseCheckout)",
        ),
        (
            |r, c| {
                r.s.git(c, &["config", "core.autocrlf", "true"]);
            },
            "git converts files (core.autocrlf=true)",
        ),
        (
            |r, c| {
                r.s.git(c, &["config", "filter.up.smudge", "cat"]);
                r.s.write(c, &[(".git/info/attributes", "*.c filter=up\n")]);
            },
            "git converts files (the filter up applies to 5 files)",
        ),
        (
            |r, c| {
                let info = format!("160000,{},sub", r.commit);
                r.s.git(c, &["update-index", "--add", "--cacheinfo", &info]);
                std::fs::create_dir(c.join("sub")).unwrap();
            },
            "the checkout holds a submodule (sub)",
        ),
        (
            |r, _| r.s.write(r.s.tmp.path(), &[(".ignore", "*.h\n")]),
            "an ignore file above the checkout applies ({tmp}/.ignore)",
        ),
        (
            |r, c| r.s.write(c, &[(".gitignore", "*.log\n*.h\n")]),
            "an ignore file differs from the served commit (.gitignore)",
        ),
        (
            |r, c| r.s.write(c, &[(".git/info/exclude", "*.h\n")]),
            "ignore rules from outside the checkout apply ({client}/.git/info/exclude)",
        ),
    ];
    let tmp = std::fs::canonicalize(r.s.tmp.path()).unwrap();
    for (change, why) in cases {
        reset(&r);
        let _ = std::fs::remove_file(r.s.tmp.path().join(".ignore"));
        for args in [
            &["config", "--unset", "core.sparseCheckout"][..],
            &["config", "--unset", "core.autocrlf"],
            &["config", "--remove-section", "filter.up"],
            &[
                "update-index",
                "--no-skip-worktree",
                "--no-assume-unchanged",
                "z.c",
            ],
        ] {
            let _ = r.s.git_output(&r.client, args);
        }
        let _ = std::fs::remove_file(r.client.join(".git/info/attributes"));
        let _ = std::fs::remove_file(r.client.join(".git/info/exclude"));
        r.s.write(&r.client, &[("a.c", "none\n")]);
        change(&r, &r.client);
        let out = r.search(&r.client, &["-l", "printk"]);
        let why = why
            .replace("{tmp}", &tmp.display().to_string())
            .replace("{client}", &r.client.display().to_string());
        assert_eq!(stderr(&out), crate::remote::ignored(&why));
        assert_eq!(stdout(&out), served, "{why}");
    }
    // A filter that applies to no file converts nothing.
    reset(&r);
    std::fs::remove_file(r.client.join(".git/info/exclude")).unwrap();
    r.s.git(&r.client, &["config", "filter.up.smudge", "cat"]);
    r.same("", &["-l", "printk"]);
}

/// A filter with a clean command alone: git status compares the cleaned
/// file with its blob, so an edit that the filter drops is not listed.
/// The client ignores local changes and says why.
#[test]
fn a_clean_only_filter_ignores_local_changes() {
    let files = [
        (".gitattributes", "*.txt filter=strip\n"),
        ("a.c", "printk a\n"),
        ("notes.txt", "printk notes\n"),
    ];
    let r = Remote::new(&files);
    let c = &r.client;
    r.s.git(c, &["config", "filter.strip.clean", "grep -v SECRET"]);
    r.s.write(c, &[("notes.txt", "printk notes\nSECRET printk\n")]);
    r.s.git(c, &["add", "notes.txt"]);
    let out = r.search(c, &["printk"]);
    let why = "git converts files (the filter strip applies to 1 file)";
    assert_eq!(stderr(&out), crate::remote::ignored(why));
    assert_eq!(stdout(&out), "a.c:printk a\nnotes.txt:printk notes\n");
}

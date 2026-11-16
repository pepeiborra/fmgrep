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

/// Tracked files that the client's own excludes ignore (its global excludes
/// file, its `info/exclude`, also from a linked worktree, a directory
/// pattern, a rule of one that the other negates) are left out, as its
/// local index leaves them out.
#[test]
fn tracked_files_the_clients_excludes_ignore_are_left_out() {
    let r = Remote::new(&FILES);
    let global = r.s.path("config");
    for (name, exclude) in [
        ("global", global.join("git/ignore")),
        ("info", r.client.join(".git/info/exclude")),
    ] {
        for pattern in ["*.h\n", "dir/sub/\n", "e.c\n"] {
            reset(&r);
            r.s.write(
                exclude.parent().unwrap(),
                &[(exclude.file_name().unwrap().to_str().unwrap(), pattern)],
            );
            r.s.write(&r.client, &[("a.c", "printk edited\n")]);
            for args in [&["printk"][..], &["-l", "printk"]] {
                r.same("", args);
            }
            std::fs::remove_file(&exclude).unwrap();
            let _ = name;
        }
    }
    // Where the two disagree, `info/exclude` decides, as in the walk.
    let info = r.client.join(".git/info/exclude");
    for (mine, global_rule) in [("e.c\n", "!e.c\n"), ("!e.c\n", "e.c\n")] {
        reset(&r);
        r.s.write(&r.client, &[(".git/info/exclude", mine)]);
        r.s.write(&global, &[("git/ignore", global_rule)]);
        r.s.write(&r.client, &[("a.c", "printk edited\n")]);
        for args in [&["printk"][..], &["-l", "printk"]] {
            r.same("", args);
        }
        std::fs::remove_file(&info).unwrap();
        std::fs::remove_file(global.join("git/ignore")).unwrap();
    }
    reset(&r);
    let tree = r.s.path("linked");
    r.s.git(
        &r.client,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &tree.display().to_string(),
            &r.commit,
        ],
    );
    r.s.write(&r.client, &[(".git/info/exclude", "*.h\n")]);
    let tree = std::fs::canonicalize(tree).unwrap();
    let out = r.search(&tree, &["-l", "printk"]);
    assert_eq!(stderr(&out), "");
    assert_eq!(stdout(&out), "a.c\ndir/b.c\ne.c\n");
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
        ("", &["--stats", "printk", "other"]),
        ("", &["--stats", "--no-heading", "-o", "pr.ntk"]),
        ("", &["--stats", "-C1", "--color", "always", "printk"]),
        ("", &["--stats", "--expr", "printk AND other"]),
        ("", &["--stats", "-g", "*.c", "printk"]),
        ("", &["--stats", "-g", "!a.c", "printk"]),
        ("", &["--stats", "printk", "a.c", "dir"]),
        ("dir", &["--stats", "printk"]),
        ("", &["nothing-matches"]),
        ("", &["-g", "*.none", "printk"]),
        ("", &["/(/"]),
    ] {
        r.same(rel, args);
    }
}

/// A file's matches past its first 1,000 matched lines print without
/// colour, whichever side searched the file: the server's big file and
/// the client's print as a local index prints them.
#[test]
fn the_colour_bound_holds_per_file_on_both_sides() {
    let big =
        |name: &str| -> String { (0..1_003).map(|i| format!("printk {name} {i}\n")).collect() };
    let (served, changed) = (big("served"), big("changed"));
    let r = Remote::new(&[
        ("a.c", "printk a\n"),
        ("s.c", &served),
        ("z.c", "printk z\n"),
    ]);
    r.s.write(
        &r.client,
        &[("a.c", "printk a changed\n"), ("m.c", &changed)],
    );
    for args in [
        &["--color", "always", "-n", "printk"][..],
        &["--color", "always", "--no-heading", "-C1", "printk"],
        &["--color", "always", "--stats", "printk"],
        &["--color", "always", "-o", "printk"],
    ] {
        r.same("", args);
    }
    let out = stdout(&r.search(&r.client, &["--color", "always", "printk"]));
    assert_eq!(
        out.matches("\x1b[1m\x1b[31m").count(),
        1 + 1_000 + 1_000 + 1
    );
}

/// More files differ than a query may list, staged and then untracked:
/// the server's answer alone, and the reason.
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
    // Untracked, they are one path, `many/`: the walk of it stops at the
    // 10,001st file, before any is read.
    r.s.git(&r.client, &["reset", "-q"]);
    let out = r.search(&r.client, &["-l", "printk"]);
    let why = format!(
        "more than 10000 files differ from the served commit {}",
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
    let cases: [Unsound; 2] = [
        (
            |r, c| {
                r.s.write(c, &[("a.c", "printk top\n")]);
                r.s.commit(c, "top");
                r.s.write(c, &[("a.c", "none\n")]);
            },
            "HEAD is not the served commit {short}",
        ),
        (
            |r, c| r.s.write(c, &[(".gitignore", "*.log\n*.h\n")]),
            "an ignore file differs from the served commit (.gitignore)",
        ),
    ];
    for (change, why) in cases {
        reset(&r);
        r.s.write(&r.client, &[("a.c", "none\n")]);
        change(&r, &r.client);
        let out = r.search(&r.client, &["-l", "printk"]);
        let why = why.replace("{short}", &r.commit[..7]);
        assert_eq!(stderr(&out), crate::remote::ignored(&why));
        assert_eq!(stdout(&out), served, "{why}");
    }
}

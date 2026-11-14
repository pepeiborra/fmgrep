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

//! A search through a server from a checkout, with a private watchman
//! daemon: git once, for the baseline, then watchman; and without one,
//! git on every search, with a warning.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::remote::{ASKED_GIT, Remote, stderr, stdout};

const FILES: [(&str, &str); 4] = [
    ("a.c", "printk a\n"),
    ("dir/b.c", "printk b\n"),
    ("dir/sub/c.h", "printk c\n"),
    ("e.c", "printk e\n"),
];

/// A `git` first on `PATH` that logs each run to `log`, then runs the real
/// one; and the `PATH` that puts it first.
fn logging_git(r: &Remote) -> (PathBuf, String) {
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH");
    let bin = r.s.path("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = r.s.path("git.log");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
        log.display(),
        real.display()
    );
    std::fs::write(bin.join("git"), script).unwrap();
    let mut mode = std::fs::metadata(bin.join("git")).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(bin.join("git"), mode).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    (log, path.into_string().unwrap())
}

/// `args` through the server from the client's top, `git` logged: the
/// output and the git runs.
fn logged(r: &Remote, log: &Path, path: &str, args: &[&str]) -> (Output, Vec<String>) {
    let _ = std::fs::remove_file(log);
    let mut cmd = r.s.fmgrep(&r.client);
    if let Some(daemon) = &r.daemon {
        cmd.env("WATCHMAN_SOCK", &daemon.sock);
    }
    let out = cmd
        .env("FMGREP_SERVER", &r.addr)
        .env("PATH", path)
        .args(args)
        .output()
        .unwrap();
    let runs = std::fs::read_to_string(log).unwrap_or_default();
    (out, runs.lines().map(str::to_owned).collect())
}

/// With watchman, the first search asks git, the later ones do not, and
/// every one answers as a local index of the checkout: an edit, a
/// directory moved in from outside, one renamed, one deleted.
#[test]
fn later_searches_ask_watchman_not_git() {
    let r = Remote::new(&FILES);
    let Some(daemon) = &r.daemon else {
        return;
    };
    let (log, path) = logging_git(&r);
    let (out, runs) = logged(&r, &log, &path, &["-l", "printk"]);
    assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n");
    assert!(runs.iter().any(|r| r.contains("status")), "{runs:?}");
    asked_again(&r, &out, "no record of an earlier search");
    let c = &r.client;
    let changes: [fn(&Remote, &Path); 7] = [
        |r, c| {
            r.s.write(c, &[("a.c", "none\n"), ("dir/n.c", "printk n\n")])
        },
        |r, c| {
            r.s.write(&r.s.path("outside"), &[("m/x.c", "printk moved\n")]);
            std::fs::rename(r.s.path("outside/m"), c.join("dir/m")).unwrap();
        },
        |_, c| std::fs::rename(c.join("dir/sub"), c.join("dir/renamed")).unwrap(),
        |_, c| std::fs::remove_dir_all(c.join("dir/renamed")).unwrap(),
        // The files of `dir` are seen through a symlink now: gone.
        |_, c| {
            std::fs::rename(c.join("dir"), c.join("dir.real")).unwrap();
            std::os::unix::fs::symlink("dir.real", c.join("dir")).unwrap();
        },
        |r, c| {
            std::fs::remove_file(c.join("e.c")).unwrap();
            r.s.write(c, &[("e.c/inner.c", "printk inner\n")]);
        },
        |_, _| {},
    ];
    for change in changes {
        change(&r, c);
        daemon.watch(c);
        let (out, runs) = logged(&r, &log, &path, &["-l", "printk"]);
        assert_eq!(stderr(&out), "");
        assert_eq!(runs, Vec::<String>::new(), "no git");
        let want = r.oracle("", &["-l", "printk"]);
        assert_eq!(stdout(&out), stdout(&want));
    }
    // A commit moves HEAD and the index: git is asked again.
    r.s.commit(c, "on top");
    let (out, runs) = logged(&r, &log, &path, &["-l", "printk"]);
    assert!(runs.iter().any(|r| r.contains("diff-tree")), "{runs:?}");
    assert_eq!(stdout(&out), stdout(&r.oracle("", &["-l", "printk"])));
    let head = std::fs::canonicalize(c.join(".git/HEAD")).unwrap();
    let index = std::fs::canonicalize(c.join(".git/index")).unwrap();
    let text = stderr(&out);
    let moved = [&head, &index].map(|p| format!("({} changed;", p.display()));
    assert!(moved.iter().any(|m| text.contains(m)), "{text}");
}

/// The one line of a search that asked git for a new baseline, `why`.
fn asked_again(r: &Remote, out: &Output, why: &str) {
    let text = stderr(out);
    let want = format!(
        "warning: asked git what differs from the served commit {} ({why}; took ",
        &r.commit[..7]
    );
    assert!(
        text.starts_with(&want) && text.ends_with("); later searches ask watchman\n"),
        "{text}"
    );
    assert_eq!(text.lines().count(), 1, "{text}");
}

/// A restarted daemon, or another one, cannot say what changed since the
/// record's token: git is asked again, and the answer is right.
#[test]
fn a_restarted_or_another_daemon_asks_git_again() {
    let mut r = Remote::new(&FILES);
    if r.daemon.is_none() {
        return;
    }
    let (log, path) = logging_git(&r);
    logged(&r, &log, &path, &["-l", "printk"]);
    r.s.write(&r.client, &[("e.c", "none\n")]);
    let daemon = r.daemon.as_mut().unwrap();
    daemon.restart();
    daemon.watch(&r.client);
    let (out, runs) = logged(&r, &log, &path, &["-l", "printk"]);
    assert!(runs.iter().any(|r| r.contains("status")), "{runs:?}");
    assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\n");
    let why = "watchman restarted, added the watch again or pruned deleted files since the last \
               search";
    asked_again(&r, &out, why);
    let mut other = r.s.fmgrep(&r.client);
    let out = other
        .env("WATCHMAN_SOCK", r.s.path("no-daemon"))
        .env("FMGREP_SERVER", &r.addr)
        .args(["-l", "printk"])
        .output()
        .unwrap();
    assert!(stderr(&out).starts_with(ASKED_GIT), "{}", stderr(&out));
    assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\n");
}

/// Without watchman, each search asks git, and says so in one line.
#[test]
fn without_watchman_every_search_asks_git_and_says_so() {
    let r = Remote::new(&FILES);
    if r.daemon.is_some() {
        return;
    }
    let (log, path) = logging_git(&r);
    for _ in 0..2 {
        let (out, runs) = logged(&r, &log, &path, &["-l", "printk"]);
        assert!(runs.iter().any(|r| r.contains("status")), "{runs:?}");
        let text = stderr(&out);
        let short = &r.commit[..7];
        let want = format!("); asked git what differs from the served commit {short} (took ");
        assert!(
            text.starts_with(ASKED_GIT) && text.contains(&want) && text.lines().count() == 1,
            "{text}"
        );
    }
}

/// The config files that git reads are stamped, also one that
/// `include.path` brings in, and the excludes file that it sets: a change
/// to one makes the next search ask git again.
#[test]
fn a_changed_config_file_asks_git_again() {
    let r = Remote::new(&FILES);
    if r.daemon.is_none() {
        return;
    }
    let (log, path) = logging_git(&r);
    let (included, excludes) = (r.s.path("included"), r.s.path("excludes"));
    std::fs::write(&excludes, "").unwrap();
    let config = format!("[core]\n\texcludesFile = {}\n", excludes.display());
    std::fs::write(&included, config).unwrap();
    let include = included.display().to_string();
    r.s.git(&r.client, &["config", "include.path", &include]);
    let asks = |r: &Remote| {
        let (out, runs) = logged(r, &log, &path, &["-l", "printk"]);
        assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n");
        runs.iter().any(|r| r.contains("status"))
    };
    assert!(asks(&r), "the baseline");
    assert!(!asks(&r), "the record");
    for file in [&excludes, &included] {
        let mut text = std::fs::read_to_string(file).unwrap();
        text.push_str("# changed\n");
        std::fs::write(file, text).unwrap();
        assert!(asks(&r), "{} changed", file.display());
        assert!(!asks(&r), "the new record");
    }
}

//! A search through a server from a checkout, with a private watchman
//! daemon: git once, for the baseline, then watchman; and without one,
//! git on every search, with a warning that comes once.

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
    let log = r.s.path("git.log");
    let path = first_on_path(r, "bin", |real| {
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            real.display()
        )
    });
    (log, path)
}

/// A `git` in the directory `dir` of the sandbox, the shell script that
/// `script` writes around the real one; and the `PATH` that puts it
/// first.
fn first_on_path(r: &Remote, dir: &str, script: impl Fn(&Path) -> String) -> String {
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH");
    let bin = r.s.path(dir);
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("git"), script(&real)).unwrap();
    let mut mode = std::fs::metadata(bin.join("git")).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(bin.join("git"), mode).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    path.into_string().unwrap()
}

/// `args` through the server from the client's top, `git` logged: the
/// output and the git runs of the last try, by [`Remote::tried`].
fn logged(r: &Remote, log: &Path, path: &str, args: &[&str]) -> (Output, Vec<String>) {
    logged_env(r, log, path, &[], args)
}

/// [`logged`], with the variables `env` set.
fn logged_env(
    r: &Remote,
    log: &Path,
    path: &str,
    env: &[(&str, &str)],
    args: &[&str],
) -> (Output, Vec<String>) {
    let out = r.tried(|| {
        let _ = std::fs::remove_file(log);
        let mut cmd = r.s.fmgrep(&r.client);
        if let Some(daemon) = &r.daemon {
            cmd.env("WATCHMAN_SOCK", &daemon.sock);
        }
        cmd.env("FMGREP_SERVER", &r.addr)
            .env("PATH", path)
            .envs(env.iter().copied())
            .args(args)
            .output()
            .unwrap()
    });
    let runs = std::fs::read_to_string(log).unwrap_or_default();
    (out, runs.lines().map(str::to_owned).collect())
}

/// `--ignore-local-changes` runs no git, and asks nothing of watchman:
/// the server's answer alone, with an edit that a merged answer would
/// show, and no line about git or watchman.
#[test]
fn ignore_local_changes_runs_no_git() {
    let r = Remote::new(&FILES);
    let (log, path) = logging_git(&r);
    r.s.write(&r.client, &[("a.c", "none\n")]);
    let args = ["--ignore-local-changes", "-l", "printk"];
    for _ in 0..2 {
        let (out, runs) = logged(&r, &log, &path, &args);
        assert_eq!(
            (stdout(&out), stderr(&out), runs),
            (
                "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n".to_owned(),
                String::new(),
                Vec::<String>::new()
            )
        );
    }
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
    // A commit moves HEAD, a ref and the index, and the entries stay: git
    // lists them once to see so, and is not asked what differs.
    r.s.commit(c, "on top");
    let (out, runs) = logged(&r, &log, &path, &["-l", "printk"]);
    // The commit tracked the new files, so git also says whether it
    // converts them.
    checked_and_converts(&out, &bare(runs));
    assert_eq!(stdout(&out), stdout(&r.oracle("", &["-l", "printk"])));
}

/// The git runs of `runs` without the `-C <checkout>` that starts each.
fn bare(runs: Vec<String>) -> Vec<String> {
    let rest = |run: String| match run.strip_prefix("-C ").and_then(|r| r.split_once(' ')) {
        Some((_, rest)) => rest.to_owned(),
        None => run,
    };
    runs.into_iter().map(rest).collect()
}

/// The one git run that tells whether the index still holds what the
/// record saw: `git ls-files -z -s -v`.
const LS_FILES: &str = "ls-files -z -s -v";

/// A search from a checkout with a daemon: the baseline, with its one
/// warning that git was asked; then `setup` (before it) and `change`
/// (after it), and the search again, with the logging git. The answer is
/// the oracle's, and the output and the git runs are returned.
fn after(
    files: &[(&str, &str)],
    setup: impl FnOnce(&Remote, &Path),
    change: impl FnOnce(&Remote, &Path),
) -> Option<(Output, Vec<String>)> {
    let r = Remote::new(files);
    let daemon = r.daemon.as_ref()?;
    let c = &r.client;
    setup(&r, c);
    let (log, path) = logging_git(&r);
    let (first, _) = logged(&r, &log, &path, &["-l", "printk"]);
    asked_again(&r, &first, "no record of an earlier search");
    change(&r, c);
    daemon.watch(c);
    let (out, runs) = logged(&r, &log, &path, &["-l", "printk"]);
    assert_eq!(stdout(&out), stdout(&r.oracle("", &["-l", "printk"])));
    Some((out, bare(runs)))
}

/// What the search ran when the index moved but its entries did not: one
/// `ls-files`, no ask.
fn checked_only(out: &Output, runs: &[String]) {
    assert_eq!(stderr(out), "");
    assert_eq!(runs, [LS_FILES]);
}

/// What it ran when the entries changed with nothing that git refuses:
/// `ls-files`, then what finds whether git would convert a file; no
/// `status`.
fn checked_and_converts(out: &Output, runs: &[String]) {
    assert_eq!(stderr(out), "");
    assert_eq!(runs[0], LS_FILES);
    let asked = |r: &String| r.starts_with("status") || r.starts_with("diff-tree");
    assert!(!runs.iter().any(asked), "{runs:?}");
}

/// A stat-only rewrite of the index (`git status` after a touch) is no
/// reason to ask git what differs.
#[test]
fn a_status_after_a_touch_does_not_ask_git() {
    let touch = |r: &Remote, c: &Path| {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(c.join("a.c"))
            .unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        file.set_modified(later).unwrap();
        r.s.git(c, &["status", "--short"]);
    };
    if let Some((out, runs)) = after(&FILES, |_, _| {}, touch) {
        checked_only(&out, &runs);
    }
}

/// Staging a changed file, a commit, and a `reset --soft` after it: the
/// entries stay, and the last two move HEAD and a ref only.
#[test]
fn add_commit_and_reset_soft_do_not_ask_git() {
    let edit = |r: &Remote, c: &Path| r.s.write(c, &[("a.c", "none\n")]);
    let add = |r: &Remote, c: &Path| {
        edit(r, c);
        r.s.git(c, &["add", "a.c"]);
    };
    if let Some((out, runs)) = after(&FILES, |_, _| {}, add) {
        checked_only(&out, &runs);
    }
    let commit = |r: &Remote, c: &Path| {
        edit(r, c);
        r.s.commit(c, "on top");
    };
    if let Some((out, runs)) = after(&FILES, |_, _| {}, commit) {
        checked_only(&out, &runs);
    }
    // HEAD is already past the served commit in the record: the soft
    // reset moves a ref alone, and no git runs.
    let soft = |r: &Remote, c: &Path| {
        r.s.git(c, &["reset", "-q", "--soft", "HEAD~1"]);
    };
    let on_top = |r: &Remote, c: &Path| {
        edit(r, c);
        r.s.commit(c, "on top");
    };
    if let Some((out, runs)) = after(&FILES, on_top, soft) {
        assert_eq!((stderr(&out), runs), (String::new(), Vec::new()));
    }
}

/// A file that git ignores comes to be tracked (`add -f`), and another
/// stops being (`rm --cached`), and is edited after: the entries
/// differ, git is not asked, and every answer is the oracle's.
#[test]
fn tracking_and_untracking_a_file_does_not_ask_git() {
    let ignored = |r: &Remote, c: &Path| {
        r.s.write(
            c,
            &[
                (".git/info/exclude", "ign.txt\n"),
                ("ign.txt", "printk i\n"),
            ],
        );
    };
    let force = |r: &Remote, c: &Path| {
        r.s.git(c, &["add", "-f", "ign.txt"]);
    };
    if let Some((out, runs)) = after(&FILES, ignored, force) {
        checked_and_converts(&out, &runs);
    }
    let untrack = |r: &Remote, c: &Path| {
        r.s.git(c, &["rm", "-q", "--cached", "e.c"]);
        r.s.write(c, &[("e.c", "none\n")]);
    };
    if let Some((out, runs)) = after(&FILES, |_, _| {}, untrack) {
        checked_and_converts(&out, &runs);
    }
}

/// Another branch, and a rebase: git writes the files that differ and
/// watchman says so; the entries differ, and git is not asked.
#[test]
fn a_checkout_and_a_rebase_do_not_ask_git() {
    let side = |r: &Remote, c: &Path| {
        r.s.git(c, &["checkout", "-q", "-b", "side"]);
        r.s.write(c, &[("a.c", "none\n"), ("n.c", "printk n\n")]);
        r.s.commit(c, "side");
        r.s.git(c, &["checkout", "-q", "-"]);
    };
    let switch = |r: &Remote, c: &Path| {
        r.s.git(c, &["checkout", "-q", "side"]);
    };
    if let Some((out, runs)) = after(&FILES, side, switch) {
        checked_and_converts(&out, &runs);
    }
    let rebase = |r: &Remote, c: &Path| {
        r.s.write(c, &[("e.c", "none\n")]);
        r.s.commit(c, "main");
        r.s.git(c, &["rebase", "-q", "HEAD", "side"]);
    };
    if let Some((out, runs)) = after(&FILES, side, rebase) {
        checked_and_converts(&out, &runs);
    }
}

/// What a new ask would refuse is still refused: a skip-worktree bit set
/// later, a submodule added, and a file that an attribute converts, now
/// tracked. Each asks git, which refuses.
#[test]
fn what_a_new_ask_refuses_is_still_refused() {
    let hide = |r: &Remote, c: &Path| {
        r.s.git(c, &["update-index", "--skip-worktree", "e.c"]);
    };
    let refused = |out: &Output, runs: &[String], why: &str| {
        let line = format!("warning: local changes are ignored ({why})");
        assert!(stderr(out).starts_with(&line), "{}", stderr(out));
        assert!(runs.iter().any(|r| r.starts_with("status")), "{runs:?}");
    };
    if let Some((out, runs)) = after(&FILES, |_, _| {}, hide) {
        let why = "git hides changes to 1 file (assume-unchanged or skip-worktree)";
        refused(&out, &runs, why);
    }
    let submodule = |r: &Remote, c: &Path| {
        let cache = format!("160000,{},sub", r.commit);
        r.s.git(c, &["update-index", "--add", "--cacheinfo", &cache]);
    };
    if let Some((out, runs)) = after(&FILES, |_, _| {}, submodule) {
        refused(&out, &runs, "the checkout holds a submodule (sub)");
    }
    let mut files = FILES.to_vec();
    files.push((".gitattributes", "*.txt text\n"));
    let untracked = |r: &Remote, c: &Path| r.s.write(c, &[("n.txt", "printk n\n")]);
    let r = Remote::new(&files);
    if let Some(daemon) = &r.daemon {
        let c = &r.client;
        untracked(&r, c);
        let (log, path) = logging_git(&r);
        logged(&r, &log, &path, &["-l", "printk"]);
        r.s.git(c, &["add", "n.txt"]);
        daemon.watch(c);
        let (out, runs) = logged(&r, &log, &path, &["-l", "printk"]);
        let why = "git converts files (the attribute text applies to n.txt)";
        refused(&out, &bare(runs), why);
    }
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

/// `args` through the server from the client's top with no daemon to ask
/// (`WATCHMAN_SOCK` names `sock`, where none listens), `git` logged: the
/// output and the git runs.
fn unwatched(
    r: &Remote,
    log: &Path,
    path: &str,
    sock: &Path,
    env: &[(&str, &str)],
    args: &[&str],
) -> (Output, Vec<String>) {
    let _ = std::fs::remove_file(log);
    let out = (r.s.fmgrep(&r.client))
        .env("WATCHMAN_SOCK", sock)
        .env("FMGREP_SERVER", &r.addr)
        .env("PATH", path)
        .envs(env.iter().copied())
        .args(args)
        .output()
        .unwrap();
    let runs = std::fs::read_to_string(log).unwrap_or_default();
    (out, runs.lines().map(str::to_owned).collect())
}

/// Without watchman each search asks git, and the line that says so comes
/// once for each reason and served commit: not with the second search,
/// again when the reason is another. `FMGREP_VERBOSE=1` says it every
/// time. Each search answers the same.
#[test]
fn without_watchman_every_search_asks_git_and_says_so_once() {
    let r = Remote::new(&FILES);
    let (log, path) = logging_git(&r);
    let (none, other) = (r.s.path("no-daemon"), r.s.path("other-daemon"));
    let search = |sock: &Path, env: &[(&str, &str)]| {
        let (out, runs) = unwatched(&r, &log, &path, sock, env, &["-l", "printk"]);
        assert!(runs.iter().any(|r| r.contains("status")), "{runs:?}");
        assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n");
        stderr(&out)
    };
    let verbose = [("FMGREP_VERBOSE", "1")];
    let lines = |text: &str| text.lines().filter(|l| l.starts_with(ASKED_GIT)).count();
    let says = |text: &str, sock: &Path| {
        let short = &r.commit[..7];
        let why = format!("no watchman daemon answers on {} (", sock.display());
        let want = format!("{ASKED_GIT}{why}");
        let took = format!("); asked git what differs from the served commit {short} (took ");
        let line = text.lines().find(|l| l.starts_with(ASKED_GIT));
        line.is_some_and(|l| l.starts_with(&want) && l.contains(&took)) && lines(text) == 1
    };
    let first = search(&none, &[]);
    assert!(says(&first, &none) && first.lines().count() == 1, "{first}");
    assert_eq!(search(&none, &[]), "", "the second search");
    assert_eq!(search(&none, &[]), "", "and the third");
    for _ in 0..2 {
        let text = search(&none, &verbose);
        assert!(says(&text, &none), "{text}");
    }
    assert_eq!(search(&none, &[]), "", "said, and verbose changed nothing");
    // Another reason: the daemon that cannot answer is another one.
    let text = search(&other, &[]);
    assert!(says(&text, &other) && text.lines().count() == 1, "{text}");
    assert_eq!(search(&other, &[]), "");
    let text = search(&none, &[]);
    assert!(says(&text, &none), "back to the first reason: {text}");
}

/// The git commands of `runs`, as the word after `-C <dir>`.
fn commands(runs: &[String]) -> Vec<&str> {
    runs.iter().filter_map(|r| r.split(' ').nth(2)).collect()
}

/// One ask of git runs these commands and no more: the count only goes
/// down. Without watchman, and for the first search with it.
#[test]
fn one_ask_of_git_runs_few_processes() {
    let r = Remote::new(&FILES);
    let (log, path) = logging_git(&r);
    let sock = r.s.path("no-daemon");
    let (out, runs) = unwatched(&r, &log, &path, &sock, &[], &["-l", "printk"]);
    assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n");
    let want = [
        "cat-file",
        "status",
        "ls-files",
        "config",
        "ls-files",
        "ls-files",
        "rev-parse",
    ];
    assert_eq!(commands(&runs), want, "without watchman: {runs:?}");
    if r.daemon.is_none() {
        return;
    }
    let (out, runs) = logged(&r, &log, &path, &["-l", "printk"]);
    assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n");
    let want = [
        "cat-file",
        "config",
        "rev-parse",
        "var",
        "var",
        "status",
        "ls-files",
        "ls-files",
        "ls-files",
        "config",
    ];
    assert_eq!(commands(&runs), want, "with watchman: {runs:?}");
}

/// A watchman that answers in between makes the line come again once it
/// cannot answer: the record, and the note of what was said, go with it.
#[test]
fn the_line_returns_once_watchman_has_answered_and_gone() {
    let r = Remote::new(&FILES);
    if r.daemon.is_none() {
        return;
    }
    let (log, path) = logging_git(&r);
    let none = r.s.path("no-daemon");
    let search = |sock: &Path| {
        let (out, _) = unwatched(&r, &log, &path, sock, &[], &["-l", "printk"]);
        assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n");
        stderr(&out)
    };
    assert!(search(&none).starts_with(ASKED_GIT));
    assert_eq!(search(&none), "");
    let (out, _) = logged(&r, &log, &path, &["-l", "printk"]);
    asked_again(&r, &out, "no record of an earlier search");
    let (out, runs) = logged(&r, &log, &path, &["-l", "printk"]);
    assert_eq!((stderr(&out), runs), (String::new(), Vec::new()));
    assert!(search(&none).starts_with(ASKED_GIT), "watchman went away");
    assert_eq!(search(&none), "");
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

/// A config file that `include.path` or `includeIf` names is stamped
/// even when it is missing or empty, and so are the worktree's own config
/// file and the system one: once one sets `core.autocrlf`, the next
/// search asks git again, and falls back with the warning.
#[test]
fn a_config_file_without_settings_is_stamped() {
    let r = Remote::new(&FILES);
    if r.daemon.is_none() {
        return;
    }
    let (log, path) = logging_git(&r);
    let search = |r: &Remote| {
        let (out, runs) = logged(r, &log, &path, &["-l", "printk"]);
        assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n");
        (runs.iter().any(|r| r.contains("status")), stderr(&out))
    };
    let git = r.client.join(".git");
    let at = |rel: &str| r.s.path(rel).display().to_string();
    let gitdir = format!("includeIf.gitdir:{}/.path", r.client.display());
    std::fs::write(r.s.path("empty"), "").unwrap();
    let outer = format!("[include]\n\tpath = {}\n", at("inner"));
    std::fs::write(r.s.path("outer"), outer).unwrap();
    // The key and value set, and the file whose text decides.
    let cases = [
        ("include.path", at("missing"), r.s.path("missing")),
        ("include.path", at("empty"), r.s.path("empty")),
        ("include.path", "relative".into(), git.join("relative")),
        ("include.path", "~/at-home".into(), r.s.path("home/at-home")),
        (&gitdir, at("if"), r.s.path("if")),
        ("include.path", at("outer"), r.s.path("inner")),
    ];
    let converts = "warning: local changes are ignored (git converts files (core.autocrlf=true))";
    for (key, value, file) in cases {
        r.s.git(&r.client, &["config", "--add", key, &value]);
        assert!(search(&r).0, "the baseline");
        assert!(!search(&r).0, "the record");
        std::fs::write(&file, "[core]\n\tautocrlf = true\n").unwrap();
        let (asked, text) = search(&r);
        assert!(asked && text.starts_with(converts), "{key} {value}: {text}");
        std::fs::write(&file, "").unwrap();
    }
    // The worktree's config file, missing, and the system one, empty.
    r.s.git(&r.client, &["config", "extensions.worktreeConfig", "true"]);
    for file in [git.join("config.worktree"), r.s.path("gitconfig-system")] {
        assert!(search(&r).0, "the baseline");
        assert!(!search(&r).0, "the record");
        std::fs::write(&file, "[core]\n\tautocrlf = true\n").unwrap();
        let (asked, text) = search(&r);
        assert!(
            asked && text.starts_with(converts),
            "{}: {text}",
            file.display()
        );
        std::fs::write(&file, "").unwrap();
    }
    // The attributes file that one of them sets, missing.
    let attributes = r.s.path("attributes").display().to_string();
    r.s.git(&r.client, &["config", "core.attributesFile", &attributes]);
    assert!(search(&r).0, "the baseline");
    assert!(!search(&r).0, "the record");
    std::fs::write(&attributes, "*.c text\n").unwrap();
    let (asked, text) = search(&r);
    let applies = "warning: local changes are ignored (git converts files (the attribute text \
                   applies to a.c))";
    assert!(asked && text.starts_with(applies), "{text}");
}

/// A file that `include.path` names in place of another while git runs
/// for the baseline: the client asks git for its settings again once git
/// is done, saves no record when they differ, and says so; the next
/// search asks git again and sees what that file sets.
#[test]
fn a_config_file_listed_while_git_runs_is_seen() {
    let r = Remote::new(&FILES);
    if r.daemon.is_none() {
        return;
    }
    let (log, path) = logging_git(&r);
    let (a, b) = (r.s.path("a").display().to_string(), r.s.path("b"));
    r.s.git(&r.client, &["config", "include.path", &a]);
    // Right after git lists its config files, `b` replaces `a`.
    let client = r.client.display().to_string();
    let racing = first_on_path(&r, "racing", |real| {
        let (real, b) = (real.display(), b.display());
        let listed = r.s.path("listed");
        let listed = listed.display();
        format!(
            "#!/bin/sh\ncase \"$*\" in\n*--show-origin*)\n    '{real}' \"$@\" > '{listed}'; rc=$?\n    \
             '{real}' -C '{client}' config include.path '{b}'\n    cat '{listed}'; exit $rc ;;\n\
             esac\nexec '{real}' \"$@\"\n"
        )
    });
    let (first, _) = logged(&r, &log, &racing, &["-l", "printk"]);
    assert_eq!(stdout(&first), "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n");
    std::fs::write(&b, "[core]\n\tautocrlf = true\n").unwrap();
    let (out, runs) = logged(&r, &log, &path, &["-l", "printk"]);
    let converts = "warning: local changes are ignored (git converts files (core.autocrlf=true))";
    let text = stderr(&out);
    assert!(
        runs.iter().any(|r| r.contains("status")) && text.starts_with(converts),
        "{text}"
    );
    let text = stderr(&first);
    let tail = "; git's config files changed while git ran; the next search asks git again\n";
    assert!(text.ends_with(tail), "{text}");
}

/// The variables that choose which config and attributes files git reads
/// are kept in the record: a search under another value of one asks git
/// again and names it, and one that turns on a system config file that
/// sets `core.autocrlf` falls back with the warning.
#[test]
fn another_choice_of_config_files_asks_git_again() {
    let r = Remote::new(&FILES);
    if r.daemon.is_none() {
        return;
    }
    let (log, path) = logging_git(&r);
    let search = |env: &[(&str, &str)]| {
        let (out, runs) = logged_env(&r, &log, &path, env, &["-l", "printk"]);
        assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n");
        (runs.iter().any(|r| r.contains("status")), out)
    };
    let system = r.s.path("gitconfig-system");
    std::fs::write(&system, "[core]\n\tautocrlf = true\n").unwrap();
    let off = [("GIT_CONFIG_NOSYSTEM", "1")];
    assert!(search(&off).0, "the baseline");
    assert!(!search(&off).0, "the record");
    let (asked, out) = search(&[]);
    let converts = "warning: local changes are ignored (git converts files (core.autocrlf=true))";
    assert!(
        asked && stderr(&out).starts_with(converts),
        "{}",
        stderr(&out)
    );
    std::fs::write(&system, "").unwrap();
    assert!(search(&[]).0, "the baseline");
    let (empty, other) = (r.s.path("empty"), r.s.path("other"));
    std::fs::write(&empty, "").unwrap();
    std::fs::create_dir(&other).unwrap();
    let (empty, other) = (empty.display().to_string(), other.display().to_string());
    let mut env = Vec::new();
    for (var, value) in [
        ("GIT_CONFIG_SYSTEM", empty.as_str()),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", empty.as_str()),
        ("HOME", other.as_str()),
        ("XDG_CONFIG_HOME", other.as_str()),
        ("GIT_ATTR_NOSYSTEM", "1"),
        ("GIT_ATTR_SOURCE", "HEAD"),
    ] {
        env.push((var, value));
        let (asked, out) = search(&env);
        assert!(asked, "{var}");
        asked_again(&r, &out, &format!("{var} changed"));
        assert!(!search(&env).0, "{var}: the new record");
    }
}

/// With `GIT_ATTR_SOURCE` set, attributes come from a tree, so a commit
/// can change what converts: git is asked again.
#[test]
fn a_commit_asks_git_again_while_attributes_follow_head() {
    let r = Remote::new(&FILES);
    let Some(daemon) = &r.daemon else {
        return;
    };
    let (log, path) = logging_git(&r);
    let env = [("GIT_ATTR_SOURCE", "HEAD")];
    logged_env(&r, &log, &path, &env, &["-l", "printk"]);
    r.s.commit(&r.client, "again");
    daemon.watch(&r.client);
    let (out, runs) = logged_env(&r, &log, &path, &env, &["-l", "printk"]);
    assert!(bare(runs).iter().any(|r| r.starts_with("status")));
    assert_eq!(stdout(&out), stdout(&r.oracle("", &["-l", "printk"])));
}

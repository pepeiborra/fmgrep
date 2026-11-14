//! A search from a git checkout through a server of a `--store-docs`
//! index of the served commit, on real repositories: the answer and the
//! warnings.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::git::Sandbox;

pub const FILES: [(&str, &str); 4] = [
    ("a.c", "printk a\nother\n"),
    ("dir/b.c", "printk b\n"),
    ("dir/sub/c.h", "printk c\n"),
    ("e.c", "printk e printk\n"),
];

/// A repository at the served commit, its `--store-docs` index served,
/// and a clone of it: the client's checkout.
pub struct Remote {
    pub s: Sandbox,
    pub origin: PathBuf,
    pub commit: String,
    pub _server: crate::Server,
    pub addr: String,
    pub client: PathBuf,
}

impl Remote {
    pub fn new(files: &[(&str, &str)]) -> Self {
        let s = Sandbox::new();
        let origin = s.repo("origin", files);
        let commit = s.head(&origin);
        let (index, note) = s.index(&origin, "idx");
        assert!(note.contains(&commit), "{note}");
        let (server, addr) = s.serve(&index, &[]);
        let client = s.path("client");
        s.git(
            s.tmp.path(),
            &["clone", "-q", &origin.display().to_string(), "client"],
        );
        let client = std::fs::canonicalize(client).unwrap();
        Self {
            s,
            origin,
            commit,
            _server: server,
            addr,
            client,
        }
    }

    /// `args` through the server from `cwd`, with `FMGREP_SERVER`.
    pub fn search(&self, cwd: &Path, args: &[&str]) -> Output {
        self.s
            .fmgrep(cwd)
            .env("FMGREP_SERVER", &self.addr)
            .args(args)
            .output()
            .expect("spawn fmgrep")
    }

    /// What a local index of the client's checkout prints for `args` from
    /// `rel` below its top: a copy of the checkout, `.git` included, is
    /// indexed, as an index inside the checkout would win over the server.
    pub fn oracle(&self, rel: &str, args: &[&str]) -> Output {
        let copy = self.s.path("oracle");
        let _ = std::fs::remove_dir_all(&copy);
        let status = std::process::Command::new("cp")
            .arg("-R")
            .arg(&self.client)
            .arg(&copy)
            .status()
            .unwrap();
        assert!(status.success());
        let index = self.s.run(&copy, &["index", "."]);
        assert!(index.status.success(), "{}", stderr(&index));
        self.s.run(&copy.join(rel), args)
    }

    /// `args` from `rel` below the client's top, through the server and
    /// as the oracle: the same stdout, timings and the index's own figures
    /// aside, the same exit status, and on stderr the same error or none.
    pub fn same(&self, rel: &str, args: &[&str]) {
        let want = self.oracle(rel, args);
        let got = self.search(&self.client.join(rel), args);
        // The copy's index warns that it holds a changed ignore file.
        let errors: Vec<String> = stderr(&want)
            .lines()
            .filter(|l| !l.starts_with("warning:"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(stderr(&got), errors.concat(), "{rel} {args:?}");
        assert_eq!(
            (comparable(&stdout(&got)), got.status.code()),
            (comparable(&stdout(&want)), want.status.code()),
            "{rel} {args:?}: {}",
            stderr(&want)
        );
    }
}

/// `stdout` without what differs from run to run or from index to index:
/// timings, and the index's file count and size.
pub fn comparable(stdout: &str) -> String {
    let re = regex::Regex::new(
        r#""(latency_us|index_time_us|elapsed|elapsed_total|num_files_indexed|index_size_bytes)":(\d+|\{[^}]*\})"#,
    )
    .unwrap();
    re.replace_all(stdout, "\"$1\":0").into_owned()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The one warning of a search whose local changes are ignored.
pub fn ignored(why: &str) -> String {
    format!("warning: local changes are ignored ({why}); these results are the server's alone\n")
}

/// Where the client cannot compare its checkout with the served commit,
/// it prints the server's answer and one warning that says why.
#[test]
fn a_search_that_cannot_compare_its_checkout_warns_once() {
    let r = Remote::new(&FILES);
    let all = "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n";

    // Outside any checkout.
    let away = std::fs::canonicalize(r.s.path("home")).unwrap();
    let out = r.search(&away, &["-l", "printk"]);
    assert_eq!(stdout(&out), all);
    let why = format!("{} is in no git checkout", away.display());
    assert_eq!(stderr(&out), ignored(&why));

    // Another repository, which lacks the served commit.
    let other = r.s.repo("other", &[("x.c", "printk x\n")]);
    let out = r.search(&other, &["-l", "printk"]);
    assert_eq!(stdout(&out), all);
    let short = &r.commit[..7];
    let why = format!(
        "this checkout does not have the served commit {short}; run git fetch origin {}, unless \
         this is another repository",
        r.commit
    );
    assert_eq!(stderr(&out), ignored(&why));

    // A shallow clone of a later commit, which lacks it too.
    r.s.write(&r.origin, &[("a.c", "printk a2\n")]);
    r.s.commit(&r.origin, "second");
    let url = format!("file://{}", r.origin.display());
    r.s.git(
        r.s.tmp.path(),
        &["clone", "-q", "--depth", "1", &url, "shallow"],
    );
    let shallow = std::fs::canonicalize(r.s.path("shallow")).unwrap();
    let out = r.search(&shallow, &["-l", "printk"]);
    assert_eq!(stderr(&out), ignored(&why));

    // No git to ask.
    let out =
        r.s.fmgrep(&r.client)
            .env("FMGREP_SERVER", &r.addr)
            .env("PATH", r.s.path("home"))
            .args(["-l", "printk"])
            .output()
            .unwrap();
    assert_eq!(stdout(&out), all);
    assert!(
        stderr(&out).starts_with("warning: local changes are ignored (cannot run git ("),
        "{}",
        stderr(&out)
    );

    // A clone that has the commit: nothing to say.
    let out = r.search(&r.client, &["-l", "printk"]);
    assert_eq!(
        (stdout(&out), stderr(&out)),
        (all.to_owned(), String::new())
    );
}

/// A served commit that is not a git commit id is never asked of git,
/// even when the checkout has a commit of that id: the search is the
/// server's alone, with the warning.
#[test]
fn a_served_commit_of_another_vcs_is_not_compared() {
    let s = Sandbox::new();
    let origin = s.repo("origin", &FILES);
    let commit = s.head(&origin);
    let (index, _) = s.index(&origin, "idx");
    let manifest = index.join("manifest.bin");
    let mut raw = std::fs::read(&manifest).unwrap();
    let git = format!("git {commit}");
    let at = raw
        .windows(git.len())
        .position(|w| w == git.as_bytes())
        .expect("the served commit in manifest.bin");
    raw[at..at + 3].copy_from_slice(b"svn");
    std::fs::write(&manifest, raw).unwrap();
    let (_server, addr) = s.serve(&index, &[]);
    s.git(
        s.tmp.path(),
        &["clone", "-q", &origin.display().to_string(), "client"],
    );
    let client = std::fs::canonicalize(s.path("client")).unwrap();
    let out = s
        .fmgrep(&client)
        .env("FMGREP_SERVER", &addr)
        .args(["-l", "printk"])
        .output()
        .unwrap();
    assert_eq!(stdout(&out), "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n");
    let why = format!(
        "the server at {addr} (FMGREP_SERVER) reports a served commit that is not a git commit id"
    );
    assert_eq!(stderr(&out), ignored(&why));
}

/// A server that reports no served commit, searched from a checkout
/// elsewhere: the warning; from its own tree or from no checkout: none.
#[test]
fn a_server_without_a_served_commit_warns_a_checkout_elsewhere() {
    let s = Sandbox::new();
    let origin = s.repo("origin", &FILES);
    let output = s.path("idx").display().to_string();
    assert!(
        s.run(&origin, &["index", ".", "--output", &output])
            .status
            .success()
    );
    let (_server, addr) = s.serve(&s.path("idx"), &["--no-watch"]);
    let client = s.repo("client", &FILES);
    let search = |cwd: &Path| {
        let out = s
            .fmgrep(cwd)
            .env("FMGREP_SERVER", &addr)
            .args(["-l", "printk"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        stderr(&out)
    };
    let why = format!("the server at {addr} (FMGREP_SERVER) reports no served commit");
    assert_eq!(search(&client), ignored(&why));
    assert_eq!(search(&origin), "");
    assert_eq!(search(&std::fs::canonicalize(s.path("home")).unwrap()), "");
}

/// A checkout at another path than the served tree: PATH arguments and
/// the cwd resolve against its top, and print as a local search does.
#[test]
fn paths_and_the_cwd_resolve_against_the_checkout() {
    let r = Remote::new(&FILES);
    for (rel, args) in [
        ("", &["printk"][..]),
        ("dir", &["-l", "printk"]),
        ("dir/sub", &["printk"]),
        ("dir", &["-l", "printk", "../a.c"]),
        ("dir", &["-c", "printk", "sub", "../e.c"]),
        ("", &["printk", "dir/b.c"]),
        ("dir", &["-l", "-g", "*.h", "printk"]),
        ("dir", &["-l", "printk", ".."]),
    ] {
        r.same(rel, args);
    }
    let out = r.search(&r.client.join("dir"), &["-l", "printk", "../../home"]);
    assert_eq!(out.status.code(), Some(2));
    let want = format!(
        "error: ../../home is outside the checkout at {}\n",
        r.client.display()
    );
    assert_eq!(stderr(&out), want);
}

/// A checkout that lacks the served commit is still where the developer
/// is: its cwd scopes the server's answer, and its PATHs resolve against
/// its top, as a local search of the checkout reads them.
#[test]
fn a_checkout_that_lacks_the_commit_keeps_its_cwd_and_paths() {
    let r = Remote::new(&FILES);
    r.s.write(&r.origin, &[("a.c", "printk a2\n")]);
    r.s.commit(&r.origin, "second");
    let url = format!("file://{}", r.origin.display());
    r.s.git(
        r.s.tmp.path(),
        &["clone", "-q", "--depth", "1", &url, "shallow"],
    );
    let dir = std::fs::canonicalize(r.s.path("shallow/dir")).unwrap();
    let short = &r.commit[..7];
    let why = format!(
        "this checkout does not have the served commit {short}; run git fetch origin {}, unless \
         this is another repository",
        r.commit
    );
    for (args, want) in [
        (&["-l", "printk"][..], "b.c\nsub/c.h\n"),
        (&["-l", "printk", "sub"], "sub/c.h\n"),
        (&["-l", "printk", "../a.c", "sub"], "../a.c\nsub/c.h\n"),
    ] {
        let out = r.search(&dir, args);
        assert_eq!(stderr(&out), ignored(&why), "{args:?}");
        assert_eq!((stdout(&out).as_str(), out.status.code()), (want, Some(0)));
    }
}

/// A search whose globs keep it from every file fails as a local search
/// does, and prints nothing on stdout, also with `--json`: from a clone of
/// the served commit, and from a checkout that lacks it, after the warning.
#[test]
fn a_search_of_no_file_fails_as_a_local_one() {
    let r = Remote::new(&FILES);
    let error = "error: no files were searched: the globs exclude every file";
    let modes: [&[&str]; 3] = [&["-l"], &["--json"], &["--json", "-c"]];
    let args = |mode: &[&'static str]| [mode, &["-g", "*.none", "printk"]].concat();
    for mode in modes {
        let out = r.search(&r.client, &args(mode));
        assert_eq!((stdout(&out), out.status.code()), (String::new(), Some(2)));
        assert_eq!(stderr(&out).lines().last(), Some(error), "{}", stderr(&out));
        assert!(!stderr(&out).contains("ignored"), "{}", stderr(&out));
    }

    r.s.write(&r.origin, &[("a.c", "printk a2\n")]);
    r.s.commit(&r.origin, "second");
    let url = format!("file://{}", r.origin.display());
    r.s.git(
        r.s.tmp.path(),
        &["clone", "-q", "--depth", "1", &url, "shallow"],
    );
    let shallow = std::fs::canonicalize(r.s.path("shallow")).unwrap();
    let why = format!(
        "this checkout does not have the served commit {}; run git fetch origin {}, unless this \
         is another repository",
        &r.commit[..7],
        r.commit
    );
    for mode in modes {
        let out = r.search(&shallow, &args(mode));
        assert_eq!(
            (stdout(&out), stderr(&out), out.status.code()),
            (
                String::new(),
                format!("{}{error}\n", ignored(&why)),
                Some(2)
            )
        );
    }
}

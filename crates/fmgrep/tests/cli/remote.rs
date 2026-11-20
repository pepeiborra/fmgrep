//! A search from a git checkout through a server of a `--store-docs`
//! index of the served commit, on real repositories: the answer and the
//! warnings.

use std::io::Write as _;
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
    /// A private watchman daemon that watches the client, when one can
    /// start; without it, every search asks git and says so.
    pub daemon: Option<crate::watchman::Daemon>,
}

/// The start of the warning of a search that asks git because watchman
/// cannot say what changed.
pub const ASKED_GIT: &str = "warning: watchman cannot say what changed (";

/// The start of the warning of a search that asks git for a new baseline.
pub const ASKED_AGAIN: &str = "warning: asked git what differs from the served commit ";

/// What a search says when its calls to watchman took longer than its
/// budget: `watchman did not answer within ...`, or `watchman
/// get-sockname did not answer within ...`.
const LATE: &str = " did not answer within ";

/// How many times [`Remote::tried`] runs a search that came late, at most.
const TRIES: usize = 10;

/// The files below `dir`, to put back.
fn cached(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for path in entries.map(|e| e.unwrap().path()) {
        match path.is_dir() {
            true => files.extend(cached(&path)),
            false => files.push((path.clone(), std::fs::read(path).unwrap())),
        }
    }
    files
}

/// The files below `dir` as `files` holds them.
fn put_back(dir: &Path, files: &[(PathBuf, Vec<u8>)]) {
    for (path, _) in cached(dir) {
        if files.iter().all(|(p, _)| *p != path) {
            std::fs::remove_file(path).unwrap();
        }
    }
    for (path, bytes) in files {
        std::fs::write(path, bytes).unwrap();
    }
}

impl Remote {
    pub fn new(files: &[(&str, &str)]) -> Self {
        Self::after(&[], files)
    }

    /// The served commit holds `files`, on top of a commit that holds
    /// `first` (none when empty).
    pub fn after(first: &[(&str, &str)], files: &[(&str, &str)]) -> Self {
        let s = Sandbox::new();
        let origin = match first {
            [] => s.repo("origin", files),
            first => {
                let origin = s.repo("origin", first);
                for entry in std::fs::read_dir(&origin).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_name() != ".git" {
                        let path = entry.path();
                        match path.is_dir() {
                            true => std::fs::remove_dir_all(path).unwrap(),
                            false => std::fs::remove_file(path).unwrap(),
                        }
                    }
                }
                s.write(&origin, files);
                s.commit(&origin, "served");
                origin
            }
        };
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
        let daemon = crate::watchman::Daemon::start();
        if let Some(daemon) = &daemon {
            daemon.watch(&client);
        }
        Self {
            s,
            origin,
            commit,
            _server: server,
            addr,
            client,
            daemon,
        }
    }

    /// `args` through the server from `cwd`, with `FMGREP_SERVER`, and the
    /// private daemon's socket when there is one; by [`Self::tried`].
    pub fn search(&self, cwd: &Path, args: &[&str]) -> Output {
        self.through(&self.addr, cwd, args)
    }

    /// [`Self::search`] through the server at `addr`.
    pub fn through(&self, addr: &str, cwd: &Path, args: &[&str]) -> Output {
        self.tried(|| self.once(addr, cwd, args))
    }

    /// One try of [`Self::through`].
    pub fn once(&self, addr: &str, cwd: &Path, args: &[&str]) -> Output {
        self.command(addr, cwd, args)
            .output()
            .expect("spawn fmgrep")
    }

    /// `args` from the client's top, once, and the `query` request it sent
    /// to the server, seen through a proxy that passes the connection on.
    fn query_sent(&self, args: &[&str]) -> (Output, serde_json::Value) {
        use std::io::BufRead as _;
        use std::net::{Shutdown, TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = listener.local_addr().unwrap().to_string();
        let server = self.addr.clone();
        let spy = std::thread::spawn(move || {
            let (client, _) = listener.accept().unwrap();
            let upstream = TcpStream::connect(server).unwrap();
            let (mut from, mut to) = (upstream.try_clone().unwrap(), client.try_clone().unwrap());
            let back = std::thread::spawn(move || std::io::copy(&mut from, &mut to));
            let (mut lines, mut input) = (Vec::new(), std::io::BufReader::new(client));
            loop {
                let mut line = String::new();
                if input.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                (&upstream).write_all(line.as_bytes()).unwrap();
                lines.push(line);
            }
            upstream.shutdown(Shutdown::Write).unwrap();
            back.join().unwrap().unwrap();
            lines
        });
        let out = self.once(&proxy, &self.client, args);
        let query = spy
            .join()
            .unwrap()
            .into_iter()
            .find(|l| l.contains(r#""method":"query""#))
            .expect("a query request");
        (out, serde_json::from_str(&query).unwrap())
    }

    /// The command of [`Self::once`].
    pub fn command(&self, addr: &str, cwd: &Path, args: &[&str]) -> std::process::Command {
        let mut cmd = self.s.fmgrep(cwd);
        if let Some(daemon) = &self.daemon {
            cmd.env("WATCHMAN_SOCK", &daemon.sock);
        }
        cmd.env("FMGREP_SERVER", addr).args(args);
        cmd
    }

    /// `run`, a search that asks the private daemon, run again while it
    /// came late, at most [`TRIES`] times, as the library tests' `tried`
    /// does: on a loaded machine the client's budget for its calls to
    /// watchman can run out; the search then asks git and says that
    /// watchman did not answer, which a test of the ask does not mean to
    /// test. Each late try is undone: the files of the cache directory,
    /// the records, go back as they were. Each late try must answer as the
    /// last one. Fails, with the late warning, when every try came late.
    pub fn tried(&self, mut run: impl FnMut() -> Output) -> Output {
        let cache = self.s.path("cache");
        let mut lates: Vec<Output> = Vec::new();
        loop {
            let records = cached(&cache);
            let out = run();
            if self.daemon.is_none() || !stderr(&out).contains(LATE) {
                let answer = |o: &Output| (comparable(&stdout(o)), o.status.code());
                for late in &lates {
                    assert_eq!(answer(late), answer(&out), "a late search answers the same");
                }
                return out;
            }
            assert!(
                lates.len() + 1 < TRIES,
                "{TRIES} tries came late: {}",
                stderr(&out)
            );
            put_back(&cache, &records);
            lates.push(out);
            // Past the test harness's capture, so that a run's log counts
            // them.
            let test = std::thread::current().name().unwrap_or("?").to_owned();
            let _ = writeln!(std::io::stderr(), "{test}: try {} came late", lates.len());
        }
    }

    /// The stderr of `out`, less the one line that says git was asked:
    /// without a daemon, because watchman cannot say; with one, for a new
    /// baseline. The other line stays, to fail the test.
    pub fn warnings(&self, out: &Output) -> String {
        let text = stderr(out);
        let asked = match self.daemon {
            Some(_) => ASKED_AGAIN,
            None => ASKED_GIT,
        };
        let n = text.lines().filter(|l| l.starts_with(asked)).count();
        assert!(n <= 1, "{text}");
        text.lines()
            .filter(|l| !l.starts_with(asked))
            .map(|l| format!("{l}\n"))
            .collect()
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
    /// The status is returned.
    pub fn same(&self, rel: &str, args: &[&str]) -> Option<i32> {
        let want = self.oracle(rel, args);
        let got = self.search(&self.client.join(rel), args);
        // The copy's index warns that it holds a changed ignore file.
        let errors: Vec<String> = stderr(&want)
            .lines()
            .filter(|l| !l.starts_with("warning:"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(self.warnings(&got), errors.concat(), "{rel} {args:?}");
        assert_eq!(
            (comparable(&stdout(&got)), got.status.code()),
            (comparable(&stdout(&want)), want.status.code()),
            "{rel} {args:?}: {}",
            stderr(&want)
        );
        want.status.code()
    }
}

/// `stdout` without what differs from run to run or from index to index:
/// timings (of `--json` and `--stats`), and the index's file count and size.
pub fn comparable(stdout: &str) -> String {
    let re = regex::Regex::new(
        r#""(latency_us|index_time_us|elapsed|elapsed_total|num_files_indexed|index_size_bytes)":(\d+|\{[^}]*\})"#,
    )
    .unwrap();
    let seconds =
        regex::Regex::new(r"(?m)^\d+\.\d+ (seconds spent searching|seconds total)$").unwrap();
    let stdout = re.replace_all(stdout, "\"$1\":0");
    seconds.replace_all(&stdout, "0 $1").into_owned()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The one warning of a search whose local changes are ignored.
pub fn ignored(why: &str) -> String {
    format!(
        "warning: local changes are ignored ({why}); these results are the server's alone; use \
         --ignore-local-changes to avoid this warning\n"
    )
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

    // A clone that has the commit, where git fails: git's own words, and
    // no advice to fetch.
    r.s.git(
        r.s.tmp.path(),
        &["clone", "-q", &r.origin.display().to_string(), "broken"],
    );
    let broken = std::fs::canonicalize(r.s.path("broken")).unwrap();
    let config = broken.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("[core\n");
    std::fs::write(&config, &text).unwrap();
    let out = r.search(&broken, &["-l", "printk"]);
    assert_eq!(stdout(&out), all);
    let line = text.lines().count();
    let why = format!("git failed: fatal: bad config line {line} in file .git/config");
    assert_eq!(stderr(&out), ignored(&why));

    // A clone that has the commit: nothing to say.
    let out = r.search(&r.client, &["-l", "printk"]);
    assert_eq!(
        (stdout(&out), r.warnings(&out)),
        (all.to_owned(), String::new())
    );
}

/// `--ignore-local-changes` prints the server's answer alone, with no
/// warning: what the fallback prints, outside a checkout, in another
/// repository, and in a clone whose changes would be merged; from below
/// the clone's top, PATHs and the cwd resolve against it, as in the
/// fallback.
#[test]
fn ignore_local_changes_prints_the_servers_answer_alone() {
    let r = Remote::new(&FILES);
    let all = "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n";
    let away = std::fs::canonicalize(r.s.path("home")).unwrap();
    let other = r.s.repo("other", &[("x.c", "printk x\n")]);
    r.s.write(&r.client, &[("a.c", "none\n"), ("new.c", "printk new\n")]);
    for cwd in [&away, &other, &r.client] {
        let out = r.search(cwd, &["--ignore-local-changes", "-l", "printk"]);
        assert_eq!(
            (stdout(&out), stderr(&out), out.status.code()),
            (all.to_owned(), String::new(), Some(0)),
            "{cwd:?}"
        );
    }
    let dir = r.client.join("dir");
    let args = ["--ignore-local-changes", "-l", "printk", "../a.c", "sub"];
    let out = r.search(&dir, &args);
    assert_eq!(
        (stdout(&out), stderr(&out)),
        ("../a.c\nsub/c.h\n".into(), String::new())
    );
}

/// A search asks for the server's output as it prints, in frames of
/// bytes, unless it has files of its own to put between the server's:
/// then it asks for one frame per file. A clean checkout, one whose only
/// change is a deleted file, and `--ignore-local-changes` have none; the
/// answer is the same either way.
#[test]
fn a_search_asks_for_frames_of_files_only_to_merge_its_own() {
    let r = Remote::new(&FILES);
    let all = "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n";
    let asked = |args: &[&str]| {
        let (out, q) = r.query_sent(args);
        let p = &q["params"];
        let rendered = p["rendered"] == true;
        assert_eq!(p["stream"], true, "{q}");
        // Without watchman the skip list lacks the directory entry.
        (stdout(&out), rendered, p["skip"][0].to_string())
    };
    let list = ["-l", "printk"];
    assert_eq!(asked(&list), (all.into(), true, "null".into()));
    // Deleted: the server leaves it out, and the client has no file to add.
    std::fs::remove_file(r.client.join("a.c")).unwrap();
    let rest = "dir/b.c\ndir/sub/c.h\ne.c\n";
    assert_eq!(asked(&list), (rest.into(), true, r#""a.c""#.into()));
    // Changed: the client searches a.c itself, and prints it first.
    r.s.write(&r.client, &[("a.c", "printk changed\n")]);
    assert_eq!(asked(&list), (all.into(), false, r#""a.c""#.into()));
    let ignoring = ["--ignore-local-changes", "-l", "printk"];
    assert_eq!(asked(&ignoring), (all.into(), true, "null".into()));
    // Counts have no output to copy: the field is left out.
    let (_, q) = r.query_sent(&["--ignore-local-changes", "--approx-counts", "printk"]);
    assert_eq!(q["params"]["mode"], "counts", "{q}");
    assert!(q["params"].get("rendered").is_none(), "{q}");
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

/// A server that reports no served commit. From the tree it serves: no
/// warning from a files-layout server, which reads that tree; the warning
/// from a `--store-docs` server, whose stored docs ignore the edits made
/// there. From another checkout: the warning; from no checkout: none. The
/// warning says why: the reason that the build of a `--store-docs` index
/// gave, or that only such an index records a commit.
#[test]
fn a_server_without_a_served_commit_warns_unless_it_reads_the_tree() {
    let s = Sandbox::new();
    let origin = s.repo("origin", &FILES);
    let output = s.path("idx").display().to_string();
    assert!(
        s.run(&origin, &["index", ".", "--output", &output])
            .status
            .success()
    );
    let (_files, files) = s.serve(&s.path("idx"), &["--no-watch"]);
    // An edit that git has not seen: the index records no commit.
    let stored = s.repo("stored", &FILES);
    s.write(&stored, &[("a.c", "printk a\nedited\n")]);
    let out = s.run(&stored, &["index", "--store-docs", "."]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (_docs, docs) = s.serve(&stored.join(".fmgrep"), &[]);
    let client = s.repo("client", &FILES);
    let home = std::fs::canonicalize(s.path("home")).unwrap();
    let search = |cwd: &Path, addr: &str| {
        let out = s
            .fmgrep(cwd)
            .args(["--server", addr, "-l", "printk"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        stderr(&out)
    };
    let why = |addr: &str, because: &str| {
        ignored(&format!(
            "the server at {addr} reports no served commit: {because}"
        ))
    };
    let edited = "a.c differs from HEAD";
    let only = "only a --store-docs index records one";
    assert_eq!(search(&origin, &files), "");
    assert_eq!(search(&stored, &docs), why(&docs, edited));
    assert_eq!(search(&client, &docs), why(&docs, edited));
    assert_eq!(search(&client, &files), why(&files, only));
    for addr in [&files, &docs] {
        assert_eq!(search(&home, addr), "");
    }
}

/// Each cause that an index has no served commit reaches the client's
/// warning word for word, and the server's status: a submodule, a dirty
/// tree, an ignore file outside the checkout, a file that differs from its
/// blob, and a directory in no checkout. An index that records a commit
/// has no reason, and its client no warning.
#[test]
fn the_warning_says_why_the_server_serves_no_commit() {
    let s = Sandbox::new();
    let client = s.repo("client", &FILES);
    let search = |addr: &str| {
        let out = s
            .fmgrep(&client)
            .args(["--server", addr, "-l", "printk"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        stderr(&out)
    };
    let warning = |addr: &str, why: &str| {
        ignored(&format!(
            "the server at {addr} reports no served commit: {why}"
        ))
    };
    // A checked out submodule, as in a checkout of a large project.
    let inner = s.repo("inner", &[("x.c", "printk x\n")]);
    let submodule = s.repo("with-submodule", &FILES);
    let url = inner.display().to_string();
    let add = ["-c", "protocol.file.allow=always", "submodule", "add", "-q"];
    s.git(&submodule, &[&add[..], &[&url, "agents/internal"]].concat());
    s.commit(&submodule, "submodule");
    let dirty = s.repo("dirty", &FILES);
    s.write(&dirty, &[("a.c", "printk edited\n")]);
    let outside = s.repo("outside", &FILES);
    s.write(&outside, &[(".git/info/exclude", "*.h\n")]);
    let filtered = s.repo("filtered", &FILES);
    s.write(&filtered, &[(".gitattributes", "*.c filter=up\n")]);
    s.commit(&filtered, "attributes");
    s.git(&filtered, &["config", "filter.up.smudge", "tr a-z A-Z"]);
    s.git(&filtered, &["config", "filter.up.clean", "tr A-Z a-z"]);
    std::fs::remove_file(filtered.join("a.c")).unwrap();
    s.git(&filtered, &["checkout", "--", "a.c"]);
    let bare = s.path("no-checkout");
    s.write(&bare, &FILES);
    let bare = std::fs::canonicalize(bare).unwrap();
    let exclude = format!(
        "the ignore file {} outside the checkout has a pattern",
        outside.join(".git/info/exclude").display()
    );
    let cases = [
        (
            &submodule,
            "the checkout holds a submodule (agents/internal)".to_owned(),
        ),
        (&dirty, "a.c differs from HEAD".to_owned()),
        (&outside, exclude),
        (&filtered, "a.c differs from its blob in HEAD".to_owned()),
        (&bare, format!("{} is in no git checkout", bare.display())),
    ];
    for (i, (dir, why)) in cases.iter().enumerate() {
        let (index, built) = s.index(dir, &format!("idx-{i}"));
        let note = format!("note: the index records no served commit ({why})");
        assert!(built.lines().any(|l| l == note), "{why}: {built}");
        let (_server, addr) = s.serve(&index, &[]);
        assert_eq!(search(&addr), warning(&addr, why), "{why}");
        // From no checkout there is nothing to ignore.
        let home = std::fs::canonicalize(s.path("home")).unwrap();
        let out = s
            .fmgrep(&home)
            .args(["--server", &addr, "-l", "printk"])
            .output()
            .unwrap();
        assert_eq!(stderr(&out), "", "{why}");
        // The server's status and the index's say it too.
        assert_eq!(s.status(&addr)["why_no_commit"], why.as_str());
        let line = format!("why_no_commit: {why}");
        assert_eq!(s.status_lines(&addr).last(), Some(&line), "{why}");
        assert_eq!(crate::git::why_of(&index).as_ref(), Some(why));
    }
}

/// An index that records a commit gives no reason: its client's search
/// prints no warning, and `status --server` shows the commit and no
/// reason.
#[test]
fn a_server_of_a_served_commit_gives_no_reason() {
    let r = Remote::new(&FILES);
    let out = r.search(&r.client, &["-l", "printk"]);
    assert!(
        !stderr(&out).contains("local changes are ignored"),
        "{}",
        stderr(&out)
    );
    let status = r.s.status(&r.addr);
    assert_eq!(status["commit"]["id"], r.commit.as_str());
    assert!(status.get("why_no_commit").is_none(), "{status}");
    let lines = r.s.status_lines(&r.addr);
    let commit = format!("commit: git {}", r.commit);
    let n = lines.len();
    assert_eq!(
        lines[n - 2..],
        [commit.as_str(), "why_no_commit: none"],
        "{lines:?}"
    );
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
/// the served commit, also with `--ignore-local-changes`, and from a
/// checkout that lacks it, after the warning.
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
        let out = r.search(
            &r.client,
            &[&["--ignore-local-changes"], &args(mode)[..]].concat(),
        );
        assert_eq!(
            (stdout(&out), stderr(&out), out.status.code()),
            (String::new(), format!("{error}\n"), Some(2))
        );
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

/// A search of no file prints what a local search prints before its error
/// (the statistics, rg's JSON summary), and fails with the same status:
/// from a clean clone, where the output is the server's bytes, and after a
/// local change, where the client merges the files.
#[test]
fn a_search_of_no_file_prints_what_a_local_one_prints_first() {
    let r = Remote::new(&FILES);
    let modes: [&[&str]; 2] = [&["--stats"], &["--json", "-o"]];
    for changed in [false, true] {
        if changed {
            r.s.write(&r.client, &[("a.c", "printk changed\n")]);
        }
        for mode in modes {
            let args = [mode, &["-g", "*.none", "printk"]].concat();
            let out = r.search(&r.client, &args);
            assert_eq!(out.status.code(), Some(2), "{changed} {args:?}");
            assert!(!stdout(&out).is_empty(), "{changed} {args:?}");
            r.same("", &args);
        }
    }
}

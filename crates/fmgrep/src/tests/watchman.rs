//! A private watchman daemon for one test: the real binary, run in the
//! foreground with its own socket, state, log, pid and config files in a
//! temporary directory. A test reaches it only through that socket and
//! never runs the watchman client, so it never reaches the user's daemon,
//! nor starts one.
//!
//! The binary is `$FMGREP_TEST_WATCHMAN`, else `watchman` on `PATH`;
//! `FMGREP_TEST_WATCHMAN=` (empty) means none. Without one, a test that
//! needs it says so on stderr in one line and passes. A daemon that does
//! not start fails the test.

use std::cell::{Cell, RefCell};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub(crate) struct Daemon {
    pub(crate) sock: PathBuf,
    program: PathBuf,
    dir: tempfile::TempDir,
    /// The daemon, a child of the test process, so that a failed call can
    /// say whether it still runs and how it exited.
    child: RefCell<Option<Child>>,
    /// A shell that kills the daemon when the test process dies: its stdin
    /// then closes before [`Self::stop`] writes a line to it.
    guard: Option<Child>,
    /// How much of the log [`Self::sent`] has read.
    read: Cell<usize>,
    /// Daemons take turns: on a loaded machine, several at once can answer
    /// later than a search waits.
    _turn: MutexGuard<'static, ()>,
}

/// The daemon's config file: it starts at any nice value (CI runners and
/// background shells raise it), and `watch-project` watches the very
/// directory asked for, whatever lies above it.
const CONFIG: &str = r#"{"min_acceptable_nice_value": 19, "root_files": []}"#;

fn program() -> Option<PathBuf> {
    match std::env::var_os("FMGREP_TEST_WATCHMAN") {
        Some(p) if p.is_empty() => None,
        Some(p) => Some(p.into()),
        None => std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join("watchman"))
            .find(|p| p.is_file()),
    }
}

impl Daemon {
    /// A new daemon, listening; `None` without watchman.
    pub(crate) fn start() -> Option<Self> {
        let Some(program) = program() else {
            let test = std::thread::current().name().unwrap_or("?").to_owned();
            // Past the test harness's capture, so the skip shows.
            let _ = writeln!(
                std::io::stderr(),
                "skipped watchman in {test}: not installed (or set FMGREP_TEST_WATCHMAN to it)"
            );
            return None;
        };
        static TURNS: Mutex<()> = Mutex::new(());
        let turn = TURNS.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = tempfile::Builder::new()
            .prefix("fmgrep-watchman-")
            .tempdir()
            .unwrap();
        std::fs::write(dir.path().join("config.json"), CONFIG).unwrap();
        let mut daemon = Self {
            sock: dir.path().join("sock"),
            program,
            dir,
            child: RefCell::new(None),
            guard: None,
            read: Cell::new(0),
            _turn: turn,
        };
        daemon.spawn();
        Some(daemon)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn spawn(&mut self) {
        let _ = std::fs::remove_file(&self.sock);
        let arg = |flag: &str, name: &str| format!("--{flag}={}", self.file(name).display());
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.file("out"))
            .unwrap();
        let child = Command::new(&self.program)
            .args(["--foreground", "--log-level=2"])
            .args([
                arg("sockname", "sock"),
                arg("statefile", "state"),
                arg("logfile", "log"),
                arg("pidfile", "pid"),
            ])
            .env("WATCHMAN_CONFIG_FILE", self.file("config.json"))
            .env_remove("WATCHMAN_SOCK")
            .stdin(Stdio::null())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap();
        let guard = Command::new("/bin/sh")
            .args(["-c", r#"read _ || kill -9 "$1""#, "sh"])
            .arg(child.id().to_string())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        *self.child.borrow_mut() = Some(child);
        self.guard = Some(guard);
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.exchange(&json!(["get-pid"])).is_err() {
            if Instant::now() > deadline {
                panic!("watchman did not start; {}", self.state());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.sent();
    }

    /// Kills the daemon and waits until it is gone.
    pub(crate) fn stop(&mut self) {
        if let Some(mut guard) = self.guard.take() {
            if let Some(mut stdin) = guard.stdin.take() {
                let _ = writeln!(stdin);
            }
            let _ = guard.wait();
        }
        if let Some(mut child) = self.child.get_mut().take() {
            let _ = (child.kill(), child.wait());
        }
    }

    /// Whether the daemon runs, or how it exited, then the end of its log
    /// and of what it printed.
    fn state(&self) -> String {
        let status = match self.child.borrow_mut().as_mut().map(Child::try_wait) {
            Some(Ok(None)) => "watchman runs".to_owned(),
            Some(Ok(Some(status))) => format!("watchman exited ({status})"),
            Some(Err(e)) => format!("watchman's state is unknown ({e})"),
            None => "watchman was stopped".to_owned(),
        };
        let tail = |name: &str| {
            let text = std::fs::read_to_string(self.file(name)).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(40)..].join("\n")
        };
        format!(
            "{status}\n--- end of its log\n{}\n--- end of its output\n{}",
            tail("log"),
            tail("out")
        )
    }

    /// Stops the daemon and starts it again on the same files: it watches
    /// again what it watched, and reads each of those trees again.
    pub(crate) fn restart(&mut self) {
        self.stop();
        self.spawn();
    }

    fn signal(&self, signal: &str) {
        let pid = std::fs::read_to_string(self.file("pid")).unwrap();
        let status = Command::new("kill")
            .args([signal, pid.trim()])
            .status()
            .unwrap();
        assert!(status.success(), "kill {signal} {pid}");
    }

    /// The daemon stops in place: it accepts connections and answers none.
    pub(crate) fn hang(&self) {
        self.signal("-STOP");
    }

    pub(crate) fn resume(&self) {
        self.signal("-CONT");
    }

    /// One command's reply, or its error. A command that gets no reply
    /// fails with [`Self::state`].
    pub(crate) fn call(&self, command: &Value) -> Result<Value, String> {
        let reply = self
            .exchange(command)
            .map_err(|e| format!("{command}: {e}; {}", self.state()))?;
        match reply.get("error") {
            Some(e) => Err(e.as_str().unwrap_or_default().to_owned()),
            None => Ok(reply),
        }
    }

    /// One command's reply, which may be an error.
    fn exchange(&self, command: &Value) -> std::io::Result<Value> {
        let mut conn = UnixStream::connect(&self.sock)?;
        conn.set_read_timeout(Some(Duration::from_secs(60)))?;
        // One write: watchman drops a client whose first read is short.
        conn.write_all(format!("{command}\n").as_bytes())?;
        for line in BufReader::new(&conn).lines() {
            let reply: Value = serde_json::from_str(&line?)?;
            if reply.get("unilateral").is_none() && reply.get("log").is_none() {
                return Ok(reply);
            }
        }
        Err(std::io::ErrorKind::UnexpectedEof.into())
    }

    /// Watches `root` itself, and returns once the daemon has read the
    /// whole tree.
    pub(crate) fn watch(&self, root: &Path) {
        let root = std::fs::canonicalize(root).unwrap();
        self.call(&json!(["watch-project", root])).unwrap();
        let sync = json!(["clock", root, {"sync_timeout": 60_000}]);
        self.call(&sync).unwrap();
        self.sent();
    }

    /// Stops watching `root`.
    pub(crate) fn unwatch(&self, root: &Path) {
        let root = std::fs::canonicalize(root).unwrap();
        self.call(&json!(["watch-del", root])).unwrap();
        self.sent();
    }

    /// The directories it watches.
    pub(crate) fn roots(&self) -> Vec<String> {
        let list = self.call(&json!(["watch-list"])).unwrap();
        let roots = list["roots"].as_array().unwrap();
        let roots = roots
            .iter()
            .map(|r| r.as_str().unwrap().to_owned())
            .collect();
        self.sent();
        roots
    }

    /// The commands that this test process sent since the last call, in
    /// order, as the daemon logged them. A `version` command marks the
    /// end, as the log is written behind the replies.
    pub(crate) fn sent(&self) -> Vec<String> {
        self.call(&json!(["version"])).unwrap();
        let me = format!(":pid={}] dispatch_command: ", std::process::id());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let log = std::fs::read(self.file("log")).unwrap();
            if log.len() < self.read.get() {
                self.read.set(0);
            }
            let new = String::from_utf8_lossy(&log[self.read.get().min(log.len())..]).into_owned();
            let mut names = Vec::new();
            let mut at = 0;
            for line in new.split_inclusive('\n') {
                at += line.len();
                let Some((_, name)) = line.trim_end().split_once(&me) else {
                    continue;
                };
                if name.ends_with("(completed)") {
                    continue;
                }
                if name == "version" {
                    self.read.set(self.read.get() + at);
                    return names;
                }
                names.push(name.to_owned());
            }
            assert!(Instant::now() < deadline, "no version line in the log");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

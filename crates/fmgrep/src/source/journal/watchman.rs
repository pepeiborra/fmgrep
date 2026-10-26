//! A running watchman daemon, never spawned: a token is the daemon's socket
//! plus a watch-project clock; a replay is a `since` query, synced by
//! watchman's own cookies. Detection, connects and calls share one deadline.

use std::cell::Cell;
use std::io::{BufRead as _, BufReader, Read, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Ask, ChangeSource, Since};

pub struct Watchman {
    sock: PathBuf,
    deadline: Instant,
    late: Cell<bool>,
    settle: bool,
}

#[derive(Serialize, Deserialize)]
struct Token {
    sock: String,
    watch: String,
    relative: String,
    clock: String,
}

fn note_late() {
    super::warn_late("watchman");
    if crate::verbose::verbose() {
        eprintln!("note: watchman did not answer in time; not using it");
    }
}

/// Reads that fail once the deadline passes.
struct Until<'a>(&'a UnixStream, Instant);

impl Read for Until<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.1.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        // macOS refuses (EINVAL) once the peer closed; the read then returns.
        let _ = self.0.set_read_timeout(Some(left));
        self.0.read(buf)
    }
}

impl Watchman {
    fn until(sock: PathBuf, deadline: Instant) -> Self {
        Self {
            sock,
            deadline,
            late: Cell::new(false),
            settle: true,
        }
    }

    /// Every connect and call within `budget` from now.
    pub fn at(sock: PathBuf, budget: Duration) -> Self {
        Self::until(sock, Instant::now() + budget)
    }

    /// The daemon that marked `token`; its marks do not sync, as an early
    /// clock only names more paths.
    pub fn of(token: &Value, budget: Duration) -> Option<Self> {
        let sock = token.get("sock")?.as_str()?;
        Some(Self {
            settle: false,
            ..Self::at(sock.into(), budget)
        })
    }

    /// `$WATCHMAN_SOCK`, else the socket of a daemon already running.
    pub fn find(budget: Duration) -> Option<Self> {
        match std::env::var_os("WATCHMAN_SOCK") {
            Some(sock) => Some(Self::at(sock.into(), budget)),
            None => Self::detect(Path::new("watchman"), budget),
        }
    }

    /// Asks `program` for the socket; `--no-spawn` starts no daemon, and a
    /// child still running at the deadline is killed.
    pub fn detect(program: &Path, budget: Duration) -> Option<Self> {
        let deadline = Instant::now() + budget;
        let mut child = Command::new(program)
            .args(["--no-spawn", "--no-pretty", "get-sockname"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                waited => {
                    let _ = (child.kill(), child.wait());
                    if waited.is_ok() {
                        note_late();
                    }
                    return None;
                }
            }
        };
        let mut out = Vec::new();
        child.stdout.take()?.read_to_end(&mut out).ok()?;
        let reply: Value = serde_json::from_slice(&out)
            .ok()
            .filter(|_| status.success())?;
        let sock = reply.get("sockname")?.as_str()?;
        Some(Self::until(sock.into(), deadline))
    }

    /// What is left of the budget, at least 1 ms, for `sync_timeout`.
    fn left_ms(&self) -> u64 {
        let left = self.deadline.saturating_duration_since(Instant::now());
        u64::try_from(left.as_millis()).unwrap_or(u64::MAX).max(1)
    }

    /// One command's reply; `None` on an error or past the deadline (one note).
    fn call(&self, command: &Value) -> Option<Value> {
        let reply = self.exchange(command);
        if reply.is_none() && Instant::now() >= self.deadline && !self.late.replace(true) {
            note_late();
        }
        reply
    }

    /// Unilateral PDUs are skipped. The connect runs on a helper thread: std
    /// has no connect timeout.
    fn exchange(&self, command: &Value) -> Option<Value> {
        let left = self.deadline.checked_duration_since(Instant::now())?;
        let (tx, rx) = mpsc::channel();
        let sock = self.sock.clone();
        std::thread::spawn(move || tx.send(UnixStream::connect(sock)));
        let mut stream = rx.recv_timeout(left).ok()?.ok()?;
        let left = self.deadline.checked_duration_since(Instant::now())?;
        stream.set_write_timeout(Some(left)).ok()?;
        // One write: watchman misreads a PDU whose first read is short.
        stream.write_all(format!("{command}\n").as_bytes()).ok()?;
        let mut lines = BufReader::new(Until(&stream, self.deadline)).lines();
        loop {
            let reply: Value = serde_json::from_str(&lines.next()?.ok()?).ok()?;
            let unilateral = reply.get("unilateral").and_then(Value::as_bool) == Some(true);
            if unilateral || reply.get("log").is_some() {
                continue;
            }
            return reply.get("error").is_none().then_some(reply);
        }
    }
}

impl ChangeSource for Watchman {
    fn kind(&self) -> &'static str {
        "watchman"
    }

    /// `None` under `ignore_dirs`: watchman would hide indexed changes.
    fn mark(&self, root: &Path) -> Option<Value> {
        let project = self.call(&json!(["watch-project", root]))?;
        let watch = project.get("watch")?.as_str()?;
        let relative = project
            .get("relative_path")
            .and_then(Value::as_str)
            .unwrap_or("");
        let config = self.call(&json!(["get-config", watch]))?;
        let ignores = config
            .pointer("/config/ignore_dirs")
            .and_then(Value::as_array);
        if ignores.is_some_and(|d| !d.is_empty()) {
            return None;
        }
        let clock = if self.settle {
            json!(["clock", watch, {"sync_timeout": self.left_ms()}])
        } else {
            json!(["clock", watch])
        };
        let clock = self.call(&clock)?;
        let token = Token {
            sock: self.sock.to_str()?.to_owned(),
            watch: watch.to_owned(),
            relative: relative.to_owned(),
            clock: clock.get("clock")?.as_str()?.to_owned(),
        };
        serde_json::to_value(token).ok()
    }

    fn since(&self, ask: &Ask<'_>) -> Since {
        let unknown = |why: &str| Since::Unknown(why.to_owned());
        let Ok(token) = serde_json::from_value::<Token>(ask.token.clone()) else {
            return unknown("unreadable token");
        };
        let watched = Path::new(&token.watch).join(&token.relative);
        if watched.components().ne(ask.root.components()) {
            return unknown("the token watches another root");
        }
        let mut query = json!({
            "since": token.clock,
            "fields": ["name"],
            "empty_on_fresh_instance": true,
            "sync_timeout": self.left_ms(),
        });
        if !token.relative.is_empty() {
            query["relative_root"] = json!(token.relative);
        }
        let Some(reply) = self.call(&json!(["query", token.watch, query])) else {
            return unknown("no answer");
        };
        if reply.get("is_fresh_instance").and_then(Value::as_bool) != Some(false) {
            return unknown("a fresh instance");
        }
        let Some(files) = reply.get("files").and_then(Value::as_array) else {
            return unknown("no file list");
        };
        match files
            .iter()
            .map(|f| f.as_str().map(str::to_owned))
            .collect()
        {
            Some(paths) => Since::Paths(paths),
            None => unknown("a malformed file list"),
        }
    }
}

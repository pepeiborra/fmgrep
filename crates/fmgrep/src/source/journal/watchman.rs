//! A watchman daemon, started by its own client when none runs: a token is
//! the daemon's socket plus a clock of a watch on the root or above it; a
//! token question is a `since` query, synced by watchman's own cookies.
//! Where no watch covers the root, `watch-project` adds one; watchman
//! sends its answer once the crawl of the tree ends, so a short deadline
//! gives up on the answer but not on the watch. A clock answered during a
//! crawl, its tail included, is the crawl's tick, synced or not; files
//! changed then get that tick too, so `since` it misses them. Each batch
//! after a crawl ticks first, so a token is a second synced clock past the
//! first; one equal to it, or a clock that does not sync in time, gives
//! none. Connects and calls share one deadline, and each failure says why.

use std::cell::Cell;
use std::io::{BufRead as _, BufReader, Read, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Token;

pub struct Watchman {
    sock: PathBuf,
    deadline: Instant,
    budget: Duration,
    /// A call failed for want of time, or during a crawl: later it may
    /// answer.
    late: Cell<bool>,
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
    /// Every connect and call within `budget` from now.
    pub fn at(sock: PathBuf, budget: Duration) -> Self {
        Self {
            sock,
            deadline: Instant::now() + budget,
            budget,
            late: Cell::new(false),
        }
    }

    /// Whether a call failed for want of time or during a crawl.
    pub fn late(&self) -> bool {
        self.late.get()
    }

    /// The socket in `$WATCHMAN_SOCK`, when it is set; tests read the one
    /// their thread gives.
    pub fn given() -> Option<PathBuf> {
        #[cfg(test)]
        return crate::tests::WATCHMAN.with_borrow(Clone::clone);
        #[cfg(not(test))]
        return std::env::var_os("WATCHMAN_SOCK").map(PathBuf::from);
    }

    /// [`Self::given`], else the socket that `watchman get-sockname` gives
    /// within [`super::FIND_WAIT`]; calls get `budget` after that. Tests
    /// run only the program they are given.
    pub fn find(budget: Duration) -> Result<Self, String> {
        #[cfg(test)]
        let program = crate::tests::WATCHMAN_PROGRAM.with_borrow(Clone::clone);
        #[cfg(not(test))]
        let program = PathBuf::from("watchman");
        match Self::given() {
            Some(sock) => Ok(Self::at(sock, budget)),
            None => Ok(Self::at(Self::detect(&program, super::FIND_WAIT)?, budget)),
        }
    }

    /// Asks `program` for the socket. Without `--no-spawn` the client
    /// starts the daemon when none runs, and answers once it listens. The
    /// reply is read on a helper thread, as a daemon could hold the pipe; a
    /// child still running at the deadline is killed, and a daemon it
    /// started keeps starting.
    pub fn detect(program: &Path, budget: Duration) -> Result<PathBuf, String> {
        let deadline = Instant::now() + budget;
        let mut child = Command::new(program)
            .args(["--no-pretty", "get-sockname"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot run watchman ({e}); install watchman"))?;
        let (tx, rx) = mpsc::channel();
        let stdout = child.stdout.take();
        std::thread::spawn(move || {
            let mut line = String::new();
            if let Some(stdout) = stdout {
                let _ = BufReader::new(stdout).read_line(&mut line);
            }
            let _ = tx.send(line);
        });
        let line = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                _ => {
                    let _ = (child.kill(), child.wait());
                    break None;
                }
            }
        };
        let Ok(line) = line else {
            return Err(format!(
                "watchman get-sockname did not answer within {} ms",
                budget.as_millis()
            ));
        };
        let reply = serde_json::from_str::<Value>(&line).ok();
        let sock = reply.and_then(|r| Some(PathBuf::from(r.get("sockname")?.as_str()?)));
        sock.ok_or_else(|| match status {
            Some(status) => format!("watchman get-sockname failed ({status})"),
            None => "watchman get-sockname sent no socket".to_owned(),
        })
    }

    /// The watch on `root` or the deepest above it, and `root` relative to
    /// it. Without one, asks watchman to watch `root` and waits for the
    /// answer, sent once the crawl ends, until the deadline only; the watch
    /// stays either way.
    pub fn watch(&self, root: &Path) -> Result<(String, String), String> {
        let list = self.call(&json!(["watch-list"]), root)?;
        let found = list
            .get("roots")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|w| Some((w, root.strip_prefix(w).ok()?.to_str()?)))
            .max_by_key(|(w, _)| w.len());
        if let Some((watch, relative)) = found {
            return Ok((watch.to_owned(), relative.to_owned()));
        }
        let project = match self.call(&json!(["watch-project", root]), root) {
            Err(_) if self.late() => return Err(asked(root)),
            project => project?,
        };
        let watch = project.get("watch").and_then(Value::as_str);
        let relative = project.get("relative_path").and_then(Value::as_str);
        match watch {
            Some(watch) => Ok((watch.to_owned(), relative.unwrap_or("").to_owned())),
            None => Err("watchman sent no watch".to_owned()),
        }
    }

    /// Three quarters of what is left of the budget, at least 1 ms, for
    /// `sync_timeout`: a sync that fails says why before the deadline, and
    /// one that succeeds leaves time for the reply.
    fn sync_ms(&self) -> u64 {
        let left = self.deadline.saturating_duration_since(Instant::now()) * 3 / 4;
        u64::try_from(left.as_millis()).unwrap_or(u64::MAX).max(1)
    }

    /// A clock of `watch`, synced.
    fn clock(&self, watch: &str, root: &Path) -> Result<String, String> {
        let reply = self.call(
            &json!(["clock", watch, {"sync_timeout": self.sync_ms()}]),
            root,
        )?;
        let clock = reply.get("clock").and_then(Value::as_str);
        clock
            .map(str::to_owned)
            .ok_or_else(|| "watchman sent no clock".to_owned())
    }

    /// A token for now, of the watch covering `root`. Refused when an
    /// `ignore_dirs` entry would hide changes under it, or when the two
    /// clocks do not sync in time or share a tick (a crawl's).
    pub fn mark(&self, root: &Path) -> Result<Token, String> {
        let (watch, relative) = self.watch(root)?;
        let config = self.call(&json!(["get-config", watch]), root)?;
        if let Some(ignores) = config.pointer("/config/ignore_dirs")
            && hides(ignores, &relative)
        {
            return Err(format!(
                "watchman's ignore_dirs hide part of {}",
                root.display()
            ));
        }
        let first = self.clock(&watch, root)?;
        let clock = self.clock(&watch, root)?;
        if !ticked(&first, &clock) {
            self.late.set(true);
            return Err(format!("watchman is still crawling {}", root.display()));
        }
        let sock = self
            .sock
            .to_str()
            .ok_or("the watchman socket is not UTF-8")?;
        Ok(Token {
            sock: sock.to_owned(),
            watch,
            relative,
            clock,
        })
    }

    /// The paths under `root` changed since `token`'s clock; refused on a
    /// fresh instance: a restart, the watch added again, or deleted files
    /// pruned since. A recrawl is not one: it names what it statted.
    pub fn since(&self, token: &Token, root: &Path) -> Result<Vec<String>, String> {
        let watched = Path::new(&token.watch).join(&token.relative);
        if watched.components().ne(root.components()) {
            return Err("the token watches another root".to_owned());
        }
        let mut query = json!({
            "since": token.clock,
            "fields": ["name"],
            "empty_on_fresh_instance": true,
            "sync_timeout": self.sync_ms(),
        });
        if !token.relative.is_empty() {
            query["relative_root"] = json!(token.relative);
        }
        let reply = self.call(&json!(["query", token.watch, query]), root)?;
        if reply.get("is_fresh_instance").and_then(Value::as_bool) != Some(false) {
            return Err("watchman restarted, added the watch again or pruned deleted files since the last search".to_owned());
        }
        let files = reply.get("files").and_then(Value::as_array);
        files
            .and_then(|f| f.iter().map(|f| f.as_str().map(str::to_owned)).collect())
            .ok_or_else(|| "watchman sent a malformed file list".to_owned())
    }

    /// A failed call's reason. Watchman words a sync that timed out as
    /// "synchronization failed: ... timed out" or as "syncToNow: timed out
    /// waiting for cookie file ...", depending on where it gave up.
    fn why(&self, root: &Path, error: String) -> String {
        let crawling = ["performing initial crawl", "re-crawling"];
        let sync = ["synchronization failed", "syncToNow"];
        if crawling.iter().any(|c| error.contains(c)) {
            self.late.set(true);
            format!("watchman is still crawling {}", root.display())
        } else if sync.iter().any(|s| error.contains(s)) && error.contains("timed out") {
            self.late.set(true);
            self.too_late()
        } else if error.contains("is not watched") {
            not_watching(root)
        } else {
            error
        }
    }

    fn too_late(&self) -> String {
        format!(
            "watchman did not answer within {} ms",
            self.budget.as_millis()
        )
    }

    /// One command's reply, or why there is none.
    fn call(&self, command: &Value, root: &Path) -> Result<Value, String> {
        let left = self.deadline.checked_duration_since(Instant::now());
        match left.map(|left| self.exchange(command, left)) {
            Some(Ok(reply)) => reply.map_err(|e| self.why(root, format!("watchman: {e}"))),
            Some(Err(e)) if Instant::now() < self.deadline => Err(e),
            _ => {
                self.late.set(true);
                Err(self.too_late())
            }
        }
    }

    /// The reply, an error in it, or why there is neither. Unilateral PDUs
    /// are skipped. The connect runs on a helper thread: std has no connect
    /// timeout.
    fn exchange(&self, command: &Value, left: Duration) -> Result<Result<Value, String>, String> {
        let (tx, rx) = mpsc::channel();
        let sock = self.sock.clone();
        std::thread::spawn(move || tx.send(UnixStream::connect(sock)));
        let failed = |e: &dyn std::fmt::Display| format!("watchman: {e}");
        let stream = rx.recv_timeout(left).map_err(|e| failed(&e))?;
        let stream = stream.map_err(|e| {
            format!(
                "no watchman daemon answers on {} ({e})",
                self.sock.display()
            )
        })?;
        stream
            .set_write_timeout(Some(left))
            .map_err(|e| failed(&e))?;
        // One write: watchman misreads a PDU whose first read is short.
        (&stream)
            .write_all(format!("{command}\n").as_bytes())
            .map_err(|e| failed(&e))?;
        let mut lines = BufReader::new(Until(&stream, self.deadline)).lines();
        loop {
            let line = lines.next().ok_or_else(|| failed(&"no reply"))?;
            let reply: Value =
                serde_json::from_str(&line.map_err(|e| failed(&e))?).map_err(|e| failed(&e))?;
            let unilateral = reply.get("unilateral").and_then(Value::as_bool) == Some(true);
            if unilateral || reply.get("log").is_some() {
                continue;
            }
            return Ok(match reply.get("error") {
                Some(e) => Err(e.as_str().unwrap_or("an error").to_owned()),
                None => Ok(reply),
            });
        }
    }
}

fn not_watching(root: &Path) -> String {
    format!("watchman is not watching {}", root.display())
}

/// Why a search that asked watchman to watch `root` just now cannot use it.
fn asked(root: &Path) -> String {
    format!(
        "fmgrep asked watchman to watch {}; later searches ask it once its crawl ends",
        root.display()
    )
}

/// Whether clock `b` is of `a`'s instance with a later tick; a clock is
/// `c:<start>:<pid>:<root number>:<tick>`.
pub(super) fn ticked(a: &str, b: &str) -> bool {
    let split = |c: &str| {
        let (instance, tick) = c.rsplit_once(':')?;
        Some((instance.to_owned(), tick.parse::<u64>().ok()?))
    };
    matches!((split(a), split(b)), (Some((ia, ta)), Some((ib, tb))) if ia == ib && tb > ta)
}

/// Whether an `ignore_dirs` entry (watch-relative) is at, under or above
/// the tree at `relative`; case-blind, as some filesystems are. An entry
/// that is not a plain relative path hides.
pub(super) fn hides(ignores: &Value, relative: &str) -> bool {
    let parts = |p: &str| -> Option<Vec<String>> {
        Path::new(p)
            .components()
            .map(|c| match c {
                Component::Normal(s) => s.to_str().map(str::to_lowercase),
                _ => None,
            })
            .collect()
    };
    let (Some(tree), Some(ignores)) = (parts(relative), ignores.as_array()) else {
        return true;
    };
    ignores.iter().any(|d| {
        d.as_str()
            .and_then(parts)
            .is_none_or(|d| d.starts_with(&tree) || tree.starts_with(&d))
    })
}

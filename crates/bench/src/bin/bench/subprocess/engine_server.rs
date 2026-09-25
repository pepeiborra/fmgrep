//! [`EngineServer`]: one `<engine> serve` child driven through
//! [`RowServer`] by the `--server` spawns of [`super::run_child`].

use std::io::{BufRead as _, BufReader, Read as _};
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{ClientReport, IndexSummary, client_argv, client_counts, serve_argv, shutdown_argv};
use crate::child_spawn::{forward_stderr, output_tails, parse_json};
use crate::serve::{RowServer, TermCounts};

/// How long a server may take to exit after `--server ADDR --shutdown`.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

const READY_POLL: Duration = Duration::from_millis(2);

/// One `<engine> serve` over a built index. Ready once it prints its bound
/// address; killed and reaped on drop.
pub struct EngineServer<'a> {
    bin: &'a Path,
    label: String,
    client_label: String,
    index_dir: &'a Path,
    index: &'a IndexSummary,
    ready_timeout: Duration,
    child: Option<Child>,
    addr: Option<String>,
    stdout_lines: Option<Receiver<String>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    drains: Vec<JoinHandle<()>>,
}

impl<'a> EngineServer<'a> {
    pub fn new(
        bin: &'a Path,
        name: &str,
        index_dir: &'a Path,
        index: &'a IndexSummary,
        ready_timeout: Duration,
    ) -> Self {
        Self {
            bin,
            label: format!("{name}#serve"),
            client_label: format!("{name}#client"),
            index_dir,
            index,
            ready_timeout,
            child: None,
            addr: None,
            stdout_lines: None,
            stderr: Arc::default(),
            drains: Vec::new(),
        }
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap_or_else(|p| p.into_inner())).into_owned()
    }

    fn addr(&self) -> Result<&str, String> {
        self.addr
            .as_deref()
            .ok_or_else(|| format!("{}: server is not ready", self.label))
    }

    /// Kill, reap and drain; the returned status is `None` if it was not running.
    fn kill(&mut self) -> Option<std::process::ExitStatus> {
        let status = self.child.take().and_then(|mut c| {
            let _ = c.kill();
            c.wait().ok()
        });
        self.join_drains();
        status
    }

    fn join_drains(&mut self) {
        for h in self.drains.drain(..) {
            let _ = h.join();
        }
    }

    /// `Err` naming `what` with the server stderr, after killing it.
    fn fail(&mut self, what: String) -> String {
        let status = self.kill();
        let status = status.map_or(String::new(), |s| format!(" ({s})"));
        let tails = output_tails(self.stderr_text().as_bytes(), b"");
        format!("{}: {what}{status}{tails}", self.label)
    }
}

impl RowServer for EngineServer<'_> {
    fn start(&mut self) -> Result<u32, String> {
        let mut child = Command::new(self.bin)
            .args(serve_argv(self.index_dir))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn `{} serve`: {e}", self.bin.display()))?;
        let pid = child.id();
        let (tx, rx) = mpsc::channel();
        if let Some(out) = child.stdout.take() {
            self.drains.push(std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    let _ = tx.send(line);
                }
            }));
        }
        if let Some(mut err) = child.stderr.take() {
            let sink = Arc::clone(&self.stderr);
            self.drains.push(std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(k) = err.read(&mut buf) {
                    if k == 0 {
                        break;
                    }
                    sink.lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .extend_from_slice(&buf[..k]);
                }
            }));
        }
        self.child = Some(child);
        self.stdout_lines = Some(rx);
        Ok(pid)
    }

    fn wait_ready(&mut self) -> Result<(), String> {
        let started = Instant::now();
        let rx = self.stdout_lines.take().ok_or("server not started")?;
        let line = loop {
            match rx.recv_timeout(READY_POLL) {
                Ok(line) => break line,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(self.fail("closed stdout before ready".into()));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            let exited = self
                .child
                .as_mut()
                .and_then(|c| c.try_wait().ok().flatten());
            if exited.is_some() {
                return Err(self.fail("exited before ready".into()));
            }
            if started.elapsed() >= self.ready_timeout {
                let what = format!("not ready after {:?}", self.ready_timeout);
                return Err(self.fail(what));
            }
        };
        match line.trim().parse::<SocketAddr>() {
            Ok(addr) => {
                self.addr = Some(addr.to_string());
                Ok(())
            }
            Err(_) => Err(self.fail(format!("readiness line {line:?} is not an address"))),
        }
    }

    fn query(&mut self, term: &str) -> Result<(TermCounts, Duration), String> {
        let argv = client_argv(self.addr()?, term);
        let start = Instant::now();
        let out = Command::new(self.bin)
            .args(&argv)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("failed to spawn `{} --server`: {e}", self.bin.display()))?;
        let latency = start.elapsed();
        forward_stderr(&self.client_label, &out.stderr);
        if !out.status.success() {
            return Err(format!(
                "{}: `--server` for term {term:?} failed: {}{}",
                self.label,
                out.status,
                output_tails(&out.stderr, &out.stdout)
            ));
        }
        let report: ClientReport = parse_json(&out.stdout, "--server", &argv)?;
        let counts =
            client_counts(self.index, term, report).map_err(|e| format!("{}: {e}", self.label))?;
        Ok((counts, latency))
    }

    fn stop(&mut self) -> Result<(), String> {
        let argv = shutdown_argv(self.addr()?);
        let out = Command::new(self.bin)
            .args(&argv)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("failed to spawn `{} --server`: {e}", self.bin.display()))?;
        if !out.status.success() {
            let what = format!(
                "`--server --shutdown` failed: {}{}",
                out.status,
                output_tails(&out.stderr, &out.stdout)
            );
            return Err(self.fail(what));
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        let status = loop {
            let child = self.child.as_mut().ok_or("server not started")?;
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(READY_POLL),
                Ok(None) => {
                    let what = format!("still running {STOP_TIMEOUT:?} after shutdown");
                    return Err(self.fail(what));
                }
                Err(e) => return Err(self.fail(format!("cannot wait: {e}"))),
            }
        };
        self.child = None;
        self.join_drains();
        forward_stderr(&self.label, self.stderr_text().as_bytes());
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "{}: exited with {status} after shutdown{}",
                self.label,
                output_tails(self.stderr_text().as_bytes(), b"")
            ))
        }
    }
}

impl Drop for EngineServer<'_> {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subprocess::tests::summary;

    /// A `/bin/sh` engine whose `serve` runs `serve_body` and whose
    /// `--server` (the shutdown) creates `$dir/stop`.
    fn fake_engine(serve_body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("engine");
        let script = format!(
            "#!/bin/sh\nd=$(dirname \"$0\")\ncase \"$1\" in\n  serve) {serve_body} ;;\n  --server) touch \"$d/stop\" ;;\nesac\n"
        );
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, bin)
    }

    const READY: &str = "echo 127.0.0.1:1";

    fn alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    fn server<'a>(bin: &'a Path, index: &'a IndexSummary, timeout: Duration) -> EngineServer<'a> {
        EngineServer::new(bin, "fake", Path::new("/nonexistent"), index, timeout)
    }

    #[test]
    fn engine_server_drop_reaps_a_ready_server() {
        let (_dir, bin) = fake_engine(&format!("{READY}; exec sleep 60"));
        let index = summary();
        let mut s = server(&bin, &index, Duration::from_secs(10));
        let pid = s.start().unwrap();
        s.wait_ready().unwrap();
        assert_eq!(s.addr().unwrap(), "127.0.0.1:1");
        assert!(alive(pid));
        drop(s);
        assert!(!alive(pid), "dropped server {pid} still running");
    }

    #[test]
    fn engine_server_exit_before_ready_is_err_with_stderr() {
        let (_dir, bin) = fake_engine("echo cannot load the index >&2; exit 3");
        let index = summary();
        let mut s = server(&bin, &index, Duration::from_secs(10));
        s.start().unwrap();
        let err = s.wait_ready().unwrap_err();
        assert!(
            err.contains("before ready") && err.contains("cannot load the index"),
            "{err}"
        );
    }

    #[test]
    fn engine_server_ready_timeout_kills() {
        let (_dir, bin) = fake_engine("exec sleep 60");
        let index = summary();
        let mut s = server(&bin, &index, Duration::from_millis(200));
        let pid = s.start().unwrap();
        let err = s.wait_ready().unwrap_err();
        assert!(err.contains("not ready after"), "{err}");
        assert!(!alive(pid), "timed-out server {pid} still running");
    }

    #[test]
    fn engine_server_stop_requires_clean_exit() {
        for (code, clean) in [(0, true), (4, false)] {
            let (_dir, bin) = fake_engine(&format!(
                "{READY}; while [ ! -f \"$d/stop\" ]; do sleep 0.01; done; exit {code}"
            ));
            let index = summary();
            let mut s = server(&bin, &index, Duration::from_secs(10));
            let pid = s.start().unwrap();
            s.wait_ready().unwrap();
            assert_eq!(s.stop().is_ok(), clean, "exit {code}");
            assert!(!alive(pid));
        }
    }
}

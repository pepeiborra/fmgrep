//! The optional tgrep row: `tgrep index`, one `tgrep serve --no-watch`, then
//! one `tgrep --count-matches` client per query.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use fmgrep::regex_pattern;
use serde_json::Value;

use crate::rows::{Guard, Sampler, count_rows, failure, run_measured, timed};
use crate::{Query, Row};

pub struct Tgrep {
    pub bin: PathBuf,
    pub version: String,
}

/// The first executable `tgrep` on `PATH` whose `--version` and `serve --help`
/// succeed; `Err` is the note saying why there is no tgrep row.
pub fn detect() -> Result<Tgrep, String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut found = std::env::split_paths(&path).map(|d| d.join("tgrep"));
    let executable = |m: std::fs::Metadata| m.is_file() && m.permissions().mode() & 0o111 != 0;
    let bin = found.find(|p| p.metadata().is_ok_and(executable));
    let bin = bin.ok_or("tgrep: not found on PATH")?;
    let probe = |args: &[&str]| {
        let out = Command::new(&bin).args(args).stdin(Stdio::null()).output();
        match out {
            Ok(out) if out.status.success() => {
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            }
            Ok(out) => Err(format!(
                "{} {}: {}",
                bin.display(),
                args.join(" "),
                out.status
            )),
            Err(e) => Err(format!("{}: {e}", bin.display())),
        }
    };
    let version = probe(&["--version"]).and_then(|v| probe(&["serve", "--help"]).map(|_| v));
    let version = version.map_err(|e| format!("tgrep: does not work ({e})"))?;
    let version = version
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok(Tgrep { bin, version })
}

pub fn row(bin: &Path, staged: &Path, n: usize, terms: &[String]) -> Result<Row, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let index = dir.path().join("tgrep-index");
    let tgrep = |args: &[&str]| {
        let mut cmd = Command::new(bin);
        cmd.args(args).arg("--index-path").arg(&index).arg(staged);
        cmd
    };
    let start = Instant::now();
    let (status, _, peak) = run_measured(&mut tgrep(&["index"]))?;
    let mut row = Row::new("tgrep", n);
    row.index_time = Some(start.elapsed());
    if !status.success() {
        return Err(format!("tgrep index: {status}"));
    }
    row.index_bytes = Some(dir_size(&index));
    row.index_peak_rss = peak;

    let start = Instant::now();
    let mut serve = tgrep(&["serve", "--no-watch"]);
    let serve = serve
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut server = Guard(serve.spawn().map_err(|e| format!("tgrep serve: {e}"))?);
    let pid = server.0.id();
    let sampler = Sampler::start(pid, Duration::from_millis(10));
    let mut last = String::new();
    loop {
        if let Ok(Some(status)) = server.0.try_wait() {
            return Err(format!("tgrep serve: {status} before ready"));
        }
        if start.elapsed() > Duration::from_secs(300) {
            return Err(format!("tgrep serve: not ready after 300 s: {last}"));
        }
        match server_status(&index, pid) {
            Ok(s) if status_ready(&s, n)? => break,
            Ok(s) => last = s.to_string(),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    row.ready = Some(start.elapsed());

    for term in terms {
        let (pattern, fixed) = match regex_pattern(term) {
            Some(p) => (p, None),
            None => (term.as_str(), Some("-F")),
        };
        let e = format!("-e={pattern}");
        let args: Vec<&str> = ["--count-matches"]
            .into_iter()
            .chain(fixed)
            .chain([&*e])
            .collect();
        let (out, latency, _) = timed(&mut tgrep(&args), None)?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        let fallback = ["scanning every file", "falling back to local index"];
        if fallback.iter().any(|m| stderr.contains(m)) || !matches!(out.status.code(), Some(0 | 1))
        {
            return Err(failure(&format!("tgrep {term:?}"), &out));
        }
        row.queries
            .push(Query::new(term, latency, count_rows(&out.stdout)?));
    }
    drop(server);
    row.footprint = sampler.stop();
    Ok(row)
}

fn dir_size(dir: &Path) -> u64 {
    let entries = std::fs::read_dir(dir).into_iter().flatten().flatten();
    let size = |e: std::fs::DirEntry| match e.metadata() {
        Ok(m) if m.is_dir() => dir_size(&e.path()),
        Ok(m) => m.len(),
        Err(_) => 0,
    };
    entries.map(size).sum()
}

/// The `status` RPC of the server `serve.json` names, if that is `pid`.
fn server_status(index: &Path, pid: u32) -> Result<Value, String> {
    let text = std::fs::read_to_string(index.join("serve.json")).map_err(|e| e.to_string())?;
    let info: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let port = info["port"].as_u64().filter(|_| info["pid"] == pid);
    let port = port.ok_or(format!("serve.json is not this server's: {text}"))?;
    let mut stream = TcpStream::connect(("127.0.0.1", port as u16)).map_err(|e| e.to_string())?;
    let timeout = Some(Duration::from_secs(5));
    stream
        .set_read_timeout(timeout)
        .map_err(|e| e.to_string())?;
    let request = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"status\"}\n";
    stream.write_all(request).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let reply: Value = serde_json::from_str(&line).map_err(|e| format!("{e}: {line:?}"))?;
    Ok(reply["result"].clone())
}

/// Ready once the startup reconcile finished with exactly `n` files; `Err`
/// when it never can be.
fn status_ready(s: &Value, n: usize) -> Result<bool, String> {
    let flag = |k: &str| {
        s[k].as_bool()
            .ok_or(format!("tgrep status lacks {k:?}: {s}"))
    };
    if !s["last_reconcile_error"].is_null() {
        return Err(format!("tgrep reconcile failed: {s}"));
    }
    let (indexing, hidden, running) = (
        flag("indexing")?,
        flag("hidden_complete")?,
        flag("reconcile_running")?,
    );
    if indexing || !hidden || running || s["last_reconcile_at"].is_null() {
        return Ok(false);
    }
    if s["num_files"].as_u64() == Some(n as u64) {
        Ok(true)
    } else {
        Err(format!(
            "tgrep serve holds {} files, {n} staged",
            s["num_files"]
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_ready_requires_reconcile_done() {
        let ready = serde_json::json!({
            "indexing": false, "hidden_complete": true, "reconcile_running": false,
            "last_reconcile_at": 1, "last_reconcile_error": null, "num_files": 2,
        });
        let with = |k: &str, v: Value| {
            let mut s = ready.clone();
            s[k] = v;
            status_ready(&s, 2)
        };
        assert_eq!(status_ready(&ready, 2), Ok(true));
        assert_eq!(with("indexing", true.into()), Ok(false));
        assert_eq!(with("hidden_complete", false.into()), Ok(false));
        assert_eq!(with("reconcile_running", true.into()), Ok(false));
        assert_eq!(with("last_reconcile_at", Value::Null), Ok(false));
        assert!(with("last_reconcile_error", "boom".into()).is_err());
        assert!(with("indexing", Value::Null).is_err());
        assert!(status_ready(&ready, 3).is_err());
    }
}

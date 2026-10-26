//! The optional tgrep row: `tgrep index`, one `tgrep serve --no-watch`, then
//! one `tgrep -n` client per query; an intersection is one `tgrep -l` per side
//! and one more over the files they share.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use fmgrep::regex_pattern;
use serde_json::Value;

use crate::rows::{Guard, LINES, Sampler, failure, hits, run_measured, timed};
use crate::{Query, Row, leaves};

pub struct Tgrep {
    pub bin: PathBuf,
    pub version: String,
}

const WRONG_TOOL: &str = "is not the indexed-search tgrep (the crates.io tgrep crate is a different tool); see BENCHMARKS.md for how to build it";

/// `FMGREP_BENCH_TGREP_BIN`, else the first executable `tgrep` on `PATH`, if
/// it is Microsoft tgrep; `Err` is the note saying why there is no tgrep row,
/// also printed to stderr when a binary was found but rejected.
pub fn detect() -> Result<Tgrep, String> {
    let found = std::env::var_os("FMGREP_BENCH_TGREP_BIN").map(PathBuf::from);
    let found = found
        .map(|b| (format!("FMGREP_BENCH_TGREP_BIN={}", b.display()), b))
        .or_else(|| {
            let path = std::env::var_os("PATH").unwrap_or_default();
            on_path(&path).map(|b| ("tgrep on PATH".to_string(), b))
        });
    let (what, bin) = found.ok_or("tgrep: not found on PATH")?;
    let tgrep = check(bin, &what);
    if let Err(note) = &tgrep {
        eprintln!("{note}");
    }
    tgrep
}

fn on_path(path: &std::ffi::OsStr) -> Option<PathBuf> {
    let executable = |m: std::fs::Metadata| m.is_file() && m.permissions().mode() & 0o111 != 0;
    std::env::split_paths(path)
        .map(|d| d.join("tgrep"))
        .find(|p| p.metadata().is_ok_and(executable))
}

/// `bin` as the tgrep row's binary if its CLI has the `index` / `serve` /
/// `--index-path` surface the row drives.
fn check(bin: PathBuf, what: &str) -> Result<Tgrep, String> {
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
    let broken = |e| format!("tgrep: does not work ({e})");
    let version = probe(&["--version"]).map_err(broken)?;
    // The crates.io tgrep exits 0 on these too, printing its own usage.
    let index = probe(&["index", "--help"]).unwrap_or_default();
    let serve = probe(&["serve", "--help"]).unwrap_or_default();
    if !index.contains("--index-path") || !serve.contains("--no-watch") {
        return Err(format!("{what} {WRONG_TOOL}"));
    }
    let version = version.lines().next().unwrap_or_default().trim();
    Ok(Tgrep {
        bin,
        version: version.to_string(),
    })
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
        let what = format!("tgrep {term:?}");
        let start = Instant::now();
        let out = match &leaves(term)[..] {
            [leaf] => {
                let (fixed, e) = pattern(leaf);
                let args: Vec<&str> = LINES.into_iter().chain(fixed).chain([&*e]).collect();
                search(&mut tgrep(&args), &what)?
            }
            all => and(&tgrep, all, &what)?,
        };
        let latency = start.elapsed();
        row.queries
            .push(Query::new(term, latency, hits(&what, &out)?));
    }
    drop(server);
    let peak = sampler.stop();
    (row.footprint, row.query_rss) = (peak.map(|p| p.footprint), peak.map(|p| p.rss));
    Ok(row)
}

/// `-F` unless `leaf` is a regex, and its `-e`.
fn pattern(leaf: &str) -> (Option<&'static str>, String) {
    match regex_pattern(leaf) {
        Some(p) => (None, format!("-e={p}")),
        None => (Some("-F"), format!("-e={leaf}")),
    }
}

/// Runs one tgrep search to exit; `Err` when it left the server.
fn search(cmd: &mut Command, what: &str) -> Result<Output, String> {
    let (out, _, _) = timed(cmd, None)?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let fallback = ["scanning every file", "falling back to local index"];
    if fallback.iter().any(|m| stderr.contains(m)) {
        return Err(failure(what, &out));
    }
    Ok(out)
}

/// One `tgrep -l` per leaf, then the lines matching any leaf in the files every
/// leaf listed, restricted by `--type-add`/`-t` (`-g` and paths bypass the index).
fn and(tgrep: &impl Fn(&[&str]) -> Command, leaves: &[&str], what: &str) -> Result<Output, String> {
    let mut shared: Option<BTreeSet<String>> = None;
    for leaf in leaves {
        let (fixed, e) = pattern(leaf);
        let args: Vec<&str> = ["-l"].into_iter().chain(fixed).chain([&*e]).collect();
        let out = search(&mut tgrep(&args), what)?;
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(failure(what, &out));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let names = stdout.lines().filter_map(|l| l.rsplit('/').next());
        let mut names: BTreeSet<String> =
            names.filter(|n| !n.is_empty()).map(String::from).collect();
        if let Some(shared) = &shared {
            names.retain(|n| shared.contains(n));
        }
        if names.is_empty() {
            let status = ExitStatus::from_raw(1 << 8);
            let (stdout, stderr) = (Vec::new(), Vec::new());
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        shared = Some(names);
    }
    let any = leaves
        .iter()
        .map(|l| format!("-e={}", regex_pattern(l).unwrap_or(l)));
    let files = shared
        .into_iter()
        .flatten()
        .map(|n| format!("--type-add=and:{n}"));
    let args: Vec<String> = any.chain(files).collect();
    let args = args.iter().map(String::as_str).chain(["-t", "and"]);
    let args: Vec<&str> = LINES.into_iter().chain(args).collect();
    search(&mut tgrep(&args), what)
}

pub fn dir_size(dir: &Path) -> u64 {
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

    /// A `tgrep` script in a fresh directory, printing `help` for any `--help`.
    fn fake(help: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("tgrep");
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in --version) echo 'tgrep 9.9';; *--help) printf '%s\\n' '{help}';; esac\n"
        );
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    #[test]
    fn check_rejects_a_tgrep_without_the_index_surface() {
        let dir = fake("USAGE: tgrep [FLAGS] [OPTIONS] <regexp> [--] [paths]");
        let bin = on_path(dir.path().as_os_str()).unwrap();
        let note = check(bin, "tgrep on PATH").err().unwrap();
        assert_eq!(note, format!("tgrep on PATH {WRONG_TOOL}"));
    }

    #[test]
    fn check_accepts_a_tgrep_with_the_index_surface() {
        let dir = fake("--index-path <INDEX_PATH> --no-watch");
        let bin = on_path(dir.path().as_os_str()).unwrap();
        let tgrep = check(bin.clone(), "tgrep on PATH").unwrap();
        assert_eq!((tgrep.bin, tgrep.version.as_str()), (bin, "tgrep 9.9"));
    }

    #[test]
    fn row_answers_intersections_as_rg_does() {
        let Ok(tgrep) = detect() else {
            eprintln!("skipped: no tgrep");
            return;
        };
        if Command::new("rg").arg("--version").output().is_err() {
            eprintln!("skipped: rg is not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let docs = ["alpha\n", "beta\n", "alpha\nx\nbeta\n", "alpha beta\n"];
        for (id, text) in docs.iter().enumerate() {
            std::fs::write(dir.path().join(format!("{id:08}.c")), text).unwrap();
        }
        let terms = [
            "alpha AND beta",
            "/al+pha/ AND beta",
            "alpha AND zzz",
            "zzz AND alpha",
            "beta",
        ]
        .map(String::from);
        let row = row(&tgrep.bin, dir.path(), docs.len(), &terms).unwrap();
        let rg = crate::rows::rg(dir.path(), docs.len(), &terms).unwrap();
        let got: Vec<_> = row
            .queries
            .iter()
            .map(|q| (q.hits.lines, q.hits.files))
            .collect();
        assert_eq!(got, [(3, 2), (3, 2), (0, 0), (0, 0), (3, 3)]);
        for (t, r) in row.queries.iter().zip(&rg.queries) {
            assert_eq!(t.hits.digest, r.hits.digest, "{}", t.term);
        }
    }

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

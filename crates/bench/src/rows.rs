//! The rg and fmgrep rows, and the process measurements every row shares.

use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fmgrep::regex_pattern;

use crate::{Query, Row};

pub fn rg(staged: &Path, n: usize, terms: &[String]) -> Result<Row, String> {
    let mut walk = Command::new("rg");
    walk.args(["--files", "--no-ignore", "--no-require-git", "--"]);
    let (out, _, _) = timed(walk.arg(staged), None)?;
    let listed = out.stdout.split(|&b| b == b'\n').filter(|l| !l.is_empty());
    if listed.count() != n {
        return Err(format!("rg --files does not list the {n} staged files"));
    }
    let mut row = Row::new("rg", n);
    for term in terms {
        let (mode, pattern) = match regex_pattern(term) {
            Some(p) => ("--null-data", p),
            None => ("-F", term.as_str()),
        };
        let mut cmd = Command::new("rg");
        cmd.args([
            "--count-matches",
            "--with-filename",
            "--no-heading",
            "-a",
            mode,
        ]);
        cmd.args(["--no-ignore", "--no-require-git", "-e", pattern, "--"]);
        let (out, latency, peak) = timed(cmd.arg(staged), Some(Duration::from_millis(1)))?;
        row.footprint = row.footprint.max(peak);
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(failure(&format!("rg {term:?}"), &out));
        }
        row.queries
            .push(Query::new(term, latency, count_rows(&out.stdout)?));
    }
    Ok(row)
}

/// Sums `path:N` lines, ended by `\n` or NUL, into (matches, files).
pub fn count_rows(stdout: &[u8]) -> Result<(u64, usize), String> {
    let (mut matches, mut files) = (0, 0);
    for line in stdout.split(|&b| b == b'\n' || b == 0) {
        if line.is_empty() {
            continue;
        }
        let line = String::from_utf8_lossy(line);
        let n = line
            .rsplit_once(':')
            .and_then(|(_, n)| n.parse::<u64>().ok());
        let n = n.ok_or_else(|| format!("not a path:count line: {line:?}"))?;
        matches += n;
        files += usize::from(n > 0);
    }
    Ok((matches, files))
}

pub fn fmgrep(
    bin: &Path,
    staged: &Path,
    store_docs: bool,
    terms: &[String],
) -> Result<Row, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let index = dir.path().join("index");
    let mut cmd = Command::new(bin);
    cmd.arg("index").arg(staged).arg("--output").arg(&index);
    if store_docs {
        cmd.arg("--store-docs");
    }
    let (status, stdout, peak) = run_measured(cmd.arg("--json"))?;
    if !status.success() {
        return Err(format!("fmgrep index: {status}"));
    }
    let summary = json(&stdout)?;
    let field = |k: &str| summary[k].as_u64().ok_or(format!("no {k} in {summary}"));
    let name = if store_docs {
        "fmgrep-store-docs"
    } else {
        "fmgrep"
    };
    let mut row = Row::new(name, field("num_files")? as usize);
    row.index_time = Some(Duration::from_micros(field("build_time_us")?));
    row.index_bytes = Some(field("index_size_bytes")?);
    row.index_peak_rss = peak;

    let start = Instant::now();
    let mut serve = Command::new(bin);
    serve
        .arg("serve")
        .arg(&index)
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    let mut server = Guard(serve.spawn().map_err(|e| format!("fmgrep serve: {e}"))?);
    let sampler = Sampler::start(server.0.id(), Duration::from_millis(10));
    // Ready once the bound address is printed as the first stdout line.
    let mut out = BufReader::new(server.0.stdout.take().expect("piped"));
    let mut addr = String::new();
    out.read_line(&mut addr).map_err(|e| e.to_string())?;
    let addr = addr.trim().to_string();
    if addr.is_empty() {
        return Err("fmgrep serve exited before ready".to_string());
    }
    row.ready = Some(start.elapsed());
    std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));

    for term in terms {
        let mut cmd = Command::new(bin);
        cmd.args(["--server", &addr, "--json", "--approx-counts", "--", term]);
        let (out, latency, _) = timed(&mut cmd, None)?;
        // Exit 1 is a term with no match.
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(failure(&format!("fmgrep --server {term:?}"), &out));
        }
        let report = json(&out.stdout)?;
        let [q] = report["queries"].as_array().map_or(&[][..], Vec::as_slice) else {
            return Err(format!("fmgrep --server {term:?} answered {report}"));
        };
        let counts = (q["total_matches"].as_u64(), q["docs_matched"].as_u64());
        let (Some(matches), Some(files), true) = (counts.0, counts.1, q["term"] == **term) else {
            return Err(format!("fmgrep --server {term:?} answered {report}"));
        };
        row.queries
            .push(Query::new(term, latency, (matches, files as usize)));
    }
    let mut shutdown = Command::new(bin);
    let (out, _, _) = timed(shutdown.args(["--server", &addr, "--shutdown"]), None)?;
    if !out.status.success() {
        return Err(failure("fmgrep --server --shutdown", &out));
    }
    let status = server.0.wait().map_err(|e| e.to_string())?;
    row.footprint = sampler.stop();
    if status.success() {
        Ok(row)
    } else {
        Err(format!("fmgrep serve: {status} after shutdown"))
    }
}

pub fn failure(what: &str, out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    format!("{what}: {}: {}", out.status, stderr.trim())
}

fn json(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let text = String::from_utf8_lossy(bytes);
    serde_json::from_str(&text).map_err(|e| format!("{e}: {text:?}"))
}

pub struct Guard(pub Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs `cmd` to exit and times spawn to exit; with a `tick`, also samples
/// its peak footprint.
pub fn timed(
    cmd: &mut Command,
    tick: Option<Duration>,
) -> Result<(Output, Duration, Option<u64>), String> {
    let start = Instant::now();
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = child
        .spawn()
        .map_err(|e| format!("cannot run {cmd:?}: {e}"))?;
    let sampler = tick.map(|tick| Sampler::start(child.id(), tick));
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let peak = sampler.and_then(Sampler::stop);
    Ok((out, start.elapsed(), peak))
}

/// Runs `cmd` to exit with stdout captured; the peak is `ru_maxrss` from
/// `wait4`, in bytes (Linux reports KiB).
pub fn run_measured(cmd: &mut Command) -> Result<(ExitStatus, Vec<u8>, Option<u64>), String> {
    let child = cmd.stdin(Stdio::null()).stdout(Stdio::piped());
    let mut child = child
        .spawn()
        .map_err(|e| format!("cannot run {cmd:?}: {e}"))?;
    let mut stdout = Vec::new();
    let mut pipe = child.stdout.take().expect("piped");
    pipe.read_to_end(&mut stdout).map_err(|e| e.to_string())?;
    let (pid, mut status) = (child.id() as libc::pid_t, 0);
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    while unsafe { libc::wait4(pid, &mut status, 0, &mut usage) } < 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(format!("wait4: {e}"));
        }
    }
    let unit = if cfg!(target_os = "linux") { 1024 } else { 1 };
    let peak = u64::try_from(usage.ru_maxrss).ok().filter(|&m| m > 0);
    Ok((ExitStatus::from_raw(status), stdout, peak.map(|m| m * unit)))
}

/// The 1-minute load average, to 0.01.
pub fn load1() -> Option<f64> {
    let mut avg = [0f64; 3];
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
    (n == 3).then(|| (avg[0] * 100.0).round() / 100.0)
}

/// Samples a process's footprint every `tick` and keeps the peak.
pub struct Sampler(Arc<AtomicBool>, JoinHandle<Option<u64>>);

impl Sampler {
    pub fn start(pid: u32, tick: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, mut peak) = (Arc::clone(&stop), footprint(pid));
        let handle = std::thread::spawn(move || {
            loop {
                std::thread::sleep(tick);
                if flag.load(Ordering::Relaxed) {
                    return peak;
                }
                peak = peak.max(footprint(pid));
            }
        });
        Self(stop, handle)
    }

    pub fn stop(self) -> Option<u64> {
        self.0.store(true, Ordering::Relaxed);
        self.1.join().unwrap_or_default()
    }
}

/// Anonymous memory: macOS `phys_footprint`, Linux resident minus shared pages.
fn footprint(pid: u32) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        let info_ptr = (&raw mut info).cast::<libc::rusage_info_t>();
        let rc =
            unsafe { libc::proc_pid_rusage(pid as libc::c_int, libc::RUSAGE_INFO_V2, info_ptr) };
        if rc == 0 && info.ri_resident_size > 0 {
            return Some(info.ri_phys_footprint);
        }
    }
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let mut pages = statm.split_whitespace().skip(1).map(str::parse::<u64>);
    let (resident, shared) = (pages.next()?.ok()?, pages.next()?.ok()?);
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096);
    (resident > 0).then(|| resident.saturating_sub(shared) * page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_rows_sums_path_count_lines() {
        assert_eq!(count_rows(b""), Ok((0, 0)));
        assert_eq!(count_rows(b"/s/1.c:3\n/s/a:b/2.c:4\n"), Ok((7, 2)));
        assert_eq!(count_rows(b"/s/1.c:3\0/s/2.c:1\0"), Ok((4, 2)));
        assert!(count_rows(b"/s/1.c\n").is_err());
    }
}

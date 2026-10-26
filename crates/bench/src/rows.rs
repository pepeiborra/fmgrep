//! The rg and fmgrep rows, and the output parsing and process measurements
//! every row shares.

use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fmgrep::regex_pattern;

use crate::{Query, Row, leaves};

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
        let mut cmd = match &leaves(term)[..] {
            [_] => {
                let (pattern, fixed) =
                    regex_pattern(term).map_or((term.as_str(), Some("-F")), |p| (p, None));
                let mut cmd = Command::new("rg");
                cmd.args(LINES).arg("--with-filename").args(WALK);
                cmd.args(fixed).args(["-e", pattern, "--"]).arg(staged);
                cmd
            }
            all => and_pipe("rg", &WALK.join(" "), staged, all),
        };
        let (out, latency, peak) = timed(&mut cmd, Some(Duration::from_millis(1)))?;
        let what = format!("rg {term:?}");
        // xargs hides rg's exit status.
        if !out.stderr.is_empty() {
            return Err(failure(&what, &out));
        }
        row.footprint = row.footprint.max(peak.map(|p| p.footprint));
        row.query_rss = row.query_rss.max(peak.map(|p| p.rss));
        let hits = hits(&what, &out)?;
        row.queries.push(Query::new(term, latency, hits));
    }
    Ok(row)
}

const WALK: [&str; 3] = ["-a", "--no-ignore", "--no-require-git"];

/// Lists the files holding every leaf, one `prog flags` per leaf, then
/// prints their lines matching any leaf; `prog` and `flags` are shell words.
pub fn and_pipe(prog: &str, flags: &str, staged: &Path, leaves: &[&str]) -> Command {
    let list = |k: usize, leaf: &str| {
        let fixed = regex_pattern(leaf).map_or(" -F", |_| "");
        format!("{prog} {flags} --files-with-matches --null{fixed} -e \"${{{k}}}\" --")
    };
    let n = leaves.len();
    let mut script = format!("{} \"${{{}}}\"", list(1, leaves[0]), n + 1);
    for (k, leaf) in leaves.iter().enumerate().skip(1) {
        script += &format!(" | xargs -0 -r {}", list(k + 1, leaf));
    }
    let any: String = (1..=n).map(|k| format!(" -e \"${{{k}}}\"")).collect();
    let lines = LINES.join(" ");
    script += &format!(" | xargs -0 -r {prog} {lines} --with-filename {flags}{any} --");
    let mut cmd = Command::new("sh");
    let patterns = leaves.iter().map(|l| regex_pattern(l).unwrap_or(l));
    cmd.args(["-c", &script, "sh"]).args(patterns).arg(staged);
    cmd
}

/// Printed by every row: `path:N:text`, one line per matching line.
pub const LINES: [&str; 4] = ["-n", "--no-heading", "--color", "never"];

pub struct Hits {
    pub lines: u64,
    pub files: usize,
    /// Of the sorted `(file id, line number)` set.
    pub digest: u64,
}

/// Reads `path:N:text` lines of staged `{id:08}.c` files, in any order and
/// under any path prefix, from a run that exited 0 or 1.
pub fn hits(what: &str, out: &Output) -> Result<Hits, String> {
    if !matches!(out.status.code(), Some(0 | 1)) {
        return Err(failure(what, out));
    }
    let lines = out.stdout.split(|&b| b == b'\n').filter(|l| !l.is_empty());
    let bad = |l: &[u8]| format!("{what}: not path:N:text: {:?}", String::from_utf8_lossy(l));
    let set: Result<Vec<_>, _> = lines
        .map(|l| staged_line(l).ok_or_else(|| bad(l)))
        .collect();
    let mut set = set?;
    set.sort_unstable();
    Ok(Hits {
        lines: set.len() as u64,
        files: set.chunk_by(|a, b| a.0 == b.0).count(),
        digest: BuildHasherDefault::<DefaultHasher>::default().hash_one(&set),
    })
}

/// The id and line number at the first `:` that ends a staged name and
/// starts `N:`.
fn staged_line(line: &[u8]) -> Option<(u32, u32)> {
    let number = |s: &[u8]| match s.iter().all(u8::is_ascii_digit) {
        true => std::str::from_utf8(s).ok()?.parse().ok(),
        false => None,
    };
    (0..line.len()).filter(|&i| line[i] == b':').find_map(|i| {
        let name = line[..i].rsplit(|&b| b == b'/').next()?;
        let id = name.strip_suffix(b".c").filter(|id| id.len() == 8)?;
        let rest = &line[i + 1..];
        let n = rest.iter().position(|&b| b == b':')?;
        Some((number(id)?, number(&rest[..n])?))
    })
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
    let summary: serde_json::Value =
        serde_json::from_slice(&stdout).map_err(|e| format!("fmgrep index --json: {e}"))?;
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
        let leaves = leaves(term);
        cmd.args(["--server", &addr]).args(LINES);
        cmd.args((leaves.len() > 1).then_some("--and"));
        cmd.arg("--").args(leaves);
        let (out, latency, _) = timed(&mut cmd, None)?;
        let hits = hits(&format!("fmgrep --server {term:?}"), &out)?;
        row.queries.push(Query::new(term, latency, hits));
    }
    let mut shutdown = Command::new(bin);
    let (out, _, _) = timed(shutdown.args(["--server", &addr, "--shutdown"]), None)?;
    if !out.status.success() {
        return Err(failure("fmgrep --server --shutdown", &out));
    }
    let status = server.0.wait().map_err(|e| e.to_string())?;
    let peak = sampler.stop();
    (row.footprint, row.query_rss) = (peak.map(|p| p.footprint), peak.map(|p| p.rss));
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

pub struct Guard(pub Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs `cmd` to exit and times spawn to exit; with a `tick`, also samples
/// its peak memory.
pub fn timed(
    cmd: &mut Command,
    tick: Option<Duration>,
) -> Result<(Output, Duration, Option<Peak>), String> {
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

/// Peak anonymous memory and peak resident set, file-backed pages included.
#[derive(Clone, Copy)]
pub struct Peak {
    pub footprint: u64,
    pub rss: u64,
}

/// Samples a process's memory every `tick` and keeps each field's peak.
pub struct Sampler(Arc<AtomicBool>, JoinHandle<Option<Peak>>);

impl Sampler {
    pub fn start(pid: u32, tick: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, mut peak) = (Arc::clone(&stop), memory(pid));
        let handle = std::thread::spawn(move || {
            loop {
                std::thread::sleep(tick);
                if flag.load(Ordering::Relaxed) {
                    return peak;
                }
                peak = match (peak, memory(pid)) {
                    (Some(a), Some(b)) => Some(Peak {
                        footprint: a.footprint.max(b.footprint),
                        rss: a.rss.max(b.rss),
                    }),
                    (a, b) => a.or(b),
                };
            }
        });
        Self(stop, handle)
    }

    pub fn stop(self) -> Option<Peak> {
        self.0.store(true, Ordering::Relaxed);
        self.1.join().unwrap_or_default()
    }
}

/// Footprint is macOS `phys_footprint` or Linux resident minus shared pages.
pub fn memory(pid: u32) -> Option<Peak> {
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        let info_ptr = (&raw mut info).cast::<libc::rusage_info_t>();
        let rc =
            unsafe { libc::proc_pid_rusage(pid as libc::c_int, libc::RUSAGE_INFO_V2, info_ptr) };
        if rc == 0 && info.ri_resident_size > 0 {
            let (footprint, rss) = (info.ri_phys_footprint, info.ri_resident_size);
            return Some(Peak { footprint, rss });
        }
    }
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let mut pages = statm.split_whitespace().skip(1).map(str::parse::<u64>);
    let (resident, shared) = (pages.next()?.ok()?, pages.next()?.ok()?);
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096);
    (resident > 0).then(|| Peak {
        footprint: resident.saturating_sub(shared) * page,
        rss: resident * page,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(code: i32, stdout: &[u8]) -> Result<Hits, String> {
        let (status, stdout) = (ExitStatus::from_raw(code << 8), stdout.to_vec());
        let stderr = Vec::new();
        let out = Output {
            status,
            stdout,
            stderr,
        };
        hits("t", &out)
    }

    #[test]
    fn hits_reads_matched_lines_of_staged_files() {
        let out = b"/s:x/00000002.c:7:a: b\n00000000.c:1:foo\n\n/s/00000002.c:9:\xff 00000001.c:3:";
        let h = run(0, out).unwrap();
        assert_eq!((h.lines, h.files), (3, 2));
        let reordered = run(0, b"/t/00000002.c:9:x\n00000002.c:7:\n/u/00000000.c:1:y\n").unwrap();
        assert_eq!(reordered.digest, h.digest);
        let other = run(0, b"00000002.c:8:\n00000002.c:9:\n00000000.c:1:\n").unwrap();
        assert_ne!(other.digest, h.digest);
        let none = run(1, b"").unwrap();
        assert_eq!((none.lines, none.files), (0, 0));
        assert!(run(2, b"").is_err());
        for bad in [
            &b"/s/00000000.c:3\n"[..],
            b"/s/00000000.c\n1:foo\n",
            b"/s/evil.c:1:foo\n",
            b"/s/0000000.c:1:foo\n",
            b"/s/00000000.c::foo\n",
        ] {
            assert!(run(0, bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn memory_reads_rss_and_footprint_of_a_live_process() {
        let peak = memory(std::process::id()).unwrap();
        assert!(peak.rss > 0 && peak.footprint > 0);
    }

    #[test]
    fn rg_pipe_script_is_unchanged() {
        let cmd = and_pipe("rg", &WALK.join(" "), Path::new("/s"), &["a", "/b+/"]);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
        let walk = "-a --no-ignore --no-require-git";
        let script = format!(
            "rg {walk} --files-with-matches --null -F -e \"${{1}}\" -- \"${{3}}\" \
             | xargs -0 -r rg {walk} --files-with-matches --null -e \"${{2}}\" -- \
             | xargs -0 -r rg -n --no-heading --color never --with-filename {walk} -e \"${{1}}\" -e \"${{2}}\" --"
        );
        assert_eq!(args, ["-c", &script, "sh", "a", "b+", "/s"]);
    }

    #[test]
    fn rg_answers_an_intersection_with_a_pipe() {
        if Command::new("rg").arg("--version").output().is_err() {
            eprintln!("skipped: rg is not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let docs = ["alpha\n", "beta\n", "alpha\nx\nbeta\n", "alpha beta\n"];
        for (id, text) in docs.iter().enumerate() {
            std::fs::write(dir.path().join(format!("{id:08}.c")), text).unwrap();
        }
        let terms = ["alpha AND beta", "/al+pha/ AND beta", "alpha AND zzz"];
        let row = rg(dir.path(), docs.len(), &terms.map(String::from)).unwrap();
        let got: Vec<_> = row
            .queries
            .iter()
            .map(|q| (q.hits.lines, q.hits.files))
            .collect();
        assert_eq!(got, [(3, 2), (3, 2), (0, 0)]);
        assert!(rg(dir.path(), docs.len(), &["/(/ AND beta".to_string()]).is_err());
    }
}

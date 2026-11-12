//! The rg and fmgrep rows, and the output parsing and process measurements
//! every row shares.

use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fmgrep::regex_pattern;

use crate::verify::{Paths, rg as rg_cmd};
use crate::{Query, Row, RowKind, Server, leaves, with_warm_up};

pub fn rg(paths: &Paths, root: &Path, terms: &[String]) -> Result<Row, String> {
    let mut row = Row::new(RowKind::Rg);
    // rg reads stdin when given no path and stdin is not a terminal.
    let (out, _, _) = timed(rg_cmd().args(["-l", "-e", "", "--"]).arg(root), None)?;
    row.searched = paths.listed("rg -l ''", &out)?;
    for (term, warm_up) in with_warm_up(terms) {
        let mut cmd = match &leaves(term)[..] {
            [_] => {
                let (pattern, fixed) =
                    regex_pattern(term).map_or((term.as_str(), Some("-F")), |p| (p, None));
                let mut cmd = rg_cmd();
                cmd.args(LINES).arg("--with-filename");
                cmd.args(fixed).args(["-e", pattern, "--"]).arg(root);
                cmd
            }
            all => {
                let mut cmd = and_pipe("rg", "", root, all, None);
                cmd.env_remove("RIPGREP_CONFIG_PATH");
                cmd
            }
        };
        let (out, latency, peak) = timed(&mut cmd, Some(Duration::from_millis(1)))?;
        let what = format!("rg {term:?}");
        // xargs hides rg's exit status.
        if !out.stderr.is_empty() {
            return Err(failure(&what, &out));
        }
        let hits = paths.hits(&what, &out)?;
        if warm_up {
            continue;
        }
        row.footprint = row.footprint.max(peak.map(|p| p.footprint));
        row.query_rss = row.query_rss.max(peak.map(|p| p.rss));
        row.queries.push(Query::new(term, latency, hits));
    }
    Ok(row)
}

/// Lists the files holding every leaf, one `prog flags` per leaf, then
/// prints their lines matching any leaf; `prog` and `flags` are shell words.
/// With `stderr`, stage k (from 1) writes its stderr to `stderr/k`.
pub fn and_pipe(
    prog: &str,
    flags: &str,
    root: &Path,
    leaves: &[&str],
    stderr: Option<&Path>,
) -> Command {
    let flags = if flags.is_empty() {
        String::new()
    } else {
        format!(" {flags}")
    };
    let list = |k: usize, leaf: &str| {
        let fixed = regex_pattern(leaf).map_or(" -F", |_| "");
        format!("{prog}{flags} --files-with-matches --null{fixed} -e \"${{{k}}}\" --")
    };
    let n = leaves.len();
    let to = |k: usize| stderr.map_or(String::new(), |_| format!(" 2>\"${{{}}}/{k}\"", n + 2));
    let mut script = format!("{} \"${{{}}}\"{}", list(1, leaves[0]), n + 1, to(1));
    for (k, leaf) in leaves.iter().enumerate().skip(1) {
        script += &format!(" | xargs -0 -r {}{}", list(k + 1, leaf), to(k + 1));
    }
    let any: String = (1..=n).map(|k| format!(" -e \"${{{k}}}\"")).collect();
    let lines = LINES.join(" ");
    script += &format!(" | xargs -0 -r {prog} {lines} --with-filename{flags}{any} --");
    script += &to(n + 1);
    let mut cmd = Command::new("sh");
    let patterns = leaves.iter().map(|l| regex_pattern(l).unwrap_or(l));
    cmd.args(["-c", &script, "sh"])
        .args(patterns)
        .arg(root)
        .args(stderr);
    cmd
}

/// Printed by every row: `path:N:text`, one line per matching line.
pub const LINES: [&str; 4] = ["-n", "--no-heading", "--color", "never"];

/// The non-empty files of the walk, sorted: what an index of it searches.
fn searched(paths: &Paths, docs: &[fmgrep::DocMeta]) -> Vec<u32> {
    let searched = docs.iter().filter(|d| d.len > 0);
    let mut searched: Vec<u32> = searched.filter_map(|d| paths.id(&d.path)).collect();
    searched.sort_unstable();
    searched
}

/// Each query through `fmgrep --server addr`, after the warm-up.
fn clients(
    bin: &Path,
    addr: &str,
    paths: &Paths,
    terms: &[String],
    row: &mut Row,
) -> Result<(), String> {
    for (term, warm_up) in with_warm_up(terms) {
        let mut cmd = Command::new(bin);
        let leaves = leaves(term);
        cmd.args(["--server", addr]).args(LINES);
        cmd.args((leaves.len() > 1).then_some("--and"));
        cmd.arg("--").args(leaves);
        let (out, latency, _) = timed(&mut cmd, None)?;
        let hits = paths.hits(&format!("fmgrep --server {term:?}"), &out)?;
        if !warm_up {
            row.queries.push(Query::new(term, latency, hits));
        }
    }
    Ok(())
}

/// Refuses a server that does not serve the walk's `files` files of `root`.
pub fn check_server(bin: &Path, addr: &str, root: &Path, files: usize) -> Result<(), String> {
    let (out, _, _) = timed(Command::new(bin).args(["--server", addr, "--status"]), None)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(stderr.trim().trim_start_matches("error: ").to_string());
    }
    let status: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("--status: {e}"))?;
    let (served, at) = (status["files"].as_u64(), status["root"].as_str());
    let (Some(served), Some(at)) = (served, at) else {
        return Err(format!("--status: no files or root in {status}"));
    };
    if status["store_docs"] != true && Path::new(at) != root {
        return Err(format!("serves {at}, not {}", root.display()));
    }
    if served != files as u64 {
        return Err(format!("serves {served} files, the checkout has {files}"));
    }
    Ok(())
}

/// A `--server` row: the queries against a server it neither starts nor
/// stops, and so does not measure.
pub fn server(
    bin: &Path,
    paths: &Paths,
    docs: &[fmgrep::DocMeta],
    server: &Server,
    terms: &[String],
) -> Result<Row, String> {
    let mut row = Row::new(server.name);
    row.searched = searched(paths, docs);
    clients(bin, &server.addr, paths, terms, &mut row)?;
    Ok(row)
}

/// The `fmgrep` and `fmgrep-store-docs` rows, served; `fmgrep-direct`, a
/// process per query.
pub fn fmgrep(
    bin: &Path,
    (paths, root, docs): (&Paths, &Path, &[fmgrep::DocMeta]),
    name: RowKind,
    terms: &[String],
) -> Result<Row, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let index = dir.path().join("index");
    let mut cmd = Command::new(bin);
    cmd.arg("index").arg(root).arg("--output").arg(&index);
    if name == RowKind::FmgrepStoreDocs {
        cmd.arg("--store-docs");
    }
    let (status, stdout, peak) = run_measured(cmd.arg("--json"))?;
    if !status.success() {
        return Err(format!("fmgrep index: {status}"));
    }
    let summary: serde_json::Value =
        serde_json::from_slice(&stdout).map_err(|e| format!("fmgrep index --json: {e}"))?;
    let field = |k: &str| summary[k].as_u64().ok_or(format!("no {k} in {summary}"));
    let (indexed, n) = (field("num_files")?, docs.len());
    if indexed != n as u64 {
        return Err(format!(
            "fmgrep indexed {indexed} files, not the {n} listed"
        ));
    }
    let mut row = Row::new(name);
    row.searched = searched(paths, docs);
    row.index_time = Some(Duration::from_micros(field("build_time_us")?));
    row.index_bytes = Some(field("index_size_bytes")?);
    row.index_peak_rss = peak;
    if name == RowKind::FmgrepDirect {
        direct(bin, (paths, root), &index, terms, &mut row)?;
        return Ok(row);
    }

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

    clients(bin, &addr, paths, terms, &mut row)?;
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

/// Each query as its own `fmgrep --index` from the corpus root, as an agent
/// without a server runs it, after the warm-up; the footprint and RSS are
/// the largest timed query's.
fn direct(
    bin: &Path,
    (paths, root): (&Paths, &Path),
    index: &Path,
    terms: &[String],
    row: &mut Row,
) -> Result<(), String> {
    let bin = bin
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", bin.display()))?;
    let mut largest = None;
    for (term, warm_up) in with_warm_up(terms) {
        let mut cmd = Command::new(&bin);
        let leaves = leaves(term);
        cmd.current_dir(root).arg("--index").arg(index).args(LINES);
        cmd.args((leaves.len() > 1).then_some("--and"));
        cmd.arg("--").args(leaves);
        let (out, latency, peak) = timed(&mut cmd, Some(Duration::from_millis(1)))?;
        let hits = paths.hits(&format!("fmgrep --index {term:?}"), &out)?;
        if warm_up {
            continue;
        }
        row.queries.push(Query::new(term, latency, hits));
        largest = Peak::max(largest, peak);
    }
    (row.footprint, row.query_rss) = (largest.map(|p| p.footprint), largest.map(|p| p.rss));
    Ok(())
}

pub fn failure(what: &str, out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    format!("{what}: {}: {}", out.status, stderr.trim())
}

/// Retries transient `ETXTBSY` errors while another process closes an
/// inherited write handle to an executable.
pub fn output_retry_etxtbsy(cmd: &mut Command) -> std::io::Result<Output> {
    #[cfg(target_os = "linux")]
    {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match cmd.output() {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                result => return result,
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    cmd.output()
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

impl Peak {
    fn max(a: Option<Peak>, b: Option<Peak>) -> Option<Peak> {
        match (a, b) {
            (Some(a), Some(b)) => Some(Peak {
                footprint: a.footprint.max(b.footprint),
                rss: a.rss.max(b.rss),
            }),
            (a, b) => a.or(b),
        }
    }
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
                peak = Peak::max(peak, memory(pid));
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

    #[test]
    fn memory_reads_rss_and_footprint_of_a_live_process() {
        let peak = memory(std::process::id()).unwrap();
        assert!(peak.rss > 0 && peak.footprint > 0);
    }

    #[test]
    fn fmgrep_clients_send_the_first_query_once_untimed_before_the_timed_ones() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (bin, log) = (root.join("fmgrep"), root.join("log"));
        let script = format!("#!/bin/sh\necho \"$*\" >>'{}'\nexit 1\n", log.display());
        crate::fake_bin::write(&bin, &script);
        output_retry_etxtbsy(&mut Command::new(&bin)).unwrap();
        std::fs::remove_file(&log).unwrap();
        let paths = Paths::from_lists(&root, b"", b"", &[]);
        let terms = ["a", "b AND c"].map(String::from);
        let mut served = Row::new(RowKind::Fmgrep);
        clients(&bin, "h:1", &paths, &terms, &mut served).unwrap();
        let mut by_process = Row::new(RowKind::FmgrepDirect);
        let tree = (&paths, root.as_path());
        direct(&bin, tree, Path::new("/i"), &terms, &mut by_process).unwrap();
        for row in [&served, &by_process] {
            let timed: Vec<_> = row.queries.iter().map(|q| q.term.as_str()).collect();
            assert_eq!(timed, ["a", "b AND c"]);
        }
        let runs = std::fs::read_to_string(&log).unwrap();
        let runs: Vec<_> = runs.lines().collect();
        let each = ["-- a", "-- a", "--and -- b c"];
        let want: Vec<String> = ["--server h:1", "--index /i"]
            .iter()
            .flat_map(|how| each.map(|what| format!("{how} -n --no-heading --color never {what}")))
            .collect();
        assert_eq!(runs, want);
    }

    #[test]
    fn rg_pipe_script_is_unchanged() {
        let cmd = and_pipe("rg", "", Path::new("/s"), &["a", "/b+/"], None);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
        let script = "rg --files-with-matches --null -F -e \"${1}\" -- \"${3}\" \
             | xargs -0 -r rg --files-with-matches --null -e \"${2}\" -- \
             | xargs -0 -r rg -n --no-heading --color never --with-filename -e \"${1}\" -e \"${2}\" --";
        assert_eq!(args, ["-c", script, "sh", "a", "b+", "/s"]);
    }

    #[test]
    fn pipe_writes_each_stage_stderr_to_its_own_file() {
        if rg_cmd().arg("--version").output().is_err() {
            eprintln!("skipped: rg is not on PATH");
            return;
        }
        let (_dir, root) = crate::tgrep::tests::tree();
        let stderr = tempfile::tempdir().unwrap();
        let run = |stderr: Option<&Path>| {
            let mut cmd = and_pipe("rg", "--debug", &root, &["alpha", "beta"], stderr);
            timed(cmd.env_remove("RIPGREP_CONFIG_PATH"), None)
                .unwrap()
                .0
        };
        let (split, merged) = (run(Some(stderr.path())), run(None));
        assert!(split.stderr.is_empty() && !merged.stderr.is_empty());
        let lines = |o: &Output| {
            let mut l: Vec<_> = o
                .stdout
                .split(|&b| b == b'\n')
                .map(<[u8]>::to_vec)
                .collect();
            l.sort();
            l
        };
        assert!(!split.stdout.is_empty() && lines(&split) == lines(&merged));
        for k in 1..=3 {
            let text = std::fs::read_to_string(stderr.path().join(k.to_string())).unwrap();
            assert!(!text.is_empty(), "stage {k}");
            assert!(text.lines().all(|l| l.starts_with("rg: DEBUG|")), "{text}");
        }
    }

    #[test]
    fn rg_answers_an_intersection_with_a_pipe() {
        if rg_cmd().arg("--version").output().is_err() {
            eprintln!("skipped: rg is not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let docs = [
            ("a/x.c", "alpha\n"),
            ("b/x.c", "beta\n"),
            ("c:1:d.txt", "alpha\nx\nbeta\n"),
            ("a/b/x.c", "alpha beta\n"),
        ];
        for (name, text) in docs {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let paths = Paths::new(&root, &[]).unwrap();
        let terms = ["alpha AND beta", "/al+pha/ AND beta", "alpha AND zzz"];
        let row = rg(&paths, &root, &terms.map(String::from)).unwrap();
        assert_eq!(row.searched.len(), 4);
        let files = |q: &Query| {
            let per_file = q.hits.per_file.iter();
            per_file
                .map(|f| (paths.name(f.file).into_owned(), f.lines))
                .collect::<Vec<_>>()
        };
        let both = [("c:1:d.txt".to_string(), 2), ("a/b/x.c".to_string(), 1)];
        let mut want = both.to_vec();
        want.sort_by_key(|f| paths.id(&f.0));
        assert_eq!(files(&row.queries[0]), want);
        assert_eq!(files(&row.queries[1]), want);
        assert_eq!(files(&row.queries[2]), []);
        assert!(rg(&paths, &root, &["/(/ AND beta".to_string()]).is_err());
    }
}

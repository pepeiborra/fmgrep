//! The optional elgrep row: one `eg-indexd` owned by the row, then one
//! `eg -n` per query reading its index; an intersection is the rg pipe with
//! `eg`. The index answers only on Linux; elsewhere every query scans.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fmgrep::regex_pattern;
use serde_json::Value;

use crate::report::bytes;
use crate::rows::{LINES, Peak, Sampler, and_pipe, failure, hits, memory, timed};
use crate::tgrep::dir_size;
use crate::{Query, Row, leaves};

pub struct Elgrep {
    pub bin: PathBuf,
    pub daemon: PathBuf,
    pub version: String,
}

/// A literal no staged file holds: the probe only waits out the cold build.
const PROBE: &str = "elgrep-bench-probe-7f3a9c0e";
const SCAN: &str = "scanning directly because of ";
/// eg's plain notice beside the `--debug` scan line, when the index refuses a pattern.
const FALLBACK: &str = "; searching without it";

/// `FMGREP_BENCH_ELGREP_BIN`, else the first executable `eg` on `PATH`, if it
/// is elgrep's with `eg-indexd` beside it; `Err` is the note saying why there
/// is no elgrep row, also printed to stderr when a binary was rejected.
pub fn detect() -> Result<Elgrep, String> {
    let found = std::env::var_os("FMGREP_BENCH_ELGREP_BIN").map(PathBuf::from);
    let found = found
        .map(|b| (format!("FMGREP_BENCH_ELGREP_BIN={}", b.display()), b))
        .or_else(|| {
            let path = std::env::var_os("PATH").unwrap_or_default();
            on_path(&path).map(|b| ("eg on PATH".to_string(), b))
        });
    let (what, bin) = found.ok_or("elgrep: eg not found on PATH")?;
    let elgrep = check(bin, &what);
    if let Err(note) = &elgrep {
        eprintln!("{note}");
    }
    elgrep
}

fn on_path(path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|d| d.join("eg"))
        .find(|p| executable(p))
}

fn executable(p: &Path) -> bool {
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn check(bin: PathBuf, what: &str) -> Result<Elgrep, String> {
    let mut cmd = Command::new(&bin);
    cmd.arg("--version").env_remove("RIPGREP_CONFIG_PATH");
    let out = cmd.stdin(Stdio::null()).output();
    let out = out.map_err(|e| format!("elgrep: {what} does not run ({e})"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let version = stdout.lines().next().unwrap_or_default().trim();
    if !out.status.success() || !version.starts_with("elgrep (eg) ") {
        return Err(format!(
            "elgrep: {what} is not elgrep's eg (--version: {version:?})"
        ));
    }
    let daemon = bin.with_file_name("eg-indexd");
    if !executable(&daemon) {
        let at = bin.display();
        return Err(format!("elgrep: no executable eg-indexd beside {at}"));
    }
    let version = version.to_string();
    Ok(Elgrep {
        bin,
        daemon,
        version,
    })
}

/// Every eg and eg-indexd of a row: its runtime root, no daemon of eg's own.
fn env<'a>(cmd: &'a mut Command, runtime: &Path) -> &'a mut Command {
    cmd.env("EG_INDEXD_RUNTIME_ROOT", runtime)
        .env("EG_INDEXD_DISABLE_AUTOSPAWN", "1")
        .env("EG_BENCH_NO_COMPARE", "1")
        .env_remove("RIPGREP_CONFIG_PATH")
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn row(e: &Elgrep, staged: &Path, n: usize, terms: &[String]) -> Result<Row, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (index, runtime) = (dir.path().join("index"), dir.path().join("runtime"));
    // One walk for every eg: the daemon builds the index with the first one's.
    let flags = [
        format!("--index-dir={}", index.display()),
        "--no-ignore".to_string(),
        "--no-require-git".to_string(),
    ];
    let eg = |args: &[&str]| {
        let mut cmd = Command::new(&e.bin);
        env(&mut cmd, &runtime).args(&flags).args(args);
        cmd
    };
    let (out, _, _) = timed(eg(&["--files", "--"]).arg(staged), None)?;
    let listed = out.stdout.split(|&b| b == b'\n').filter(|l| !l.is_empty());
    let listed = listed.count();
    if !out.status.success() || listed != n {
        return Err(format!("eg --files lists {listed} of the {n} staged files"));
    }
    let mut row = Row::new("elgrep", n);

    let daemon = if cfg!(target_os = "linux") {
        let start = Instant::now();
        let daemon = Daemon::start(e, &runtime)?;
        let (out, _, _) = timed(eg(&["--bench", "-F", "-e", PROBE, "--"]).arg(staged), None)?;
        let build = probe(&out, n)?;
        row.ready = Some(start.elapsed());
        row.index_time = Some(Duration::from_secs_f64(build / 1000.0));
        row.index_bytes = Some(dir_size(&index.join("index")));
        Some(daemon)
    } else {
        None
    };

    let mut quoted: Vec<String> = flags.iter().map(|f| quote(f)).collect();
    quoted.push("--debug".to_string());
    let (prog, quoted) = (quote(&e.bin.to_string_lossy()), quoted.join(" "));
    let mut scans = Scans::default();
    for term in terms {
        let mut cmd = match &leaves(term)[..] {
            [_] => {
                let (pattern, fixed) =
                    regex_pattern(term).map_or((term.as_str(), Some("-F")), |p| (p, None));
                let mut cmd = eg(&["--debug", "--with-filename"]);
                cmd.args(LINES).args(fixed).args(["-e", pattern, "--"]);
                cmd.arg(staged);
                cmd
            }
            all => {
                let mut cmd = and_pipe(&prog, &quoted, staged, all);
                env(&mut cmd, &runtime);
                cmd
            }
        };
        let (out, latency, peak) = timed(&mut cmd, Some(Duration::from_millis(1)))?;
        let what = format!("eg {term:?}");
        scans
            .add(&out.stderr)
            .map_err(|l| failure(&what, &out) + &l)?;
        row.footprint = row.footprint.max(peak.map(|p| p.footprint));
        row.query_rss = row.query_rss.max(peak.map(|p| p.rss));
        row.queries
            .push(Query::new(term, latency, hits(&what, &out)?));
    }
    match daemon.map(Daemon::stop) {
        Some((peak, index_peak)) => {
            row.index_peak_rss = index_peak;
            let rss = |p: Peak| (bytes(p.rss), bytes(p.footprint));
            if let Some((rss, footprint)) = peak.map(rss) {
                row.notes
                    .push(format!("eg-indexd peak RSS {rss}, footprint {footprint}"));
            }
        }
        None => row.notes.push(format!(
            "the index answers only on Linux, so no eg-indexd ran on {}",
            std::env::consts::OS
        )),
    }
    row.notes.push(scans.note(terms.len()));
    Ok(row)
}

/// The probe's cold-build wait in ms; `Err` unless the index answered it
/// over exactly the `n` staged files, none skipped as binary.
fn probe(out: &Output, n: usize) -> Result<f64, String> {
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| failure(&format!("eg --bench: {e}"), out))?;
    if v["ok"] != true || !matches!(out.status.code(), Some(0 | 1)) {
        return Err(failure(
            &format!("eg --bench: {}", v["unsupported_reason"]),
            out,
        ));
    }
    let count = |k: &str| v["counts"][k].as_u64().unwrap_or(u64::MAX);
    let (files, binary) = (count("total_manifest_files"), count("binary_skipped_files"));
    if (files, binary) != (n as u64, 0) {
        return Err(format!(
            "eg's index holds {files} files, {binary} skipped as binary; {n} staged"
        ));
    }
    let build = v["timings_ms"]["cold_build_total"].as_f64();
    let build = build.filter(|_| v["cold_build"] == true);
    build.ok_or(format!("eg --bench built no index: {v}"))
}

/// Queries `--debug` said were scanned instead of read from the index.
#[derive(Default)]
struct Scans {
    queries: usize,
    reasons: BTreeMap<String, usize>,
}

impl Scans {
    /// Tallies one query's stderr; `Err` is a line that is not `--debug`'s.
    fn add(&mut self, stderr: &[u8]) -> Result<(), String> {
        let mut reasons = BTreeSet::new();
        let text = String::from_utf8_lossy(stderr);
        for line in text.lines().filter(|l| !l.is_empty()) {
            if line.starts_with("eg: ") && line.ends_with(FALLBACK) {
                continue;
            }
            if !line.starts_with("eg: DEBUG|") {
                return Err(format!("\nnot a --debug line: {line}"));
            }
            if let Some((_, reason)) = line.split_once(SCAN) {
                reasons.insert(reason.trim().to_string());
            }
        }
        self.queries += usize::from(!reasons.is_empty());
        for r in reasons {
            *self.reasons.entry(r).or_default() += 1;
        }
        Ok(())
    }

    fn note(&self, of: usize) -> String {
        let k = self.queries;
        let why = match self.reasons.len() {
            0 => String::new(),
            1 => format!(": {}", self.reasons.keys().next().expect("one")),
            _ => {
                let each = self.reasons.iter().map(|(r, k)| format!("{r} ({k})"));
                format!(": {}", each.collect::<Vec<_>>().join("; "))
            }
        };
        format!("{k} of {of} queries scanned without the index{why}")
    }
}

/// The row's `eg-indexd`, killed with its workers on drop; samples itself
/// and the peak RSS of its build workers.
struct Daemon {
    child: Child,
    sampler: Option<Sampler>,
    workers: Option<Workers>,
}

impl Daemon {
    fn start(e: &Elgrep, runtime: &Path) -> Result<Self, String> {
        let mut cmd = Command::new(&e.daemon);
        env(&mut cmd, runtime).arg("--runtime-root").arg(runtime);
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        let child = cmd.stderr(Stdio::null()).spawn();
        let child = child.map_err(|err| format!("eg-indexd: {err}"))?;
        let pid = child.id();
        let (sampler, workers) = (
            Sampler::start(pid, Duration::from_millis(10)),
            Workers::start(pid),
        );
        let mut daemon = Daemon {
            child,
            sampler: Some(sampler),
            workers: Some(workers),
        };
        let start = Instant::now();
        let read = |f: &str| std::fs::read_to_string(runtime.join(f)).unwrap_or_default();
        loop {
            if let Ok(Some(status)) = daemon.child.try_wait() {
                return Err(format!("eg-indexd: {status} before startup-ready"));
            }
            let owner = read("daemon.lock");
            if !owner.trim().is_empty() && read("startup-ready").trim() == owner.trim() {
                return Ok(daemon);
            }
            if start.elapsed() > Duration::from_secs(5) {
                return Err("eg-indexd: no startup-ready after 5 s".to_string());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Kills it; its peak memory and its workers' peak RSS.
    fn stop(mut self) -> (Option<Peak>, Option<u64>) {
        let workers = self.workers.take().and_then(Workers::stop);
        self.kill();
        (self.sampler.take().and_then(Sampler::stop), workers)
    }

    fn kill(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            for pid in children(self.child.id()) {
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.kill();
        if let Some(w) = self.workers.take() {
            w.stop();
        }
        if let Some(s) = self.sampler.take() {
            s.stop();
        }
    }
}

/// Peak RSS of a process's children, sampled every 10 ms.
struct Workers(Arc<AtomicBool>, JoinHandle<Option<u64>>);

impl Workers {
    fn start(pid: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut peak = None;
            while !flag.load(Ordering::Relaxed) {
                let rss = children(pid)
                    .into_iter()
                    .filter_map(|c| memory(c).map(|m| m.rss));
                peak = peak.max(rss.max());
                std::thread::sleep(Duration::from_millis(10));
            }
            peak
        });
        Self(stop, handle)
    }

    fn stop(self) -> Option<u64> {
        self.0.store(true, Ordering::Relaxed);
        self.1.join().unwrap_or_default()
    }
}

/// Linux `/proc/PID/task/*/children`; empty elsewhere.
fn children(pid: u32) -> Vec<u32> {
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task"))
        .into_iter()
        .flatten();
    let lists = tasks
        .flatten()
        .filter_map(|t| std::fs::read_to_string(t.path().join("children")).ok());
    let lists: Vec<String> = lists.collect();
    lists
        .iter()
        .flat_map(|l| l.split_whitespace())
        .filter_map(|p| p.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `eg` printing `version` beside an `eg-indexd` when `daemon`.
    fn fake(version: &str, daemon: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut bins = vec![("eg", format!("#!/bin/sh\necho '{version}'\n"))];
        if daemon {
            bins.push(("eg-indexd", "#!/bin/sh\n".to_string()));
        }
        for (name, script) in bins {
            let bin = dir.path().join(name);
            std::fs::write(&bin, script).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    #[test]
    fn check_rejects_an_eg_that_is_not_elgrep() {
        let dir = fake("eg 0.3 (an evergreen client)", true);
        let bin = on_path(dir.path().as_os_str()).unwrap();
        let note = check(bin, "eg on PATH").err().unwrap();
        let want =
            "elgrep: eg on PATH is not elgrep's eg (--version: \"eg 0.3 (an evergreen client)\")";
        assert_eq!(note, want);
    }

    #[test]
    fn check_requires_eg_indexd_beside_eg() {
        let dir = fake("elgrep (eg) 9.9", false);
        let bin = on_path(dir.path().as_os_str()).unwrap();
        let note = check(bin.clone(), "eg on PATH").err().unwrap();
        let want = format!("elgrep: no executable eg-indexd beside {}", bin.display());
        assert_eq!(note, want);
    }

    #[test]
    fn check_accepts_elgrep() {
        let dir = fake("elgrep (eg) 9.9 (ripgrep 1.0)", true);
        let bin = on_path(dir.path().as_os_str()).unwrap();
        let e = check(bin.clone(), "eg on PATH").unwrap();
        assert_eq!(
            (&e.bin, e.version.as_str()),
            (&bin, "elgrep (eg) 9.9 (ripgrep 1.0)")
        );
        assert_eq!(e.daemon, dir.path().join("eg-indexd"));
    }

    #[test]
    fn stderr_tallies_scans_and_rejects_other_lines() {
        let debug = "eg: DEBUG|eg::index|mod.rs:1: eg index: mode=auto\n";
        let scan =
            |r: &str| format!("eg: DEBUG|eg::index::readiness|r.rs:9: eg index: {SCAN}{r}\n");
        let (a, b) = (
            "no daemon owns this index",
            "a pattern with no selective n-gram",
        );
        let mut scans = Scans::default();
        scans.add(debug.as_bytes()).unwrap();
        assert_eq!(scans.note(1), "0 of 1 queries scanned without the index");
        let notice = format!("eg: the sparse index cannot serve a pattern{FALLBACK}\n");
        scans.add((notice + &scan(a)).as_bytes()).unwrap();
        assert_eq!(
            scans.note(1),
            format!("1 of 1 queries scanned without the index: {a}")
        );
        let mut scans = Scans::default();
        scans.add((scan(a) + &scan(a)).as_bytes()).unwrap();
        assert_eq!(
            scans.note(2),
            format!("1 of 2 queries scanned without the index: {a}")
        );
        scans.add((scan(b) + debug + &scan(a)).as_bytes()).unwrap();
        let want = format!("2 of 3 queries scanned without the index: {b} (1); {a} (2)");
        assert_eq!(scans.note(3), want);
        let err = scans.add(b"eg: /s/x.c: Permission denied\n").unwrap_err();
        assert!(err.ends_with("eg: /s/x.c: Permission denied"), "{err}");
    }

    #[test]
    fn row_answers_intersections_as_rg_does() {
        let Ok(e) = detect() else {
            eprintln!("skipped: no elgrep");
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
        let row = row(&e, dir.path(), docs.len(), &terms).unwrap();
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
        let scans = row.notes.last().unwrap();
        assert!(
            scans.contains(" of 5 queries scanned without the index"),
            "{scans}"
        );
    }
}

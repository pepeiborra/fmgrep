//! The optional elgrep row: one `eg-indexd` owned by the row, then one
//! `eg -n` per query reading its index; an intersection is the rg pipe with
//! `eg`, each stage's stderr apart. The index answers only on Linux;
//! elsewhere every query scans.

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
use crate::rows::{LINES, Peak, Sampler, and_pipe, failure, memory, timed};
use crate::tgrep::dir_size;
use crate::verify::Paths;
use crate::{Query, Row, RowKind, case_flag, leaves, with_warm_up};

pub struct Elgrep {
    pub bin: PathBuf,
    pub daemon: PathBuf,
    pub version: String,
}

/// A literal no file holds: the probe only waits out the cold build.
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
    let out = crate::rows::output_retry_etxtbsy(cmd.stdin(Stdio::null()));
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

/// `watchman` is the program whose watches of `root` the row removes on
/// Linux; `None` leaves watchman alone.
pub fn row(
    e: &Elgrep,
    paths: &Paths,
    root: &Path,
    terms: &[String],
    watchman: Option<&Path>,
) -> Result<Row, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (index, runtime) = (dir.path().join("index"), dir.path().join("runtime"));
    let stderr = dir.path().join("stderr");
    std::fs::create_dir(&stderr).map_err(|e| format!("{}: {e}", stderr.display()))?;
    // One walk for every eg: the daemon builds the index with the first one's.
    let flags = [format!("--index-dir={}", index.display())];
    let eg = |args: &[&str]| {
        let mut cmd = Command::new(&e.bin);
        env(&mut cmd, &runtime).args(&flags).args(args);
        cmd
    };
    let (out, _, _) = timed(eg(&["-l", "-e", "", "--"]).arg(root), None)?;
    let mut row = Row::new(RowKind::Elgrep);
    row.searched = paths.listed("eg -l ''", &out)?;

    // Dropped after the daemon: the watches come back once the row is over.
    let linux = watchman.filter(|_| cfg!(target_os = "linux"));
    let _unwatched = linux.map(|w| Unwatched::new(w, root));
    let daemon = if cfg!(target_os = "linux") {
        let start = Instant::now();
        let mut daemon = Daemon::start(e, &runtime)?;
        let (out, _, _) = timed(eg(&["--bench", "-F", "-e", PROBE, "--"]).arg(root), None)?;
        let build = probe(&out)?;
        let mut ask = || {
            if let Ok(Some(status)) = daemon.child.try_wait() {
                return Err(format!(
                    "eg-indexd: {status} while the row waited for its index"
                ));
            }
            let (out, _, _) = timed(eg(&["--debug", "-F", "-e", PROBE, "--"]).arg(root), None)?;
            Ok(out.stderr)
        };
        let scanned = answered(&mut ask, Duration::from_millis(200), WAIT)?;
        row.ready = Some(start.elapsed());
        if let Some(note) = scanned.note(build) {
            row.notes.push(note);
        }
        row.index_time = Some(Duration::from_secs_f64(build / 1000.0));
        row.index_bytes = Some(dir_size(&index.join("index")));
        Some(daemon)
    } else {
        None
    };

    let mut quoted: Vec<String> = flags.iter().map(|f| quote(f)).collect();
    quoted.push("--debug".to_string());
    let (prog, quoted) = (quote(&e.bin.to_string_lossy()), quoted.join(" "));
    let (mut scans, mut warm_up_scans) = (Scans::default(), Scans::default());
    for (term, warm_up) in with_warm_up(terms) {
        let case = case_flag(term);
        let (mut cmd, stages) = match &leaves(term)[..] {
            &[leaf] => {
                let (pattern, fixed) =
                    regex_pattern(leaf).map_or((leaf, Some("-F")), |p| (p, None));
                let mut cmd = eg(&["--debug", "--with-filename"]);
                cmd.args(LINES).args(case).args(fixed);
                cmd.args(["-e", pattern, "--"]).arg(root);
                (cmd, 0)
            }
            all => {
                let flags = case.map_or(quoted.clone(), |c| format!("{quoted} {c}"));
                let mut cmd = and_pipe(&prog, &flags, root, all, Some(&stderr));
                env(&mut cmd, &runtime);
                (cmd, all.len() + 1)
            }
        };
        let (out, latency, peak) = timed(&mut cmd, Some(Duration::from_millis(1)))?;
        let what = format!("eg {term:?}");
        let stage = |k: usize| {
            let f = stderr.join(k.to_string());
            std::fs::read(&f).map_err(|e| failure(&format!("{what}: {}: {e}", f.display()), &out))
        };
        let files = (1..=stages).map(stage).collect::<Result<Vec<_>, _>>()?;
        let stderrs: Vec<&[u8]> = std::iter::once(&out.stderr[..])
            .chain(files.iter().map(Vec::as_slice))
            .collect();
        let tally = if warm_up {
            &mut warm_up_scans
        } else {
            &mut scans
        };
        tally.add(&stderrs).map_err(|l| failure(&what, &out) + &l)?;
        let hits = paths.hits(&what, &out)?;
        if warm_up {
            continue;
        }
        row.footprint = row.footprint.max(peak.map(|p| p.footprint));
        row.query_rss = row.query_rss.max(peak.map(|p| p.rss));
        row.queries.push(Query::new(term, latency, hits));
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

/// The probe's cold-build wait in ms.
fn probe(out: &Output) -> Result<f64, String> {
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| failure(&format!("eg --bench: {e}"), out))?;
    if v["ok"] != true || !matches!(out.status.code(), Some(0 | 1)) {
        return Err(failure(
            &format!("eg --bench: {}", v["unsupported_reason"]),
            out,
        ));
    }
    let build = v["timings_ms"]["cold_build_total"].as_f64();
    let build = build.filter(|_| v["cold_build"] == true);
    build.ok_or(format!("eg --bench built no index: {v}"))
}

/// How long the row waits for eg-indexd's index to answer after its first build.
const WAIT: Duration = Duration::from_secs(30 * 60);
/// Probes in a row that the index must answer: a daemon busy with its
/// watches can miss one query's 30 ms rendezvous just after it answered one.
const ANSWERED: usize = 3;

/// The probe searches that scanned after the first build, and the time
/// from the first of them to the last probe.
struct Waited {
    scans: Scans,
    took: Duration,
    timed_out: bool,
}

impl Waited {
    fn note(&self, build_ms: f64) -> Option<String> {
        let k = self.scans.queries;
        let why = self
            .scans
            .reasons
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        let (build, took) = (build_ms / 1000.0, self.took.as_secs_f64());
        if self.timed_out {
            let wait = WAIT.as_secs() / 60;
            return Some(format!(
                "the index did not answer in {wait} min after its first build ({build:.2} s); \
                 {k} probe searches scanned ({why}); the queries ran anyway"
            ));
        }
        (k > 0).then(|| {
            format!(
                "after its first build ({build:.2} s), eg-indexd scanned {k} probe \
                 searches over {took:.2} s ({why}); the row timed the queries once \
                 {ANSWERED} probes in a row read the index, and ready counts that wait"
            )
        })
    }
}

/// Runs `ask` (a probe search under `--debug`; its stderr) every `pause`
/// until `ANSWERED` in a row read the index, or `limit` passed.
///
/// `eg --bench` returns once the first build is published, but eg-indexd
/// may still scan every query: on AOSP it drops the watches of the
/// directories its walk skips (about 37,000), their events overflow the
/// kernel's inotify queue (16,384 events by default), and it then counts
/// every path as changed until its next build, about 30 s later.
fn answered(
    ask: &mut dyn FnMut() -> Result<Vec<u8>, String>,
    pause: Duration,
    limit: Duration,
) -> Result<Waited, String> {
    let start = Instant::now();
    let (mut scans, mut first, mut run) = (Scans::default(), None, 0);
    while run < ANSWERED {
        if start.elapsed() > limit {
            let took = first.map_or(Duration::ZERO, |f: Instant| f.elapsed());
            return Ok(Waited {
                scans,
                took,
                timed_out: true,
            });
        }
        let before = scans.queries;
        scans
            .add(&[&ask()?])
            .map_err(|l| format!("eg probe: {l}"))?;
        if scans.queries > before {
            first.get_or_insert_with(Instant::now);
            run = 0;
        } else {
            run += 1;
        }
        if run < ANSWERED {
            std::thread::sleep(pause);
        }
    }
    let took = first.map_or(Duration::ZERO, |f| f.elapsed());
    Ok(Waited {
        scans,
        took,
        timed_out: false,
    })
}

/// watchman's watches of the row's tree and of every directory under it,
/// removed for the row and put back on drop.
///
/// watchman writes a cookie file about once a minute into a watched tree
/// with no client asking: into its `.git`, or into its root where it has
/// none (AOSP). eg-indexd watches every directory before its first build
/// and counts each cookie as a change; a build that raced one does not
/// prove the index fresh, and `eg --bench` waits for that proof.
struct Unwatched {
    watchman: PathBuf,
    removed: Vec<PathBuf>,
}

impl Unwatched {
    /// Removes every watch of `tree` or under it that a running watchman
    /// lists, and names each watch above it, which it leaves alone; starts
    /// no watchman.
    fn new(watchman: &Path, tree: &Path) -> Self {
        let listed = call(watchman, &["watch-list".as_ref()], None);
        let roots = listed.map_or_else(|_| Vec::new(), |o| listed_roots(&o.stdout));
        let (inside, above) = split_roots(roots, tree);
        for r in above {
            eprintln!("{}", above_note(&r, tree));
        }
        let mut removed = Vec::new();
        for r in inside {
            let what = r.display();
            match call(watchman, &["watch-del".as_ref(), r.as_os_str()], None) {
                Ok(_) => eprintln!(
                    "elgrep: removed watchman's watch of {what} for the row; \
                     if the run is killed, put it back with: watchman watch {what}"
                ),
                Err(e) => {
                    eprintln!("elgrep: could not remove watchman's watch of {what}: {e}");
                    continue;
                }
            }
            removed.push(r);
        }
        let watchman = watchman.to_path_buf();
        Self { watchman, removed }
    }
}

impl Drop for Unwatched {
    /// Watches each root again, the same root, and waits for the crawl, as
    /// `scripts/fsmonitor.sh` does.
    fn drop(&mut self) {
        for root in std::mem::take(&mut self.removed) {
            let what = root.display();
            let sync = serde_json::json!(["query", root, {"expression": ["false"], "sync_timeout": 600000}]);
            let back = call(&self.watchman, &["watch".as_ref(), root.as_os_str()], None)
                .and_then(|_| call(&self.watchman, &["-j".as_ref()], Some(&sync)));
            match back {
                Ok(_) => eprintln!("elgrep: watchman watches {what} again"),
                Err(e) => eprintln!(
                    "elgrep: could not watch {what} again ({e}); \
                     put it back with: watchman watch {what}"
                ),
            }
        }
    }
}

/// `watchman --no-spawn --no-pretty ARGS`, with `input` on its stdin;
/// `Err` says why it failed.
fn call(watchman: &Path, args: &[&OsStr], input: Option<&Value>) -> Result<Output, String> {
    let mut cmd = Command::new(watchman);
    cmd.args(["--no-spawn", "--no-pretty"]).args(args);
    let stdin = if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let child = cmd
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = child.spawn().and_then(|mut c| {
        if let (Some(mut i), Some(v)) = (c.stdin.take(), input) {
            std::io::Write::write_all(&mut i, v.to_string().as_bytes())?;
        }
        c.wait_with_output()
    });
    let out = out.map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(out);
    }
    // watchman prints a command's error as JSON, and a lost daemon on stderr.
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let err = String::from_utf8_lossy(&out.stderr);
    let why = v["error"].as_str().or(err.lines().last());
    Err(why.map_or_else(|| out.status.to_string(), |w| w.trim().to_string()))
}

/// The roots that `watchman watch-list` printed.
fn listed_roots(stdout: &[u8]) -> Vec<PathBuf> {
    let v: Value = serde_json::from_slice(stdout).unwrap_or_default();
    let roots = v["roots"].as_array().into_iter().flatten();
    roots.filter_map(Value::as_str).map(PathBuf::from).collect()
}

/// Of `roots`, those of `tree` or under it, whose cookies land in the
/// tree, and those above it, which the row leaves alone.
fn split_roots(roots: Vec<PathBuf>, tree: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (inside, rest): (Vec<_>, Vec<_>) = roots.into_iter().partition(|r| r.starts_with(tree));
    let above = rest.into_iter().filter(|r| tree.starts_with(r)).collect();
    (inside, above)
}

fn above_note(root: &Path, tree: &Path) -> String {
    format!(
        "elgrep: watchman watches {}, above {}; the row leaves that watch alone, \
         and its cookie files may keep elgrep from proving its index current",
        root.display(),
        tree.display()
    )
}

/// Queries `--debug` said were scanned instead of read from the index.
#[derive(Default)]
struct Scans {
    queries: usize,
    reasons: BTreeMap<String, usize>,
}

impl Scans {
    /// Tallies one query's stderrs, each written one whole line at a time;
    /// `Err` is a line that is not `--debug`'s.
    fn add(&mut self, stderrs: &[&[u8]]) -> Result<(), String> {
        let mut reasons = BTreeSet::new();
        for stderr in stderrs {
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
            crate::fake_bin::write(&dir.path().join(name), &script);
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
        scans.add(&[debug.as_bytes()]).unwrap();
        assert_eq!(scans.note(1), "0 of 1 queries scanned without the index");
        let notice = format!("eg: the sparse index cannot serve a pattern{FALLBACK}\n");
        scans.add(&[(notice + &scan(a)).as_bytes()]).unwrap();
        assert_eq!(
            scans.note(1),
            format!("1 of 1 queries scanned without the index: {a}")
        );
        let mut scans = Scans::default();
        scans.add(&[(scan(a) + &scan(a)).as_bytes()]).unwrap();
        assert_eq!(
            scans.note(2),
            format!("1 of 2 queries scanned without the index: {a}")
        );
        scans
            .add(&[(scan(b) + debug + &scan(a)).as_bytes()])
            .unwrap();
        let want = format!("2 of 3 queries scanned without the index: {b} (1); {a} (2)");
        assert_eq!(scans.note(3), want);
        let err = scans
            .add(&[b"eg: /s/x.c: Permission denied\n"])
            .unwrap_err();
        assert!(err.ends_with("eg: /s/x.c: Permission denied"), "{err}");
    }

    #[test]
    fn interleaved_stage_stderr_is_rejected_with_its_text_and_read_apart_when_split() {
        let reg = "/mnt/work/cargo/registry/src/index.crates.io-1949cf8c6b5b557f";
        let file = "/mnt/work/aosp/external/libwebm/webm_parser/fuzzing/corpus/a823a019c0b19c97a1d35722cd843109ab016c17";
        let binary = format!(":686: ignoring {file}: found binary data at offset 18605");
        let merged = format!(
            "eg: DEBUG|eg::index|{reg}/elgrep-1.0.0/src/index/mod.rs:119eg: : eg index: \
             mode=autoDEBUG backend=|Postingsgrep_printer::summary threads=|\
             {reg}/grep-printer-0.3.1/src/summary.rs12\n{binary}\n"
        );
        let stage1 = format!(
            "eg: DEBUG|grep_printer::summary|{reg}/grep-printer-0.3.1/src/summary.rs{binary}\n"
        );
        let stage2 = format!(
            "eg: DEBUG|eg::index|{reg}/elgrep-1.0.0/src/index/mod.rs:119: eg index: \
             mode=auto backend=Postings threads=12\n"
        );
        let mut scans = Scans::default();
        let err = scans.add(&[b"", merged.as_bytes()]).unwrap_err();
        assert!(
            err.ends_with(&format!("not a --debug line: {binary}")),
            "{err}"
        );
        scans
            .add(&[b"", stage1.as_bytes(), stage2.as_bytes()])
            .unwrap();
        assert_eq!(scans.note(1), "0 of 1 queries scanned without the index");

        let scan = |r: &str| format!("eg: DEBUG|eg::index|r.rs:9: eg index: {SCAN}{r}\n");
        let (a, b) = (scan("a"), scan("b"));
        scans
            .add(&[b"", stage1.as_bytes(), a.as_bytes(), a.as_bytes()])
            .unwrap();
        assert_eq!(scans.note(2), "1 of 2 queries scanned without the index: a");
        scans.add(&[a.as_bytes(), b.as_bytes()]).unwrap();
        let want = "2 of 3 queries scanned without the index: a (2); b (1)";
        assert_eq!(scans.note(3), want);
    }

    /// A probe's stderr: read from the index, or scanned.
    fn probe_stderr(scan: bool) -> Vec<u8> {
        let mut s = "eg: DEBUG|eg::index|mod.rs:1: eg index: mode=auto\n".to_string();
        if scan {
            s += &format!(
                "eg: DEBUG|eg::index::readiness|r.rs:9: eg index: {SCAN}the corpus changed\n"
            );
        }
        s.into_bytes()
    }

    #[test]
    fn answered_waits_for_probes_in_a_row_that_read_the_index() {
        let mut seq = [true, false, true, false, false, false, true].into_iter();
        let mut calls = 0;
        let mut ask = || {
            calls += 1;
            Ok(probe_stderr(seq.next().unwrap()))
        };
        let w = answered(&mut ask, Duration::ZERO, WAIT).unwrap();
        assert_eq!((calls, w.scans.queries, w.timed_out), (6, 2, false));
        let note = w.note(302_970.0).unwrap();
        let start = "after its first build (302.97 s), eg-indexd scanned 2 probe searches over ";
        let end = " s (the corpus changed); the row timed the queries once 3 probes in a row \
                   read the index, and ready counts that wait";
        assert!(note.starts_with(start) && note.ends_with(end), "{note}");

        let mut ask = || Ok(probe_stderr(false));
        let w = answered(&mut ask, Duration::ZERO, WAIT).unwrap();
        assert_eq!((w.scans.queries, w.note(1.0)), (0, None));
    }

    #[test]
    fn answered_gives_up_after_its_limit_and_says_so() {
        let mut ask = || Ok(probe_stderr(true));
        let limit = Duration::from_millis(5);
        let w = answered(&mut ask, Duration::from_millis(1), limit).unwrap();
        assert!(w.timed_out && w.scans.queries > 0);
        let note = w.note(1_500.0).unwrap();
        let start = "the index did not answer in 30 min after its first build (1.50 s); ";
        let end = " probe searches scanned (the corpus changed); the queries ran anyway";
        assert!(note.starts_with(start) && note.ends_with(end), "{note}");
        let mut ask = || Ok(b"eg: /s/x.c: Permission denied\n".to_vec());
        let err = answered(&mut ask, Duration::ZERO, WAIT).err().unwrap();
        let want = "not a --debug line: eg: /s/x.c: Permission denied";
        assert!(err.ends_with(want), "{err}");
    }

    #[test]
    fn the_row_removes_the_watches_of_its_tree_and_under_it_and_names_those_above() {
        let list = br#"{"version": "1", "roots": ["/src/linux", "/s/aosp", "/s/aosp/sub", "/s", "/s/aosp2"]}"#;
        let roots = listed_roots(list);
        assert_eq!(roots.len(), 5);
        let (inside, above) = split_roots(roots, Path::new("/s/aosp"));
        let paths = |p: &[&str]| p.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            (inside, above.clone()),
            (paths(&["/s/aosp", "/s/aosp/sub"]), paths(&["/s"]))
        );
        let want = "elgrep: watchman watches /s, above /s/aosp; the row leaves that watch \
                    alone, and its cookie files may keep elgrep from proving its index current";
        assert_eq!(above_note(&above[0], Path::new("/s/aosp")), want);
        for list in [&b"not json"[..], br#"{"error": "x"}"#] {
            assert!(listed_roots(list).is_empty());
        }
        let none = Unwatched::new(Path::new("/nonexistent/watchman"), Path::new("/s/aosp"));
        assert!(none.removed.is_empty());
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
        let (_dir, root) = crate::tgrep::tests::tree();
        let paths = Paths::new(&root, &[]).unwrap();
        let terms = crate::tgrep::tests::TERMS.map(String::from);
        // No watchman: a test does not touch the user's daemon.
        let row = row(&e, &paths, &root, &terms, None).unwrap();
        let rg = crate::rows::rg(&paths, &root, &terms).unwrap();
        let got: Vec<_> = row
            .queries
            .iter()
            .map(|q| (q.hits.lines(), q.hits.files()))
            .collect();
        assert_eq!(got, crate::tgrep::tests::TALLIES);
        assert_eq!(row.searched, rg.searched);
        for (t, r) in row.queries.iter().zip(&rg.queries) {
            assert_eq!(t.hits.per_file, r.hits.per_file, "{}", t.term);
        }
        let scans = row.notes.last().unwrap();
        assert!(
            scans.contains(" of 7 queries scanned without the index"),
            "{scans}"
        );
    }
}

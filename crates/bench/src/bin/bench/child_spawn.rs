//! Engine child plumbing: binary discovery, the per-child timeout, the
//! measured `index` spawn and protocol-JSON parsing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::mem::{Sampler, reaped_peak, sampler_interval};
use crate::subprocess::ChildSpec;

/// Default per-child timeout (seconds) when
/// `$FMGREP_BENCH_CHILD_TIMEOUT_SECS` is unset or invalid; bounds the
/// `index` spawn and server readiness separately.
const DEFAULT_CHILD_TIMEOUT_SECS: u64 = 300;

/// Output kept in a child failure message.
const SNIP: usize = 2000;

/// Poll interval while waiting for a child to exit.
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(20);

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// Locate the child binary: `$bin_env_var` override, then a sibling of the
/// running `bench` executable, then `$PATH` (see module docs).
pub fn resolve_child_bin(spec: &ChildSpec) -> Result<PathBuf, String> {
    let name = exe_name(spec.bin_name);
    if let Ok(p) = std::env::var(spec.bin_env_var) {
        let cand = PathBuf::from(&p);
        if cand.is_file() {
            return Ok(cand);
        }
        return Err(format!(
            "child binary override ${}={p:?} does not exist or is not a file",
            spec.bin_env_var
        ));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let cand = dir.join(&name);
        if cand.is_file() {
            return Ok(cand);
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let cand = dir.join(&name);
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }
    Err(format!(
        "child binary `{name}` not found (no ${} override, no sibling of the bench executable, not on $PATH); run `cargo build --release -p fmgrep -p bench` first",
        spec.bin_env_var
    ))
}

/// Per-child timeout: `$FMGREP_BENCH_CHILD_TIMEOUT_SECS`, defaulting to
/// [`DEFAULT_CHILD_TIMEOUT_SECS`] (with a stderr warning on invalid values).
pub fn child_timeout() -> Duration {
    match std::env::var("FMGREP_BENCH_CHILD_TIMEOUT_SECS") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(secs) if secs > 0 => Duration::from_secs(secs),
            _ => {
                eprintln!(
                    "warning: invalid FMGREP_BENCH_CHILD_TIMEOUT_SECS={raw:?}; using default {DEFAULT_CHILD_TIMEOUT_SECS}s"
                );
                Duration::from_secs(DEFAULT_CHILD_TIMEOUT_SECS)
            }
        },
        Err(_) => Duration::from_secs(DEFAULT_CHILD_TIMEOUT_SECS),
    }
}

/// One finished spawn: its stdout plus the harness-measured RSS inputs.
pub struct Spawned {
    pub stdout: Vec<u8>,
    pub samples: Vec<crate::mem::Sample>,
    pub child_peak: Option<u64>,
}

/// Spawn `bin` with `argv` (stdin null), RSS-sampled from spawn to reap,
/// killed at `timeout`; stderr is forwarded as `[label] <line>`. `Err` on
/// any failure but the RSS measurement, which degrades to empty/`None`.
pub fn run_spawn(
    bin: &Path,
    argv: &[String],
    label: &str,
    timeout: Duration,
) -> Result<Spawned, String> {
    use std::io::Read as _;
    let mut child = Command::new(bin);
    for arg in argv {
        child.arg(arg);
    }
    let mut child = child
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn `{}`: {e}", bin.display()))?;

    let sampler = sampler_interval().map(|tick| Sampler::start(child.id(), tick));

    // Concurrent stdout/stderr drainers (scoped threads), so a chatty child
    // can never deadlock against a full pipe.
    let (mut stdout_bytes, mut stderr_bytes) = (Vec::new(), Vec::new());
    let mut stdout_handle = child.stdout.take();
    let mut stderr_handle = child.stderr.take();
    let exit_status = std::thread::scope(|s| {
        let stdout_reader = s.spawn(|| {
            if let Some(mut h) = stdout_handle.take() {
                let _ = h.read_to_end(&mut stdout_bytes);
            }
        });
        let stderr_reader = s.spawn(|| {
            if let Some(mut h) = stderr_handle.take() {
                let _ = h.read_to_end(&mut stderr_bytes);
            }
        });

        // Poll for exit with a timeout; kill overruns loudly.
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Err(e) => break Err(e),
                Ok(Some(status)) => break Ok(status),
                Ok(None) if start.elapsed() >= timeout => {
                    let _ = child.kill();
                    // Reap so the reader threads see EOF and join.
                    let _ = child.wait();
                    break Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("child exceeded {}s timeout", timeout.as_secs()),
                    ));
                }
                Ok(None) => std::thread::sleep(CHILD_POLL_INTERVAL),
            }
        };
        stdout_reader.join().ok();
        stderr_reader.join().ok();
        status.map_err(|e| e.to_string())
    })?;

    let samples = sampler.map(Sampler::stop).unwrap_or_default();
    let child_peak = reaped_peak();

    forward_stderr(label, &stderr_bytes);
    if !exit_status.success() {
        let shown: Vec<&str> = argv.iter().map(String::as_str).collect();
        let mut err = format!(
            "child `{}` {} failed with status {exit_status}",
            bin.display(),
            shown.join(" "),
        );
        err.push_str(&output_tails(&stderr_bytes, &stdout_bytes));
        return Err(err);
    }

    Ok(Spawned {
        stdout: stdout_bytes,
        samples,
        child_peak,
    })
}

/// The last [`SNIP`] chars of a failed child's stderr and stdout, for an
/// error message (the full text was already forwarded).
pub fn output_tails(stderr: &[u8], stdout: &[u8]) -> String {
    let mut out = String::new();
    for (what, bytes) in [("stderr", stderr), ("stdout", stdout)] {
        let text = String::from_utf8_lossy(bytes);
        if text.trim().is_empty() {
            continue;
        }
        let skip = text.chars().count().saturating_sub(SNIP);
        let tail: String = text.chars().skip(skip).collect();
        out.push_str(&format!("\n--- child {what} (tail) ---\n{tail}"));
    }
    out
}

/// Pass a child's stderr through, each line tagged `[label]`.
pub fn forward_stderr(label: &str, stderr: &[u8]) {
    for line in String::from_utf8_lossy(stderr).lines() {
        eprintln!("[{label}] {line}");
    }
}

/// Parse one spawn's single-line JSON stdout, failing loudly (with an
/// excerpt) when the child exited 0 without the protocol object.
pub fn parse_json<T>(stdout: &[u8], what: &str, argv: &[String]) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_slice(stdout).map_err(|e| {
        let excerpt: String = String::from_utf8_lossy(stdout).chars().take(500).collect();
        let shown: Vec<&str> = argv.iter().map(String::as_str).collect();
        format!(
            "child {what} (`{}`) exited 0 but stdout is not the protocol JSON: {e}; stdout excerpt: {excerpt:?}",
            shown.join(" "),
        )
    })
}

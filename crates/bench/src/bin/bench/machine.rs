//! Machine and version stamps for the results file. Best effort: every field
//! that cannot be read is `"unknown"`.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::freshness::utc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamps {
    /// CPU, cores, RAM.
    pub machine: String,
    pub os: String,
    pub rg: String,
    /// `fmgrep --version` plus the workspace's git revision.
    pub fmgrep: String,
    /// UTC.
    pub date: String,
}

pub const UNKNOWN: &str = "unknown";

/// Trimmed stdout of a successful command; `None` otherwise or when empty.
fn run(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn gib(bytes: u64) -> String {
    format!("{:.0} GB", bytes as f64 / (1u64 << 30) as f64)
}

fn machine() -> Option<String> {
    if cfg!(target_os = "macos") {
        let cpu = run("sysctl", &["-n", "machdep.cpu.brand_string"])?;
        let cores = run("sysctl", &["-n", "hw.ncpu"])?;
        let mem: u64 = run("sysctl", &["-n", "hw.memsize"])?.parse().ok()?;
        return Some(format!("{cpu}, {cores} cores, {}", gib(mem)));
    }
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    let cpu = cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("model name")?.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_else(|| UNKNOWN.to_string());
    let cores = cpuinfo
        .lines()
        .filter(|l| l.starts_with("processor"))
        .count();
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib: u64 = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(format!("{cpu}, {cores} cores, {}", gib(kib * 1024)))
}

fn os() -> Option<String> {
    let uname = run("uname", &["-srm"])?;
    let release = if cfg!(target_os = "macos") {
        run("sw_vers", &["-productVersion"]).map(|v| format!("macOS {v}"))
    } else {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            })
    };
    Some(match release {
        Some(r) => format!("{r} ({uname})"),
        None => uname,
    })
}

fn fmgrep(bin: Option<&Path>, workspace: &Path) -> Option<String> {
    let version = run(bin?, &["--version"])?;
    let ws = workspace.to_str()?;
    let Some(rev) = run("git", &["-C", ws, "rev-parse", "--short", "HEAD"]) else {
        return Some(version);
    };
    let dirty = run(
        "git",
        &["-C", ws, "status", "--porcelain", "--untracked-files=no"],
    )
    .is_some();
    Some(format!(
        "{version} ({rev}{})",
        if dirty { "-dirty" } else { "" }
    ))
}

/// Stamps for this machine, the rg the rg row runs, and `fmgrep_bin`.
pub fn gather(fmgrep_bin: Option<&Path>, workspace: &Path) -> Stamps {
    let or_unknown = |v: Option<String>| v.unwrap_or_else(|| UNKNOWN.to_string());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    Stamps {
        machine: or_unknown(machine()),
        os: or_unknown(os()),
        rg: or_unknown(bench::rg::rg_version()),
        fmgrep: or_unknown(fmgrep(fmgrep_bin, workspace)),
        date: utc(now),
    }
}

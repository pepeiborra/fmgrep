//! Platform memory reads behind the measurement policy in `mem.rs`: the
//! live per-pid resident size and footprint (`proc_pid_rusage` on macOS,
//! `/proc/<pid>/statm` on Linux) and the cumulative
//! `getrusage(RUSAGE_CHILDREN)` watermark, all in bytes. Every failure mode
//! yields `None`.

/// One live read of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemReading {
    /// Resident pages, file-backed included.
    pub rss: u64,
    /// Anonymous/private memory: macOS `phys_footprint`, Linux resident
    /// minus shared (file-backed) pages.
    pub footprint: u64,
}

/// Read `pid`'s memory, or `None` when unavailable: a dead pid, missing
/// `/proc`, another platform, or a zero resident size (a zombie racing the
/// tick). Never panics.
pub fn read_child_mem(pid: u32) -> Option<MemReading> {
    #[cfg(target_os = "macos")]
    {
        read_child_mem_macos(pid)
    }
    #[cfg(target_os = "linux")]
    {
        read_child_mem_linux(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

fn nonzero(reading: MemReading) -> Option<MemReading> {
    (reading.rss > 0).then_some(reading)
}

#[cfg(target_os = "macos")]
fn read_child_mem_macos(pid: u32) -> Option<MemReading> {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V2,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        )
    };
    if rc != 0 {
        return None;
    }
    nonzero(MemReading {
        rss: info.ri_resident_size,
        footprint: info.ri_phys_footprint,
    })
}

/// `statm` fields 2 and 3: resident and shared pages.
#[cfg(target_os = "linux")]
fn read_child_mem_linux(pid: u32) -> Option<MemReading> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let mut fields = statm.split_whitespace().skip(1);
    let resident: u64 = fields.next()?.parse().ok()?;
    let shared: u64 = fields.next()?.parse().ok()?;
    let page = page_size_bytes();
    nonzero(MemReading {
        rss: resident.checked_mul(page)?,
        footprint: resident.saturating_sub(shared).checked_mul(page)?,
    })
}

/// Page size in bytes, via `sysconf(_SC_PAGESIZE)` with a 4096 fallback
/// (Linux is overwhelmingly 4 KiB; a wrong fallback only scales a
/// best-effort gauge, never fails the run).
#[cfg(target_os = "linux")]
fn page_size_bytes() -> u64 {
    let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if n > 0 { n as u64 } else { 4096 }
}

/// `getrusage(RUSAGE_CHILDREN)` `ru_maxrss`, normalized to bytes.
///
/// * macOS reports bytes; Linux reports kilobytes (×1024 here).
/// * `None` on failure or on platforms where the unit is unknown — the peak
///   then degrades to the sampler maximum (see [`crate::mem::phase_stats`]).
pub fn rusage_children_max_rss_bytes() -> Option<u64> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) };
        if rc != 0 || usage.ru_maxrss <= 0 {
            return None;
        }
        #[cfg(target_os = "macos")]
        {
            Some(usage.ru_maxrss as u64)
        }
        #[cfg(target_os = "linux")]
        {
            (usage.ru_maxrss as u64).checked_mul(1024)
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// `getrusage(RUSAGE_SELF)` `ru_maxrss`, normalized to bytes: the peak RSS
/// of the harness process itself, children excluded.
///
/// Same unit rules and `None` failure mode as
/// [`rusage_children_max_rss_bytes`].
pub fn rusage_self_max_rss_bytes() -> Option<u64> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        if rc != 0 || usage.ru_maxrss <= 0 {
            return None;
        }
        #[cfg(target_os = "macos")]
        {
            Some(usage.ru_maxrss as u64)
        }
        #[cfg(target_os = "linux")]
        {
            (usage.ru_maxrss as u64).checked_mul(1024)
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_reads_are_not_readings() {
        let read = |rss| MemReading { rss, footprint: 0 };
        assert_eq!(nonzero(read(0)), None);
        assert_eq!(nonzero(read(1)), Some(read(1)));
    }

    #[test]
    fn bogus_pid_read_degrades_to_none() {
        // Near-impossible pid: must be None, never panic. (Covers the live
        // platform branch; other-platform branches are unconditional None.)
        assert_eq!(read_child_mem(u32::MAX - 1), None);
    }

    #[test]
    fn self_peak_rss_is_positive() {
        // The harness's own watermark must be real on macOS/Linux.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        assert!(rusage_self_max_rss_bytes().unwrap_or(0) > 0);
    }

    #[test]
    fn self_pid_reads_positive() {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let read = read_child_mem(std::process::id()).expect("self is readable");
            assert!(read.rss > 0 && read.footprint > 0, "{read:?}");
        }
    }
}

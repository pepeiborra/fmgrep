//! Disk pre-flight: what the selection will write before it writes it. Rows
//! run sequentially and each row's index is deleted when its row ends, so
//! peak disk is the shared staging plus the worst single row; a row with no
//! size estimate is reported as a lower bound, never counted as zero.

use std::path::{Path, PathBuf};

use crate::roster::{RosterEntry, Runner, row_name};

/// Headroom kept free on the staging volume.
const MARGIN_BYTES: u64 = 1 << 30;

/// Estimated index bytes per byte of indexed text; `None` where the row's
/// index size is not modelled. `--store-docs` adds docs.shard, one byte per
/// text byte, to the blobs.
fn index_ratio(name: &str) -> Option<f64> {
    match name {
        "fmgrep" => Some(2.4),
        "fmgrep-store-docs" => Some(3.4),
        _ => None,
    }
}

/// Peak disk one row adds on top of the shared staging.
pub struct RowEstimate {
    pub name: &'static str,
    pub bytes: u64,
    /// False when part of the row's cost is unmodelled: `bytes` is a lower
    /// bound, not an estimate.
    pub complete: bool,
}

/// The whole selection's disk forecast.
pub struct Preflight {
    pub volume: PathBuf,
    pub free_bytes: Option<u64>,
    pub shared_stage: u64,
    pub rows: Vec<RowEstimate>,
    pub peak: u64,
    pub peak_complete: bool,
}

/// Free bytes on `path`'s volume; `None` when the filesystem cannot say.
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

/// Forecast for `rows` over `base_text` bytes replicated `replicas` times.
pub fn estimate(rows: &[&RosterEntry], base_text: u64, replicas: usize) -> Preflight {
    let full_text = base_text.saturating_mul(replicas as u64);
    // Replicas are hard-linked, so the shared staging costs one copy.
    let shared_stage = base_text;
    let rows: Vec<RowEstimate> = rows
        .iter()
        .map(|entry| {
            let name = row_name(entry);
            let index = index_ratio(name).map(|r| (full_text as f64 * r) as u64);
            RowEstimate {
                name,
                bytes: index.unwrap_or(0),
                // rg writes no index; every other row needs a modelled ratio.
                complete: matches!(entry.runner, Runner::Rg) || index.is_some(),
            }
        })
        .collect();
    let worst = rows.iter().max_by_key(|r| r.bytes);
    let peak = shared_stage + worst.map_or(0, |r| r.bytes);
    let peak_complete = worst.is_some_and(|r| r.complete);
    let volume = std::env::temp_dir();
    let free_bytes = free_bytes(&volume);
    Preflight {
        volume,
        free_bytes,
        shared_stage,
        rows,
        peak,
        peak_complete,
    }
}

impl Preflight {
    /// Loud refusal when the forecast does not fit, naming the worst row and
    /// the shortfall; `None` when it fits or free space is unknown.
    pub fn refusal(&self) -> Option<String> {
        let free = self.free_bytes?;
        let need = self.peak.saturating_add(MARGIN_BYTES);
        if need <= free {
            return None;
        }
        let worst = self.rows.iter().max_by_key(|r| r.bytes)?;
        Some(format!(
            "this selection does not fit on {}: row {:?} peaks at {} (shared staging {} + row {}{}), \
             needs {} with the {} margin, {} free — short by {}. \
             Re-run with fewer replicas or a narrower --rows.",
            self.volume.display(),
            worst.name,
            crate::report::fmt_bytes(self.peak),
            crate::report::fmt_bytes(self.shared_stage),
            crate::report::fmt_bytes(worst.bytes),
            if self.peak_complete {
                ""
            } else {
                ", partly unmodelled"
            },
            crate::report::fmt_bytes(need),
            crate::report::fmt_bytes(MARGIN_BYTES),
            crate::report::fmt_bytes(free),
            crate::report::fmt_bytes(need - free),
        ))
    }

    pub fn print(&self) {
        println!(
            "disk: {} free on {} (staging + index volume)",
            self.free_bytes
                .map_or("unknown".to_string(), crate::report::fmt_bytes),
            self.volume.display()
        );
        let rows: Vec<String> = self
            .rows
            .iter()
            .map(|r| {
                format!(
                    "{} {}{}",
                    r.name,
                    crate::report::fmt_bytes(r.bytes),
                    if r.complete { "" } else { "+" }
                )
            })
            .collect();
        println!(
            "disk estimate: shared staging {} | per row: {} | peak {}{} (rows run in sequence, each index deleted after its row; `+` = partly unmodelled)",
            crate::report::fmt_bytes(self.shared_stage),
            rows.join(", "),
            crate::report::fmt_bytes(self.peak),
            if self.peak_complete { "" } else { "+" }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roster::ROSTER;

    fn rows() -> Vec<&'static RosterEntry> {
        ROSTER.iter().collect()
    }

    fn row<'a>(pf: &'a Preflight, name: &str) -> &'a RowEstimate {
        pf.rows.iter().find(|r| r.name == name).expect(name)
    }

    #[test]
    fn peak_is_shared_stage_plus_worst_row() {
        let text = 1_000_000u64;
        let pf = estimate(&rows(), text, 3);
        let full = text * 3;
        assert_eq!(pf.shared_stage, text, "replicas are linked, not copied");
        assert_eq!(row(&pf, "rg").bytes, 0);
        assert_eq!(
            row(&pf, "fmgrep-store-docs").bytes,
            (full as f64 * 3.4) as u64
        );
        let worst = pf.rows.iter().map(|r| r.bytes).max().unwrap();
        assert_eq!(pf.peak, pf.shared_stage + worst);
        assert_eq!(worst, row(&pf, "fmgrep-store-docs").bytes);
    }

    #[test]
    fn rg_alone_needs_the_shared_staging() {
        let rg: Vec<&RosterEntry> = ROSTER
            .iter()
            .filter(|e| matches!(e.runner, Runner::Rg))
            .collect();
        let pf = estimate(&rg, 1_000_000, 2);
        assert_eq!(pf.shared_stage, 1_000_000);
        assert!(row(&pf, "rg").complete);
        assert_eq!(pf.peak, 1_000_000);
    }

    #[test]
    fn refusal_names_the_row_and_the_shortfall() {
        let mut pf = estimate(&rows(), 1 << 30, 8);
        pf.free_bytes = Some(1 << 30);
        let err = pf
            .refusal()
            .expect("8 replicas of 1 GiB cannot fit in 1 GiB");
        assert!(err.contains("fmgrep-store-docs"), "{err}");
        assert!(err.contains("short by"), "{err}");
        assert!(err.contains("--rows"), "{err}");
        pf.free_bytes = Some(u64::MAX / 2);
        assert!(pf.refusal().is_none(), "a fitting selection is not refused");
        pf.free_bytes = None;
        assert!(
            pf.refusal().is_none(),
            "unknown free space is not a refusal"
        );
    }
}

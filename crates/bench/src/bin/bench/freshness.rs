//! Row binary provenance: each row's binary with its mtime and size, refused
//! when a source it was built from is newer. "Built from" is cargo's dep-info
//! (`<bin>.d` beside the binary), so a binary cargo would not relink is never
//! flagged; a binary without one (an env override) is reported but unchecked.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bench::EngineBinary;

use crate::child_spawn::resolve_child_bin;
use crate::report::fmt_bytes;
use crate::roster::{RosterEntry, Runner, row_name};
use crate::subprocess::ChildSpec;

pub const REBUILD: &str = "cargo build --release -p fmgrep -p bench";

pub struct RowBinary {
    pub row: &'static str,
    pub stamp: EngineBinary,
    /// `None` when there is no dep-info to check against.
    newer_source: Option<Option<PathBuf>>,
}

/// The binary of every selected row that has one (rg is the system `rg`).
pub fn row_binaries(rows: &[&RosterEntry]) -> Result<Vec<RowBinary>, String> {
    rows.iter()
        .filter_map(|entry| {
            let bin = match &entry.runner {
                Runner::Child {
                    bin_name,
                    bin_env_var,
                    label,
                    store_docs,
                } => resolve_child_bin(&ChildSpec {
                    bin_name,
                    bin_env_var,
                    label,
                    store_docs: *store_docs,
                }),
                Runner::Rg => return None,
            };
            Some(bin.and_then(|bin| inspect(row_name(entry), &bin)))
        })
        .collect()
}

fn inspect(row: &'static str, bin: &Path) -> Result<RowBinary, String> {
    let meta = std::fs::metadata(bin).map_err(|e| format!("cannot stat {}: {e}", bin.display()))?;
    let mtime = meta
        .modified()
        .map_err(|e| format!("no mtime for {}: {e}", bin.display()))?;
    Ok(RowBinary {
        row,
        stamp: EngineBinary {
            path: bin.display().to_string(),
            mtime_ms: mtime
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
            size_bytes: meta.len(),
        },
        newer_source: dep_info(bin).map(|deps| newest_after(&deps, mtime)),
    })
}

/// Sources listed on the first line of cargo's `<bin>.d` (`bin: dep dep…`,
/// spaces in paths escaped as `\ `).
fn dep_info(bin: &Path) -> Option<Vec<PathBuf>> {
    let text = std::fs::read_to_string(bin.with_extension("d")).ok()?;
    let (_, deps) = text.lines().next()?.split_once(": ")?;
    Some(split_escaped(deps).into_iter().map(PathBuf::from).collect())
}

fn split_escaped(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.last_mut().unwrap().extend(chars.next()),
            ' ' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out.retain(|s| !s.is_empty());
    out
}

/// The newest of `deps` modified after `built`.
fn newest_after(deps: &[PathBuf], built: SystemTime) -> Option<PathBuf> {
    deps.iter()
        .filter_map(|d| Some((std::fs::metadata(d).ok()?.modified().ok()?, d)))
        .filter(|(t, _)| *t > built)
        .max_by_key(|(t, _)| *t)
        .map(|(_, d)| d.clone())
}

pub fn print_header(bins: &[RowBinary]) {
    println!("engine binaries:");
    for b in bins {
        let status = match &b.newer_source {
            None => " — unchecked (no cargo dep-info)".to_owned(),
            Some(None) => String::new(),
            Some(Some(src)) => format!(" — STALE: {} is newer", src.display()),
        };
        println!(
            "  {}: {} (mtime {}, {}){status}",
            b.row,
            b.stamp.path,
            utc(b.stamp.mtime_ms),
            fmt_bytes(b.stamp.size_bytes)
        );
    }
}

/// The refusal for a run over stale binaries, if any is.
pub fn stale_refusal(bins: &[RowBinary]) -> Option<String> {
    let stale: Vec<String> = bins
        .iter()
        .filter_map(|b| match &b.newer_source {
            Some(Some(src)) => Some(format!(
                "  {}: {} predates {}",
                b.row,
                b.stamp.path,
                src.display()
            )),
            _ => None,
        })
        .collect();
    (!stale.is_empty()).then(|| {
        format!(
            "stale engine binaries:\n{}\nrebuild with `{REBUILD}`, or pass --allow-stale to run them anyway",
            stale.join("\n")
        )
    })
}

/// `YYYY-MM-DDTHH:MM:SSZ` for `ms` since the epoch.
pub fn utc(ms: u128) -> String {
    let secs = (ms / 1000) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn utc_formats_known_instants() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_790_179_099_196), "2026-09-23T15:58:19Z");
    }

    #[test]
    fn dep_info_splits_on_unescaped_spaces() {
        assert_eq!(
            split_escaped("/a/b.rs /c\\ d/e.rs  /f.rs"),
            ["/a/b.rs", "/c d/e.rs", "/f.rs"]
        );
    }

    #[test]
    fn a_source_newer_than_the_binary_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let (bin, src) = (dir.path().join("eng"), dir.path().join("lib.rs"));
        std::fs::write(&src, b"").unwrap();
        std::fs::write(&bin, b"bin").unwrap();
        std::fs::write(
            dir.path().join("eng.d"),
            format!("{}: {}\n", bin.display(), src.display()),
        )
        .unwrap();
        let old = SystemTime::now() - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let fresh = inspect("eng", &bin).unwrap();
        assert_eq!(fresh.stamp.size_bytes, 3);
        assert!(stale_refusal(&[fresh]).is_none());

        std::fs::File::options()
            .write(true)
            .open(&bin)
            .unwrap()
            .set_modified(old - Duration::from_secs(60))
            .unwrap();
        let refusal = stale_refusal(&[inspect("eng", &bin).unwrap()]).expect("stale");
        assert!(
            refusal.contains(REBUILD) && refusal.contains("lib.rs"),
            "{refusal}"
        );

        std::fs::remove_file(dir.path().join("eng.d")).unwrap();
        assert!(inspect("eng", &bin).unwrap().newer_source.is_none());
    }
}

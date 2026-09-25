//! Box load per row: 1/5/15-minute load averages at the row's start and end.

use bench::{BenchmarkReport, LoadSpan};

/// load1 at or above this marks a row's timings as likely inflated.
pub const LOAD1_WARN: f64 = 4.0;

/// Current 1/5/15-minute load averages, rounded to 0.01; `None` when the OS
/// cannot say.
pub fn loadavg() -> Option<[f64; 3]> {
    let mut avg = [0f64; 3];
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
    (n == 3).then(|| avg.map(|x| (x * 100.0).round() / 100.0))
}

/// Run one row between two load samples and set its `load` (`None` if
/// either sample failed), warning on stderr at each end where load1 >=
/// [`LOAD1_WARN`]. An `Err` passes through.
pub fn stamp(
    row: &str,
    run: impl FnOnce() -> Result<BenchmarkReport, String>,
) -> Result<BenchmarkReport, String> {
    let start = loadavg();
    warn(row, "start", start);
    let mut report = run()?;
    let end = loadavg();
    warn(row, "end", end);
    report.load = start.zip(end).map(|(loadavg_start, loadavg_end)| LoadSpan {
        loadavg_start,
        loadavg_end,
    });
    Ok(report)
}

fn warn(row: &str, at: &str, sample: Option<[f64; 3]>) {
    match sample {
        Some([load1, ..]) if load1 >= LOAD1_WARN => eprintln!(
            "warning: load1 {load1:.2} >= {LOAD1_WARN} at the {at} of row {row}: timings are likely inflated"
        ),
        _ => {}
    }
}

/// True when load1 reached [`LOAD1_WARN`] at either end of the row.
pub fn inflated(span: &LoadSpan) -> bool {
    span.loadavg_start[0] >= LOAD1_WARN || span.loadavg_end[0] >= LOAD1_WARN
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn span(start: f64, end: f64) -> LoadSpan {
        LoadSpan {
            loadavg_start: [start, 0.0, 0.0],
            loadavg_end: [end, 9.0, 9.0],
        }
    }

    fn report() -> BenchmarkReport {
        BenchmarkReport {
            index_name: "row".to_string(),
            index_time: Duration::ZERO,
            index_size_bytes: 0,
            num_files_indexed: 0,
            queries: Vec::new(),
            memory: None,
            load: None,
            mode: bench::QueryMode::Scan,
            ready_time: None,
        }
    }

    #[test]
    fn inflated_at_either_end_from_the_threshold() {
        assert!(!inflated(&span(3.99, 3.99)), "only load1 counts");
        assert!(inflated(&span(4.0, 0.5)));
        assert!(inflated(&span(0.5, 4.0)));
    }

    #[test]
    fn loadavg_is_finite_and_non_negative() {
        let avg = loadavg().expect("getloadavg works on unix");
        assert!(avg.iter().all(|x| x.is_finite() && *x >= 0.0), "{avg:?}");
    }

    #[test]
    fn stamp_sets_load_and_passes_errors_through() {
        let stamped = stamp("row", || Ok(report())).unwrap();
        assert!(stamped.load.is_some());
        assert_eq!(
            stamp("row", || Err("boom".to_string())).unwrap_err(),
            "boom"
        );
    }
}

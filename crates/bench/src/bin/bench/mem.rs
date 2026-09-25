//! Child-process memory: a sampler thread polling a live child's RSS and
//! footprint (averages, sampler maxima) and the `getrusage(RUSAGE_CHILDREN)`
//! peak of reaped children (bytes on macOS, KiB on Linux; normalized in
//! `rss.rs`).
//! Failures degrade to `None`, never to a run failure.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::rss::{MemReading, read_child_mem, rusage_children_max_rss_bytes};

/// Default sampler tick; each tick is one syscall or one small file read.
pub const DEFAULT_SAMPLER_INTERVAL: Duration = Duration::from_millis(10);

/// One observation: wall time since sampler start plus a [`MemReading`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub elapsed: Duration,
    pub rss_bytes: u64,
    pub footprint_bytes: u64,
}

impl Sample {
    fn at(start: Instant, read: MemReading) -> Self {
        Self {
            elapsed: start.elapsed(),
            rss_bytes: read.rss,
            footprint_bytes: read.footprint,
        }
    }
}

/// Resolve the sampler tick from already-read env values.
///
/// `off` (`$FMGREP_BENCH_RSS_OFF`) disables sampling. `raw`
/// (`$FMGREP_BENCH_RSS_INTERVAL_MS`): `None` = unset/default, `N > 0`
/// overrides the tick, `0` disables, invalid values warn and fall back to
/// [`DEFAULT_SAMPLER_INTERVAL`].
pub fn resolve_interval(off: bool, raw: Option<&str>) -> Option<Duration> {
    if off {
        return None;
    }
    match raw {
        None => Some(DEFAULT_SAMPLER_INTERVAL),
        Some(s) => match s.parse::<u64>() {
            Ok(0) => None,
            Ok(ms) => Some(Duration::from_millis(ms)),
            Err(_) => {
                eprintln!(
                    "warning: invalid FMGREP_BENCH_RSS_INTERVAL_MS={s:?}; using default {:?}",
                    DEFAULT_SAMPLER_INTERVAL
                );
                Some(DEFAULT_SAMPLER_INTERVAL)
            }
        },
    }
}

/// [`resolve_interval`] over the live environment.
pub fn sampler_interval() -> Option<Duration> {
    let off = matches!(
        std::env::var("FMGREP_BENCH_RSS_OFF").as_deref(),
        Ok("1") | Ok("true")
    );
    let raw = std::env::var("FMGREP_BENCH_RSS_INTERVAL_MS").ok();
    resolve_interval(off, raw.as_deref())
}

/// One child's peak from the cumulative `RUSAGE_CHILDREN` watermark.
///
/// `prev` is the watermark before this child, `current` the reading after
/// reaping it. Returns `(new_prev, child_peak)`: a risen watermark means this
/// child set it and peaked at `current`; otherwise its peak is only bounded
/// above by `prev` and `child_peak` is `None` (callers fall back to the
/// sampler maximum).
pub fn observe_peak(prev: u64, current: Option<u64>) -> (u64, Option<u64>) {
    match current {
        Some(m) if m > prev => (m, Some(m)),
        _ => (prev, None),
    }
}

/// Tracks the cumulative `RUSAGE_CHILDREN` watermark across sequentially
/// reaped children within one harness run.
#[derive(Debug, Default)]
pub struct PeakTracker {
    watermark: u64,
}

impl PeakTracker {
    pub const fn new() -> Self {
        Self { watermark: 0 }
    }

    /// Read the watermark after reaping one child; returns that child's
    /// `getrusage`-anchored peak, or `None` when it set no new maximum
    /// (see [`observe_peak`]).
    pub fn observe(&mut self) -> Option<u64> {
        let (next, peak) = observe_peak(self.watermark, rusage_children_max_rss_bytes());
        self.watermark = next;
        peak
    }
}

/// Harness-side sampler: a thread polling [`read_child_mem`] for a live
/// child pid at a fixed interval. Construction never fails; read failures are
/// skipped samples, so a dead pid or an unsupported platform simply yields
/// fewer (or zero) samples.
pub struct Sampler {
    samples: Arc<Mutex<Vec<Sample>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Sampler {
    /// Start sampling `pid` immediately (the first read is synchronous, so
    /// even a child that dies within one interval leaves one sample attempt).
    /// The sampler never touches the child — stopping is a flag plus a join —
    /// and a thread spawn failure degrades to sync-only samples.
    pub fn start(pid: u32, interval: Duration) -> Self {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let start = Instant::now();
        if let Some(read) = read_child_mem(pid) {
            samples.lock().unwrap().push(Sample::at(start, read));
        }
        let thread_samples = Arc::clone(&samples);
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name(format!("rss-sampler-{pid}"))
            .spawn(move || {
                loop {
                    std::thread::sleep(interval);
                    if thread_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Some(read) = read_child_mem(pid) {
                        thread_samples.lock().unwrap().push(Sample::at(start, read));
                    }
                }
            })
            .ok();
        Self {
            samples,
            stop,
            handle,
        }
    }

    /// Signal the thread to stop, join it, and return timestamped samples
    /// (possibly empty — callers degrade, never fail).
    pub fn stop(self) -> Vec<Sample> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle {
            let _ = h.join();
        }
        match Arc::try_unwrap(self.samples) {
            Ok(m) => m.into_inner().unwrap_or_default(),
            // Unreachable after join (no other clones survive), but clone
            // rather than lose samples if it ever happens.
            Err(shared) => shared.lock().unwrap().clone(),
        }
    }
}

/// Watermark shared by the harness's sequentially reaped children. A
/// poisoned mutex degrades to `None` peaks.
static REAPED: Mutex<PeakTracker> = Mutex::new(PeakTracker::new());

/// Discard the peaks of every child reaped so far, so an earlier row's
/// bigger child is never credited to this row.
pub fn prime_reaped() {
    if let Ok(mut t) = REAPED.lock() {
        t.observe();
    }
}

/// Peak of the children reaped since the previous call (or prime), when
/// they set a new watermark.
pub fn reaped_peak() -> Option<u64> {
    REAPED.lock().ok().and_then(|mut t| t.observe())
}

pub fn opt_max(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// `(peak, avg)` of one phase: the sampler maximum raised to `reaped`, and
/// the sample mean (`None` without samples).
pub fn phase_stats(samples: &[u64], reaped: Option<u64>) -> (Option<u64>, Option<u64>) {
    let avg = (!samples.is_empty()).then(|| samples.iter().sum::<u64>() / samples.len() as u64);
    (opt_max(samples.iter().max().copied(), reaped), avg)
}

/// `(rss, footprint)` readings of `samples`.
pub fn split(samples: &[Sample]) -> (Vec<u64>, Vec<u64>) {
    samples
        .iter()
        .map(|s| (s.rss_bytes, s.footprint_bytes))
        .unzip()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_stats_raises_peak_to_reaped_peak() {
        assert_eq!(
            phase_stats(&[100, 300, 200], Some(900)),
            (Some(900), Some(200))
        );
        assert_eq!(
            phase_stats(&[100, 300, 200], Some(250)),
            (Some(300), Some(200))
        );
        assert_eq!(phase_stats(&[100, 300, 200], None), (Some(300), Some(200)));
    }

    #[test]
    fn phase_stats_without_samples_keeps_reaped_peak() {
        assert_eq!(phase_stats(&[], Some(700)), (Some(700), None));
        assert_eq!(phase_stats(&[], None), (None, None));
    }

    #[test]
    fn prime_hides_a_smaller_later_child() {
        let sh = |script: &str| {
            let st = std::process::Command::new("/bin/sh")
                .args(["-c", script])
                .status();
            assert!(st.unwrap().success());
        };
        sh(r#"x=$(printf "%020000000d" 0)"#);
        prime_reaped();
        sh(":");
        assert_eq!(reaped_peak(), None);
    }

    #[test]
    fn observe_peak_detects_new_watermark_only() {
        // First child sets the watermark: its peak is the reading.
        assert_eq!(observe_peak(0, Some(1000)), (1000, Some(1000)));
        // Same reading after the next child: no new max → unknown, bounded.
        assert_eq!(observe_peak(1000, Some(1000)), (1000, None));
        // Lower reading (smaller child): unknown, watermark holds.
        assert_eq!(observe_peak(1000, Some(400)), (1000, None));
        // getrusage failure: unknown, watermark holds.
        assert_eq!(observe_peak(1000, None), (1000, None));
        // Bigger child raises it: peak is the new reading.
        assert_eq!(observe_peak(1000, Some(2500)), (2500, Some(2500)));
    }

    #[test]
    fn interval_resolution_prefers_off_then_value_then_default() {
        assert_eq!(resolve_interval(true, None), None);
        assert_eq!(resolve_interval(true, Some("5")), None);
        assert_eq!(
            resolve_interval(false, None),
            Some(DEFAULT_SAMPLER_INTERVAL)
        );
        assert_eq!(
            resolve_interval(false, Some("25")),
            Some(Duration::from_millis(25))
        );
        assert_eq!(resolve_interval(false, Some("0")), None);
        // Invalid values degrade to the default, never to disabled.
        assert_eq!(
            resolve_interval(false, Some("bogus")),
            Some(DEFAULT_SAMPLER_INTERVAL)
        );
    }
}

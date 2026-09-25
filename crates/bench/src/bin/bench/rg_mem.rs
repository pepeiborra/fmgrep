//! Child-process memory of the `rg` row. The row notifies a thread-local
//! [`bench::rg::SpawnObserver`] at each spawn, and this observer samples the
//! child with the [`mem`](crate::mem) sampler and the `getrusage` watermark;
//! only the open-time spawns (probe, walk check) are
//! [`bench::rg::SpawnPhase::Index`]. Bookkeeping never fails the run.

use std::sync::Mutex;
use std::time::Duration;

use bench::rg::{RgUnifiedIndex, SpawnObserver, SpawnPhase, with_spawn_observer};
use bench::{BenchmarkReport, MemoryStats, StagedDocs, run_unified_benchmark};

use crate::mem::{PeakTracker, Sample, Sampler, phase_stats, sampler_interval, split};
use crate::subprocess::STAGED_EXT;

/// Sampler tick for `rg` children: 1 ms by default, because the probe and
/// walk-check children live milliseconds and the shared 10 ms tick would
/// only catch their startup read. An explicit
/// `$FMGREP_BENCH_RSS_INTERVAL_MS` is honored verbatim (including `0`);
/// `$FMGREP_BENCH_RSS_OFF=1` disables.
fn rg_sampler_interval() -> Option<Duration> {
    match std::env::var("FMGREP_BENCH_RSS_INTERVAL_MS") {
        Ok(_) => sampler_interval(),
        Err(_) => sampler_interval().and(Some(Duration::from_millis(1))),
    }
}

/// Per-phase accumulation: pooled sampler readings plus the running maximum
/// of per-child `getrusage` peaks.
#[derive(Debug, Default)]
struct PhaseAgg {
    samples: Vec<u64>,
    footprints: Vec<u64>,
    peak: Option<u64>,
}

impl PhaseAgg {
    /// Fold one reaped child in: its sampler readings plus its
    /// `getrusage`-anchored peak (`None` when it set no new watermark).
    fn add_child(&mut self, samples: &[Sample], child_peak: Option<u64>) {
        let (rss, footprints) = split(samples);
        self.samples.extend(rss);
        self.footprints.extend(footprints);
        if let Some(p) = child_peak {
            self.peak = Some(self.peak.map_or(p, |q: u64| q.max(p)));
        }
    }

    /// Summarize to `(peak, avg)`: peak is the max of the recorded child
    /// peaks and the sampler maximum (so `peak >= avg` whenever samples
    /// exist); avg is the pooled mean; both `None` when unmeasured.
    fn summarize(&self) -> (Option<u64>, Option<u64>) {
        let avg = if self.samples.is_empty() {
            None
        } else {
            Some(self.samples.iter().sum::<u64>() / self.samples.len() as u64)
        };
        let sample_max = self.samples.iter().max().copied();
        let peak = match (self.peak, sample_max) {
            (Some(p), Some(s)) => Some(p.max(s)),
            (Some(p), None) => Some(p),
            (None, s) => s,
        };
        (peak, avg)
    }
}

/// Observer state: one [`PhaseAgg`] per phase, the transient sampler for
/// the in-flight child (spawns are strictly sequential — at most one is
/// ever active), and the primed [`PeakTracker`].
struct State {
    index: PhaseAgg,
    query: PhaseAgg,
    active: Option<(Option<Sampler>, SpawnPhase)>,
    peaks: PeakTracker,
}

/// [`SpawnObserver`] measuring every `rg` child with the shared `mem.rs`
/// sampler + `getrusage` watermark. Construct one per benchmark run and
/// pass it to [`with_spawn_observer`]; read the result with [`finish`](Self::finish).
pub struct RgObserver {
    state: Mutex<State>,
}

impl RgObserver {
    fn new() -> Self {
        // Prime the watermark to the *current* RUSAGE_CHILDREN reading so
        // children reaped earlier in this harness process can never donate
        // their peak to an rg child; the priming read is discarded.
        let mut peaks = PeakTracker::new();
        peaks.observe();
        Self {
            state: Mutex::new(State {
                index: PhaseAgg::default(),
                query: PhaseAgg::default(),
                active: None,
                peaks,
            }),
        }
    }

    /// Aggregate both phases into harness [`MemoryStats`].
    /// `child_peak_rss_bytes` is the max over all reaped `rg` children.
    fn finish(&self) -> MemoryStats {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (index_peak, index_avg) = state.index.summarize();
        let (query_peak, query_avg) = state.query.summarize();
        let (index_peak_footprint_bytes, index_avg_footprint_bytes) =
            phase_stats(&state.index.footprints, None);
        let (query_peak_footprint_bytes, query_avg_footprint_bytes) =
            phase_stats(&state.query.footprints, None);
        MemoryStats {
            index_peak_rss_bytes: index_peak,
            index_avg_rss_bytes: index_avg,
            query_peak_rss_bytes: query_peak,
            query_avg_rss_bytes: query_avg,
            child_peak_rss_bytes: match (state.index.peak, state.query.peak) {
                (Some(i), Some(q)) => Some(i.max(q)),
                (Some(i), None) => Some(i),
                (None, q) => q,
            },
            index_peak_footprint_bytes,
            index_avg_footprint_bytes,
            query_peak_footprint_bytes,
            query_avg_footprint_bytes,
        }
    }
}

impl SpawnObserver for RgObserver {
    fn child_spawned(&self, pid: u32, phase: SpawnPhase) {
        // Started synchronously so the first read lands while even a
        // millisecond child is still alive; a disabled sampler stores None
        // and only getrusage peaks remain.
        let sampler = rg_sampler_interval().map(|tick| Sampler::start(pid, tick));
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active = Some((sampler, phase));
    }

    fn child_reaped(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some((sampler, phase)) = state.active.take() else {
            // Unbalanced reap (never happens with the current adapter) —
            // ignore rather than fail the run.
            return;
        };
        // The child is reaped, so the watermark diff attributes to exactly
        // this child (see mem::PeakTracker).
        let child_peak = state.peaks.observe();
        let samples = sampler.map(Sampler::stop).unwrap_or_default();
        match phase {
            SpawnPhase::Index => state.index.add_child(&samples, child_peak),
            SpawnPhase::Query => state.query.add_child(&samples, child_peak),
        }
    }
}

/// Run the `rg` row over `staged` with child-RSS measurement. Counts and
/// timings are the shared driver's; an invalid pattern is an `Err` naming
/// it, never silent zeros.
pub fn run_rg_unified(staged: &StagedDocs, terms: &[&str]) -> Result<BenchmarkReport, String> {
    let observer = std::rc::Rc::new(RgObserver::new());
    let mut report = with_spawn_observer(observer.clone(), || {
        run_unified_benchmark(|| RgUnifiedIndex::open(staged, STAGED_EXT), terms)
    })
    .map_err(|e| format!("rg run failed:\n{e}"))?;
    report.memory = Some(observer.finish());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples whose footprint is a tenth of their RSS.
    fn samples(rss: &[u64]) -> Vec<Sample> {
        rss.iter()
            .map(|&r| Sample {
                elapsed: Duration::ZERO,
                rss_bytes: r,
                footprint_bytes: r / 10,
            })
            .collect()
    }

    #[test]
    fn phase_agg_summarize_pools_samples_and_maxes_peaks() {
        let mut agg = PhaseAgg::default();
        assert_eq!(agg.summarize(), (None, None));
        // Two children: first sets a getrusage peak, second does not.
        agg.add_child(&samples(&[100, 200]), Some(1000));
        agg.add_child(&samples(&[300]), None);
        // Peak = max(child peaks, sampler max); avg = pooled mean.
        assert_eq!(agg.summarize(), (Some(1000), Some(200)));
        // A later child raising the watermark lifts the peak; avg re-means.
        agg.add_child(&samples(&[400]), Some(1500));
        assert_eq!(agg.summarize(), (Some(1500), Some(250)));
        assert_eq!(phase_stats(&agg.footprints, None), (Some(40), Some(25)));
    }

    #[test]
    fn phase_agg_peak_without_samples_still_anchors() {
        // Sampler disabled (or every read missed): the getrusage peak still
        // anchors something real; the average stays unmeasured, never zero.
        let mut agg = PhaseAgg::default();
        agg.add_child(&samples(&[]), Some(700));
        assert_eq!(agg.summarize(), (Some(700), None));
    }

    #[test]
    fn phase_agg_sampler_max_covers_unrecorded_children() {
        // A child that set no new watermark is only known via its sampler
        // readings: the phase peak keeps their maximum.
        let mut agg = PhaseAgg::default();
        agg.add_child(&samples(&[500, 900]), None);
        assert_eq!(agg.summarize(), (Some(900), Some(700)));
    }
}

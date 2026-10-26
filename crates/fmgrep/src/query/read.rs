//! A filtered literal answered by reading the files the filter keeps
//! instead of locating its occurrences.

use std::ops::{Range, RangeInclusive};

use crate::probe::{Answer, Query};
use crate::sharded::DocfmIndex;

/// Unmeasured µs per occurrence per sample-rate step: a warm locate on an
/// AWS Xeon core, 1.76 µs at rate 16.
const STEP_US: f64 = 1.76 / 16.0;
/// Bounds on a measured locate cost.
const LOCATE_RANGE: RangeInclusive<f64> = 0.02..=20.0;
/// Nanoseconds per byte read.
const BYTE_NS: f64 = 0.15;

/// µs per occurrence a shard locates: `measured` clamped, else `rate` steps.
pub fn locate_us(rate: u32, measured: Option<f64>) -> f64 {
    match measured {
        Some(us) if us.is_finite() => us.clamp(*LOCATE_RANGE.start(), *LOCATE_RANGE.end()),
        _ => f64::from(rate) * STEP_US,
    }
}

/// µs per file read and per occurrence located.
#[derive(Clone, Copy)]
pub struct Costs {
    pub file_us: f64,
    pub locate_us: f64,
}

/// A literal's occurrences in the shards a search overlaps.
#[derive(Clone, Copy, Default)]
struct Spread {
    /// In the busiest shard: shards locate in parallel, each serially.
    busiest: u64,
    /// Occurrences per byte of those shards.
    per_byte: f64,
    /// Whether the mode reads the files a locate finds.
    fetched: bool,
}

/// Per literal, the answer that reads the ids `kept` yields, when that costs
/// less than locating it in the shards overlapping `cover`; None to locate.
/// `fetched` says whether a located literal's files are read too.
pub fn read_instead(
    index: &DocfmIndex,
    literals: &[Option<&[u8]>],
    cover: &[Range<usize>],
    kept: impl Iterator<Item = usize>,
    doc_lens: &[u64],
    costs: Costs,
    fetched: impl Fn(&[u8]) -> bool,
) -> Vec<Option<Answer>> {
    let none = || literals.iter().map(|_| None).collect();
    let counts: Vec<Query<'_>> = literals.iter().flatten().map(|t| Query::Count(t)).collect();
    if counts.is_empty() {
        return none();
    }
    let counted = index.probe_all(&counts, Some(cover));
    let spreads: Vec<Spread> = (counted.iter().zip(literals.iter().flatten()))
        .map(|(a, l)| {
            let bytes: u64 = a
                .counts
                .iter()
                .map(|c| doc_lens[c.0.clone()].iter().sum::<u64>())
                .sum();
            let total: u64 = a.counts.iter().map(|c| c.1).sum();
            Spread {
                busiest: a.counts.iter().map(|c| c.1).max().unwrap_or(0),
                per_byte: total as f64 / bytes.max(1) as f64,
                fetched: fetched(l),
            }
        })
        .collect();
    // Charging a fetched locate every kept file bounds it from above.
    let bound = Spread {
        busiest: spreads.iter().map(|s| s.busiest).max().unwrap_or(0),
        per_byte: f64::MAX,
        fetched: spreads.iter().any(|s| s.fetched),
    };
    let (mut ids, mut bytes) = (Vec::new(), 0);
    for id in kept {
        ids.push(id);
        bytes += doc_lens[id];
        if !reads(bound, ids.len(), bytes, costs) {
            return none();
        }
    }
    let mut counted = counted.into_iter().zip(spreads);
    literals
        .iter()
        .map(|l| {
            l.as_ref()?;
            let (a, spread) = counted.next()?;
            reads(spread, ids.len(), bytes, costs).then(|| Answer {
                exact: Vec::new(),
                candidates: Some(ids.clone()),
                probe_us: a.probe_us,
                counts: Vec::new(),
            })
        })
        .collect()
}

/// Whether reading `files` of `bytes` costs less than locating `spread` plus,
/// when fetched, reading the files an even spread hits.
fn reads(spread: Spread, files: usize, bytes: u64, costs: Costs) -> bool {
    let read = files as f64 * costs.file_us + bytes as f64 * BYTE_NS / 1000.0;
    let mut locate = spread.busiest as f64 * costs.locate_us;
    if spread.fetched && files > 0 {
        let hits = spread.per_byte * bytes as f64;
        locate += read * -(-hits / files as f64).exp_m1();
    }
    locate > read
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(locate_us: f64, file_us: f64) -> Costs {
        Costs { file_us, locate_us }
    }

    fn located(busiest: u64) -> Spread {
        Spread {
            busiest,
            ..Spread::default()
        }
    }

    #[test]
    fn reading_beats_locating_only_when_cheaper() {
        assert!(!reads(located(0), 0, 0, cost(1.5, 0.0)));
        assert!(!reads(located(0), 1, 1, cost(1.5, 12.0)));
        assert!(reads(located(1000), 100, 1 << 20, cost(1.5, 12.0)));
        assert!(!reads(located(1000), 300, 1 << 20, cost(1.5, 12.0)));
        assert!(!reads(located(1000), 1, 16 << 20, cost(1.5, 0.0)));
        let at = |files| reads(located(1000), files, 0, cost(1.5, 10.0));
        assert!(at(149) && !at(150));
        assert!(reads(located(1000), 100, 0, cost(1.5, 12.0)));
        assert!(!reads(located(1000), 100, 0, cost(0.3, 12.0)));
        let at = |files| reads(located(1000), files, 0, cost(0.75, 10.0));
        assert!(at(74) && !at(75));
    }

    #[test]
    fn a_fetched_locate_also_pays_for_the_files_it_finds() {
        let fetched = |busiest, per_byte| Spread {
            busiest,
            per_byte,
            fetched: true,
        };
        let c = cost(0.3, 10.0);
        // 1000 files of 1 KB, 3000 occurrences: locating is cheaper unless
        // the occurrences land in most of the files.
        assert!(!reads(located(3000), 1000, 1 << 20, c));
        assert!(reads(fetched(3000, 1.0), 1000, 1 << 20, c));
        assert!(!reads(fetched(3000, 1e-5), 1000, 1 << 20, c));
        assert!(!reads(fetched(0, 1.0), 1000, 1 << 20, c));
        let every = |busiest| fetched(busiest, f64::MAX);
        assert!(reads(every(1), 1000, 1 << 20, c) && !reads(every(0), 1000, 1 << 20, c));
    }

    #[test]
    fn the_locate_cost_is_measured_clamped_else_rate_scaled() {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(locate_us(16, None), 1.76));
        assert!(near(locate_us(32, None), 3.52));
        assert!(near(locate_us(4, None), 0.44));
        assert_eq!(locate_us(16, Some(0.3)), 0.3);
        assert_eq!(locate_us(16, Some(0.0)), 0.02);
        assert_eq!(locate_us(16, Some(1e9)), 20.0);
        assert!(near(locate_us(16, Some(f64::NAN)), 1.76));
        assert!(near(locate_us(16, Some(f64::INFINITY)), 1.76));
    }

    #[test]
    fn serve_measures_a_finite_clamped_locate_cost() {
        use crate::tests::support::{TWO_FILES, index_it, stage_corpus};
        let indexed = |tmp: &std::path::Path, files: &[(&str, &[u8])]| {
            let index = tmp.join("idx");
            index_it(&stage_corpus(tmp, files), &index, false);
            index
        };
        let measured = |index: &std::path::Path| {
            let manifest = crate::manifest::read_manifest(index).unwrap();
            let (fm, mut loaded) =
                super::super::load(index, &manifest, None, false, false).unwrap();
            assert_eq!(loaded.locate_us, locate_us(16, None));
            loaded.measure_locate(&fm);
            (fm, loaded.locate_us)
        };
        let tmp = tempfile::tempdir().unwrap();
        let wide = "e".repeat(2000) + "\n";
        let files: Vec<(String, &[u8])> = (0..3)
            .map(|i| (format!("{i}.c"), wide.as_bytes()))
            .collect();
        let files: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), *b)).collect();
        crate::tests::SHARD_BYTES.set(Some(2100));
        let index = indexed(tmp.path(), &files);
        crate::tests::SHARD_BYTES.set(None);
        let (fm, us) = measured(&index);
        let shards = crate::manifest::read_manifest(&index).unwrap().blobs.len();
        assert_eq!((fm.sample_rate(), shards), (16, 3));
        assert!(fm.locate_us().is_some());
        assert!(us.is_finite() && LOCATE_RANGE.contains(&us), "{us}");

        let tmp = tempfile::tempdir().unwrap();
        let index = indexed(tmp.path(), &TWO_FILES);
        let (fm, us) = measured(&index);
        assert_eq!((fm.locate_us(), us), (None, locate_us(16, None)));

        let tmp = tempfile::tempdir().unwrap();
        crate::tests::SAMPLE_RATE.set(Some(32));
        let index = indexed(tmp.path(), &TWO_FILES);
        crate::tests::SAMPLE_RATE.set(None);
        let manifest = crate::manifest::read_manifest(&index).unwrap();
        let (_, loaded) = super::super::load(&index, &manifest, None, false, false).unwrap();
        assert!((loaded.locate_us - 3.52).abs() < 1e-9);
    }
}

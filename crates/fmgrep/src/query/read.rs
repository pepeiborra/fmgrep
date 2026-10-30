//! A filtered literal answered by reading the files the filter keeps
//! instead of locating its occurrences.

use std::fmt::Write;
use std::ops::{Range, RangeInclusive};

use crate::probe::{Answer, Query};
use crate::sharded::DocfmIndex;
use crate::verbose::{Us, plans, plural};

/// Unmeasured µs per located occurrence.
const LOCATE_US: f64 = 0.7;
/// Bounds on a measured locate cost.
const LOCATE_RANGE: RangeInclusive<f64> = 0.02..=20.0;
/// Nanoseconds per byte read.
const BYTE_NS: f64 = 0.15;

/// µs per occurrence a shard locates: `measured` clamped, else the default.
pub fn locate_us(measured: Option<f64>) -> f64 {
    match measured {
        Some(us) if us.is_finite() => us.clamp(*LOCATE_RANGE.start(), *LOCATE_RANGE.end()),
        _ => LOCATE_US,
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
            if plans() {
                let (read, locate) = estimates(bound, ids.len(), bytes, costs);
                eprintln!(
                    "plan:\nterms located: reading the first {} kept file{} already costs {} >= the dearest locate {}",
                    ids.len(),
                    plural(ids.len()),
                    Us(read),
                    Us(locate)
                );
            }
            return none();
        }
    }
    if plans() {
        eprint!("{}", note(literals, &spreads, ids.len(), bytes, costs));
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
    let (read, locate) = estimates(spread, files, bytes, costs);
    locate > read
}

/// [`reads`]' two costs, µs: reading, then locating.
fn estimates(spread: Spread, files: usize, bytes: u64, costs: Costs) -> (f64, f64) {
    let read = files as f64 * costs.file_us + bytes as f64 * BYTE_NS / 1000.0;
    let mut locate = spread.busiest as f64 * costs.locate_us;
    if spread.fetched && files > 0 {
        let hits = spread.per_byte * bytes as f64;
        locate += read * -(-hits / files as f64).exp_m1();
    }
    (read, locate)
}

/// The verbose plan: per literal, both costs and which one won.
fn note(
    literals: &[Option<&[u8]>],
    spreads: &[Spread],
    files: usize,
    bytes: u64,
    costs: Costs,
) -> String {
    let terms: Vec<(usize, String)> = (literals.iter().enumerate())
        .filter_map(|(k, l)| l.map(|l| (k, String::from_utf8_lossy(l).into_owned())))
        .collect();
    let width = terms.iter().map(|t| t.1.chars().count()).max().unwrap_or(0);
    let mut out = String::from("plan:\n");
    for ((k, term), &spread) in terms.iter().zip(spreads) {
        let (read, locate) = estimates(spread, files, bytes, costs);
        let s = plural(files);
        let base = spread.busiest as f64 * costs.locate_us;
        let mut how = format!("busiest shard {} x {}", spread.busiest, Us(costs.locate_us));
        if locate > base {
            let _ = write!(how, " + fetch {}", Us(locate - base));
        }
        let _ = match locate > read {
            true => writeln!(
                out,
                "term {k} {term:<width$}  read {files} file{s} ({bytes} bytes) instead of locating: read est {} < locate est {} ({how})",
                Us(read),
                Us(locate)
            ),
            false => writeln!(
                out,
                "term {k} {term:<width$}  located: locate est {} ({how}) <= read est {} ({files} file{s} x {} + {bytes} bytes)",
                Us(locate),
                Us(read),
                Us(costs.file_us)
            ),
        };
    }
    out
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
    fn the_locate_cost_is_measured_clamped_else_the_default() {
        assert_eq!(locate_us(None), 0.7);
        assert_eq!(locate_us(Some(0.3)), 0.3);
        assert_eq!(locate_us(Some(0.0)), 0.02);
        assert_eq!(locate_us(Some(1e9)), 20.0);
        assert_eq!(locate_us(Some(f64::NAN)), 0.7);
        assert_eq!(locate_us(Some(f64::INFINITY)), 0.7);
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
            let (fm, mut loaded) = super::super::load(index, &manifest, None, false).unwrap();
            assert_eq!(loaded.locate_us, locate_us(None));
            loaded.measure_locate(&fm);
            (fm, loaded.locate_us)
        };
        let tmp = tempfile::tempdir().unwrap();
        let wide = "E".repeat(2000) + "\n";
        let files: Vec<(String, &[u8])> = (0..3)
            .map(|i| (format!("{i}.c"), wide.as_bytes()))
            .collect();
        let files: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), *b)).collect();
        crate::tests::SHARD_BYTES.set(Some(2100));
        let index = indexed(tmp.path(), &files);
        crate::tests::SHARD_BYTES.set(None);
        let (fm, us) = measured(&index);
        let shards = crate::manifest::read_manifest(&index).unwrap().blobs.len();
        assert_eq!(shards, 3);
        assert!(fm.locate_us().is_some());
        assert!(us.is_finite() && LOCATE_RANGE.contains(&us), "{us}");

        let tmp = tempfile::tempdir().unwrap();
        let index = indexed(tmp.path(), &TWO_FILES);
        let (fm, us) = measured(&index);
        assert_eq!((fm.locate_us(), us), (None, locate_us(None)));
    }
}

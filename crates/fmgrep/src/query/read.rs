//! A filtered literal answered by reading the files the filter keeps
//! instead of locating its occurrences.

use std::ops::Range;

use crate::probe::{Answer, Query};
use crate::sharded::DocfmIndex;

/// Microseconds per occurrence a shard locates.
const LOCATE_US: f64 = 1.5;
/// Nanoseconds per byte read.
const BYTE_NS: f64 = 0.15;

/// Per literal, the answer that reads the ids `kept` yields, when that costs
/// less than locating it in the shards overlapping `cover`; None to locate.
pub fn read_instead(
    index: &DocfmIndex,
    literals: &[Option<&[u8]>],
    cover: &[Range<usize>],
    kept: impl Iterator<Item = usize>,
    doc_lens: &[u64],
    file_us: f64,
) -> Vec<Option<Answer>> {
    let none = || literals.iter().map(|_| None).collect();
    let counts: Vec<Query<'_>> = literals.iter().flatten().map(|t| Query::Count(t)).collect();
    if counts.is_empty() {
        return none();
    }
    let counted = index.probe_all(&counts, Some(cover));
    let busiest = |a: &Answer| a.counts.iter().map(|c| c.1).max().unwrap_or(0);
    let most = counted.iter().map(busiest).max().unwrap_or(0);
    let (mut ids, mut bytes) = (Vec::new(), 0);
    for id in kept {
        ids.push(id);
        bytes += doc_lens[id];
        if !reads(most, ids.len(), bytes, file_us) {
            return none();
        }
    }
    let mut counted = counted.into_iter();
    literals
        .iter()
        .map(|l| {
            l.as_ref()?;
            let a = counted.next()?;
            reads(busiest(&a), ids.len(), bytes, file_us).then(|| Answer {
                exact: Vec::new(),
                candidates: Some(ids.clone()),
                probe_us: a.probe_us,
                counts: Vec::new(),
            })
        })
        .collect()
}

/// Whether reading `files` of `bytes` costs less than locating `busiest`
/// occurrences: shards locate in parallel, each serially.
fn reads(busiest: u64, files: usize, bytes: u64, file_us: f64) -> bool {
    busiest as f64 * LOCATE_US > files as f64 * file_us + bytes as f64 * BYTE_NS / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_beats_locating_only_when_cheaper() {
        assert!(!reads(0, 0, 0, 0.0));
        assert!(!reads(0, 1, 1, 12.0));
        assert!(reads(1000, 100, 1 << 20, 12.0));
        assert!(!reads(1000, 300, 1 << 20, 12.0));
        assert!(!reads(1000, 1, 16 << 20, 0.0));
        let at = |files| reads(1000, files, 0, 10.0);
        assert!(at(149) && !at(150));
    }
}

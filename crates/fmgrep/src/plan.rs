//! How the index cuts its document sequence into shards: a pure function of
//! the per-document lengths and the byte cap, carried on disk as the blob
//! inventory labels, so a search re-derives it without reading the
//! environment.

use std::ops::Range;

/// Global doc ranges, one per shard: contiguous, ascending, covering
/// `0..num_docs`, each holding at least one non-empty doc.
pub type ShardPlan = Vec<Range<usize>>;

const SHARD_BYTES_VAR: &str = "FMGREP_SHARD_BYTES";
const DEFAULT_SHARD_BYTES: u64 = 100 * 1024 * 1024;

/// Shard cap for `index`, from `FMGREP_SHARD_BYTES`.
pub fn shard_bytes() -> Result<u64, String> {
    crate::bytes::env_bytes(SHARD_BYTES_VAR, DEFAULT_SHARD_BYTES)
}

/// The split, one document at a time: [`Cutter::push`] closes a shard the
/// moment the next document would take it past the cap, [`Cutter::finish`]
/// closes the trailing one.
pub struct Cutter {
    cap: u64,
    start: usize,
    at: usize,
    bytes: u64,
    kept: bool,
}

impl Cutter {
    pub fn of(cap: u64) -> Self {
        Self {
            cap,
            start: 0,
            at: 0,
            bytes: 0,
            kept: false,
        }
    }

    /// Feed the next document's length; `Some` is the shard it closed.
    pub fn push(&mut self, len: u64) -> Option<Range<usize>> {
        let at = self.at;
        self.at += 1;
        if len == 0 {
            return None;
        }
        let cost = len.saturating_add(1);
        let mut cut = None;
        if self.kept && self.bytes.saturating_add(cost) > self.cap {
            cut = Some(self.start..at);
            (self.start, self.bytes) = (at, 0);
        }
        self.bytes = self.bytes.saturating_add(cost);
        self.kept = true;
        cut
    }

    /// The trailing shard, once the stream is over.
    pub fn finish(&mut self) -> Option<Range<usize>> {
        self.kept.then(|| {
            self.kept = false;
            self.start..self.at
        })
    }
}

/// Cut documents of these lengths into shards of at most `cap` text bytes.
///
/// Whole documents only, greedy in doc order, each costing its length plus
/// its separator; a document over the cap forms its own shard. Empty
/// documents index nothing and ride along in whichever range spans them.
/// Empty when no document holds bytes.
#[cfg(test)]
pub fn plan_shards(lens: &[u64], cap: u64) -> ShardPlan {
    let mut cutter = Cutter::of(cap);
    let mut plan: ShardPlan = lens.iter().filter_map(|&len| cutter.push(len)).collect();
    plan.extend(cutter.finish());
    plan
}

/// Text bytes a slot holding `lens` builds: every non-empty document plus its
/// separator, the same cost [`Cutter`] packs.
pub fn shard_text_bytes(lens: &[u64]) -> u64 {
    lens.iter()
        .filter(|&&len| len > 0)
        .fold(0u64, |sum, &len| sum.saturating_add(len).saturating_add(1))
}

/// Inventory label of the blob holding shard `range`.
pub fn shard_label(range: &Range<usize>) -> String {
    format!("shard-{:08}-{:08}.bin", range.start, range.end)
}

fn parse_label(label: &str) -> Option<Range<usize>> {
    let (start, end) = label
        .strip_prefix("shard-")?
        .strip_suffix(".bin")?
        .split_once('-')?;
    if start.len() != 8 || end.len() != 8 {
        return None;
    }
    Some(start.parse().ok()?..end.parse().ok()?)
}

/// Re-derive the plan a persisted index was built with.
///
/// Every failure is a tampered inventory, not a recoverable one: a label that
/// does not parse, ranges that do not partition `0..num_docs`, or a range
/// holding no indexable document.
pub fn plan_from_labels(
    labels: &[String],
    num_docs: usize,
    doc_lens: &[u64],
) -> Result<ShardPlan, String> {
    let n = labels.len();
    let refuse = |what: String| {
        Err(format!(
            "{what} across {n} blob(s); refusing a tampered index"
        ))
    };
    if n == 0 {
        return refuse("manifest lists no shard blob".to_string());
    }
    if doc_lens.len() != num_docs {
        return refuse(format!(
            "manifest holds {} doc length(s) for {num_docs} doc(s)",
            doc_lens.len()
        ));
    }
    let mut plan = ShardPlan::with_capacity(n);
    for label in labels {
        let Some(range) = parse_label(label) else {
            return refuse(format!(
                "manifest blob label {label:?} is not a shard range"
            ));
        };
        plan.push(range);
    }
    let mut at = 0usize;
    for (slot, range) in plan.iter().enumerate() {
        if range.start != at || range.end < range.start || range.end > num_docs {
            return refuse(format!(
                "manifest shard labels do not partition {num_docs} doc(s) at slot {slot}"
            ));
        }
        if !doc_lens[range.clone()].iter().any(|&l| l > 0) {
            return refuse(format!(
                "manifest shard label at slot {slot} covers no indexable doc"
            ));
        }
        at = range.end;
    }
    if at != num_docs {
        return refuse(format!(
            "manifest shard labels cover {at} of {num_docs} doc(s)"
        ));
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Doc lengths, cap, and the expected shard boundaries as `(start, end)`.
    type PlanCase = (&'static [u64], u64, &'static [(usize, usize)]);

    /// Boundaries are a pure function of the lengths and the cap, empties
    /// included.
    #[test]
    fn plan_is_a_pure_function_of_doc_lens_and_cap() {
        let cases: &[PlanCase] = &[
            // Exact fit: costs 3+3+3 == cap, one shard.
            (&[2, 2, 2], 9, &[(0, 3)]),
            // One byte under: the last doc starts a second shard.
            (&[2, 2, 2], 8, &[(0, 2), (2, 3)]),
            // Every doc its own shard.
            (&[2, 2, 2], 1, &[(0, 1), (1, 2), (2, 3)]),
            // An over-cap doc stands alone; its neighbours pack normally.
            (&[1, 100, 1], 4, &[(0, 1), (1, 2), (2, 3)]),
            // Leading, interior and trailing empties ride along.
            (&[0, 2, 0, 2, 0], 3, &[(0, 3), (3, 5)]),
            (&[0, 2, 0, 2, 0], 99, &[(0, 5)]),
            // No indexable byte: no shard at all.
            (&[0, 0], 99, &[]),
            (&[], 99, &[]),
        ];
        for (lens, cap, want) in cases {
            let want: ShardPlan = want.iter().map(|&(a, b)| a..b).collect();
            assert_eq!(plan_shards(lens, *cap), want, "lens={lens:?} cap={cap}");
        }
    }

    /// Feeding lengths one at a time equals the batch plan, so the live cuts
    /// and [`plan_shards`] cannot disagree.
    #[test]
    fn cutter_matches_plan_shards() {
        let mut lens: Vec<u64> = Vec::new();
        let mut seed = 0x9E3779B97F4A7C15u64;
        for _ in 0..400 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            lens.push(seed % 7);
            for cap in [0u64, 1, 2, 3, 8, 64, u64::MAX] {
                let mut cutter = Cutter::of(cap);
                let mut plan: ShardPlan = lens.iter().filter_map(|&l| cutter.push(l)).collect();
                plan.extend(cutter.finish());
                assert_eq!(plan, plan_shards(&lens, cap), "cap={cap} lens={lens:?}");
            }
        }
    }

    #[test]
    fn one_shard_below_the_cap_covers_every_doc() {
        let lens = [0, 7, 0, 5, 0];
        let plan = plan_shards(&lens, DEFAULT_SHARD_BYTES);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0], 0..lens.len());
    }

    /// The default is 100 MiB, and it keeps the GHC corpus (26787 docs,
    /// 88781005 text bytes, 88.8 MB with separators) in a single shard.
    #[test]
    fn default_cap_keeps_the_ghc_corpus_in_one_shard() {
        assert_eq!(DEFAULT_SHARD_BYTES, 100 * 1024 * 1024);
        let (docs, text) = (26_787usize, 88_781_005u64);
        let mut lens = vec![text / docs as u64; docs];
        lens[0] += text % docs as u64;
        assert_eq!(lens.iter().sum::<u64>(), text);
        assert_eq!(plan_shards(&lens, DEFAULT_SHARD_BYTES), vec![0..docs]);
        // And it is the last power-of-two step that does: 50 MiB cuts it.
        assert!(plan_shards(&lens, DEFAULT_SHARD_BYTES / 2).len() > 1);
    }

    #[test]
    fn labels_round_trip() {
        let lens = [3, 0, 4, 5];
        let plan = plan_shards(&lens, 5);
        let labels: Vec<String> = plan.iter().map(shard_label).collect();
        assert_eq!(
            labels,
            [
                "shard-00000000-00000002.bin",
                "shard-00000002-00000003.bin",
                "shard-00000003-00000004.bin",
            ]
        );
        assert_eq!(plan_from_labels(&labels, lens.len(), &lens).unwrap(), plan);
    }

    #[test]
    fn tampered_labels_refused_loudly() {
        let lens = [3u64, 0, 4, 5];
        let good: Vec<String> = plan_shards(&lens, 5).iter().map(shard_label).collect();
        let swap = |slot: usize, to: &str| {
            let mut v = good.clone();
            v[slot] = to.to_string();
            v
        };
        let cases: Vec<Vec<String>> = vec![
            Vec::new(),
            swap(1, "index.bin"),
            // Gap, overlap, and a last range stopping short.
            swap(1, "shard-00000002-00000002.bin"),
            swap(1, "shard-00000001-00000003.bin"),
            swap(2, "shard-00000003-00000003.bin"),
            // An empty-only range.
            [
                "shard-00000000-00000001.bin",
                "shard-00000001-00000002.bin",
                "shard-00000002-00000004.bin",
            ]
            .map(String::from)
            .to_vec(),
        ];
        for labels in cases {
            let err = plan_from_labels(&labels, lens.len(), &lens)
                .err()
                .unwrap_or_else(|| panic!("labels {labels:?} must be refused"));
            assert!(
                err.contains("blob(s)") && err.contains("tampered"),
                "got: {err}"
            );
        }
        // A doc_lens table of the wrong length is refused too.
        assert!(plan_from_labels(&good, lens.len(), &lens[..2]).is_err());
    }
}

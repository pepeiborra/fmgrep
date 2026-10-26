//! One shard: its doc-range bookkeeping, its counts, its blob bytes.

use std::io::Write;
use std::ops::Range;

use crate::Document;

use crate::automaton::{DeferredPlan, MIN_ANCHOR_COUNT, RegexPlan, automaton_locate};
use crate::blobs::Blob;
use crate::cursor::{BackwardCursor, LocatingCursor};
use crate::fold::{FoldPlan, fold_locate};
use crate::pattern::{PreparedPattern, anchor, count_overlapping, deferred, others};
use crate::plan::shard_label;

/// One shard's docfm index. Separators are indexed text here, so the
/// caller's separator guard is load-bearing.
pub type Structure = docfm::FmIndex<Blob>;

impl BackwardCursor for docfm::Cursor<'_> {
    fn extend_front(&self, bytes: &[u8]) -> Self {
        docfm::Cursor::extend_front(self, bytes)
    }

    fn count(&self) -> u64 {
        docfm::Cursor::count(self)
    }
}

impl LocatingCursor for docfm::Cursor<'_> {
    fn locate(&self, visit: &mut dyn FnMut(usize)) {
        docfm::Cursor::locate(self, visit);
    }
}

/// False only when `term` provably occurs nowhere in `structure`.
fn may_contain(structure: &Structure, term: &[u8]) -> bool {
    term.iter().all(|&b| structure.contains_symbol(b))
}

/// A shard's doc-id bookkeeping. Doc ranges partition `0..num_docs`
/// contiguously.
pub struct ShardMeta {
    /// Global doc ids this shard answers for, empties included.
    pub docs: Range<usize>,
    /// Shard-local document ordinal -> global doc id, ascending.
    pub kept: Vec<usize>,
    /// Shard text bytes: every non-empty document plus its separator.
    pub text_len: usize,
}

impl ShardMeta {
    /// Bookkeeping of shard `docs` from its doc lengths, lockstep with the
    /// range.
    pub fn new(docs: Range<usize>, lens: &[u64]) -> Self {
        assert_eq!(docs.len(), lens.len(), "shard lengths must match its range");
        let kept: Vec<usize> = docs
            .clone()
            .zip(lens)
            .filter(|&(_, &l)| l > 0)
            .map(|(i, _)| i)
            .collect();
        let text_len = lens
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| l as usize + 1)
            .sum();
        Self {
            docs,
            kept,
            text_len,
        }
    }

    /// The shard-local ordinal of a located `hit`, if it names a document.
    fn sub_of(&self, hit: usize) -> Option<usize> {
        (hit < self.kept.len()).then_some(hit)
    }

    /// `structure` if it indexes this shard's non-empty document count.
    pub fn admit(&self, structure: Structure) -> Result<Structure, String> {
        let n = structure.num_docs();
        if n != self.kept.len() {
            return Err(format!(
                "structure indexes {n} docs, shard {} holds {}",
                shard_label(&self.docs),
                self.kept.len()
            ));
        }
        Ok(structure)
    }

    /// Ascending `(global doc id, count)` for `term`, zero counts omitted.
    pub fn counts(&self, structure: &Structure, term: &[u8]) -> Vec<(usize, u64)> {
        if !may_contain(structure, term) {
            return Vec::new();
        }
        let mut subs = Vec::new();
        structure.locate(term, &mut |hit| {
            if let Some(sub) = self.sub_of(hit) {
                subs.push(sub);
            }
        });
        self.tally(subs)
    }

    /// `term`'s occurrences in this shard, without a locate.
    pub fn count(&self, structure: &Structure, term: &[u8]) -> u64 {
        match may_contain(structure, term) {
            true => structure.cursor().extend_front(term).count(),
            false => 0,
        }
    }

    /// Whether this shard holds a doc of ascending disjoint `within`.
    pub fn overlaps(&self, within: &[Range<usize>]) -> bool {
        let first = within.partition_point(|r| r.end <= self.docs.start);
        within.get(first).is_some_and(|r| r.start < self.docs.end)
    }

    /// Ascending `(global doc id, occurrences)` of located shard ordinals:
    /// sorted when sparse, so the cost does not grow with the shard's docs.
    fn tally(&self, mut subs: Vec<usize>) -> Vec<(usize, u64)> {
        if subs.len() >= self.kept.len() / DENSE_TALLY {
            let mut counts = vec![0u64; self.kept.len()];
            for sub in subs {
                counts[sub] += 1;
            }
            return counts
                .into_iter()
                .enumerate()
                .filter(|&(_, c)| c > 0)
                .map(|(sub, c)| (self.kept[sub], c))
                .collect();
        }
        subs.sort_unstable();
        let mut out: Vec<(usize, u64)> = Vec::new();
        for sub in subs {
            match out.last_mut() {
                Some((doc, c)) if *doc == self.kept[sub] => *c += 1,
                _ => out.push((self.kept[sub], 1)),
            }
        }
        out
    }

    /// Ascending global doc ids that may hold a match of `pattern`: a walk
    /// where one is planned, else by its required literals, rarest first;
    /// empty when `pattern` does not [`narrows`](crate::pattern::narrows).
    pub fn regex_docs(&self, structure: &Structure, pattern: &PreparedPattern) -> Vec<usize> {
        let anchor = anchor(pattern);
        if let Some(plan) = pattern.fold() {
            return self.fold_docs(structure, plan, anchor);
        }
        if let Some(plan) = pattern.automaton() {
            return self.automaton_docs(structure, plan, anchor);
        }
        let Some(term) = anchor.filter(|t| may_contain(structure, t)) else {
            return Vec::new();
        };
        let others = others(pattern);
        if others.is_empty() && deferred(pattern).is_none() {
            return self.term_docs(structure, term);
        }
        let count = self.count(structure, term);
        if let Some(plan) = deferred(pattern)
            .filter(|_| count >= MIN_ANCHOR_COUNT)
            .and_then(DeferredPlan::plan)
        {
            return self.automaton_docs(structure, plan, anchor);
        }
        let terms: Vec<&[u8]> = std::iter::once(term)
            .chain(others.iter().map(Vec::as_slice))
            .collect();
        let counts: Vec<u64> = std::iter::once(count)
            .chain(others.iter().map(|t| self.count(structure, t)))
            .collect();
        rarest_first(&counts, |i| self.term_docs(structure, terms[i]))
    }

    /// Ascending global doc ids holding `term`.
    fn term_docs(&self, structure: &Structure, term: &[u8]) -> Vec<usize> {
        self.counts(structure, term)
            .into_iter()
            .map(|(doc, _)| doc)
            .collect()
    }

    /// Ascending global doc ids holding a match of the folded `plan`.
    /// `anchor` is the pattern's required literal, used on give-up.
    fn fold_docs(
        &self,
        structure: &Structure,
        plan: &FoldPlan,
        anchor: Option<&[u8]>,
    ) -> Vec<usize> {
        self.docs_at(structure, fold_locate(structure.cursor(), plan), anchor)
    }

    /// Ascending global doc ids the automaton `plan` admits.
    /// `anchor` is the pattern's required literal, used on give-up.
    fn automaton_docs(
        &self,
        structure: &Structure,
        plan: &RegexPlan,
        anchor: Option<&[u8]>,
    ) -> Vec<usize> {
        self.docs_at(
            structure,
            automaton_locate(structure.cursor(), plan),
            anchor,
        )
    }

    /// The documents located `hits` land in. A walk that gave up (`None`)
    /// falls back to the required literal, and only without one to the
    /// whole shard.
    pub fn docs_at(
        &self,
        structure: &Structure,
        hits: Option<Vec<usize>>,
        anchor: Option<&[u8]>,
    ) -> Vec<usize> {
        let Some(hits) = hits else {
            return match anchor {
                Some(term) => self.term_docs(structure, term),
                None => self.docs.clone().collect(),
            };
        };
        let subs = hits.into_iter().filter_map(|h| self.sub_of(h)).collect();
        self.tally(subs).into_iter().map(|(doc, _)| doc).collect()
    }

    /// Structure bytes, plus the shard text when the index stores the
    /// documents.
    pub fn size_bytes(&self, structure: &Structure, store_docs: bool) -> u64 {
        let text = if store_docs { self.text_len as u64 } else { 0 };
        structure.heap_bytes() + text
    }
}

/// Located occurrences that cost about one candidate document's read.
const LOCATE_RATIO: u64 = 8;

/// Candidates from required literal `counts` (anchor first, winning ties):
/// the rarest literal's docs, narrowed by each literal cheap under
/// [`LOCATE_RATIO`]; empty when any count is 0.
fn rarest_first(counts: &[u64], mut docs_of: impl FnMut(usize) -> Vec<usize>) -> Vec<usize> {
    if counts.contains(&0) {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..counts.len()).collect();
    order.sort_by_key(|&i| counts[i]);
    let Some((&first, rest)) = order.split_first() else {
        return Vec::new();
    };
    let mut docs = docs_of(first);
    for &i in rest {
        if docs.is_empty() || counts[i] > LOCATE_RATIO * docs.len() as u64 {
            break;
        }
        docs = intersect(&docs, &docs_of(i));
    }
    docs
}

/// Elements of both ascending `a` and `b`.
fn intersect(a: &[usize], b: &[usize]) -> Vec<usize> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                (i, j) = (i + 1, j + 1);
            }
        }
    }
    out
}

/// Hits at or above `kept / DENSE_TALLY` tally in a per-doc array.
const DENSE_TALLY: usize = 4;

/// A built shard: its structure plus its bookkeeping.
pub struct Shard {
    pub structure: Structure,
    pub meta: ShardMeta,
}

/// The shard's blob bytes and inventory label, shared by both persist paths.
pub fn encode_shard(
    structure: &Structure,
    docs: &Range<usize>,
    out: &mut dyn Write,
) -> Result<String, String> {
    structure.encode_into(out).map_err(|e| e.to_string())?;
    Ok(shard_label(docs))
}

/// Probe budget for the build-time locate self-check.
const VERIFY_PROBES: usize = 32;
/// Candidate probe lengths, shortest first; each probe locates at the
/// shortest whose shard-wide count is within [`VERIFY_MAX_HITS`].
const VERIFY_PROBE_LENS: [usize; 5] = [4, 8, 16, 32, 64];
/// Located hits allowed per probe, bounding the check's cost however common
/// its substring is.
const VERIFY_MAX_HITS: u64 = 1024;

/// A sampled substring with the count its source document holds of each
/// prefix, so the check outlives the shard's documents without retaining
/// their bytes.
pub struct Probe {
    /// Shard-local ordinal of the source document.
    sub: usize,
    term: Vec<u8>,
    /// `(prefix length, count in the source document)`, shortest first.
    wants: Vec<(usize, u64)>,
}

/// Up to [`VERIFY_PROBES`] substrings, at most two per document and never two
/// at the same offset, striding over `kept`. Call before the shard text moves
/// into the structure: each probe's expected counts are taken here.
pub fn sample_probes(kept: &[&Document]) -> Vec<Probe> {
    let n = kept.len();
    let mut probes = Vec::new();
    if n == 0 {
        return probes;
    }
    let longest = VERIFY_PROBE_LENS[VERIFY_PROBE_LENS.len() - 1];
    let stride = (n / (VERIFY_PROBES / 2)).max(1);
    let mut sub = 0;
    while sub < n && probes.len() < VERIFY_PROBES {
        let hay = &kept[sub].bytes;
        let mid = hay.len() / 2;
        let offsets: &[usize] = if mid == 0 { &[0] } else { &[0, mid] };
        for &off in offsets {
            if off < hay.len() && probes.len() < VERIFY_PROBES {
                let term = hay[off..(off + longest).min(hay.len())].to_vec();
                let mut wants: Vec<(usize, u64)> = Vec::new();
                for len in VERIFY_PROBE_LENS.map(|l| l.min(term.len())) {
                    if wants.last().is_none_or(|&(prev, _)| prev < len) {
                        let finder = memchr::memmem::Finder::new(&term[..len]);
                        wants.push((len, count_overlapping(hay, &finder)));
                    }
                }
                probes.push(Probe { sub, term, wants });
            }
        }
        sub += stride;
    }
    probes
}

/// Visits every occurrence of a term, in docfm's locate unit.
type Locate<'a> = dyn Fn(&[u8], &mut dyn FnMut(usize)) + 'a;

/// Every probe, at its shortest prefix within [`VERIFY_MAX_HITS`], must
/// `locate` its sampled count into its own document, as many hits as `count`
/// reports, and no hit past the shard's `kept` documents; each longer
/// prefix's count must cover its document's. Other documents' hits are not
/// scored: a mis-attribution shows as a short count in the probe's own
/// document.
pub fn self_check(
    count: &dyn Fn(&[u8]) -> u64,
    locate: &Locate<'_>,
    kept: usize,
    probes: &[Probe],
) -> bool {
    probes.iter().all(|p| {
        let mut pick = None;
        for &(len, want) in &p.wants {
            let count = count(&p.term[..len]);
            if count < want {
                return false;
            }
            if count <= VERIFY_MAX_HITS {
                pick = Some((len, want, count));
                break;
            }
        }
        let Some((len, want, count)) = pick else {
            return true;
        };
        let (mut here, mut total, mut stray) = (0u64, 0u64, false);
        locate(&p.term[..len], &mut |hit| {
            total += 1;
            if hit < kept {
                here += u64::from(hit == p.sub);
            } else {
                stray = true;
            }
        });
        !stray && here == want && total == count
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`rarest_first`] over `sets`, recording which literals it located.
    fn run(counts: &[u64], sets: &[&[usize]]) -> (Vec<usize>, Vec<usize>) {
        let mut located = Vec::new();
        let docs = rarest_first(counts, |i| {
            located.push(i);
            sets[i].to_vec()
        });
        (docs, located)
    }

    #[test]
    fn a_tie_locates_the_anchor() {
        assert_eq!(run(&[3, 3], &[&[1, 2], &[2, 3]]), (vec![2], vec![0, 1]));
        assert_eq!(run(&[9], &[&[4]]), (vec![4], vec![0]));
    }

    #[test]
    fn a_zero_count_locates_nothing() {
        assert_eq!(run(&[5, 0], &[&[1], &[]]), (vec![], vec![]));
    }

    #[test]
    fn a_literal_locates_up_to_the_ratio() {
        let two: &[usize] = &[1, 2];
        let at = LOCATE_RATIO * 2;
        assert_eq!(run(&[at, 2], &[&[2, 3], two]), (vec![2], vec![1, 0]));
        assert_eq!(run(&[at + 1, 2], &[&[2, 3], two]), (vec![1, 2], vec![1]));
        // The cheaper of two others locates first, the dearer never.
        assert_eq!(
            run(&[at + 1, 2, 3], &[&[1], two, &[1]]),
            (vec![1], vec![1, 2])
        );
    }

    #[test]
    fn an_intersection_only_shrinks() {
        assert_eq!(intersect(&[1, 3, 5, 7], &[0, 3, 4, 7, 9]), vec![3, 7]);
        assert!(intersect(&[1], &[]).is_empty());
        assert_eq!(run(&[1, 1, 1], &[&[1], &[2], &[1]]), (vec![], vec![0, 1]));
    }
}

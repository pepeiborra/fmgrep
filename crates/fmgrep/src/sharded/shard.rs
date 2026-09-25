//! One shard: its doc-range bookkeeping, its counts, its blob bytes.

use std::io::Write;
use std::ops::Range;

use crate::Document;

use crate::automaton::{DeferredPlan, RegexPlan, automaton_locate, min_anchor_count};
use crate::blobs::Blob;
use crate::cursor::{BackwardCursor, LocatingCursor};
use crate::fold::{FoldPlan, fold_locate};
use crate::pattern::{PreparedPattern, anchor, deferred};
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

/// One query's hits from the shards, appended in shard order.
#[derive(Default)]
pub struct Hits {
    /// Ascending `(doc id, count)` the FM-index settles exactly.
    pub exact: Vec<(usize, u64)>,
    /// Occurrences attributed to no doc.
    pub occurrences: u64,
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

    /// This shard's part of `term`'s occurrences: per doc if the shard holds
    /// a doc of ascending `place`, else counted without a locate.
    pub fn occurrences(
        &self,
        structure: &Structure,
        term: &[u8],
        place: &[usize],
        hits: &mut Hits,
    ) {
        let first = place.partition_point(|&doc| doc < self.docs.start);
        if place.get(first).is_some_and(|&doc| doc < self.docs.end) {
            hits.exact.extend(self.counts(structure, term));
        } else if may_contain(structure, term) {
            hits.occurrences += structure.cursor().extend_front(term).count();
        }
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
    /// where one is planned, else the anchor's documents; empty when
    /// `pattern` does not [`narrows`](crate::pattern::narrows).
    pub fn regex_docs(&self, structure: &Structure, pattern: &PreparedPattern) -> Vec<usize> {
        let anchor = anchor(pattern);
        if let Some(plan) = pattern.fold() {
            return self.fold_docs(structure, plan, anchor);
        }
        if let Some(plan) = pattern.automaton() {
            return self.automaton_docs(structure, plan, anchor);
        }
        let Some(term) = anchor else {
            return Vec::new();
        };
        if !may_contain(structure, term) {
            return Vec::new();
        }
        let common = || structure.cursor().extend_front(term).count() >= min_anchor_count();
        if let Some(plan) = deferred(pattern)
            .filter(|_| common())
            .and_then(DeferredPlan::plan)
        {
            return self.automaton_docs(structure, plan, anchor);
        }
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
                Some(term) => self
                    .counts(structure, term)
                    .into_iter()
                    .map(|(doc, _)| doc)
                    .collect(),
                None => self.docs.clone().collect(),
            };
        };
        let subs = hits.into_iter().filter_map(|h| self.sub_of(h)).collect();
        self.tally(subs).into_iter().map(|(doc, _)| doc).collect()
    }

    /// Structure bytes, plus the shard text when the index stores the
    /// documents: the wire size accounting.
    pub fn size_bytes(&self, structure: &Structure, store_docs: bool) -> u64 {
        let text = if store_docs { self.text_len as u64 } else { 0 };
        structure.heap_bytes() + text
    }
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
                        wants.push((len, crate::pattern::count_occurrences(hay, &term[..len])));
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
pub type Locate<'a> = dyn Fn(&[u8], &mut dyn FnMut(usize)) + 'a;

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

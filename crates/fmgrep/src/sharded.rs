//! The index: one docfm FM-index per corpus shard, one blob per shard. Shard
//! doc ranges partition `0..num_docs` contiguously, so merging per-shard hits
//! is a concatenation of ascending disjoint lists.

mod build;
mod shard;

use std::borrow::Cow;
use std::ops::{Range, RangeInclusive};
use std::time::Instant;

use rayon::prelude::*;

use crate::{Document, UNIFIED_SEPARATOR};

use crate::blobs::{Blob, BlobSink, BlobSlots, labels_of, map_slots};
use crate::docs::{Cut, Cuts};
use crate::pattern::PreparedPattern;
use crate::plan::{plan_from_labels, shard_label, shard_text_bytes};
use crate::probe::{Answer, Query};
use crate::sharded::build::{build_shards, refuse};
use crate::sharded::shard::{Shard, ShardMeta, Structure, encode_shard, sample_probes, self_check};

const DOC_SAMPLE_RATE: u32 = 16;
const SAMPLE_RATE_VAR: &str = "FMGREP_SAMPLE_RATE";
/// LF steps per sampled pass, about: rows times the rate.
const SAMPLE_STEPS: u64 = 4096;
/// Rows a locate sample takes per shard.
const SAMPLE_ROWS: RangeInclusive<u64> = 256..=4096;
/// Contiguous rows per sampled chunk: a few batches of docfm's lanes.
const SAMPLE_CHUNK: u64 = 64;

// The shard text's separator is docfm's document end.
const _: () = assert!(docfm::DOC_END == UNIFIED_SEPARATOR);

/// `term`, unless it is empty or holds the separator: it then matches nothing.
fn literal_term(term: &[u8]) -> Option<&[u8]> {
    (!term.is_empty() && !term.contains(&UNIFIED_SEPARATOR)).then_some(term)
}

/// docfm's sample rate for a build, from `FMGREP_SAMPLE_RATE`.
fn sample_rate() -> Result<u32, String> {
    #[cfg(test)]
    if let Some(rate) = crate::tests::SAMPLE_RATE.get() {
        return Ok(rate);
    }
    let rate = crate::env::count(SAMPLE_RATE_VAR, DOC_SAMPLE_RATE.into())?;
    u32::try_from(rate)
        .ok()
        .filter(|&r| r > 0)
        .ok_or_else(|| format!("{SAMPLE_RATE_VAR}={rate} is not a positive 32-bit integer"))
}

/// `Err` for a shard over docfm's text limit, before any of its bytes are
/// read: it would otherwise fail deep inside the build.
fn over_limit(cut: &Cut, max: u64) -> Result<(), String> {
    let bytes = shard_text_bytes(&cut.lens);
    if bytes > max {
        return Err(format!(
            "error: docfm build: shard {} holds {bytes} text bytes, over docfm's \
             {max}-byte limit; lower FMGREP_SHARD_BYTES to split it",
            shard_label(&cut.docs),
        ));
    }
    Ok(())
}

/// One shard: its documents, its text, its structure, its self-check. The
/// documents are dropped before construction.
fn build_one(cut: &Cut, docs: Cow<'_, [Document]>, rate: u32) -> Shard {
    // A separator byte in a document would make hits unattributable and
    // poison its whole shard.
    if let Some(d) = docs.iter().find(|d| d.bytes.contains(&UNIFIED_SEPARATOR)) {
        refuse(format!("doc {:?} contains separator byte 0x00", d.path));
    }
    let meta = ShardMeta::new(cut.docs.clone(), &cut.lens);
    let mut text = Vec::with_capacity(meta.text_len);
    for d in docs.iter().filter(|d| !d.bytes.is_empty()) {
        text.extend_from_slice(&d.bytes);
        text.push(UNIFIED_SEPARATOR);
    }
    // Sampled before the documents go, so the check outlives them.
    let kept: Vec<&Document> = docs.iter().filter(|d| !d.bytes.is_empty()).collect();
    let probes = sample_probes(&kept);
    drop(kept);
    drop(docs);
    // `text` moves into the structure: no shard text stays live.
    let built = docfm::FmIndex::build(text, rate).unwrap_or_else(|e| refuse(e.to_string()));
    let structure = docfm::FmIndex::view(Blob::Owned(built.into_bytes()))
        .unwrap_or_else(|e| refuse(e.to_string()));
    let structure = meta.admit(structure).unwrap_or_else(|e| refuse(e));
    let count = |term: &[u8]| structure.cursor().extend_front(term).count();
    let locate = |term: &[u8], visit: &mut dyn FnMut(usize)| structure.locate(term, visit);
    if !self_check(&count, &locate, meta.kept.len(), &probes) {
        refuse("locate self-check failed".to_string());
    }
    Shard { structure, meta }
}

/// µs per occurrence locating `rows` rows of `shard`'s commonest byte, in
/// chunks spread through one third of its rows; fastest of the 3 thirds.
fn sample_locate(shard: &Structure, rows: u64) -> Option<f64> {
    let root = shard.cursor();
    let byte = (1..=u8::MAX).max_by_key(|&b| root.extend_front(&[b]).count())?;
    let cursor = root.extend_front(&[byte]);
    if cursor.count() < rows {
        return None;
    }
    let chunks = rows.div_ceil(SAMPLE_CHUNK);
    let third = cursor.count() / 3;
    let stride = third / chunks;
    let len = SAMPLE_CHUNK.min(stride);
    let mut docs = Vec::with_capacity(rows as usize);
    let mut fastest = f64::INFINITY;
    for pass in 0..3 {
        docs.clear();
        let start = Instant::now();
        for chunk in 0..chunks {
            let at = pass * third + chunk * stride;
            cursor.rows(at..at + len).locate(&mut |d| docs.push(d));
        }
        fastest = fastest.min(start.elapsed().as_secs_f64());
    }
    Some(fastest * 1e6 / (chunks * len) as f64)
}

/// One shard's documents, borrowed when the caller already holds them.
type SlotDocs<'d> = Result<Cow<'d, [Document]>, String>;

/// Build every shard `cuts` closes into `emit`, ascending: each shard's
/// documents are read by `read`, never the corpus's.
fn build_each<'d>(
    cuts: &Cuts,
    cap: u64,
    rate: u32,
    window: Option<usize>,
    read: &(dyn Fn(&Cut) -> SlotDocs<'d> + Sync),
    emit: &mut dyn FnMut(usize, Shard) -> Result<(), String>,
) -> Result<(), String> {
    let build = |cut: &Cut| {
        over_limit(cut, docfm::MAX_TEXT_BYTES)?;
        Ok(build_one(cut, read(cut)?, rate))
    };
    build_shards(cuts, cap, window, &build, emit)
}

/// Every shard's structure and doc bookkeeping; a loaded shard is a view
/// over the mapped `blobs.shard`.
pub struct DocfmIndex {
    metas: Vec<ShardMeta>,
    shards: Vec<Structure>,
    total_docs: usize,
}

impl DocfmIndex {
    /// Persist each shard, built at the `FMGREP_SAMPLE_RATE` sample rate, and
    /// drop it: the documents, texts and FM-indexes of the shards in flight,
    /// never the corpus. `window` is the shards in flight; `None` derives it
    /// from the build budget. Returns the index size in bytes.
    pub fn build_into(
        cuts: &Cuts,
        cap: u64,
        window: Option<usize>,
        store_docs: bool,
        sink: &mut BlobSink,
    ) -> Result<u64, String> {
        let rate = sample_rate()?;
        let mut index_size_bytes = 0u64;
        build_each(
            cuts,
            cap,
            rate,
            window,
            &|cut| cuts.read(cut).map(Cow::Owned),
            &mut |_slot, shard| {
                index_size_bytes += shard.meta.size_bytes(&shard.structure, store_docs);
                sink.write_slot(|out| encode_shard(&shard.structure, &shard.meta.docs, out))
            },
        )?;
        Ok(index_size_bytes)
    }

    /// Map every shard of the persisted slots, re-deriving the plan from the
    /// labels and `doc_lens`. `blobs.shard` must not change while loaded.
    pub fn load(blobs: &BlobSlots<'_>, doc_lens: &[u64]) -> Result<Self, String> {
        let plan = plan_from_labels(labels_of(blobs), doc_lens.len(), doc_lens)?;
        let metas: Vec<ShardMeta> = plan
            .into_iter()
            .map(|range| ShardMeta::new(range.clone(), &doc_lens[range]))
            .collect();
        let mut shards = Vec::with_capacity(metas.len());
        for (slot, (blob, meta)) in map_slots(blobs)?.into_iter().zip(&metas).enumerate() {
            let structure =
                docfm::FmIndex::view(blob).map_err(|e| format!("shard slot {slot}: {e}"))?;
            shards.push(
                meta.admit(structure)
                    .map_err(|e| format!("shard slot {slot}: {e}; refusing a tampered index"))?,
            );
        }
        Ok(Self {
            metas,
            shards,
            total_docs: doc_lens.len(),
        })
    }

    /// Documents this index answers for.
    pub fn num_files_indexed(&self) -> usize {
        self.total_docs
    }

    /// Its shards' sample rate; the build default without shards.
    pub fn sample_rate(&self) -> u32 {
        self.shards
            .first()
            .map_or(DOC_SAMPLE_RATE, |s| s.sample_rate())
    }

    /// Warm µs per located occurrence, sampled; None when no shard is wide enough.
    pub fn locate_us(&self) -> Option<f64> {
        let last = self.shards.len().checked_sub(1)?;
        let mut picks = vec![0, last / 2, last];
        picks.dedup();
        let rows = (SAMPLE_STEPS / u64::from(self.sample_rate()))
            .clamp(*SAMPLE_ROWS.start(), *SAMPLE_ROWS.end());
        let mut us: Vec<f64> = picks
            .par_iter()
            .filter_map(|&i| sample_locate(&self.shards[i], rows))
            .collect();
        us.sort_by(f64::total_cmp);
        us.get(us.len() / 2).copied()
    }

    /// Per-shard hits of the shards overlapping `within` (all when None),
    /// concatenated in shard order, so the result stays ascending.
    fn map_shards<T, F>(&self, within: Option<&[Range<usize>]>, hits: F) -> Vec<T>
    where
        T: Send,
        F: Fn(&Structure, &ShardMeta) -> Vec<T> + Sync,
    {
        let hits = |(s, m): (&Structure, &ShardMeta)| match within.is_none_or(|w| m.overlaps(w)) {
            true => hits(s, m),
            false => Vec::new(),
        };
        let shards = self.shards.iter().zip(&self.metas);
        if self.shards.len() <= 1 {
            return shards.flat_map(hits).collect();
        }
        self.shards
            .par_iter()
            .zip(&self.metas)
            .flat_map_iter(hits)
            .collect()
    }

    /// Literal term: ascending `(doc, count)`, zero counts omitted. Complete:
    /// the FM-index answers every document, no fetch. An empty term or one
    /// holding the separator counts 0 everywhere. Counts are document-wide
    /// and overlapping, which is rg's only for a literal that can neither
    /// overlap itself nor cross a line.
    fn probe_literal(&self, term: &[u8], within: Option<&[Range<usize>]>) -> Vec<(usize, u64)> {
        match literal_term(term) {
            Some(term) => self.map_shards(within, |s, meta| meta.counts(s, term)),
            None => Vec::new(),
        }
    }

    /// Docs that could match `pattern`; the rest count 0 with no fetch.
    fn regex_candidates(
        &self,
        pattern: &PreparedPattern,
        within: Option<&[Range<usize>]>,
    ) -> Option<Vec<usize>> {
        if !crate::pattern::narrows(pattern) {
            return None;
        }
        Some(self.map_shards(within, |s, meta| meta.regex_docs(s, pattern)))
    }

    /// One answer per query, in order; shards outside ascending disjoint
    /// `within` answer nothing.
    pub fn probe_all(&self, queries: &[Query<'_>], within: Option<&[Range<usize>]>) -> Vec<Answer> {
        queries
            .iter()
            .map(|q| {
                let start = Instant::now();
                let mut counts = Vec::new();
                let (exact, candidates) = match *q {
                    Query::Literal(term) => (self.probe_literal(term, within), Some(Vec::new())),
                    Query::LiteralWithin { term, within: w } => {
                        let exact = match literal_term(term) {
                            Some(term) => {
                                self.map_shards(within, |s, meta| match meta.overlaps(w) {
                                    true => meta.counts(s, term),
                                    false => Vec::new(),
                                })
                            }
                            None => Vec::new(),
                        };
                        (exact, Some(Vec::new()))
                    }
                    Query::Count(term) => {
                        // Sequential: a count-only shard is one backward search.
                        let kept = |(_, m): &(&Structure, &ShardMeta)| {
                            within.is_none_or(|w| m.overlaps(w))
                        };
                        for (s, meta) in self.shards.iter().zip(&self.metas).filter(kept) {
                            match literal_term(term).map_or(0, |term| meta.count(s, term)) {
                                0 => {}
                                n => counts.push((meta.docs.clone(), n)),
                            }
                        }
                        (Vec::new(), Some(Vec::new()))
                    }
                    Query::Regex(pattern) => (Vec::new(), self.regex_candidates(pattern, within)),
                };
                Answer {
                    exact,
                    candidates,
                    probe_us: start.elapsed().as_micros() as u64,
                    counts,
                }
            })
            .collect()
    }
}

#[cfg(test)]
impl DocfmIndex {
    /// Build at `cap` from documents already in memory, every shard resident:
    /// the in-process oracle for the persist path.
    fn build_with_cap(docs: &[Document], cap: u64) -> Self {
        let lens: Vec<u64> = docs.iter().map(|d| d.bytes.len() as u64).collect();
        let paths: Vec<String> = docs.iter().map(|d| d.path.clone()).collect();
        let cuts = Cuts::closed(None, crate::plan::plan_shards(&lens, cap), &lens, &paths)
            .unwrap_or_else(|e| refuse(e));
        let (mut metas, mut shards) = (Vec::new(), Vec::new());
        build_each(
            &cuts,
            cap,
            DOC_SAMPLE_RATE,
            None,
            &|cut| Ok(Cow::Borrowed(&docs[cut.docs.clone()])),
            &mut |_slot, shard| {
                metas.push(shard.meta);
                shards.push(shard.structure);
                Ok(())
            },
        )
        .unwrap_or_else(|e| refuse(e));
        Self {
            metas,
            shards,
            total_docs: docs.len(),
        }
    }

    /// The files layout's size accounting.
    fn index_size_bytes(&self) -> u64 {
        self.shards
            .iter()
            .zip(&self.metas)
            .map(|(s, m)| m.size_bytes(s, false))
            .sum()
    }

    /// Slot count this index persists.
    fn blob_slots(&self) -> usize {
        self.metas.len()
    }

    /// Stream slot `slot` into `out`; its manifest inventory label.
    fn encode_into(&self, slot: usize, out: &mut dyn std::io::Write) -> Result<String, String> {
        let (Some(structure), Some(meta)) = (self.shards.get(slot), self.metas.get(slot)) else {
            return Err(format!(
                "blob slot {slot} out of range for a {}-shard index",
                self.metas.len()
            ));
        };
        encode_shard(structure, &meta.docs, out)
    }
}

#[cfg(test)]
mod tests;

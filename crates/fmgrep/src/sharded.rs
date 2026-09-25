//! The index: one docfm FM-index per corpus shard, one blob per shard. Shard
//! doc ranges partition `0..num_docs` contiguously, so merging per-shard hits
//! is a concatenation of ascending disjoint lists.

mod build;
mod shard;

use std::borrow::Cow;
use std::sync::Mutex;
use std::time::Instant;

use crate::{Document, UNIFIED_SEPARATOR};

use crate::blobs::{Blob, BlobSink, BlobSlots, labels_of, map_slots};
use crate::docs::{Cut, Cuts};
use crate::fetch::fetch_threads;
use crate::pattern::PreparedPattern;
use crate::plan::{plan_from_labels, shard_label, shard_text_bytes};
use crate::probe::{Answer, Candidates, Query};
use crate::sharded::build::{build_shards, refuse};
use crate::sharded::shard::{
    Hits, Shard, ShardMeta, Structure, encode_shard, sample_probes, self_check,
};

const DOC_SAMPLE_RATE: u32 = 32;

// The shard text's separator is docfm's document end.
const _: () = assert!(docfm::DOC_END == UNIFIED_SEPARATOR);

/// Shard probes in flight at most.
const MAX_PROBE_THREADS: usize = 4;

/// `term`, unless it is empty or holds the separator: it then matches nothing.
fn literal_term(term: &[u8]) -> Option<&[u8]> {
    (!term.is_empty() && !term.contains(&UNIFIED_SEPARATOR)).then_some(term)
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
fn build_one(cut: &Cut, docs: Cow<'_, [Document]>) -> Shard {
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
    let built = docfm::FmIndex::build(text, DOC_SAMPLE_RATE, rayon::current_num_threads())
        .unwrap_or_else(|e| refuse(e.to_string()));
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

/// One shard's documents, borrowed when the caller already holds them.
type SlotDocs<'d> = Result<Cow<'d, [Document]>, String>;

/// Build every shard `cuts` closes into `emit`, ascending: each shard's
/// documents are read by `read`, never the corpus's.
fn build_each<'d>(
    cuts: &Cuts,
    cap: u64,
    threads: Option<usize>,
    read: &(dyn Fn(&Cut) -> SlotDocs<'d> + Sync),
    emit: &(dyn Fn(usize, Shard) -> Result<(), String> + Sync),
) -> Result<(), String> {
    let build = |cut: &Cut| {
        over_limit(cut, docfm::MAX_TEXT_BYTES)?;
        Ok(build_one(cut, read(cut)?))
    };
    build_shards(cuts, cap, threads, &build, emit)
}

/// What one build produced, for the manifest and the wire report.
pub struct BuildOutcome {
    pub num_files_indexed: usize,
    pub index_size_bytes: u64,
}

/// Every shard's structure and doc bookkeeping; a loaded shard is a view
/// over the mapped `blobs.shard`.
pub struct DocfmIndex {
    metas: Vec<ShardMeta>,
    shards: Vec<Structure>,
    total_docs: usize,
    /// Whether size accounting counts the shard text.
    store_docs: bool,
}

impl DocfmIndex {
    /// Persist each shard as it is built and drop it: the documents, texts
    /// and FM-indexes of the shards in flight, never the corpus. `threads`
    /// is the shards in flight; `None` derives it from the environment.
    pub fn build_into(
        cuts: &Cuts,
        cap: u64,
        threads: Option<usize>,
        store_docs: bool,
        sink: &mut BlobSink,
    ) -> Result<BuildOutcome, String> {
        // Uncontended: the emit gate already serializes the slots.
        let out = Mutex::new((sink, 0u64));
        build_each(
            cuts,
            cap,
            threads,
            &|cut| cuts.read(cut).map(Cow::Owned),
            &|_slot, shard| {
                let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
                out.1 += shard.meta.size_bytes(&shard.structure, store_docs);
                out.0
                    .write_slot(|out| encode_shard(&shard.structure, &shard.meta.docs, out))
            },
        )?;
        let index_size_bytes = out.into_inner().unwrap_or_else(|e| e.into_inner()).1;
        Ok(BuildOutcome {
            num_files_indexed: cuts.num_docs(),
            index_size_bytes,
        })
    }

    /// Map every shard of the persisted slots, re-deriving the plan from the
    /// labels and `doc_lens`. `blobs.shard` must not change while loaded.
    pub fn load(blobs: &BlobSlots<'_>, doc_lens: &[u64], store_docs: bool) -> Result<Self, String> {
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
            store_docs,
        })
    }

    /// Documents this index answers for.
    pub fn num_files_indexed(&self) -> usize {
        self.total_docs
    }

    /// Size accounting reported on the wire.
    pub fn index_size_bytes(&self) -> u64 {
        self.shards
            .iter()
            .zip(&self.metas)
            .map(|(s, m)| m.size_bytes(s, self.store_docs))
            .sum()
    }

    /// Per-shard hits concatenated in shard order, so the result stays
    /// ascending and thread-count invariant.
    fn map_shards<T, F>(&self, hits: F) -> Vec<T>
    where
        T: Send,
        F: Fn(&Structure, &ShardMeta) -> Vec<T> + Sync,
    {
        let threads = fetch_threads()
            .min(MAX_PROBE_THREADS)
            .min(self.shards.len());
        let pairs: Vec<(&Structure, &ShardMeta)> = self.shards.iter().zip(&self.metas).collect();
        if threads <= 1 {
            return pairs.iter().flat_map(|&(s, m)| hits(s, m)).collect();
        }
        let chunk = pairs.len().div_ceil(threads);
        std::thread::scope(|scope| {
            let hits = &hits;
            let workers: Vec<_> = pairs
                .chunks(chunk)
                .map(|part| {
                    scope.spawn(move || {
                        part.iter()
                            .flat_map(|&(s, m)| hits(s, m))
                            .collect::<Vec<T>>()
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().expect("shard locate worker panicked"))
                .collect()
        })
    }

    /// Literal term: ascending `(doc, count)`, zero counts omitted. Complete:
    /// the FM-index answers every document, no fetch. An empty term or one
    /// holding the separator counts 0 everywhere. Counts are document-wide
    /// and overlapping, which is rg's only for a literal that can neither
    /// overlap itself nor cross a line.
    pub fn probe_literal(&self, term: &[u8]) -> Vec<(usize, u64)> {
        match literal_term(term) {
            Some(term) => self.map_shards(|s, meta| meta.counts(s, term)),
            None => Vec::new(),
        }
    }

    /// Docs that could match `pattern`; the rest count 0 with no fetch.
    pub fn regex_candidates(&self, pattern: &PreparedPattern) -> Candidates {
        if !crate::pattern::narrows(pattern) {
            return Candidates::All;
        }
        Candidates::Docs(self.map_shards(|s, meta| meta.regex_docs(s, pattern)))
    }

    /// One answer per query, in order.
    pub fn probe_all(&self, queries: &[Query<'_>]) -> Vec<Answer> {
        queries
            .iter()
            .map(|q| {
                let start = Instant::now();
                let (exact, candidates, occurrences) = match *q {
                    Query::Literal(term) => {
                        (self.probe_literal(term), Candidates::Docs(Vec::new()), 0)
                    }
                    Query::Regex(pattern) => (Vec::new(), self.regex_candidates(pattern), 0),
                    Query::Occurrences { term, place } => {
                        // Sequential: a count-only shard is one backward search.
                        let mut hits = Hits::default();
                        if let Some(term) = literal_term(term) {
                            for (s, meta) in self.shards.iter().zip(&self.metas) {
                                meta.occurrences(s, term, place, &mut hits);
                            }
                        }
                        (hits.exact, Candidates::Docs(Vec::new()), hits.occurrences)
                    }
                };
                Answer {
                    exact,
                    candidates,
                    probe_us: start.elapsed().as_micros() as u64,
                    occurrences,
                }
            })
            .collect()
    }
}

#[cfg(test)]
impl DocfmIndex {
    /// Build at `cap` from documents already in memory, every shard resident:
    /// the in-process oracle for the persist path.
    pub fn build_with_cap(docs: &[Document], cap: u64, store_docs: bool) -> Self {
        let lens: Vec<u64> = docs.iter().map(|d| d.bytes.len() as u64).collect();
        let paths: Vec<String> = docs.iter().map(|d| d.path.clone()).collect();
        let cuts = Cuts::closed(None, crate::plan::plan_shards(&lens, cap), &lens, &paths)
            .unwrap_or_else(|e| refuse(e));
        // Uncontended: the emit gate already serializes the shards.
        let built: Mutex<(Vec<ShardMeta>, Vec<Structure>)> = Mutex::new((Vec::new(), Vec::new()));
        build_each(
            &cuts,
            cap,
            None,
            &|cut| Ok(Cow::Borrowed(&docs[cut.docs.clone()])),
            &|_slot, shard| {
                let mut built = built.lock().unwrap_or_else(|e| e.into_inner());
                built.0.push(shard.meta);
                built.1.push(shard.structure);
                Ok(())
            },
        )
        .unwrap_or_else(|e| refuse(e));
        let (metas, shards) = built.into_inner().unwrap_or_else(|e| e.into_inner());
        Self {
            metas,
            shards,
            total_docs: docs.len(),
            store_docs,
        }
    }

    /// Slot count this index persists.
    pub fn blob_slots(&self) -> usize {
        self.metas.len()
    }

    /// Stream slot `slot` into `out`; its manifest inventory label.
    pub fn encode_into(&self, slot: usize, out: &mut dyn std::io::Write) -> Result<String, String> {
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

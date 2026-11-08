//! The sampled rows on their way through a build. Each is one `u64`, its row
//! in the high half; the rows' origin and what the low half holds change
//! from stage to stage, so each stage is its own type and only
//! [`DocSamples`] reaches the encoder.

use rayon::slice::ParallelSliceMut;

use crate::build::{SUPER, each};

/// msbwt's `row << 32 | position` per sampled position, in no particular
/// order; its rows count from 0.
pub struct MsbwtRows(Vec<u64>);

impl MsbwtRows {
    /// What `msbwt::bwt_sampled` returned.
    #[inline]
    pub fn new(rows: Vec<u64>) -> Self {
        Self(rows)
    }

    /// The same samples in the index's rows, which count from 1: row 0 is
    /// the sentinel's.
    pub fn into_text_rows(mut self) -> TextRows {
        each(self.0.chunks_mut(SUPER), |chunk| {
            for x in chunk {
                *x += 1 << 32;
            }
        });
        TextRows(self.0)
    }
}

/// `row << 32 | position` per sampled position, in no particular order, in
/// the index's rows.
#[derive(Default)]
pub struct TextRows(Vec<u64>);

impl TextRows {
    /// The samples' doc ids, `starts` the first position of each document,
    /// in row order; with `sentinel`, the sentinel row's sample in document
    /// 0 too.
    pub fn into_doc_samples(mut self, starts: &[u32], sentinel: bool) -> DocSamples {
        each(self.0.chunks_mut(SUPER), |chunk| {
            for x in chunk {
                let doc = starts.partition_point(|&s| s <= *x as u32) - 1;
                *x = *x >> 32 << 32 | doc as u64;
            }
        });
        if sentinel {
            self.0.push(0);
        }
        self.0.par_sort_unstable();
        DocSamples(self.0)
    }
}

/// `row << 32 | doc` per sampled row, ascending: what a blob stores.
pub struct DocSamples(Vec<u64>);

impl DocSamples {
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The sampled rows, ascending.
    #[inline]
    pub fn rows(&self) -> impl Iterator<Item = usize> {
        self.0.iter().map(|&x| (x >> 32) as usize)
    }

    /// The doc id of each sampled row, in row order.
    #[inline]
    pub fn docs(&self) -> impl Iterator<Item = u64> {
        self.0.iter().map(|&x| u64::from(x as u32))
    }
}

//! What the layers' tests share.

use dicetest::prelude::*;

/// `f` inside a rayon pool of `threads` threads.
pub(crate) fn in_pool<R: Send>(threads: usize, f: impl FnOnce() -> R + Send) -> R {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap()
        .install(f)
}

/// Positions around every block and super boundary (`1 << lb` and `span`
/// digits), plus random ones.
pub(crate) fn edges(len: usize, span: usize, lb: u32, fate: &mut Fate) -> Vec<usize> {
    let block = 1usize << lb;
    let mut at: Vec<usize> = (0..=len / block + 1)
        .flat_map(|k| [k * block, k * block + block / 2])
        .chain([span, len])
        .flat_map(|e| [e.saturating_sub(1), e, e + 1])
        .chain((0..200).map(|_| fate.roll(dice::uni_usize(0..=len))))
        .filter(|&i| i <= len)
        .collect();
    at.sort_unstable();
    at.dedup();
    at
}

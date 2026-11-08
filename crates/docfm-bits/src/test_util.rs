//! What the layers' tests share.

/// `f` inside a rayon pool of `threads` threads.
pub(crate) fn in_pool<R: Send>(threads: usize, f: impl FnOnce() -> R + Send) -> R {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap()
        .install(f)
}

pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// Positions around every block and super boundary (`1 << lb` and `span`
/// digits), plus random ones.
pub(crate) fn edges(len: usize, span: usize, lb: u32, rng: &mut Rng) -> Vec<usize> {
    let block = 1usize << lb;
    let mut at: Vec<usize> = (0..=len / block + 1)
        .flat_map(|k| [k * block, k * block + block / 2])
        .chain([span, len])
        .flat_map(|e| [e.saturating_sub(1), e, e + 1])
        .chain((0..200).map(|_| rng.below(len as u64 + 1) as usize))
        .filter(|&i| i <= len)
        .collect();
    at.sort_unstable();
    at.dedup();
    at
}

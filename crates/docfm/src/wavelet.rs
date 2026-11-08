//! A balanced `k`-ary wavelet matrix over the sentinel-free BWT codes,
//! `k = 1 << d` for `d` in 1, 2, 4: `depth` levels of `n` digits
//! (`digits.rs`), the most significant digit of each code first. Each level
//! holds the digits of the codes in the order the level above left them,
//! stably sorted by that level's digit. A rank or an access reads one block
//! per level: 8 levels for a byte alphabet at `d = 1`, 4 at 2, 2 at 4.

use crate::build::each;
use crate::digits::{self, Digits};

/// Most levels: codes are bytes.
pub const MAX_DEPTH: usize = 8;

/// Most `depth * k` entries of the per-level digit starts.
pub const MAX_STARTS: usize = 32;

/// The levels' places in a blob and what a rank needs besides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    pub n: usize,
    pub depth: usize,
    /// Bits per digit.
    pub d: u32,
    /// log2 of the digits per block.
    pub lb: u32,
    pub levels: [Digits; MAX_DEPTH],
    /// Entry `l * k + x`: positions of level `l` holding a digit below `x`.
    pub starts: [u32; MAX_STARTS],
    /// Where each code's positions start below the last level.
    pub bottom: [u32; 256],
}

impl Shape {
    /// Levels from `at` with the given payload bytes (padding included),
    /// each 64-byte aligned; also returns where the last ends.
    pub fn new(
        n: usize,
        depth: usize,
        d: u32,
        lb: u32,
        payload: &[u32; MAX_DEPTH],
        at: usize,
    ) -> (Self, usize) {
        let mut levels = [Digits::default(); MAX_DEPTH];
        let mut end = at;
        for (level, &p) in levels.iter_mut().zip(payload).take(depth) {
            *level = Digits::at(end, n, d, lb);
            end = (level.payload + p as usize).next_multiple_of(64);
        }
        let shape = Self {
            n,
            depth,
            d,
            lb,
            levels,
            starts: [0; MAX_STARTS],
            bottom: [0; 256],
        };
        (shape, end)
    }
}

/// Levels of `d`-bit digits for `sigma` codes; one code needs none.
pub fn depth_for(sigma: usize, d: u32) -> usize {
    bits_for(sigma).div_ceil(d as usize)
}

/// Bits for `sigma` codes.
pub fn bits_for(sigma: usize) -> usize {
    (usize::BITS - sigma.saturating_sub(1).leading_zeros()) as usize
}

#[derive(Clone, Copy)]
pub struct WaveletMatrix<'a> {
    bytes: &'a [u8],
    shape: &'a Shape,
}

impl<'a> WaveletMatrix<'a> {
    pub fn new(bytes: &'a [u8], shape: &'a Shape) -> Self {
        Self { bytes, shape }
    }

    /// Occurrences of digit `x` in `0..i` of level `l`, unclamped.
    pub fn level_rank(&self, l: usize, x: usize, i: usize) -> usize {
        let s = self.shape;
        let level = s.levels[l];
        match s.d {
            1 => level.rank::<1>(self.bytes, s.lb, i, x),
            2 => level.rank::<2>(self.bytes, s.lb, i, x),
            _ => level.rank::<4>(self.bytes, s.lb, i, x),
        }
    }

    /// Occurrences of `code` in `0..min(i, n)`. Saturating: tampered levels
    /// miscount, never panic.
    #[inline]
    pub fn rank(&self, code: u8, i: usize) -> usize {
        match self.shape.d {
            1 => self.rank_d::<1>(code, i),
            2 => self.rank_d::<2>(code, i),
            _ => self.rank_d::<4>(code, i),
        }
    }

    /// The code at `i < n` and its occurrences in `0..i`.
    #[inline]
    pub fn access_rank(&self, i: usize) -> (u8, usize) {
        match self.shape.d {
            1 => self.access_rank_d::<1>(i),
            2 => self.access_rank_d::<2>(i),
            _ => self.access_rank_d::<4>(i),
        }
    }

    #[inline(always)]
    fn rank_d<const D: u32>(&self, code: u8, i: usize) -> usize {
        let s = self.shape;
        let k = 1usize << D;
        let mut p = i.min(s.n);
        for (l, level) in s.levels.iter().enumerate().take(s.depth) {
            let x = usize::from(code) >> ((s.depth - 1 - l) * D as usize) & (k - 1);
            let r = level.rank::<D>(self.bytes, s.lb, p, x);
            p = (s.starts[(l * k + x) % MAX_STARTS] as usize + r).min(s.n);
        }
        p.saturating_sub(s.bottom[usize::from(code)] as usize)
    }

    #[inline(always)]
    fn access_rank_d<const D: u32>(&self, i: usize) -> (u8, usize) {
        let s = self.shape;
        let k = 1usize << D;
        let mut p = i.min(s.n);
        let mut code = 0usize;
        for (l, level) in s.levels.iter().enumerate().take(s.depth) {
            let (x, r) = level.access_rank::<D>(self.bytes, s.lb, p);
            code = (code << D | x) & 255;
            p = (s.starts[(l * k + x) % MAX_STARTS] as usize + r).min(s.n);
        }
        (code as u8, p.saturating_sub(s.bottom[code] as usize))
    }
}

/// Codes per parallel job of the encoder's passes over the BWT.
const CHUNK: usize = 1 << 16;

/// The occurrences of each code in each `CHUNK` of `codes`, one job per
/// chunk on the current rayon pool.
fn histograms(codes: &[u8]) -> Vec<[u32; 256]> {
    let mut parts = vec![[0u32; 256]; codes.len().div_ceil(CHUNK)];
    each(
        parts.iter_mut().zip(codes.chunks(CHUNK)),
        |(part, chunk)| {
            for &c in chunk {
                part[usize::from(c)] += 1;
            }
        },
    );
    parts
}

/// Writes `src` into `dst` stably sorted by `digit`, below `k <= 16`, given
/// each `CHUNK` of `src`'s count per digit; one job per chunk on the current
/// rayon pool. A run of one digit moves as one copy: BWT runs average
/// several bytes.
fn partition(
    src: &[u8],
    dst: &mut [u8],
    digit: impl Fn(u8) -> usize + Sync,
    k: usize,
    counts: &[[usize; 16]],
) {
    // Each chunk's piece of each digit's bucket, in chunk order.
    let mut pieces: Vec<[&mut [u8]; 16]> = counts
        .iter()
        .map(|_| std::array::from_fn(|_| &mut [][..]))
        .collect();
    let mut rest = dst;
    for x in 0..k {
        for (count, piece) in counts.iter().zip(&mut pieces) {
            let (here, after) = std::mem::take(&mut rest).split_at_mut(count[x]);
            piece[x] = here;
            rest = after;
        }
    }
    each(
        pieces.into_iter().zip(src.chunks(CHUNK)),
        |(piece, chunk)| {
            let mut at = [0usize; 16];
            let mut from = 0;
            while from < chunk.len() {
                let x = digit(chunk[from]);
                let len = chunk[from..]
                    .iter()
                    .position(|&c| digit(c) != x)
                    .unwrap_or(chunk.len() - from);
                if let Some(dst) = piece[x].get_mut(at[x]..at[x] + len) {
                    dst.copy_from_slice(&chunk[from..from + len]);
                }
                at[x] += len;
                from += len;
            }
        },
    );
}

/// Each `CHUNK` of `seq`'s count per `digit`, one job per chunk on the
/// current rayon pool.
fn digit_counts(seq: &[u8], digit: impl Fn(u8) -> usize + Sync) -> Vec<[usize; 16]> {
    let mut counts = vec![[0usize; 16]; seq.len().div_ceil(CHUNK)];
    each(
        counts.iter_mut().zip(seq.chunks(CHUNK)),
        |(count, chunk)| {
            for &c in chunk {
                count[digit(c)] += 1;
            }
        },
    );
    counts
}

/// The encoded levels of `codes` (all below `sigma`) in `d`-bit digits and
/// blocks of `1 << lb`, each its supers, heads and payload; the digit starts
/// of each; and where each code starts below the last. Each level's passes
/// run in chunks on the current rayon pool; the levels below the first take
/// one scratch copy of the codes, two past four levels.
pub fn encode(
    codes: &[u8],
    sigma: usize,
    d: u32,
    lb: u32,
) -> (Vec<Vec<u8>>, [u32; MAX_STARTS], [u32; 256]) {
    let depth = depth_for(sigma, d);
    let (n, k) = (codes.len(), 1usize << d);
    let chunk_codes = histograms(codes);
    let mut counts = [0usize; 256];
    for part in &chunk_codes {
        for (c, &p) in counts.iter_mut().zip(part) {
            *c += p as usize;
        }
    }
    let shift = |l: usize| ((depth - 1 - l) * d as usize) as u32;
    let digit = |c: u8, l: usize| usize::from(c) >> shift(l) & (k - 1);
    let mut levels = Vec::with_capacity(depth);
    let mut starts = [0u32; MAX_STARTS];
    let mut scratch = [Vec::new(), Vec::new()];
    for l in 0..depth {
        let mut at = [0usize; 16];
        for (c, &count) in counts.iter().enumerate() {
            at[digit(c as u8, l)] += count;
        }
        let mut sum = 0;
        for (x, a) in at.iter().enumerate().take(k) {
            starts[l * k + x] = sum as u32;
            sum += a;
        }
        // Level `l` reads the codes in the order level `l - 1` left them.
        let [even, odd] = &mut scratch;
        let (seq, next): (&[u8], &mut Vec<u8>) = match l {
            0 => (codes, even),
            _ if l % 2 == 1 => (&even[..], odd),
            _ => (&odd[..], even),
        };
        levels.push(digits::encode(seq, d, shift(l), lb));
        if l + 1 < depth {
            // The codes' chunk histograms give level 0's digit counts.
            let chunk_digits = if l == 0 {
                chunk_codes
                    .iter()
                    .map(|part| {
                        let mut count = [0usize; 16];
                        for (c, &p) in part.iter().enumerate() {
                            count[digit(c as u8, 0)] += p as usize;
                        }
                        count
                    })
                    .collect()
            } else {
                digit_counts(seq, |c| digit(c, l))
            };
            next.resize(n, 0);
            partition(seq, next, |c| digit(c, l), k, &chunk_digits);
        }
    }
    // Below the last level codes sort by their digits reversed.
    let reversed = |c: usize| (0..depth).fold(0, |r, l| r * k + digit(c as u8, depth - 1 - l));
    let mut order: Vec<usize> = (0..sigma).collect();
    order.sort_by_key(|&c| reversed(c));
    let mut bottom = [0u32; 256];
    let mut at = 0;
    for c in order {
        bottom[c] = at as u32;
        at += counts[c];
    }
    (levels, starts, bottom)
}

/// Payload bytes of each encoded level of `n` digits.
pub fn payloads(levels: &[Vec<u8>], n: usize, d: u32, lb: u32) -> [u32; MAX_DEPTH] {
    let (s, h) = digits::index_bytes(n, d, lb);
    std::array::from_fn(|l| levels.get(l).map_or(0, |v| (v.len() - s - h) as u32))
}

/// Copies the encoded `levels` where `shape` places them in the zeroed `out`.
pub fn write(levels: &[Vec<u8>], shape: &Shape, out: &mut [u8]) {
    for (level, at) in levels.iter().zip(&shape.levels) {
        out[at.supers..at.supers + level.len()].copy_from_slice(level);
    }
}

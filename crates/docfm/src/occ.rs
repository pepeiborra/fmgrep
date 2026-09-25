//! Bit-sliced occurrence blocks over the sentinel-free BWT codes. A super, one
//! per `SUPER` positions, holds `[u32; sigma]` occurrences before it, padded
//! to 64 bytes. A block, one per `BLOCK` positions, holds `[u16; sigma]`
//! occurrences from the super's start to the block's midpoint, padded to 64
//! bytes, then `BLOCK / 64` groups of `bits` little-endian u64 planes (plane
//! `j` holds bit `j` of each position's code); positions past `n` are code 0.
//! A rank reads one super, one block count and at most `BLOCK / 2` positions
//! of one block.

use crate::layout::{HEADER_BYTES, Header};
use crate::par;

pub const BLOCK: usize = 4096;
/// Bounded by the u16 block counts.
pub const SUPER: usize = 65536;

#[derive(Clone, Copy)]
pub struct Occ<'a> {
    bytes: &'a [u8],
    header: &'a Header,
}

impl<'a> Occ<'a> {
    pub fn new(bytes: &'a [u8], header: &'a Header) -> Self {
        Self { bytes, header }
    }

    #[inline]
    fn block(&self, i: usize) -> &'a [u8] {
        let h = self.header;
        &self.bytes[h.blocks_at + i / BLOCK * h.block_stride..][..h.block_stride]
    }

    /// Occurrences of `code` in `0..min(i, n)`. Saturating: tampered counts
    /// miscount, never panic.
    #[inline]
    pub fn rank(&self, code: u8, i: usize) -> usize {
        let i = i.min(self.header.n);
        self.rank_in(self.block(i), usize::from(code), i)
    }

    /// The code at `i < n` and its occurrences in `0..i`, from one block.
    #[inline]
    pub fn access_rank(&self, i: usize) -> (u8, usize) {
        let h = self.header;
        let i = i.min(h.n);
        let block = self.block(i);
        let off = i % BLOCK;
        let group = h.counts_bytes + off / 64 * h.bits * 8;
        let code = (0..h.bits).fold(0usize, |code, j| {
            let word = u64::from_le_bytes(*block[group + j * 8..].first_chunk().expect("in block"));
            code | ((word >> (off % 64)) as usize & 1) << j
        });
        (code as u8, self.rank_in(block, code, i))
    }

    #[inline]
    fn rank_in(&self, block: &[u8], code: usize, i: usize) -> usize {
        let h = self.header;
        let code = code.min(h.sigma.saturating_sub(1));
        let sup = HEADER_BYTES + i / SUPER * h.super_stride + 4 * code;
        let base = u32::from_le_bytes(*self.bytes[sup..].first_chunk().expect("in supers"))
            as usize
            + usize::from(u16::from_le_bytes([block[2 * code], block[2 * code + 1]]));
        let planes = &block[h.counts_bytes..];
        let (off, mid) = (i % BLOCK, BLOCK / 2);
        if off >= mid {
            base + matches(h.bits, planes, code, mid, off)
        } else {
            base.saturating_sub(matches(h.bits, planes, code, off, mid))
        }
    }
}

/// Positions in `from..to` of one block's `planes` holding `code`.
#[inline]
fn matches(bits: usize, planes: &[u8], code: usize, from: usize, to: usize) -> usize {
    match bits {
        0 => to - from,
        1 => matches_n::<1>(planes, code, from, to),
        2 => matches_n::<2>(planes, code, from, to),
        3 => matches_n::<3>(planes, code, from, to),
        4 => matches_n::<4>(planes, code, from, to),
        5 => matches_n::<5>(planes, code, from, to),
        6 => matches_n::<6>(planes, code, from, to),
        7 => matches_n::<7>(planes, code, from, to),
        _ => matches_n::<8>(planes, code, from, to),
    }
}

#[inline(always)]
fn matches_n<const BITS: usize>(planes: &[u8], code: usize, from: usize, to: usize) -> usize {
    let flip: [u64; BITS] = std::array::from_fn(|j| if code >> j & 1 == 1 { 0 } else { u64::MAX });
    let words = planes.as_chunks::<8>().0;
    let mut total = 0;
    for g in from / 64..to.div_ceil(64) {
        let group = &words[g * BITS..][..BITS];
        let mut m = u64::MAX;
        for j in 0..BITS {
            m &= u64::from_le_bytes(group[j]) ^ flip[j];
        }
        let lo = g * 64;
        if from > lo {
            m &= u64::MAX << (from - lo);
        }
        if to < lo + 64 {
            m &= (1u64 << (to - lo)) - 1;
        }
        total += m.count_ones() as usize;
    }
    total
}

/// Writes the supers and blocks of `codes` (all below `sigma`) into the
/// zeroed blob `out`, on up to `threads` threads.
pub fn write_occ(codes: &[u8], h: &Header, out: &mut [u8], threads: usize) {
    let sigma = h.sigma.max(1);
    let supers = h.n / SUPER + 1;
    let per = par::chunk_len(supers, threads);
    // The last range's counts are never needed.
    let heads = codes.chunks(per * SUPER).take(supers.div_ceil(per) - 1);
    let counts = par::map(heads.collect(), |chunk| {
        let mut count = vec![0u32; sigma];
        for &code in chunk {
            count[usize::from(code)] += 1;
        }
        count
    });
    let (head, tail) = out.split_at_mut(h.blocks_at);
    let supers_out = &mut head[HEADER_BYTES..][..supers * h.super_stride];
    let blocks_out = &mut tail[..(h.n / BLOCK + 1) * h.block_stride];
    let mut before = vec![0u32; sigma];
    let mut ranges = Vec::new();
    for (i, (sup, blocks)) in supers_out
        .chunks_mut(per * h.super_stride)
        .zip(blocks_out.chunks_mut(per * SUPER / BLOCK * h.block_stride))
        .enumerate()
    {
        ranges.push((i * per * SUPER, before.clone(), sup, blocks));
        for (b, c) in before.iter_mut().zip(counts.get(i).into_iter().flatten()) {
            *b += c;
        }
    }
    par::map(ranges, |(start, before, sup, blocks)| {
        write_range(codes, h, start, before, sup, blocks)
    });
}

/// Supers and blocks from position `start`, a multiple of `SUPER`, with
/// `before` the occurrences in `0..start`.
fn write_range(
    codes: &[u8],
    h: &Header,
    start: usize,
    mut before: Vec<u32>,
    sup: &mut [u8],
    blocks: &mut [u8],
) {
    let bits = h.bits;
    let mut at_super = before.clone();
    for (k, block) in blocks.chunks_mut(h.block_stride).enumerate() {
        let pos = start + k * BLOCK;
        if pos.is_multiple_of(SUPER) {
            at_super.copy_from_slice(&before);
            let dst = &mut sup[k * BLOCK / SUPER * h.super_stride..][..h.super_stride];
            for (d, v) in dst.as_chunks_mut::<4>().0.iter_mut().zip(&before) {
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
        let (counts, planes) = block.split_at_mut(h.counts_bytes);
        for g in 0..BLOCK / 64 {
            if g == BLOCK / 128 {
                for ((d, b), s) in counts
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(&before)
                    .zip(&at_super)
                {
                    d.copy_from_slice(&((b - s) as u16).to_le_bytes());
                }
            }
            let mut words = [0u64; 8];
            for bit in 0..64 {
                let code = codes.get(pos + g * 64 + bit).copied().unwrap_or(0);
                before[usize::from(code)] += 1;
                for (j, w) in words.iter_mut().enumerate().take(bits) {
                    *w |= u64::from(code >> j & 1) << bit;
                }
            }
            let group = &mut planes[g * bits * 8..][..bits * 8];
            for (d, w) in group.as_chunks_mut::<8>().0.iter_mut().zip(words) {
                d.copy_from_slice(&w.to_le_bytes());
            }
        }
    }
}

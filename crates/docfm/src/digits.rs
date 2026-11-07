//! A sequence of `len` digits of `d` bits (`d` in 1, 2, 4; `k = 1 << d`
//! values) cut into blocks of `1 << lb` digits, each stored as nothing (one
//! digit throughout), plain digits, or runs, with per-digit counts per
//! block. Supers hold `SUPER_BITS / d` digits, so a super's plain payload
//! is at most 8 KiB and in-super counts fit a u16. Three sections, each a
//! multiple of 8 bytes:
//!
//! - supers: a u32 byte offset of the super's first payload, then a u32
//!   count per digit `1..k` before the super;
//! - heads, one per block: a u16 with its kind in bits 0..2 (0 one digit,
//!   2 plain, 3 runs) and in bits 2..16 that digit or its payload's byte
//!   offset from its super's, then a u16 count per digit `1..k` from its
//!   super's start; the count of digit 0 is what the others leave;
//! - payloads, then 8 zero bytes. Plain is the block's digits, `d` bits
//!   each from bit 0 of its first byte. Runs is a bit stream: each run's
//!   digit in `d` bits (for `d = 1` only the first, the rest alternate)
//!   and its length, the last run included, Elias-gamma coded (`z` zeros,
//!   a one, the low `z` bits of the length).
//!
//! Reads clamp to the bytes given: tampered heads miscount, never read out
//! of bounds or decode past a block.

use crate::build::each;

/// Bits per super, over the digit width.
pub const SUPER_BITS: usize = 65536;

const ONE: u16 = 0;
const PLAIN: u16 = 2;
const RUNS: u16 = 3;

/// Digits per super.
pub fn super_len(d: u32) -> usize {
    SUPER_BITS >> d.trailing_zeros()
}

/// Bytes of the supers and of the heads of `len` digits of `d` bits in
/// blocks of `1 << lb`.
pub fn index_bytes(len: usize, d: u32, lb: u32) -> (usize, usize) {
    let k = 1usize << d;
    (
        4 * k * (len / super_len(d) + 1),
        (2 * k * ((len >> lb) + 1)).next_multiple_of(8),
    )
}

/// Most payload bytes of `len` digits of `d` bits in blocks of `1 << lb`,
/// padding included.
pub fn max_payload(len: usize, d: u32, lb: u32) -> usize {
    len.div_ceil(1 << lb) * ((d as usize) << lb) / 8 + 8
}

/// Where one sequence's sections start in a blob.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Digits {
    pub supers: usize,
    pub heads: usize,
    pub payload: usize,
}

#[inline]
fn u16_at(b: &[u8], at: usize) -> usize {
    match b.get(at..at.wrapping_add(2)) {
        Some(w) => usize::from(u16::from_le_bytes(w.try_into().expect("2 bytes"))),
        None => 0,
    }
}

#[inline]
fn u32_at(b: &[u8], at: usize) -> usize {
    match b.get(at..at.wrapping_add(4)) {
        Some(w) => u32::from_le_bytes(w.try_into().expect("4 bytes")) as usize,
        None => 0,
    }
}

/// The 64 bits from byte `at`, zero past the end.
#[inline]
fn u64_at(b: &[u8], at: usize) -> u64 {
    match b.get(at..at.wrapping_add(8)) {
        Some(w) => u64::from_le_bytes(w.try_into().expect("8 bytes")),
        None => {
            let mut w = [0u8; 8];
            let tail = b.get(at..).unwrap_or(&[]);
            w[..tail.len()].copy_from_slice(tail);
            u64::from_le_bytes(w)
        }
    }
}

/// One bit at the bottom of each `D`-bit slot of `w` that holds `x`.
#[inline(always)]
fn equal<const D: u32>(w: u64, x: usize) -> u64 {
    match D {
        1 => {
            if x & 1 == 1 {
                w
            } else {
                !w
            }
        }
        2 => {
            let m = !(w ^ ((x as u64 & 3) * 0x5555_5555_5555_5555));
            m & m >> 1 & 0x5555_5555_5555_5555
        }
        _ => {
            let m = !(w ^ ((x as u64 & 15) * 0x1111_1111_1111_1111));
            let m = m & m >> 1;
            m & m >> 2 & 0x1111_1111_1111_1111
        }
    }
}

impl Digits {
    /// The sections of `len` digits of `d` bits in blocks of `1 << lb`
    /// from `at`.
    pub fn at(at: usize, len: usize, d: u32, lb: u32) -> Self {
        let (s, h) = index_bytes(len, d, lb);
        Self {
            supers: at,
            heads: at + s,
            payload: at + s + h,
        }
    }

    /// Occurrences of digit `x` before block `i >> lb`, given its head.
    #[inline]
    fn before<const D: u32>(&self, bytes: &[u8], lb: u32, i: usize, x: usize) -> usize {
        let k = 1usize << D;
        let sup = self.supers + 4 * k * (i / super_len(D));
        let head = self.heads + 2 * k * (i >> lb);
        if x != 0 {
            return u32_at(bytes, sup + 4 * x) + u16_at(bytes, head + 2 * x);
        }
        let others: usize = (1..k)
            .map(|y| u32_at(bytes, sup + 4 * y) + u16_at(bytes, head + 2 * y))
            .sum();
        (i >> lb << lb).saturating_sub(others)
    }

    /// The kind word of block `i >> lb` and its payload's byte offset.
    #[inline]
    fn head<const D: u32>(&self, bytes: &[u8], lb: u32, i: usize) -> (u16, usize) {
        let k = 1usize << D;
        let word = u16_at(bytes, self.heads + 2 * k * (i >> lb)) as u16;
        let sup = self.supers + 4 * k * (i / super_len(D));
        (
            word,
            self.payload + u32_at(bytes, sup) + usize::from(word >> 2),
        )
    }

    /// Occurrences of digit `x` in `0..i`.
    #[inline]
    pub fn rank<const D: u32>(&self, bytes: &[u8], lb: u32, i: usize, x: usize) -> usize {
        let off = i & ((1 << lb) - 1);
        let base = self.before::<D>(bytes, lb, i, x);
        if off == 0 {
            return base;
        }
        let (word, at) = self.head::<D>(bytes, lb, i);
        base + match word & 3 {
            PLAIN => plain_rank::<D>(bytes, at, off, x),
            RUNS => runs::<D>(bytes, at, off, 1 << lb, Some(x)).1,
            _ => {
                if usize::from(word >> 2) == x {
                    off
                } else {
                    0
                }
            }
        }
    }

    /// The digit at `i` and its occurrences in `0..i`.
    #[inline]
    pub fn access_rank<const D: u32>(&self, bytes: &[u8], lb: u32, i: usize) -> (usize, usize) {
        let off = i & ((1 << lb) - 1);
        let (word, at) = self.head::<D>(bytes, lb, i);
        let (x, here) = match word & 3 {
            PLAIN => {
                let x = plain_digit::<D>(bytes, at, off);
                (x, plain_rank::<D>(bytes, at, off, x))
            }
            RUNS => runs::<D>(bytes, at, off, 1 << lb, None),
            _ => (usize::from(word >> 2) & ((1 << D) - 1), off),
        };
        (x, self.before::<D>(bytes, lb, i, x) + here)
    }
}

#[inline(always)]
fn plain_digit<const D: u32>(bytes: &[u8], at: usize, off: usize) -> usize {
    let per = 64 / D as usize;
    let w = u64_at(bytes, at + off / per * 8);
    (w >> (off % per * D as usize)) as usize & ((1 << D) - 1)
}

#[inline(always)]
fn plain_rank<const D: u32>(bytes: &[u8], at: usize, off: usize, x: usize) -> usize {
    let per = 64 / D as usize;
    let mut n = 0;
    for w in 0..off / per {
        n += equal::<D>(u64_at(bytes, at + 8 * w), x).count_ones() as usize;
    }
    let rest = off % per * D as usize;
    if rest > 0 {
        let w = u64_at(bytes, at + off / per * 8);
        n += (equal::<D>(w, x) & ((1u64 << rest) - 1)).count_ones() as usize;
    }
    n
}

/// Decodes the runs of the block at byte `at` up to the one holding `off`:
/// the digit there and the occurrences of `want` (else of that digit)
/// before `off`.
#[inline(always)]
fn runs<const D: u32>(
    bytes: &[u8],
    at: usize,
    off: usize,
    block: usize,
    want: Option<usize>,
) -> (usize, usize) {
    let mask = (1u64 << D) - 1;
    let mut pos = at * 8;
    let mut window = u64_at(bytes, at);
    let mut avail = 64u32;
    let mut digit = 0usize;
    let mut seen = [0usize; 16];
    let mut start = 0usize;
    let mut first = true;
    loop {
        if avail < 40 {
            window = u64_at(bytes, pos / 8) >> (pos % 8);
            avail = 64 - (pos % 8) as u32;
        }
        if first || D > 1 {
            digit = (window & mask) as usize;
            window >>= D;
            avail -= D;
            pos += D as usize;
        } else {
            digit ^= 1;
        }
        first = false;
        let z = window.trailing_zeros().min(15);
        let len = (1usize << z) | ((window >> (z + 1)) as usize & ((1 << z) - 1));
        let used = 2 * z + 1;
        window >>= used;
        avail -= used;
        pos += used as usize;
        let end = start + len;
        if end > off || end >= block {
            let x = want.unwrap_or(digit);
            let here = if digit == x {
                off.saturating_sub(start)
            } else {
                0
            };
            return (digit, seen[x & 15] + here);
        }
        seen[digit] += len;
        start = end;
    }
}

/// Appends bit fields to a byte vector, least significant bit first.
struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    held: u32,
}

impl BitWriter<'_> {
    /// Appends the low `n <= 32` bits of `v`.
    #[inline]
    fn put(&mut self, v: u64, n: u32) {
        self.acc |= v << self.held;
        self.held += n;
        while self.held >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.held -= 8;
        }
    }

    /// Appends the Elias-gamma code of `x >= 1`: `z` zeros, a one, the low
    /// `z` bits of `x`.
    #[inline]
    fn gamma(&mut self, x: usize) {
        let z = x.ilog2();
        let code = ((x as u64 & ((1 << z) - 1)) << 1 | 1) << z;
        self.put(code, 2 * z + 1);
    }

    /// Flushes the last partial byte.
    fn finish(self) {
        if self.held > 0 {
            self.out.push(self.acc as u8);
        }
    }
}

/// Appends `block`, digits of `d` bits at bit `shift` of each byte, to
/// `payload`; returns its kind word (offset not yet added) and its count per
/// digit. `runs` is scratch.
fn encode_block(
    block: &[u8],
    d: u32,
    shift: u32,
    payload: &mut Vec<u8>,
    runs: &mut Vec<(u8, usize)>,
) -> (u16, [usize; 16]) {
    let mask = (1u8 << d) - 1;
    let digit = |b: u8| b >> shift & mask;
    let mut counts = [0usize; 16];
    runs.clear();
    let mut from = 0;
    while from < block.len() {
        let x = digit(block[from]);
        let len = block[from..]
            .iter()
            .position(|&b| digit(b) != x)
            .unwrap_or(block.len() - from);
        counts[usize::from(x)] += len;
        runs.push((x, len));
        from += len;
    }
    if let [(x, _)] = runs[..] {
        return (ONE | u16::from(x) << 2, counts);
    }
    let digit_bits = if d == 1 { 1 } else { d as usize * runs.len() };
    let gamma: usize = runs.iter().map(|&(_, r)| 2 * r.ilog2() as usize + 1).sum();
    let plain = (block.len() * d as usize).div_ceil(8);
    let at = payload.len();
    let kind = if (digit_bits + gamma).div_ceil(8) < plain {
        let mut bits = BitWriter {
            out: payload,
            acc: 0,
            held: 0,
        };
        for (j, &(x, r)) in runs.iter().enumerate() {
            if j == 0 || d > 1 {
                bits.put(u64::from(x), d);
            }
            bits.gamma(r);
        }
        bits.finish();
        RUNS
    } else {
        payload.resize(at + plain, 0);
        let per = 8 / d as usize;
        for (dst, bytes) in payload[at..].iter_mut().zip(block.chunks(per)) {
            *dst = bytes
                .iter()
                .enumerate()
                .fold(0, |acc, (j, &b)| acc | digit(b) << (j * d as usize));
        }
        PLAIN
    };
    (kind, counts)
}

/// The supers, heads and payload of the `d`-bit digits at bit `shift` of
/// each of `seq` in blocks of `1 << lb`, one job per super on the current
/// rayon pool; the payload ends in 8 zero bytes.
pub fn encode(seq: &[u8], d: u32, shift: u32, lb: u32) -> Vec<u8> {
    let (len, k, block) = (seq.len(), 1usize << d, 1usize << lb);
    let span = super_len(d);
    // Per super: its heads, its payload and its count per digit.
    let mut units = vec![(Vec::new(), Vec::new(), [0usize; 16]); len.div_ceil(span)];
    each(
        units.iter_mut().enumerate(),
        |(s, (heads, payload, seen))| {
            let start = s * span;
            let mut runs = Vec::new();
            for from in (start..(start + span).min(len)).step_by(block) {
                let to = (from + block).min(len);
                let at = payload.len();
                let (kind, counts) = encode_block(&seq[from..to], d, shift, payload, &mut runs);
                let word = if kind & 3 == ONE {
                    kind
                } else {
                    kind | (at as u16) << 2
                };
                heads.extend_from_slice(&word.to_le_bytes());
                for &c in &seen[1..k] {
                    heads.extend_from_slice(&(c as u16).to_le_bytes());
                }
                for (s, c) in seen.iter_mut().zip(counts) {
                    *s += c;
                }
            }
        },
    );
    let (sb, hb) = index_bytes(len, d, lb);
    let mut out = Vec::with_capacity(sb + hb + len * d as usize / 32);
    let (mut before, mut payload) = ([0usize; 16], 0usize);
    for s in 0..sb / (4 * k) {
        out.extend_from_slice(&(payload as u32).to_le_bytes());
        for &c in &before[1..k] {
            out.extend_from_slice(&(c as u32).to_le_bytes());
        }
        if let Some((_, unit, seen)) = units.get(s) {
            payload += unit.len();
            for (b, c) in before.iter_mut().zip(seen) {
                *b += c;
            }
        }
    }
    for (heads, _, _) in &units {
        out.extend_from_slice(heads);
    }
    // The block at `len`, if a whole one inside a super: empty, after the
    // super's digits.
    if len.is_multiple_of(block) && !len.is_multiple_of(span) {
        out.extend_from_slice(&ONE.to_le_bytes());
        let seen = units.last().map_or([0; 16], |u| u.2);
        for &c in &seen[1..k] {
            out.extend_from_slice(&(c as u16).to_le_bytes());
        }
    }
    out.resize(sb + hb, 0);
    for (_, unit, _) in &units {
        out.extend_from_slice(unit);
    }
    out.extend_from_slice(&[0; 8]);
    out
}

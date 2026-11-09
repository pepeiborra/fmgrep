//! Marked rows as Elias-Fano. Of `count` marks over `rows` rows, each keeps
//! its low `low` bits, `low = floor(log2(rows / count))`, packed; the high
//! parts are unary over `rows >> low` + 1 buckets: mark `i` sets bit
//! `(row >> low) + i`, and each bucket ends in a zero. Three sections of
//! little-endian words, then padding to 64 bytes:
//!
//! - the low bits, `count * low` bits from bit 0;
//! - the high bits, `count + buckets` bits;
//! - a u32 position of every `SAMPLE`-th zero of the high bits.
//!
//! Reads clamp, so no section content can index past it or loop past its
//! bits.

/// Zeros per select sample.
const SAMPLE: usize = 64;

/// Where the sections of `count` marks over `rows` rows start, from 0.
#[derive(Clone, Copy, Debug)]
struct Shape {
    low: u32,
    highs_at: usize,
    high_bits: usize,
    samples_at: usize,
    len: usize,
}

impl Shape {
    fn new(rows: usize, count: usize) -> Self {
        let low = rows
            .checked_div(count)
            .map_or(0, |gap| gap.max(1).ilog2().min(32));
        let buckets = (rows >> low) + 1;
        let high_bits = count + buckets;
        let highs_at = 8 * (count * low as usize).div_ceil(64);
        let samples_at = highs_at + 8 * high_bits.div_ceil(64);
        let len = (samples_at + 4 * (buckets / SAMPLE + 1)).next_multiple_of(64);
        Self {
            low,
            highs_at,
            high_bits,
            samples_at,
            len,
        }
    }
}

pub fn section_bytes(rows: usize, count: usize) -> usize {
    Shape::new(rows, count).len
}

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

/// `width <= 57` bits of `b` from bit `at`.
#[inline]
pub fn bits_at(b: &[u8], at: usize, width: u32) -> u64 {
    (u64_at(b, at / 8) >> (at % 8)) & ((1u64 << width) - 1)
}

/// Entry `byte * 8 + r`: the position of set bit `r` of `byte`, 8 if none.
const SELECT_IN_BYTE: [u8; 2048] = {
    let mut t = [8u8; 2048];
    let mut byte = 0;
    while byte < 256 {
        let (mut bit, mut r) = (0, 0);
        while bit < 8 {
            if byte >> bit & 1 == 1 {
                t[byte * 8 + r] = bit as u8;
                r += 1;
            }
            bit += 1;
        }
        byte += 1;
    }
    t
};

/// Position of set bit `r < popcount(w)` of `w`: the byte holding it from
/// byte-wise prefix popcounts, then a table.
#[inline]
pub fn select_in_word(w: u64, r: usize) -> usize {
    const L8: u64 = 0x0101_0101_0101_0101;
    let mut s = w - (w >> 1 & 0x5555_5555_5555_5555);
    s = (s & 0x3333_3333_3333_3333) + (s >> 2 & 0x3333_3333_3333_3333);
    s = (s + (s >> 4)) & 0x0f0f_0f0f_0f0f_0f0f;
    // Byte `b` of `prefix`: set bits in bytes `0..=b`.
    let prefix = s.wrapping_mul(L8);
    // Bytes whose prefix is at most `r`: the target byte is the next one.
    let r8 = (r as u64) * L8;
    let le = ((r8 | 0x8080_8080_8080_8080) - prefix) & 0x8080_8080_8080_8080;
    let byte = (le.count_ones() as usize).min(7);
    let before = if byte == 0 {
        0
    } else {
        (prefix >> (8 * byte - 8) & 0xff) as usize
    };
    let in_byte = (w >> (8 * byte)) as usize & 0xff;
    8 * byte + usize::from(SELECT_IN_BYTE[in_byte * 8 + (r - before).min(7)])
}

#[derive(Clone, Copy)]
pub struct Marks<'a> {
    bytes: &'a [u8],
    shape: Shape,
    rows: usize,
    count: usize,
}

impl<'a> Marks<'a> {
    /// `bytes` holds exactly `section_bytes(rows, count)`.
    pub fn new(bytes: &'a [u8], rows: usize, count: usize) -> Self {
        debug_assert_eq!(bytes.len(), section_bytes(rows, count));
        Self {
            bytes,
            shape: Shape::new(rows, count),
            rows,
            count,
        }
    }

    /// Word `w` of the high bits.
    #[inline]
    fn high_word(&self, w: usize) -> u64 {
        u64_at(self.bytes, self.shape.highs_at + 8 * w)
    }

    #[inline]
    fn high(&self, p: usize) -> bool {
        p < self.shape.high_bits && self.high_word(p / 64) >> (p % 64) & 1 == 1
    }

    #[inline]
    fn low(&self, k: usize) -> u64 {
        let low = self.shape.low;
        if low == 0 {
            return 0;
        }
        bits_at(self.bytes, k * low as usize, low)
    }

    /// Position of zero `j` of the high bits, or their end.
    #[inline]
    fn select0(&self, j: usize) -> usize {
        let s = self.shape;
        let sample = j / SAMPLE;
        let at = s.samples_at + 4 * sample;
        let mut pos = match self.bytes.get(at..at + 4) {
            Some(w) => u32::from_le_bytes(w.try_into().expect("4 bytes")) as usize,
            None => return s.high_bits,
        };
        let mut left = j - sample * SAMPLE;
        while pos < s.high_bits {
            let mut zeros = !(self.high_word(pos / 64) >> (pos % 64));
            let span = (64 - pos % 64).min(s.high_bits - pos);
            if span < 64 {
                zeros &= (1u64 << span) - 1;
            }
            let here = zeros.count_ones() as usize;
            if left < here {
                return pos + select_in_word(zeros, left);
            }
            left -= here;
            pos += span;
        }
        s.high_bits
    }

    /// Marks before `row` and the high bit of the first at or past it.
    #[inline]
    fn seek(&self, row: usize) -> (usize, usize) {
        let s = self.shape;
        let h = row >> s.low;
        let mut p = if h == 0 { 0 } else { self.select0(h - 1) + 1 };
        let mut k = p.saturating_sub(h).min(self.count);
        let want = (row & ((1usize << s.low) - 1)) as u64;
        while k < self.count && self.high(p) && self.low(k) < want {
            k += 1;
            p += 1;
        }
        (k, p)
    }

    /// The marks the high bits hold, for checking against the header.
    pub fn total(&self) -> usize {
        (0..self.shape.high_bits.div_ceil(64))
            .map(|w| {
                let mut word = self.high_word(w);
                let span = self.shape.high_bits - 64 * w;
                if span < 64 {
                    word &= (1u64 << span) - 1;
                }
                word.count_ones() as usize
            })
            .sum()
    }

    /// Whether `row` is marked, and the marks before it; `row` clamps to
    /// `rows`.
    #[inline]
    pub fn get_rank(&self, row: usize) -> (bool, usize) {
        let row = row.min(self.rows);
        let (k, p) = self.seek(row);
        let want = (row & ((1usize << self.shape.low) - 1)) as u64;
        (
            row < self.rows && k < self.count && self.high(p) && self.low(k) == want,
            k,
        )
    }

    /// `(row, marks before it)` of each marked row in `lo..hi`, ascending;
    /// both clamp to `rows`.
    pub fn range(&self, lo: usize, hi: usize) -> impl Iterator<Item = (usize, usize)> + 'a {
        let marks = *self;
        let hi = hi.min(self.rows);
        let (mut k, mut p) = self.seek(lo.min(self.rows));
        std::iter::from_fn(move || {
            while k < marks.count && p < marks.shape.high_bits {
                if !marks.high(p) {
                    p += 1;
                    continue;
                }
                let row = ((p - k) << marks.shape.low) | marks.low(k) as usize;
                // Below `lo` only in a corrupt blob: stop there too, a miscount.
                if row >= hi || row < lo {
                    k = marks.count;
                    return None;
                }
                k += 1;
                p += 1;
                return Some((row, k - 1));
            }
            None
        })
    }
}

/// Fills `out` with the rows set in `bits` (bits from `rows` on ignored);
/// returns their count. `out` must be `section_bytes(rows, count)` bytes.
pub fn write(out: &mut [u8], rows: usize, bits: &[u64]) -> usize {
    let word = |w: usize| {
        let x = bits.get(w).copied().unwrap_or(0);
        let end = rows - 64 * w;
        if end < 64 { x & ((1u64 << end) - 1) } else { x }
    };
    let count: usize = (0..rows.div_ceil(64))
        .map(|w| word(w).count_ones() as usize)
        .sum();
    let s = Shape::new(rows, count);
    debug_assert_eq!(out.len(), s.len);
    let mask = (1u64 << s.low) - 1;
    let mut marked = (0..rows.div_ceil(64)).flat_map(|w| {
        let mut x = word(w);
        std::iter::from_fn(move || {
            (x != 0).then(|| {
                let row = 64 * w + x.trailing_zeros() as usize;
                x &= x - 1;
                row
            })
        })
    });
    pack(
        &mut out[..s.highs_at],
        marked.clone().map(|row| row as u64 & mask),
        s.low,
    );
    // High bits a word at a time: mark `k` at `(row >> low) + k`.
    let mut high_words = vec![0u64; s.high_bits.div_ceil(64)];
    for (k, row) in marked.by_ref().enumerate() {
        let p = (row >> s.low) + k;
        high_words[p / 64] |= 1 << (p % 64);
    }
    for (dst, w) in out[s.highs_at..s.samples_at].chunks_mut(8).zip(&high_words) {
        dst.copy_from_slice(&w.to_le_bytes());
    }
    // The position of every `SAMPLE`-th zero.
    let mut zeros = 0usize;
    for (w, &word) in high_words.iter().enumerate() {
        let span = (s.high_bits - 64 * w).min(64);
        let mut free = !word;
        if span < 64 {
            free &= (1u64 << span) - 1;
        }
        let here = free.count_ones() as usize;
        let mut next = zeros.next_multiple_of(SAMPLE);
        while next < zeros + here {
            let p = 64 * w + select_in_word(free, next - zeros);
            let at = s.samples_at + 4 * (next / SAMPLE);
            out[at..at + 4].copy_from_slice(&(p as u32).to_le_bytes());
            next += SAMPLE;
        }
        zeros += here;
    }
    count
}

/// Writes `values`, each below `1 << width` (`width <= 32`), into `out` as
/// consecutive `width`-bit fields from bit 0, little-endian.
pub fn pack(out: &mut [u8], values: impl Iterator<Item = u64>, width: u32) {
    if width == 0 {
        return;
    }
    let (mut acc, mut held, mut at) = (0u64, 0u32, 0usize);
    for v in values {
        acc |= v << held;
        held += width;
        while held >= 8 {
            out[at] = acc as u8;
            (acc, held, at) = (acc >> 8, held - 8, at + 1);
        }
    }
    if held > 0 {
        out[at] = acc as u8;
    }
}

/// The low bits per mark and where the high bits start, for tests that
/// tamper with them.
#[cfg(test)]
pub fn layout_of(rows: usize, count: usize) -> (u32, usize) {
    let s = Shape::new(rows, count);
    (s.low, s.highs_at)
}

/// Where the select samples start and how many, for tests that tamper with
/// them.
#[cfg(test)]
pub fn samples_of(rows: usize, count: usize) -> (usize, usize) {
    let s = Shape::new(rows, count);
    (s.samples_at, ((rows >> s.low) + 1) / SAMPLE + 1)
}

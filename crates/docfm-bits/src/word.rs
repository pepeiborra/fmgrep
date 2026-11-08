//! Word-level primitives, with no structure of their own: little-endian
//! reads that clamp to the bytes given, slot equality, the bit writer,
//! Elias-gamma codes, packed bit fields and select in a word.
//!
//! Private to the crate: several of these hold only under a precondition
//! their callers in the layers above establish.

/// The u16 at byte `at`, zero if it does not fit.
#[inline]
pub(crate) fn u16_at(b: &[u8], at: usize) -> usize {
    match b.get(at..at.wrapping_add(2)) {
        Some(w) => usize::from(u16::from_le_bytes(w.try_into().expect("2 bytes"))),
        None => 0,
    }
}

/// The u32 at byte `at`, zero if it does not fit.
#[inline]
pub(crate) fn u32_at(b: &[u8], at: usize) -> usize {
    match b.get(at..at.wrapping_add(4)) {
        Some(w) => u32::from_le_bytes(w.try_into().expect("4 bytes")) as usize,
        None => 0,
    }
}

/// The 64 bits from byte `at`, zero past the end.
#[inline]
pub(crate) fn u64_at(b: &[u8], at: usize) -> u64 {
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

/// One bit at the bottom of each `D`-bit slot of `w` that holds `x`. XOR with
/// `x` in every slot zeroes the slots that match, NOT makes them all ones, and
/// ANDing each slot's bits together leaves its bottom bit set only then. The
/// final mask drops what shifted in from the slot above.
#[inline(always)]
pub(crate) fn equal<const D: u32>(w: u64, x: usize) -> u64 {
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

/// Appends bit fields to a byte vector, least significant bit first.
pub(crate) struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    held: u32,
}

impl<'a> BitWriter<'a> {
    /// Appends after the last byte of `out`.
    #[inline]
    pub(crate) fn new(out: &'a mut Vec<u8>) -> Self {
        Self {
            out,
            acc: 0,
            held: 0,
        }
    }

    /// Appends the low `n <= 32` bits of `v`.
    #[inline]
    pub(crate) fn put(&mut self, v: u64, n: u32) {
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
    pub(crate) fn gamma(&mut self, x: usize) {
        let z = x.ilog2();
        let code = ((x as u64 & ((1 << z) - 1)) << 1 | 1) << z;
        self.put(code, 2 * z + 1);
    }

    /// Flushes the last partial byte.
    #[inline]
    pub(crate) fn finish(self) {
        if self.held > 0 {
            self.out.push(self.acc as u8);
        }
    }
}

/// The value of the Elias-gamma code at the bottom of `window` and the bits
/// it takes, at most 31; more than 15 zeros read as 15.
#[inline(always)]
pub(crate) fn gamma_decode(window: u64) -> (usize, u32) {
    let z = window.trailing_zeros().min(15);
    let len = (1usize << z) | ((window >> (z + 1)) as usize & ((1 << z) - 1));
    (len, 2 * z + 1)
}

/// `width <= 57` bits of `b` from bit `at`.
#[inline]
pub(crate) fn bits_at(b: &[u8], at: usize, width: u32) -> u64 {
    (u64_at(b, at / 8) >> (at % 8)) & ((1u64 << width) - 1)
}

/// Writes `values`, each below `1 << width` (`width <= 32`), into `out` as
/// consecutive `width`-bit fields from bit 0, little-endian.
pub(crate) fn pack(out: &mut [u8], values: impl Iterator<Item = u64>, width: u32) {
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
pub(crate) fn select_in_word(w: u64, r: usize) -> usize {
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

#[cfg(test)]
mod tests;

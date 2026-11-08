//! Integers of one width, packed end to end from bit 0 of their first byte,
//! little-endian.

use crate::word::{bits_at, pack};

/// The field width of a packed sequence: 1 to 32 bits, which its reads can
/// serve at any bit offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packed {
    width: u32,
}

impl Packed {
    /// The width of values below `count`, at least one bit.
    #[inline]
    pub fn for_values(count: u32) -> Self {
        Self {
            width: (u32::BITS - count.saturating_sub(1).leading_zeros()).max(1),
        }
    }

    /// Bits per value.
    #[inline]
    pub fn width(self) -> u32 {
        self.width
    }

    /// Bytes of `count` values, padded to a multiple of 8.
    #[inline]
    pub fn section_bytes(self, count: usize) -> usize {
        8 * (count * self.width as usize).div_ceil(64)
    }

    /// Writes `values` from the start of `out`, each cut to the width.
    #[inline]
    pub fn write(self, out: &mut [u8], values: impl Iterator<Item = u64>) {
        let mask = (1u64 << self.width) - 1;
        pack(out, values.map(|v| v & mask), self.width);
    }

    /// Value `k` of the sequence starting at byte `from` of `bytes`, zero
    /// past the end.
    #[inline]
    pub fn get(self, bytes: &[u8], from: usize, k: usize) -> u64 {
        bits_at(bytes, 8 * from + k * self.width as usize, self.width)
    }
}

#[cfg(test)]
mod tests;

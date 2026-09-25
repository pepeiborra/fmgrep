//! Sparse marks over `rows` rows: `[u32; rows / BUCKET + 2]` marks before
//! each `BUCKET`-row bucket (the last is the total), then a u16 in-bucket
//! offset per marked row in row order, padded to 64 bytes. Reads clamp, so
//! no section content can index past it.

pub const BUCKET: usize = 4096;

pub fn section_bytes(rows: usize, marks: usize) -> usize {
    (4 * dir_len(rows) + 2 * marks).next_multiple_of(64)
}

fn dir_len(rows: usize) -> usize {
    rows / BUCKET + 2
}

#[derive(Clone, Copy)]
pub struct Marks<'a> {
    bytes: &'a [u8],
    rows: usize,
    count: usize,
}

impl<'a> Marks<'a> {
    /// `bytes` holds exactly `section_bytes(rows, count)`.
    pub fn new(bytes: &'a [u8], rows: usize, count: usize) -> Self {
        debug_assert_eq!(bytes.len(), section_bytes(rows, count));
        Self { bytes, rows, count }
    }

    fn dir(&self, k: usize) -> usize {
        u32::from_le_bytes(*self.bytes[4 * k..].first_chunk().expect("in the directory")) as usize
    }

    /// The directory's total, unclamped.
    pub fn total(&self) -> usize {
        self.dir(dir_len(self.rows) - 1)
    }

    /// Whether `row` is marked, and the marks before it; `row` clamps to
    /// `rows`.
    #[inline]
    pub fn get_rank(&self, row: usize) -> (bool, usize) {
        let row = row.min(self.rows);
        let lo = self.dir(row / BUCKET).min(self.count);
        let hi = self.dir(row / BUCKET + 1).clamp(lo, self.count);
        let at = 4 * dir_len(self.rows);
        let offsets = self.bytes[at + 2 * lo..at + 2 * hi].as_chunks::<2>().0;
        let want = (row % BUCKET) as u16;
        let k = offsets.partition_point(|o| u16::from_le_bytes(*o) < want);
        (
            offsets
                .get(k)
                .is_some_and(|o| u16::from_le_bytes(*o) == want),
            lo + k,
        )
    }
}

/// Fills `out` with the rows set in `bits` (bits from `rows` on ignored);
/// returns their count. `out` must be `section_bytes(rows, count)` bytes.
pub fn write(out: &mut [u8], rows: usize, bits: &[u64]) -> usize {
    let (dir, offsets) = out.split_at_mut(4 * dir_len(rows));
    let mut offsets = offsets.as_chunks_mut::<2>().0.iter_mut();
    let mut count = 0usize;
    for (k, d) in dir.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        d.copy_from_slice(&(count as u32).to_le_bytes());
        let end = ((k + 1) * BUCKET).min(rows);
        for w in k * BUCKET / 64..end.div_ceil(64) {
            let mut word = bits.get(w).copied().unwrap_or(0);
            if end < w * 64 + 64 {
                word &= (1 << (end - w * 64)) - 1;
            }
            while word != 0 {
                let row = w * 64 + word.trailing_zeros() as usize;
                *offsets.next().expect("sized for the marks") =
                    ((row % BUCKET) as u16).to_le_bytes();
                count += 1;
                word &= word - 1;
            }
        }
    }
    count
}

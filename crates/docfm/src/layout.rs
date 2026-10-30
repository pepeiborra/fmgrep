//! Blob v7, all little-endian. The occurrence blocks start on a 16 KiB
//! boundary and the blob length is a multiple of 16 KiB, so blobs laid end
//! to end from a page-aligned offset keep every block page-aligned.
//!
//! | offset | field |
//! |---|---|
//! | 0 | magic `DOCFM\0\x07\0` |
//! | 8 | `n` u64: text bytes; rows are `n + 1` |
//! | 16 | sentinel row u64 |
//! | 24 | `max` gap u32, sigma u32 (alphabet popcount) |
//! | 32 | alphabet `[u64; 4]` |
//! | 64 | positions per occurrence block u32 |
//! | 68 | scheme u32 = 1; gaps `min`, `max`, `ws` u32 |
//! | 84 | walk bound u32 = `ws - 1`; 88..96 reserved zero |
//! | 96 | `c` `[u32; 257]`: first row per code, `c[sigma] = n + 1` |
//! | 1124 | reserved zero |
//! | 2148 | `num_docs` u32, `num_samples` u32; 2156..2176 reserved zero |
//! | 2176 | supers, then at the next 16 KiB boundary the occurrence blocks (`occ`) |
//! | | marks (`marks`) over `n + 1` rows |
//! | | samples: u32 doc id per marked row, in row order |
//!
//! A row is marked iff its text position `p < n` starts a document, or
//! starts a word, subword or line at least `min` past the previous mark, or
//! is `max` past it (`ws` on whitespace); see `build::doc_samples`.

use crate::marks::{Marks, section_bytes};
use crate::occ::{BLOCK, Occ, SUPER};
use crate::{DOC_END, Error, MAX_TEXT_BYTES};

const MAGIC: [u8; 8] = *b"DOCFM\0\x07\0";
const SCHEME: u32 = 1;
const BLOCK_AT: usize = 64;
const SCHEME_AT: usize = 68;
const RESERVED_END: usize = 96;
const C_AT: usize = 96;
const DOCS_AT: usize = 2148;
pub const HEADER_BYTES: usize = 2176;
pub const PAGE: usize = 16384;

/// Sampling gaps, in text bytes past the previous sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gaps {
    /// Least gap before a word, subword or line start.
    pub min: u32,
    /// Gap forcing a sample.
    pub max: u32,
    /// Gap forcing a sample on whitespace.
    pub ws: u32,
}

impl Gaps {
    /// What every build samples at.
    pub const WORDS: Self = Self {
        min: 8,
        max: 16,
        ws: 64,
    };

    /// Most LF steps from a row to a marked one.
    pub fn walk_bound(self) -> u32 {
        self.ws - 1
    }

    pub fn check(self) -> bool {
        1 <= self.min && self.min <= self.max && self.max <= self.ws
    }
}

/// The parsed header plus the section offsets it implies.
pub struct Header {
    pub n: usize,
    pub sentinel_row: usize,
    pub sampling: Gaps,
    pub alphabet: [u64; 4],
    pub code_of: [Option<u8>; 256],
    pub sigma: usize,
    /// Bit planes per occurrence block.
    pub bits: usize,
    pub c: [u32; 257],
    pub super_stride: usize,
    pub counts_bytes: usize,
    pub block_stride: usize,
    pub blocks_at: usize,
    pub marks_at: usize,
    pub samples_at: usize,
    pub num_docs: usize,
    pub num_samples: usize,
    pub blob_len: usize,
}

/// Planes needed for `sigma` codes; one code needs none.
pub fn bits_for(sigma: usize) -> usize {
    (usize::BITS - sigma.saturating_sub(1).leading_zeros()) as usize
}

fn corrupt(why: String) -> Error {
    Error::Corrupt(why)
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(*b[at..].first_chunk().expect("in bounds"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(*b[at..].first_chunk().expect("in bounds"))
}

/// The gaps of a header with this version's magic.
fn gaps_at(b: &[u8]) -> Result<Gaps, Error> {
    let field = |k: usize| u32_at(b, SCHEME_AT + 4 * k);
    let gaps = Gaps {
        min: field(1),
        max: field(2),
        ws: field(3),
    };
    let tail_zero = b[SCHEME_AT + 20..RESERVED_END].iter().all(|&x| x == 0);
    let max = u32_at(b, 24);
    if field(0) != SCHEME
        || !gaps.check()
        || gaps.max != max
        || field(4) != gaps.walk_bound()
        || !tail_zero
    {
        return Err(corrupt(format!(
            "v7 sampling fields {:?} with max gap {max}",
            [0, 1, 2, 3, 4].map(field)
        )));
    }
    Ok(gaps)
}

impl Header {
    pub fn new(
        n: usize,
        sentinel_row: usize,
        sampling: Gaps,
        alphabet: [u64; 4],
        c: [u32; 257],
        num_docs: usize,
        num_samples: usize,
    ) -> Self {
        let mut code_of = [None; 256];
        let mut sigma = 0;
        for (byte, code) in code_of.iter_mut().enumerate() {
            if alphabet[byte / 64] >> (byte % 64) & 1 == 1 {
                *code = Some(sigma as u8);
                sigma += 1;
            }
        }
        let bits = bits_for(sigma);
        let super_stride = (4 * sigma.max(1)).next_multiple_of(64);
        let counts_bytes = (2 * sigma.max(1)).next_multiple_of(64);
        let block_stride = counts_bytes + BLOCK / 8 * bits;
        let blocks_at = (HEADER_BYTES + (n / SUPER + 1) * super_stride).next_multiple_of(PAGE);
        let marks_at = blocks_at + (n / BLOCK + 1) * block_stride;
        let samples_at = marks_at + section_bytes(n + 1, num_samples);
        Self {
            n,
            sentinel_row,
            sampling,
            alphabet,
            code_of,
            sigma,
            bits,
            c,
            super_stride,
            counts_bytes,
            block_stride,
            blocks_at,
            marks_at,
            samples_at,
            num_docs,
            num_samples,
            blob_len: (samples_at + num_samples * 4).next_multiple_of(PAGE),
        }
    }

    /// Most LF steps from a row to a marked one.
    pub fn walk_bound(&self) -> usize {
        self.sampling.walk_bound() as usize
    }

    /// Deepest forward tail before the cursor's caps: deeper ones scan more
    /// marks than they resolve.
    pub fn depth_limit(&self) -> usize {
        (self.sampling.max as usize / 2).saturating_sub(1)
    }

    /// Text bytes per mark as `num / den`.
    pub fn density(&self) -> (u64, u64) {
        (self.n as u64, self.num_samples as u64)
    }

    pub fn marks<'a>(&self, bytes: &'a [u8]) -> Marks<'a> {
        Marks::new(
            &bytes[self.marks_at..self.samples_at],
            self.n + 1,
            self.num_samples,
        )
    }

    /// Sample `k`, clamped to the last; `num_samples` must be positive.
    pub fn sample(&self, bytes: &[u8], k: usize) -> usize {
        u32_at(bytes, self.samples_at + 4 * k.min(self.num_samples - 1)) as usize
    }

    pub fn write(&self, out: &mut [u8]) {
        out[..8].copy_from_slice(&MAGIC);
        let Gaps { min, max, ws } = self.sampling;
        let fields = [SCHEME, min, max, ws, self.sampling.walk_bound()];
        for (dst, v) in out[SCHEME_AT..]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(fields)
        {
            dst.copy_from_slice(&v.to_le_bytes());
        }
        out[8..16].copy_from_slice(&(self.n as u64).to_le_bytes());
        out[16..24].copy_from_slice(&(self.sentinel_row as u64).to_le_bytes());
        out[24..28].copy_from_slice(&self.sampling.max.to_le_bytes());
        out[28..32].copy_from_slice(&(self.sigma as u32).to_le_bytes());
        for (dst, w) in out[32..64]
            .as_chunks_mut::<8>()
            .0
            .iter_mut()
            .zip(self.alphabet)
        {
            dst.copy_from_slice(&w.to_le_bytes());
        }
        out[BLOCK_AT..BLOCK_AT + 4].copy_from_slice(&(BLOCK as u32).to_le_bytes());
        for (dst, v) in out[C_AT..].as_chunks_mut::<4>().0.iter_mut().zip(&self.c) {
            dst.copy_from_slice(&v.to_le_bytes());
        }
        out[DOCS_AT..DOCS_AT + 4].copy_from_slice(&(self.num_docs as u32).to_le_bytes());
        out[DOCS_AT + 4..DOCS_AT + 8].copy_from_slice(&(self.num_samples as u32).to_le_bytes());
    }

    pub fn parse(b: &[u8]) -> Result<Self, Error> {
        if b.len() < MAGIC.len() {
            return Err(corrupt("truncated".into()));
        }
        if b[..8] != MAGIC {
            return Err(corrupt(format!(
                "magic/version {:?}, want {MAGIC:?}",
                &b[..8]
            )));
        }
        if b.len() < HEADER_BYTES {
            return Err(corrupt("truncated".into()));
        }
        let block = u32_at(b, BLOCK_AT) as usize;
        if block != BLOCK {
            return Err(corrupt(format!(
                "{block} positions per block, want {BLOCK}"
            )));
        }
        let n = u64_at(b, 8);
        if n > MAX_TEXT_BYTES {
            return Err(corrupt(format!("text length {n} over the bound")));
        }
        let n = n as usize;
        let sentinel_row = u64_at(b, 16);
        if sentinel_row > n as u64 {
            return Err(corrupt(format!("sentinel row {sentinel_row} past {n}")));
        }
        let sampling = gaps_at(b)?;
        let alphabet = std::array::from_fn(|k| u64_at(b, 32 + 8 * k));
        let c = std::array::from_fn(|k| u32_at(b, C_AT + 4 * k));
        let num_docs = u32_at(b, DOCS_AT) as usize;
        let num_samples = u32_at(b, DOCS_AT + 4) as usize;
        if (num_docs == 0) != (n == 0) || num_docs > num_samples || num_samples > n {
            return Err(corrupt(format!(
                "{num_docs} docs and {num_samples} samples over {n} bytes"
            )));
        }
        let h = Self::new(
            n,
            sentinel_row as usize,
            sampling,
            alphabet,
            c,
            num_docs,
            num_samples,
        );
        let sigma = u32_at(b, 28) as usize;
        if sigma != h.sigma || (sigma == 0) != (n == 0) {
            return Err(corrupt(format!(
                "sigma {sigma}, alphabet of {} symbols, over {n} bytes",
                h.sigma
            )));
        }
        if b.len() < h.blob_len {
            return Err(corrupt("truncated".into()));
        }
        if b.len() > h.blob_len {
            return Err(corrupt("trailing bytes".into()));
        }
        let rows = h.c[..=sigma].iter().map(|&r| r as usize);
        if h.c[0] != 1
            || rows.clone().zip(rows.skip(1)).any(|(a, b)| a >= b)
            || h.c[sigma] as usize != n + 1
        {
            return Err(corrupt(format!(
                "symbol rows not increasing from 1 to {}",
                n + 1
            )));
        }
        if n > 0
            && (h.code_of[usize::from(DOC_END)] != Some(0)
                || (h.c[1] - h.c[0]) as usize != num_docs)
        {
            return Err(corrupt(format!(
                "{num_docs} docs but not as many DOC_END bytes"
            )));
        }
        let bwt = Occ::new(b, &h);
        for code in 0..sigma {
            let (got, want) = (
                bwt.rank(code as u8, n),
                (h.c[code + 1] - h.c[code]) as usize,
            );
            if got != want {
                return Err(corrupt(format!(
                    "code {code}: {got} occurrences, want {want}"
                )));
            }
        }
        let marks = h.marks(b);
        if marks.total() != h.num_samples {
            return Err(corrupt(format!(
                "{} marked rows for {} samples",
                marks.total(),
                h.num_samples
            )));
        }
        let (sentinel_marked, sentinel_rank) = marks.get_rank(h.sentinel_row);
        if n > 0 && !sentinel_marked {
            return Err(corrupt("the sentinel row is not sampled".into()));
        }
        if n > 0 && h.sample(b, sentinel_rank) != 0 {
            return Err(corrupt("the sentinel row is not in doc 0".into()));
        }
        Ok(h)
    }
}

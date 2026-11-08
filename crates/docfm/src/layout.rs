//! Blob v8, all little-endian. The blob length is a multiple of 16 KiB, so
//! blobs laid end to end from a page-aligned offset stay page-aligned. The
//! struct `OnDisk` is the first 2368 bytes of this table, field for field.
//!
//! | offset | field |
//! |---|---|
//! | 0 | magic `DOCFM\0\x08\0` |
//! | 8 | `n` u64: text bytes; rows are `n + 1` |
//! | 16 | sentinel row u64 |
//! | 24 | `max` gap u32, sigma u32 (alphabet popcount) |
//! | 32 | alphabet `[u64; 4]` |
//! | 64 | digits per wavelet block u32, a power of two in 256..=4096 |
//! | 68 | scheme u32 = 1; gaps `min`, `max`, `ws` u32 |
//! | 84 | walk bound u32 = `ws - 1`; 88..96 reserved zero |
//! | 96 | `c` `[u32; 257]`: first row per code, `c[sigma] = n + 1` |
//! | 1124 | `bottom` `[u32; 256]`: where each code starts below the last level, zero from sigma |
//! | 2148 | `num_docs` u32, `num_samples` u32; 2156..2176 reserved zero |
//! | 2176 | digit starts `[u32; 32]`: entry `l * k + x`, the positions of level `l` holding a digit below `x`, zero from `depth * k` |
//! | 2304 | payload bytes `[u32; 8]` of each level, padding included, zero from the depth |
//! | 2336 | bits per digit `d` u32: 1, 2 or 4 (`k = 1 << d`); 2340..2368 reserved zero |
//! | 2368 | levels (`wavelet`): each its supers, heads and payload (`digits`), 64-byte aligned |
//! | | marks (`marks`) over `n + 1` rows |
//! | | samples: doc id per marked row, in row order, `ceil(log2 num_docs)` bits each (at least 1), packed |
//!
//! A row is marked iff its text position `p < n` starts a document, or
//! starts a word, subword or line at least `min` past the previous mark, or
//! is `max` past it (`ws` on whitespace); see `build::doc_samples`.

use crate::marks::{Marks, section_bytes};
use crate::packed::Packed;
use crate::wavelet::{self, MAX_DEPTH, MAX_STARTS, Params, Shape, WaveletMatrix, depth_for};
use crate::{DOC_END, Error, MAX_TEXT_BYTES};
use zerocopy::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes};

const MAGIC: [u8; 8] = *b"DOCFM\0\x08\0";
const SCHEME: u32 = 1;
pub const HEADER_BYTES: usize = 2368;
pub const PAGE: usize = 16384;
/// log2 of the digits per wavelet block a build writes.
pub const BLOCK_LOG: u32 = 8;
/// Bits per wavelet digit a build writes.
pub const DIGIT_BITS: u32 = 4;

/// What a build writes: its sampling gaps and its wavelet shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub gaps: Gaps,
    /// Bits per wavelet digit: 1, 2 or 4.
    pub d: u32,
    /// log2 of the digits per wavelet block, 8..=12.
    pub lb: u32,
}

impl Config {
    /// What [`crate::FmIndex::build`] writes.
    pub const DEFAULT: Self = Self {
        gaps: Gaps::WORDS,
        d: DIGIT_BITS,
        lb: BLOCK_LOG,
    };
}

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
        min: 4,
        max: 8,
        ws: 32,
    };

    /// Most LF steps from a row to a marked one.
    pub fn walk_bound(self) -> u32 {
        self.ws - 1
    }

    pub fn check(self) -> bool {
        1 <= self.min && self.min <= self.max && self.max <= self.ws
    }
}

/// The wavelet levels as a build encoded them.
pub struct Levels {
    pub params: Params,
    pub starts: [u32; MAX_STARTS],
    /// Payload bytes of each level, padding included.
    pub payload: [u32; MAX_DEPTH],
    pub bottom: [u32; 256],
}

/// The parsed header plus the section offsets it implies.
pub struct Header {
    pub n: usize,
    pub sentinel_row: usize,
    pub sampling: Gaps,
    pub alphabet: [u64; 4],
    pub code_of: [Option<u8>; 256],
    pub sigma: usize,
    pub c: [u32; 257],
    pub payload: [u32; MAX_DEPTH],
    pub wavelet: Shape,
    pub marks_at: usize,
    pub samples_at: usize,
    /// The packed doc ids.
    pub ids: Packed,
    pub num_docs: usize,
    pub num_samples: usize,
    pub blob_len: usize,
}

fn corrupt(why: String) -> Error {
    Error::Corrupt(why)
}

/// The header as it lies on disk; the table above gives each field's meaning.
#[derive(FromBytes, IntoBytes, Immutable)]
#[repr(C)]
struct OnDisk {
    magic: [u8; 8],
    n: U64,
    sentinel_row: U64,
    max_gap: U32,
    sigma: U32,
    alphabet: [U64; 4],
    block: U32,
    scheme: U32,
    gap_min: U32,
    gap_max: U32,
    gap_ws: U32,
    walk_bound: U32,
    reserved_gaps: [u8; 8],
    c: [U32; 257],
    bottom: [U32; 256],
    num_docs: U32,
    num_samples: U32,
    reserved_docs: [u8; 20],
    starts: [U32; MAX_STARTS],
    payload: [U32; MAX_DEPTH],
    digit_bits: U32,
    reserved_levels: [u8; 28],
}

const _: () = assert!(size_of::<OnDisk>() == HEADER_BYTES);

/// The gaps of a header with this version's magic.
fn gaps_of(raw: &OnDisk) -> Result<Gaps, Error> {
    let fields = [
        raw.scheme,
        raw.gap_min,
        raw.gap_max,
        raw.gap_ws,
        raw.walk_bound,
    ]
    .map(U32::get);
    let [scheme, min, max, ws, walk_bound] = fields;
    let gaps = Gaps { min, max, ws };
    let max_gap = raw.max_gap.get();
    if scheme != SCHEME
        || !gaps.check()
        || gaps.max != max_gap
        || walk_bound != gaps.walk_bound()
        || raw.reserved_gaps != [0; 8]
    {
        return Err(corrupt(format!(
            "v8 sampling fields {fields:?} with max gap {max_gap}"
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
        (num_docs, num_samples): (usize, usize),
        levels: &Levels,
    ) -> Self {
        let mut code_of = [None; 256];
        let mut sigma = 0;
        for (byte, code) in code_of.iter_mut().enumerate() {
            if alphabet[byte / 64] >> (byte % 64) & 1 == 1 {
                *code = Some(sigma as u8);
                sigma += 1;
            }
        }
        let (wavelet, marks_at) = Shape::new(
            n,
            sigma,
            levels.params,
            levels.starts,
            levels.bottom,
            &levels.payload,
            HEADER_BYTES,
        );
        let samples_at = marks_at + section_bytes(n + 1, num_samples);
        let ids = Packed::for_values(num_docs as u32);
        Self {
            n,
            sentinel_row,
            sampling,
            alphabet,
            code_of,
            sigma,
            c,
            payload: levels.payload,
            wavelet,
            marks_at,
            samples_at,
            ids,
            num_docs,
            num_samples,
            blob_len: (samples_at + ids.section_bytes(num_samples)).next_multiple_of(PAGE),
        }
    }

    /// The BWT's wavelet levels in `bytes`.
    pub fn bwt<'a>(&'a self, bytes: &'a [u8]) -> WaveletMatrix<'a> {
        WaveletMatrix::new(bytes, &self.wavelet)
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

    /// Text bytes per mark as `num / den`, not counting the sentinel row's mark.
    pub fn density(&self) -> (u64, u64) {
        (
            self.n as u64,
            (self.num_samples - usize::from(self.n > 0)) as u64,
        )
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
        let k = k.min(self.num_samples - 1);
        self.ids.get(bytes, self.samples_at, k) as usize
    }

    pub fn write(&self, out: &mut [u8]) {
        let Gaps { min, max, ws } = self.sampling;
        let params = self.wavelet.params();
        let raw = OnDisk {
            magic: MAGIC,
            n: U64::new(self.n as u64),
            sentinel_row: U64::new(self.sentinel_row as u64),
            max_gap: U32::new(max),
            sigma: U32::new(self.sigma as u32),
            alphabet: self.alphabet.map(U64::new),
            block: U32::new(1 << params.lb()),
            scheme: U32::new(SCHEME),
            gap_min: U32::new(min),
            gap_max: U32::new(max),
            gap_ws: U32::new(ws),
            walk_bound: U32::new(self.sampling.walk_bound()),
            reserved_gaps: [0; 8],
            c: self.c.map(U32::new),
            bottom: self.wavelet.bottom().map(U32::new),
            num_docs: U32::new(self.num_docs as u32),
            num_samples: U32::new(self.num_samples as u32),
            reserved_docs: [0; 20],
            starts: self.wavelet.starts().map(U32::new),
            payload: self.payload.map(U32::new),
            digit_bits: U32::new(params.d()),
            reserved_levels: [0; 28],
        };
        out[..HEADER_BYTES].copy_from_slice(raw.as_bytes());
    }

    pub fn parse(b: &[u8]) -> Result<Self, Error> {
        let Some(magic) = b.get(..MAGIC.len()) else {
            return Err(corrupt("truncated".into()));
        };
        if magic != MAGIC {
            return Err(corrupt(format!("magic/version {magic:?}, want {MAGIC:?}")));
        }
        let Ok((raw, _)) = OnDisk::read_from_prefix(b) else {
            return Err(corrupt("truncated".into()));
        };
        let block = raw.block.get();
        if !block.is_power_of_two() || !(256..=4096).contains(&block) {
            return Err(corrupt(format!(
                "{block} digits per block, want a power of two in 256..=4096"
            )));
        }
        let d = raw.digit_bits.get();
        if ![1, 2, 4].contains(&d) {
            return Err(corrupt(format!("{d} bits per digit, want 1, 2 or 4")));
        }
        let params = Params::new(d, block.ilog2()).expect("checked above");
        let n = raw.n.get();
        if n > MAX_TEXT_BYTES {
            return Err(corrupt(format!("text length {n} over the bound")));
        }
        let n = n as usize;
        let sentinel_row = raw.sentinel_row.get();
        if sentinel_row > n as u64 {
            return Err(corrupt(format!("sentinel row {sentinel_row} past {n}")));
        }
        let sampling = gaps_of(&raw)?;
        let alphabet = raw.alphabet.map(U64::get);
        let c = raw.c.map(U32::get);
        let num_docs = raw.num_docs.get() as usize;
        let num_samples = raw.num_samples.get() as usize;
        // At most one sample per row, of which there are n + 1.
        if (num_docs == 0) != (n == 0) || num_docs > num_samples || num_samples > n + 1 {
            return Err(corrupt(format!(
                "{num_docs} docs and {num_samples} samples over {n} bytes"
            )));
        }
        let sigma = raw.sigma.get() as usize;
        let depth = depth_for(sigma.min(256), params);
        let k = 1usize << d;
        let levels = Levels {
            params,
            starts: raw.starts.map(U32::get),
            payload: raw.payload.map(U32::get),
            bottom: raw.bottom.map(U32::get),
        };
        let max_payload = wavelet::max_level_payload(n, params);
        for l in 0..MAX_DEPTH {
            let payload = levels.payload[l] as usize;
            let starts = levels.starts.get(l * k..(l + 1) * k).unwrap_or(&[]);
            let fits = if l < depth {
                (8..=max_payload).contains(&payload)
                    && starts[0] == 0
                    && starts.is_sorted()
                    && starts[k - 1] as usize <= n
            } else {
                payload == 0 && starts.iter().all(|&s| s == 0)
            };
            if !fits {
                return Err(corrupt(format!(
                    "level {l} of {depth}: digit starts {starts:?}, {payload} payload bytes"
                )));
            }
        }
        if levels.bottom.iter().enumerate().any(
            |(k, &at)| {
                if k < sigma { at as usize > n } else { at != 0 }
            },
        ) || levels.starts[depth * k..].iter().any(|&s| s != 0)
            || raw.reserved_docs != [0; 20]
            || raw.reserved_levels != [0; 28]
        {
            return Err(corrupt("code starts or reserved bytes".into()));
        }
        let h = Self::new(
            n,
            sentinel_row as usize,
            sampling,
            alphabet,
            c,
            (num_docs, num_samples),
            &levels,
        );
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
        let bwt = h.bwt(b);
        for l in 0..depth {
            for x in 0..k {
                let start = h.wavelet.starts()[l * k + x] as usize;
                let end = h.wavelet.starts().get(l * k + x + 1).filter(|_| x + 1 < k);
                let want = end.map_or(n, |&e| e as usize) - start;
                let got = bwt.level_rank(l, n, params.digit(x).expect("below k"));
                if got != want {
                    return Err(corrupt(format!(
                        "level {l}: {got} digits {x}, the starts say {want}"
                    )));
                }
            }
        }
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

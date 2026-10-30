//! A byte-alphabet FM-index over NUL-terminated documents: counts by
//! backward search, and the document of every occurrence from sampled
//! document ids. The index is one blob, queried in place from any
//! `AsRef<[u8]>` (a memory-mapped file included) without copying.
//!
//! Rows are the `n + 1` suffixes of the text in sorted order, a virtual
//! sentinel ending the text below every byte; row 0 is the empty suffix.
//!
//! ```
//! let index = docfm::FmIndex::build(b"hello world\0say hello\0".to_vec())?;
//! assert_eq!(index.cursor().extend_front(b"hello").count(), 2);
//!
//! let mut docs = Vec::new();
//! index.locate(b"hello", &mut |doc| docs.push(doc));
//! docs.sort();
//! assert_eq!(docs, [0, 1]);
//!
//! let bytes = index.into_bytes();
//! let view = docfm::FmIndex::view(bytes.as_slice())?;
//! assert_eq!(view.num_docs(), 2);
//! # Ok::<(), docfm::Error>(())
//! ```
//!
//! The blob layout is in the crate's `src/layout.rs`.

#![warn(missing_docs)]

mod build;
mod forward;
mod layout;
mod marks;
mod occ;
#[cfg(test)]
mod tests;

use crate::layout::{Gaps, Header};
use crate::occ::Occ;

/// Longest text [`FmIndex::build`] accepts: libsais's `i32` temporary array
/// and rows bound it.
pub const MAX_TEXT_BYTES: u64 = i32::MAX as u64;

/// Ends every document, the last included.
pub const DOC_END: u8 = 0;

/// An index over its blob bytes `D`, owned or borrowed from a mapping.
pub struct FmIndex<D> {
    bytes: D,
    header: Header,
}

/// Counters of one locate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocateStats {
    /// Occurrences visited.
    pub occurrences: u64,
    /// Occurrences resolved from a tail's sample.
    pub forward: u64,
    /// LF steps walked back to a sample, over all occurrences.
    pub walk_steps: u64,
    /// Longest walk.
    pub max_walk: u32,
    /// Tail marks the forward pass probed.
    pub probes: u64,
    /// Occurrences read from their own mark.
    pub zero_step: u64,
    /// Nanoseconds before the walk.
    pub forward_ns: u64,
    /// Nanoseconds in the walk.
    pub walk_ns: u64,
}

/// Why a build, a write or a view failed.
#[derive(Debug)]
pub enum Error {
    /// The text, of this many bytes, is over [`MAX_TEXT_BYTES`].
    TextTooLong(usize),
    /// The text does not end in [`DOC_END`].
    Unterminated,
    /// libsais refused the text.
    Construction(String),
    /// Writing the blob failed.
    Io(std::io::Error),
    /// The blob is not a docfm blob of this version, or fails its checks.
    Corrupt(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TextTooLong(n) => {
                write!(
                    f,
                    "text of {n} bytes exceeds the {MAX_TEXT_BYTES}-byte docfm bound"
                )
            }
            Self::Unterminated => write!(f, "docfm text must end in DOC_END (0x00)"),
            Self::Construction(e) => write!(f, "docfm construction failed: {e}"),
            Self::Io(e) => write!(f, "docfm blob I/O failed: {e}"),
            Self::Corrupt(why) => write!(f, "corrupt docfm blob: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl FmIndex<Vec<u8>> {
    /// `text` is documents, each followed by one [`DOC_END`]. Samples the doc
    /// id at document, word, subword and line starts, so a locate walks at
    /// most 63 LF steps. Runs on the current rayon pool; the blob does not
    /// depend on it.
    pub fn build(text: Vec<u8>) -> Result<Self, Error> {
        Self::view(build::build(text, Gaps::WORDS)?)
    }

    /// The blob, for storing and a later [`FmIndex::view`].
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl<D: AsRef<[u8]>> FmIndex<D> {
    /// Checks the header, the last occurrence block and the marks total;
    /// copies nothing. A blob consistent there but wrong elsewhere can
    /// miscount, and can panic only with "corrupt docfm index" in a locate.
    pub fn view(bytes: D) -> Result<Self, Error> {
        let header = Header::parse(bytes.as_ref())?;
        Ok(Self { bytes, header })
    }

    /// Write the blob to `out`.
    pub fn encode_into(&self, out: &mut dyn std::io::Write) -> Result<(), Error> {
        out.write_all(self.bytes.as_ref()).map_err(Error::Io)
    }

    /// Whether `byte` occurs in the text.
    pub fn contains_symbol(&self, byte: u8) -> bool {
        self.header.code_of[usize::from(byte)].is_some()
    }

    /// The empty query: every row.
    pub fn cursor(&self) -> Cursor<'_> {
        Cursor {
            view: View {
                bytes: self.bytes.as_ref(),
                header: &self.header,
            },
            lo: 0,
            hi: self.header.n + 1,
            root: true,
            tails: [(0, 0); RING],
            front: [0; RING],
            len: 0,
        }
    }

    /// Documents in the text: its [`DOC_END`] count.
    pub fn num_docs(&self) -> usize {
        self.header.num_docs
    }

    /// Visit the doc id of every occurrence of `pattern`.
    pub fn locate(&self, pattern: &[u8], visit: &mut dyn FnMut(usize)) {
        self.cursor().extend_front(pattern).locate(visit);
    }

    /// Bytes this index holds: the blob plus the parsed header.
    pub fn heap_bytes(&self) -> u64 {
        (self.bytes.as_ref().len() + std::mem::size_of::<Header>()) as u64
    }
}

#[derive(Clone, Copy)]
struct View<'a> {
    bytes: &'a [u8],
    header: &'a Header,
}

impl View<'_> {
    fn bwt(&self) -> Occ<'_> {
        Occ::new(self.bytes, self.header)
    }

    /// Rows before `row` in the sentinel-free BWT.
    fn bwt_index(&self, row: usize) -> usize {
        row - usize::from(self.header.sentinel_row < row)
    }

    /// `c[code]` + occurrences of `code` in BWT rows `0..row`, at most `n + 1`.
    fn step(&self, code: u8, row: usize) -> usize {
        (self.header.c[usize::from(code)] as usize + self.bwt().rank(code, self.bwt_index(row)))
            .min(self.header.n + 1)
    }

    /// The row of the suffix one position earlier. `row` is not the
    /// sentinel's row.
    fn lf(&self, row: usize) -> usize {
        let (code, rank) = self.bwt().access_rank(self.bwt_index(row));
        (self.header.c[usize::from(code)] as usize + rank).min(self.header.n)
    }

    /// The doc id of the suffix at `row`, a non-empty suffix, one walk at a
    /// time.
    #[cfg(test)]
    fn doc(&self, mut row: usize) -> usize {
        let marks = self.header.marks(self.bytes);
        let mut steps = 0;
        loop {
            let (marked, rank) = marks.get_rank(row);
            if marked {
                return self.sampled(rank);
            }
            steps += 1;
            assert!(
                steps <= self.header.walk_bound(),
                "corrupt docfm index: no sample within the walk bound"
            );
            row = self.lf(row);
        }
    }

    fn sampled(&self, rank: usize) -> usize {
        let doc = self.header.sample(self.bytes, rank);
        assert!(
            doc < self.header.num_docs,
            "corrupt docfm index: doc {doc} out of range"
        );
        doc
    }

    /// Visit the doc id of each row of `rows`, all non-empty suffixes, with
    /// up to [`LANES`] walks interleaved; under `STATS` returns the LF steps
    /// and the longest walk. Each walk stays inside its document: its start
    /// is sampled.
    // Out of line: inlined into the forward pass it runs slower.
    #[inline(never)]
    fn docs<const STATS: bool>(
        &self,
        mut rows: impl Iterator<Item = usize>,
        visit: &mut dyn FnMut(usize),
    ) -> (u64, usize) {
        let marks = self.header.marks(self.bytes);
        let bound = self.header.walk_bound();
        let (mut total, mut longest) = (0u64, 0usize);
        // (row, LF steps taken)
        let mut lanes = [(0usize, 0usize); LANES];
        let mut live = 0;
        for (lane, row) in lanes.iter_mut().zip(rows.by_ref()) {
            *lane = (row, 0);
            live += 1;
        }
        while live > 0 {
            let mut i = 0;
            while i < live {
                let (row, steps) = lanes[i];
                let (marked, rank) = marks.get_rank(row);
                if !marked {
                    assert!(
                        steps < bound,
                        "corrupt docfm index: no sample within the walk bound"
                    );
                    lanes[i] = (self.lf(row), steps + 1);
                    i += 1;
                    continue;
                }
                if STATS {
                    total += steps as u64;
                    longest = longest.max(steps);
                }
                visit(self.sampled(rank));
                match rows.next() {
                    Some(next) => {
                        lanes[i] = (next, 0);
                        i += 1;
                    }
                    None => {
                        live -= 1;
                        lanes[i] = lanes[live];
                    }
                }
            }
        }
        (total, longest)
    }
}

/// Locate walks in flight: independent LF chains overlap their cache misses.
const LANES: usize = 16;

/// Ancestor slots a cursor keeps: every forward depth.
const RING: usize = forward::MAX_DEPTH + 1;

// Rows fit the ring's u32 slots.
const _: () = assert!(MAX_TEXT_BYTES < u32::MAX as u64);

/// A half-open row interval: the suffixes prefixed by the query so far.
#[derive(Clone, Copy)]
pub struct Cursor<'a> {
    view: View<'a>,
    lo: usize,
    hi: usize,
    root: bool,
    /// Slot `(len - i) % RING`: the rows of `path[i..]`, `path` the bytes
    /// prepended since the root or [`Cursor::rows`].
    tails: [(u32, u32); RING],
    /// Slot `(len - 1 - j) % RING`: `path[j]`.
    front: [u8; RING],
    /// Bytes prepended since the root or [`Cursor::rows`].
    len: u32,
}

impl Cursor<'_> {
    /// Prepend `bytes`. Total: an out-of-alphabet byte gives an empty cursor.
    pub fn extend_front(&self, bytes: &[u8]) -> Self {
        let mut next = *self;
        for &byte in bytes.iter().rev() {
            if next.lo == next.hi {
                break;
            }
            next.root = false;
            let slot = next.len as usize % RING;
            next.tails[slot] = (next.lo as u32, next.hi as u32);
            next.front[slot] = byte;
            next.len = next.len.wrapping_add(1);
            let Some(code) = self.view.header.code_of[usize::from(byte)] else {
                next.hi = next.lo;
                break;
            };
            next.lo = self.view.step(code, next.lo);
            next.hi = self.view.step(code, next.hi).max(next.lo);
        }
        next
    }

    /// Its rows `within`, clamped to its count; the root stays the root.
    /// Drops the ancestors: its locate walks every row back.
    pub fn rows(&self, within: std::ops::Range<u64>) -> Self {
        let at = |row: u64| self.lo + row.min(self.count()) as usize;
        Self {
            lo: at(within.start),
            hi: at(within.end).max(at(within.start)),
            len: 0,
            ..*self
        }
    }

    /// Occurrences of the query; `n + 1` at the root.
    pub fn count(&self) -> u64 {
        (self.hi - self.lo) as u64
    }

    /// Visit the doc id of every occurrence, in no fixed order; the root
    /// visits nothing. Occurrences resolve from samples at or a few bytes
    /// into the query first; the rest walk back.
    pub fn locate(&self, visit: &mut dyn FnMut(usize)) {
        self.locate_from::<false>(self.forward_depth(), visit, None);
    }

    /// [`Cursor::locate`], counted and timed.
    pub fn locate_stats(&self, visit: &mut dyn FnMut(usize)) -> LocateStats {
        let start = std::time::Instant::now();
        self.locate_from::<true>(self.forward_depth(), visit, Some(start))
    }
}

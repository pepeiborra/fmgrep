//! A byte-alphabet FM-index over NUL-terminated documents: counts by
//! backward search, and the document of every occurrence from sampled
//! document ids. The index is one blob, queried in place from any
//! `AsRef<[u8]>` (a memory-mapped file included) without copying.
//!
//! Rows are the `n + 1` suffixes of the text in sorted order, a virtual
//! sentinel ending the text below every byte; row 0 is the empty suffix.
//!
//! ```
//! let index = docfm::FmIndex::build(b"hello world\0say hello\0".to_vec(), 32)?;
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
mod layout;
mod marks;
mod occ;
#[cfg(test)]
mod tests;

use crate::layout::Header;
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

/// Why a build, a write or a view failed.
#[derive(Debug)]
pub enum Error {
    /// The text, of this many bytes, is over [`MAX_TEXT_BYTES`].
    TextTooLong(usize),
    /// A sample rate of 0.
    ZeroSampleRate,
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
            Self::ZeroSampleRate => write!(f, "docfm sample rate must be at least 1"),
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
    /// id of every row whose text position is a multiple of `sample_rate` or
    /// starts a document, so a locate walks fewer than `sample_rate` LF steps
    /// per occurrence. Runs on the current rayon pool; the blob does not
    /// depend on it.
    pub fn build(text: Vec<u8>, sample_rate: u32) -> Result<Self, Error> {
        Self::view(build::build(text, sample_rate)?)
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
        }
    }

    /// Documents in the text: its [`DOC_END`] count.
    pub fn num_docs(&self) -> usize {
        self.header.num_docs
    }

    /// The sample rate it was built at.
    pub fn sample_rate(&self) -> u32 {
        self.header.sample_rate
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
                steps < self.header.sample_rate as usize,
                "corrupt docfm index: no sample within the rate"
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
    /// up to [`LANES`] walks interleaved. Each walk stays inside its
    /// document: its start is sampled.
    fn docs(&self, mut rows: impl Iterator<Item = usize>, visit: &mut dyn FnMut(usize)) {
        let marks = self.header.marks(self.bytes);
        let rate = self.header.sample_rate as usize;
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
                        steps + 1 < rate,
                        "corrupt docfm index: no sample within the rate"
                    );
                    lanes[i] = (self.lf(row), steps + 1);
                    i += 1;
                    continue;
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
    }
}

/// Locate walks in flight: independent LF chains overlap their cache misses.
const LANES: usize = 16;

/// A half-open row interval: the suffixes prefixed by the query so far.
#[derive(Clone, Copy)]
pub struct Cursor<'a> {
    view: View<'a>,
    lo: usize,
    hi: usize,
    root: bool,
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
    pub fn rows(&self, within: std::ops::Range<u64>) -> Self {
        let at = |row: u64| self.lo + row.min(self.count()) as usize;
        Self {
            lo: at(within.start),
            hi: at(within.end).max(at(within.start)),
            ..*self
        }
    }

    /// Occurrences of the query; `n + 1` at the root.
    pub fn count(&self) -> u64 {
        (self.hi - self.lo) as u64
    }

    /// Visit the doc id of every occurrence, in no fixed order; the root
    /// visits nothing.
    pub fn locate(&self, visit: &mut dyn FnMut(usize)) {
        if self.root {
            return;
        }
        self.view.docs(self.lo..self.hi, visit);
    }
}

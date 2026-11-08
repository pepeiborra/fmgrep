//! Rows and BWT positions number different sequences: the marks take rows
//! of the `n + 1` sorted suffixes, the wavelet levels positions of the
//! sentinel-free BWT, one fewer past the sentinel's row.

use std::ops::{Add, Sub};

/// One of the `n + 1` rows, or the end of a row interval.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Row(usize);

impl Row {
    #[inline]
    pub const fn new(row: usize) -> Self {
        Self(row)
    }

    /// The row's number, as the marks take it.
    #[inline]
    pub const fn get(self) -> usize {
        self.0
    }

    /// Where this row's BWT byte sits, given the sentinel's row.
    #[inline]
    pub fn bwt_pos(self, sentinel: Row) -> BwtPos {
        BwtPos(self.0 - usize::from(sentinel.0 < self.0))
    }

    /// The rows `lo..hi`.
    #[inline]
    pub fn range(lo: Row, hi: Row) -> impl Iterator<Item = Row> {
        (lo.0..hi.0).map(Row)
    }
}

impl Add<usize> for Row {
    type Output = Row;

    #[inline]
    fn add(self, rows: usize) -> Row {
        Row(self.0 + rows)
    }
}

/// How many rows `self` is past `earlier`.
impl Sub for Row {
    type Output = usize;

    #[inline]
    fn sub(self, earlier: Row) -> usize {
        self.0 - earlier.0
    }
}

/// A position in the sentinel-free BWT; only [`Row::bwt_pos`] makes one.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct BwtPos(usize);

impl BwtPos {
    /// The position, as the wavelet levels take it.
    #[inline]
    pub const fn get(self) -> usize {
        self.0
    }
}

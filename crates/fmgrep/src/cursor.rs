//! The seam both index-side walks step: an FM interval refined one byte
//! string at a time, and the locate that places its occurrences.

/// An FM interval that can be refined by prepending bytes.
pub trait BackwardCursor: Sized {
    /// Refine by prepending `bytes` (left to right in text order).
    ///
    /// Total: a byte outside the structure's alphabet yields an empty cursor,
    /// never a panic.
    fn extend_front(&self, bytes: &[u8]) -> Self;

    /// Occurrences of the current query; 0 means the branch is dead.
    fn count(&self) -> u64;
}

/// A cursor whose occurrences can be placed in the indexed text, without
/// re-searching the query.
pub trait LocatingCursor: BackwardCursor {
    /// Visit every occurrence of the current query, in any order, in the
    /// structure's locate unit. Empty cursors visit nothing.
    fn locate(&self, visit: &mut dyn FnMut(usize));
}

/// Naive cursor over a text: the walks' reference semantics.
#[cfg(test)]
pub struct ScanCursor<'a> {
    text: &'a [u8],
    query: Vec<u8>,
}

#[cfg(test)]
impl<'a> From<&'a [u8]> for ScanCursor<'a> {
    fn from(text: &'a [u8]) -> Self {
        Self {
            text,
            query: Vec::new(),
        }
    }
}

#[cfg(test)]
impl BackwardCursor for ScanCursor<'_> {
    fn extend_front(&self, bytes: &[u8]) -> Self {
        let mut query = Vec::with_capacity(bytes.len() + self.query.len());
        query.extend_from_slice(bytes);
        query.extend_from_slice(&self.query);
        Self {
            text: self.text,
            query,
        }
    }

    fn count(&self) -> u64 {
        if self.query.is_empty() {
            return self.text.len() as u64 + 1;
        }
        crate::pattern::count_occurrences(self.text, &self.query)
    }
}

#[cfg(test)]
impl LocatingCursor for ScanCursor<'_> {
    fn locate(&self, visit: &mut dyn FnMut(usize)) {
        if self.query.is_empty() || self.text.len() < self.query.len() {
            return;
        }
        for start in 0..=self.text.len() - self.query.len() {
            if &self.text[start..start + self.query.len()] == self.query.as_slice() {
                visit(start);
            }
        }
    }
}

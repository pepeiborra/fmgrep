//! What the query drive asks the index, and what it answers.

use std::ops::Range;

use crate::pattern::PreparedPattern;

/// [`Answer::candidates`] with `None` expanded against the corpus size.
pub fn candidate_docs(candidates: Option<Vec<usize>>, num_docs: usize) -> Vec<usize> {
    candidates.unwrap_or_else(|| (0..num_docs).collect())
}

/// One term as the query drive hands it to the index.
pub enum Query<'a> {
    Literal(&'a [u8]),
    /// A literal's per-shard counts, no locate: [`Answer::counts`].
    Count(&'a [u8]),
    /// [`Query::Literal`], exact on at least the docs of ascending disjoint
    /// `within`.
    LiteralWithin {
        term: &'a [u8],
        within: &'a [Range<usize>],
    },
    Regex(&'a PreparedPattern),
}

/// The index's answer for one query: the counts the FM-index settles, plus
/// the documents a pattern still has to be checked against (empty for a
/// literal).
pub struct Answer {
    pub exact: Vec<(usize, u64)>,
    /// Regex candidates; `None` is every doc.
    pub candidates: Option<Vec<usize>>,
    pub probe_us: u64,
    /// Ascending disjoint doc ranges with their nonzero counts; empty unless
    /// [`Query::Count`].
    pub counts: Vec<(Range<usize>, u64)>,
}

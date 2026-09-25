//! What the query drive asks the index, and what it answers.

use crate::pattern::PreparedPattern;

/// Regex candidate set. `All` means `0..num_docs` without materializing it.
pub enum Candidates {
    All,
    Docs(Vec<usize>),
}

/// `All` expanded against the corpus size.
pub fn candidate_docs(candidates: Candidates, num_docs: usize) -> Vec<usize> {
    match candidates {
        Candidates::All => (0..num_docs).collect(),
        Candidates::Docs(ids) => ids,
    }
}

/// One term as the query drive hands it to the index.
pub enum Query<'a> {
    Literal(&'a [u8]),
    /// A literal's occurrences: [`Answer::exact`] holds at least every doc
    /// of ascending `place`, [`Answer::occurrences`] the rest.
    Occurrences {
        term: &'a [u8],
        place: &'a [usize],
    },
    Regex(&'a PreparedPattern),
}

/// The index's answer for one query: the counts the FM-index settles, plus
/// the documents a pattern still has to be checked against (empty for a
/// literal).
pub struct Answer {
    pub exact: Vec<(usize, u64)>,
    pub candidates: Candidates,
    pub probe_us: u64,
    /// Occurrences attributed to no doc; 0 unless [`Query::Occurrences`].
    pub occurrences: u64,
}

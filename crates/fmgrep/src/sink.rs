//! Where the query drive emits match output.

use crate::matching::DocOutcome;
use crate::wire::QueryReport;

/// One document's matches, borrowed from the fetch batch and valid only for
/// the call. `outcome.lines` index `raw`.
pub struct DocMatches<'a> {
    pub doc: usize,
    pub path: &'a str,
    pub raw: &'a [u8],
    pub outcome: &'a DocOutcome,
}

/// Consumer of a query's match output.
///
/// `corpus` once, then per term: `term_start`, one `doc` call per matching
/// document strictly ascending by doc id, then `term_end` with that term's
/// finished report. Nothing is retained across calls, so a streaming sink
/// peaks at one fetch batch. Never called in [`crate::matching::QueryOutput::Counts`].
///
/// Any error aborts the query: a sink may already have received documents for
/// the failing term and will not see its `term_end`.
pub trait MatchSink {
    /// The shape of a full scan of this index: document count and total
    /// document bytes. rg-style stats need it even when nothing matches.
    fn corpus(&mut self, docs: usize, bytes: u64) -> Result<(), String> {
        let (_, _) = (docs, bytes);
        Ok(())
    }

    fn term_start(&mut self, term: &str) -> Result<(), String> {
        let _ = term;
        Ok(())
    }

    fn doc(&mut self, m: DocMatches<'_>) -> Result<(), String>;

    fn term_end(&mut self, report: &QueryReport) -> Result<(), String> {
        let _ = report;
        Ok(())
    }
}

/// Drops match output: the [`crate::dispatch`] seam, which reports counts
/// only.
pub struct Discard;

impl MatchSink for Discard {
    fn doc(&mut self, _: DocMatches<'_>) -> Result<(), String> {
        Ok(())
    }
}

//! Where the query drive emits match output.

use grep_printer::Stats;

use crate::wire::QueryReport;

/// One matching document, valid only for the call: its path as printed, its
/// printed bytes and rg's stats for it.
pub struct DocMatches<'a> {
    pub path: &'a str,
    pub printed: &'a [u8],
    pub stats: &'a Stats,
}

/// Consumer of a query's match output: `corpus` once, then per term
/// `term_start`, one `doc` per matching document ascending by indexed path,
/// and `term_end`. Never called in [`crate::matching::QueryOutput::Counts`];
/// an error aborts the query.
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

/// Drops match output: a search that prints its report only.
pub struct Discard;

impl MatchSink for Discard {
    fn doc(&mut self, _: DocMatches<'_>) -> Result<(), String> {
        Ok(())
    }
}

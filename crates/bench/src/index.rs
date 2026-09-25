//! In-process benchmark index traits: one index over the whole corpus,
//! literal and native regex.

use crate::RegexError;

/// One index over the whole corpus; construction is the caller's (see
/// [`crate::run_unified_benchmark`]).
pub trait UnifiedIndex {
    fn name(&self) -> &str;

    /// One `(doc_idx, count)` pair per matching document, ascending by
    /// `doc_idx`, zero-count documents omitted. An empty term or one holding
    /// `UNIFIED_SEPARATOR` yields an empty vec. Overlapping matches are
    /// implementation-defined, but all impls must agree (tests compare
    /// against an overlapping naive scan).
    fn search(&self, term: &[u8]) -> Vec<(usize, u64)>;

    /// Documents the index actually answers for, not the input length: a
    /// dropped document does not count, so file-set skew is observable on
    /// [`crate::BenchmarkReport::num_files_indexed`].
    fn num_files_indexed(&self) -> usize;

    /// Approximate in-memory size.
    fn index_size_bytes(&self) -> u64;
}

/// Native regex search, engaged for every `/.../`-spelled term; there is
/// deliberately no emulation fallback.
///
/// As [`UnifiedIndex::search`], but counts are non-overlapping and no match
/// may span a document boundary. An empty pattern is a [`RegexError`].
pub trait UnifiedRegexSearch: UnifiedIndex {
    fn search_regex(&self, pattern: &str) -> Result<Vec<(usize, u64)>, RegexError>;
}

//! The byte that joins documents in a shard's concatenated text.

/// Separator byte joining documents in a shard's text.
///
/// Contract value `0x00` (NUL). The corpus loaders skip NUL-containing files,
/// so no document byte ever equals it: a literal term containing it can never
/// match (a search returns no hits rather than scanning), and no match for a
/// separator-free term can span a document boundary.
pub const UNIFIED_SEPARATOR: u8 = 0x00;

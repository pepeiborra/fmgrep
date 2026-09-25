//! Concatenated-corpus layout the index is built over.

use crate::walk::Document;

/// Separator byte joining documents in a [`UnifiedCorpus`].
///
/// Contract value `0x00` (NUL). The corpus loaders skip NUL-containing files,
/// so no document byte ever equals it: a literal term containing it can never
/// match (a search returns no hits rather than scanning), and no match for a
/// separator-free term can span a document boundary.
pub const UNIFIED_SEPARATOR: u8 = 0x00;

/// Concatenated-corpus layout.
///
/// Layout contract: `bytes` is every document in id order, each followed by
/// exactly one [`UNIFIED_SEPARATOR`] (including the last); `offsets` has
/// `num_docs + 1` entries with document `i` at
/// `bytes[offsets[i]..offsets[i + 1] - 1]` and `offsets[num_docs] ==
/// bytes.len()`.
#[derive(Debug, Clone)]
pub struct UnifiedCorpus {
    bytes: Vec<u8>,
    offsets: Vec<usize>,
}

impl UnifiedCorpus {
    /// Concatenate `docs` per the layout contract above.
    ///
    /// Panics if any document contains [`UNIFIED_SEPARATOR`]: the separator
    /// must be unambiguous, and the corpus loader already excludes such files.
    pub fn concatenate(docs: &[Document]) -> Self {
        for d in docs {
            assert!(
                !d.bytes.contains(&UNIFIED_SEPARATOR),
                "doc {:?} contains the unified-corpus separator byte 0x00",
                d.path
            );
        }
        let mut bytes = Vec::new();
        let mut offsets = Vec::with_capacity(docs.len() + 1);
        for d in docs {
            offsets.push(bytes.len());
            bytes.extend_from_slice(&d.bytes);
            bytes.push(UNIFIED_SEPARATOR);
        }
        offsets.push(bytes.len());
        Self { bytes, offsets }
    }

    pub fn num_docs(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    /// Concatenated length including separators.
    pub fn len(&self) -> usize {
        self.offsets.last().copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The full concatenated buffer (document bytes + separators).
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[cfg(test)]
    fn offsets(&self) -> &[usize] {
        &self.offsets
    }

    /// Byte range of document `i` within [`UnifiedCorpus::bytes`],
    /// excluding its trailing separator. Returns `None` when out of range.
    #[cfg(test)]
    fn doc_range(&self, doc_idx: usize) -> Option<std::ops::Range<usize>> {
        let (Some(&start), Some(&next)) =
            (self.offsets.get(doc_idx), self.offsets.get(doc_idx + 1))
        else {
            return None;
        };
        // Every document is followed by exactly one separator.
        Some(start..next - 1)
    }

    /// Raw bytes of document `i` (no separator); `None` when out of range.
    #[cfg(test)]
    fn doc_bytes(&self, doc_idx: usize) -> Option<&[u8]> {
        Some(&self.bytes[self.doc_range(doc_idx)?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unified_docs() -> Vec<Document> {
        vec![
            Document {
                id: 0,
                path: "a.c".into(),
                bytes: b"printk hello".to_vec(),
            },
            Document {
                id: 1,
                path: "b.c".into(),
                bytes: b"".to_vec(),
            },
            Document {
                id: 2,
                path: "c.c".into(),
                bytes: b"printk printk".to_vec(),
            },
        ]
    }

    #[test]
    fn corpus_concatenation_matches_contract() {
        let docs = unified_docs();
        let c = UnifiedCorpus::concatenate(&docs);
        assert_eq!(c.num_docs(), 3);
        // "printk hello" (12) + SEP + "" (0) + SEP + "printk printk" (13) + SEP
        assert_eq!(c.len(), 12 + 1 + 1 + 13 + 1);
        assert_eq!(c.offsets(), &[0, 13, 14, 28]);
        assert_eq!(c.doc_bytes(0), Some(b"printk hello".as_slice()));
        assert_eq!(c.doc_bytes(1), Some(b"".as_slice()));
        assert_eq!(c.doc_bytes(2), Some(b"printk printk".as_slice()));
        assert_eq!(c.doc_bytes(3), None);
        assert_eq!(c.doc_range(0), Some(0..12));
        assert_eq!(c.doc_range(1), Some(13..13));
        assert_eq!(c.doc_range(2), Some(14..27));
        // Round-trip: ranges slice back the original bytes.
        for (i, d) in docs.iter().enumerate() {
            assert_eq!(&c.bytes()[c.doc_range(i).unwrap()], d.bytes.as_slice());
        }
    }

    #[test]
    fn corpus_rejects_separator_in_docs() {
        let bad = vec![Document {
            id: 0,
            path: "nul.c".into(),
            bytes: b"has\x00nul".to_vec(),
        }];
        let r = std::panic::catch_unwind(|| UnifiedCorpus::concatenate(&bad));
        assert!(r.is_err(), "separator byte in a doc must panic");
    }

    #[test]
    fn corpus_empty_input_is_well_formed() {
        let c = UnifiedCorpus::concatenate(&[]);
        assert_eq!(c.num_docs(), 0);
        assert_eq!(c.len(), 0);
        assert!(c.is_empty());
        assert_eq!(c.offsets(), &[0]);
    }
}

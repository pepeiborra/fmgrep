//! Per-shard routing of a pattern whose anchor is too long to plan eagerly.

use super::*;
use crate::automaton::min_anchor_count;

#[test]
fn a_common_anchor_walks_and_a_rare_one_locates() {
    let common = "union x\n".repeat(min_anchor_count() as usize + 1);
    let docs = docs_of(&[common.as_bytes(), b"union {", b"union x"]);
    let index = DocfmIndex::build_with_cap(&docs, common.len() as u64 + 8, false);
    assert_eq!(index.blob_slots(), 2);
    assert_eq!(
        candidates(&index, "union[ \\t]+\\{", docs.len()),
        vec![1, 2]
    );
}

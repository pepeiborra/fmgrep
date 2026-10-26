//! Per-shard pruning by every required literal of a pattern.

use super::*;

const CAPS: [u64; 5] = [1, 8, 24, 64, 1 << 20];

/// `pattern`'s candidates on one shard, after checking that every cut of
/// `docs` keeps each matching doc.
fn pruned(docs: &[Document], pattern: &str) -> Vec<usize> {
    let prepared = PreparedPattern::prepare(pattern).unwrap();
    let matching: Vec<usize> = (0..docs.len())
        .filter(|&d| prepared.count(&docs[d].bytes) > 0)
        .collect();
    let mut got = Vec::new();
    for cap in CAPS {
        got = candidates(&DocfmIndex::build_with_cap(docs, cap), pattern, docs.len());
        assert!(
            matching.iter().all(|d| got.contains(d)),
            "cap={cap} {pattern:?}: {got:?} misses {matching:?}"
        );
    }
    got
}

#[test]
fn a_rarer_literal_prunes_the_anchor_docs() {
    let mut bytes: Vec<&[u8]> = vec![b"x_info"; 20];
    bytes.extend([&b"stg_x_info"[..], b"stg_ only"]);
    assert_eq!(pruned(&docs_of(&bytes), "stg_\\w+_info"), vec![20, 21]);
}

#[test]
fn a_cheap_second_literal_intersects() {
    let docs = docs_of(&[b"alpha", b"omega", b"alpha omega", b"omega alpha"]);
    assert_eq!(pruned(&docs, "alpha.*omega"), vec![2, 3]);
}

#[test]
fn a_shard_missing_a_literal_yields_nothing() {
    let docs = docs_of(&[b"omega omega omega", b"omega omega", b"alpha omega"]);
    assert_eq!(pruned(&docs, "alpha.*omega"), vec![2]);
    assert!(pruned(&docs, "alpha.*omega.*zeta").is_empty());
}

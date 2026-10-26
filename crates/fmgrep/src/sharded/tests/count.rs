//! Count-only and shard-restricted literal probes.

use std::ops::Range;

use super::*;

fn docs() -> Vec<Document> {
    let bytes: Vec<Vec<u8>> = (0..40)
        .map(|i| match i % 7 {
            0 => Vec::new(),
            1 => format!("printk {i} printk").into_bytes(),
            3 => b"aaaa tail".to_vec(),
            _ => format!("doc {i}").into_bytes(),
        })
        .collect();
    docs_of(&bytes.iter().map(Vec::as_slice).collect::<Vec<_>>())
}

const COUNTED: [&[u8]; 7] = [b"printk", b"aa", b"a", b"doc 1", b"missing", b"", b"a\0b"];

fn counts(index: &DocfmIndex, terms: &[&[u8]]) -> Vec<Vec<(Range<usize>, u64)>> {
    let qs: Vec<Query<'_>> = terms.iter().map(|t| Query::Count(t)).collect();
    index
        .probe_all(&qs)
        .into_iter()
        .map(|a| {
            assert!(a.exact.is_empty());
            assert!(candidate_docs(a.candidates, index.num_files_indexed()).is_empty());
            a.counts
        })
        .collect()
}

/// `probe_literal` summed per shard, nonzero shards only.
fn per_shard(index: &DocfmIndex, term: &[u8]) -> Vec<(Range<usize>, u64)> {
    let hits = index.probe_literal(term);
    index
        .metas
        .iter()
        .map(|m| {
            let n = hits
                .iter()
                .filter(|(d, _)| m.docs.contains(d))
                .map(|&(_, c)| c)
                .sum();
            (m.docs.clone(), n)
        })
        .filter(|&(_, c)| c > 0)
        .collect()
}

#[test]
fn count_equals_located_totals_per_shard() {
    let docs = docs();
    for cap in [1u64, 40, 1 << 20] {
        let built = DocfmIndex::build_with_cap(&docs, cap);
        let want: Vec<_> = COUNTED.iter().map(|t| per_shard(&built, t)).collect();
        assert!(want[4].is_empty() && want[5].is_empty() && want[6].is_empty());
        assert!(!want[0].is_empty());
        let dir = tempfile::tempdir().unwrap();
        for index in [round_trip(&built, dir.path(), &docs), built] {
            assert_eq!(counts(&index, &COUNTED), want, "cap={cap}");
        }
    }
}

fn ranges(pairs: &[(usize, usize)]) -> Vec<Range<usize>> {
    pairs.iter().map(|&(a, b)| a..b).collect()
}

fn within(index: &DocfmIndex, term: &[u8], within: &[Range<usize>]) -> Vec<(usize, u64)> {
    index
        .probe_all(&[Query::LiteralWithin { term, within }])
        .remove(0)
        .exact
}

#[test]
fn literal_within_skips_shards_outside_within() {
    let docs = docs_of(&[
        b"printk a",
        b"x printk",
        b"b printk",
        b"",
        b"printk printk",
        b"printk",
    ]);
    // One doc per shard; the empty doc shares one.
    let index = DocfmIndex::build_with_cap(&docs, 1);
    assert_eq!(index.metas.len(), 5);
    let everywhere = index.probe_literal(b"printk");
    assert_eq!(everywhere.len(), 5);
    for w in [&[][..], &[(0, 1)], &[(0, 1), (2, 5)], &[(3, 4)], &[(4, 6)]].map(ranges) {
        let shard_in = |d: usize| {
            index.metas.iter().any(|m| {
                m.docs.contains(&d)
                    && w.iter()
                        .any(|r| r.start < m.docs.end && m.docs.start < r.end)
            })
        };
        let want: Vec<(usize, u64)> = everywhere
            .iter()
            .copied()
            .filter(|&(d, _)| shard_in(d))
            .collect();
        assert_eq!(within(&index, b"printk", &w), want, "{w:?}");
    }
    for term in [&b""[..], b"a\0b"] {
        assert!(within(&index, term, &ranges(&[(0, 6)])).is_empty());
    }
}

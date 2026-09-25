//! Occurrence counts and sparse tallies against a naive oracle.

use super::*;

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// Docs over a 4-letter alphabet, empties included.
fn random_docs(seed: u64, n: usize) -> Vec<Document> {
    let mut rng = Rng(seed);
    let bytes: Vec<Vec<u8>> = (0..n)
        .map(|_| {
            (0..rng.below(60))
                .map(|_| b"abcd"[rng.below(4) as usize])
                .collect()
        })
        .collect();
    docs_of(&bytes.iter().map(Vec::as_slice).collect::<Vec<_>>())
}

/// Substrings of every length 1..=9 at a few offsets per doc, plus absent
/// and random terms.
fn terms_of(docs: &[Document], seed: u64) -> Vec<Vec<u8>> {
    let mut rng = Rng(seed);
    let mut terms: Vec<Vec<u8>> = vec![b"qqqq".to_vec(), b"abcq".to_vec()];
    for d in docs.iter().step_by(3) {
        for len in 1..=9usize {
            if d.bytes.len() >= len {
                let at = rng.below((d.bytes.len() - len + 1) as u64) as usize;
                terms.push(d.bytes[at..at + len].to_vec());
            }
        }
    }
    terms.extend((0..40).map(|_| {
        (0..1 + rng.below(8))
            .map(|_| b"abcd"[rng.below(4) as usize])
            .collect()
    }));
    terms
}

fn oracle(docs: &[Document], term: &[u8]) -> Vec<(usize, u64)> {
    docs.iter()
        .enumerate()
        .filter_map(|(i, d)| {
            let c = crate::pattern::count_occurrences(&d.bytes, term);
            (c > 0).then_some((i, c))
        })
        .collect()
}

fn total(hits: &[(usize, u64)]) -> u64 {
    hits.iter().map(|&(_, c)| c).sum()
}

/// `(exact, occurrences)` per term, `place` shared by every term.
fn occurrences(
    index: &DocfmIndex,
    terms: &[Vec<u8>],
    place: &[usize],
) -> Vec<(Vec<(usize, u64)>, u64)> {
    let qs: Vec<Query<'_>> = terms
        .iter()
        .map(|term| Query::Occurrences { term, place })
        .collect();
    index
        .probe_all(&qs)
        .into_iter()
        .map(|a| {
            assert!(candidate_docs(a.candidates, index.num_files_indexed()).is_empty());
            assert!(a.exact.windows(2).all(|w| w[0].0 < w[1].0));
            (a.exact, a.occurrences)
        })
        .collect()
}

/// Random ascending doc subsets, the empty one first.
fn places(n: usize, seed: u64) -> Vec<Vec<usize>> {
    let mut rng = Rng(seed);
    let mut out = vec![Vec::new()];
    for density in [2, 8, 64] {
        out.push((0..n).filter(|_| rng.below(density) == 0).collect());
    }
    out
}

fn check_occurrences(index: &DocfmIndex, docs: &[Document], terms: &[Vec<u8>]) {
    for place in places(docs.len(), 5) {
        for (term, (exact, rest)) in terms.iter().zip(occurrences(index, terms, &place)) {
            let want = oracle(docs, term);
            let at = format!("term={term:?} place={place:?}");
            assert_eq!(total(&exact) + rest, total(&want), "{at}");
            if place.is_empty() {
                assert!(exact.is_empty(), "{at}");
            }
            for &doc in &place {
                let count = |hits: &[(usize, u64)]| {
                    hits.iter().find(|&&(d, _)| d == doc).map_or(0, |&(_, c)| c)
                };
                assert_eq!(count(&exact), count(&want), "{at} doc {doc}");
            }
        }
    }
    // Other queries in a batch answer as they do alone.
    let patterns = prepared();
    let plain = queries(&patterns);
    let mut mixed = queries(&patterns);
    mixed.extend(
        terms
            .iter()
            .map(|term| Query::Occurrences { term, place: &[] }),
    );
    let got = shape(index.probe_all(&mixed), docs.len());
    assert_eq!(got[..plain.len()], answers_of(index, &plain)[..]);
}

#[test]
fn occurrences_equal_located_totals() {
    let docs = random_docs(21, 120);
    let mut terms = terms_of(&docs, 21);
    terms.extend([Vec::new(), vec![b'a', crate::UNIFIED_SEPARATOR, b'b']]);
    for cap in [1u64, 40, 1 << 20] {
        let built = DocfmIndex::build_with_cap(&docs, cap, false);
        check_occurrences(&built, &docs, &terms);
        // Loaded, as built.
        let dir = tempfile::tempdir().unwrap();
        let loaded = round_trip(&built, dir.path(), &docs);
        for place in places(docs.len(), 9) {
            assert_eq!(
                occurrences(&loaded, &terms, &place),
                occurrences(&built, &terms, &place),
                "cap={cap}"
            );
        }
    }
}

#[test]
fn sparse_counts_match_naive_many_docs_per_shard() {
    let mut bytes: Vec<Vec<u8>> = (0..600)
        .map(|i| format!("doc {i} body").into_bytes())
        .collect();
    bytes[17].extend_from_slice(b" rare rare");
    bytes[599].extend_from_slice(b" rare");
    let docs = docs_of(&bytes.iter().map(Vec::as_slice).collect::<Vec<_>>());
    for term in [
        b"rare".as_slice(),
        b"doc 1",
        b"doc 59",
        b"body",
        b"o",
        b"missing",
    ] {
        assert_eq!(
            DocfmIndex::build_with_cap(&docs, 1 << 20, false).probe_literal(term),
            oracle(&docs, term)
        );
    }
}

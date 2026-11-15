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
        .probe_all(&qs, None)
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
    let hits = index.probe_literal(term, None);
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
        .probe_all(&[Query::LiteralWithin { term, within }], None)
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
    let everywhere = index.probe_literal(b"printk", None);
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

#[test]
fn fold_count_equals_located_fold_hits_per_shard() {
    let bytes: Vec<String> = (0..40)
        .map(|i| match i % 5 {
            0 => String::new(),
            1 => format!("printk {i} PRINTK"),
            2 => format!("Print\u{212A} {i}"),
            3 => "prINTk printk".to_string(),
            _ => format!("doc {i}"),
        })
        .collect();
    let docs = docs_of(&bytes.iter().map(String::as_bytes).collect::<Vec<_>>());
    let variants: [&[u8]; 4] = [b"printk", b"PRINTK", "Print\u{212A}".as_bytes(), b"prINTk"];
    let pattern = PreparedPattern::prepare_with("printk", ci()).unwrap();
    let plan = pattern.fold().expect("plan");
    for cap in [1u64, 40, 1 << 20] {
        let built = DocfmIndex::build_with_cap(&docs, cap);
        let located: Vec<(Range<usize>, u64)> = (built.shards.iter().zip(&built.metas))
            .map(|(s, m)| {
                let hits = crate::fold::fold_locate(s.cursor(), plan).expect("under the cap");
                (m.docs.clone(), hits.len() as u64)
            })
            .filter(|&(_, n)| n > 0)
            .collect();
        let mut spelled: Vec<(Range<usize>, u64)> = Vec::new();
        for (r, n) in counts(&built, &variants).into_iter().flatten() {
            match spelled.iter_mut().find(|(s, _)| *s == r) {
                Some((_, total)) => *total += n,
                None => spelled.push((r, n)),
            }
        }
        spelled.sort_by_key(|(r, _)| r.start);
        assert_eq!(located, spelled, "cap={cap}");
        let dir = tempfile::tempdir().unwrap();
        for index in [round_trip(&built, dir.path(), &docs), built] {
            let walk = index.walk(&pattern, None).expect("a fold plan");
            assert_eq!(walk.counts(), located, "cap={cap}");
        }
    }
}

/// A term counted by its walk and then located from it gets the documents
/// the regex probe walks again for, on every shard cut, within any range,
/// and where the walk gives up.
#[test]
fn a_walked_term_locates_the_documents_the_regex_probe_does() {
    let mut texts: Vec<String> = (0..40)
        .map(|i| match i % 5 {
            0 => String::new(),
            1 => format!("printk {i} PRINTK"),
            2 => format!("Print\u{212A} {i}"),
            3 => "prINTk printk abcdefghi".to_string(),
            _ => format!("doc {i}"),
        })
        .collect();
    // Every spelling of `abcdefghi`: 512 live intervals, past the walk's cap.
    texts.extend((0..512u32).map(|m| {
        let spelled = "abcdefghi".chars().enumerate();
        spelled
            .map(|(k, c)| match m >> k & 1 {
                1 => c.to_ascii_uppercase(),
                _ => c,
            })
            .collect::<String>()
    }));
    let docs = docs_of(&texts.iter().map(String::as_bytes).collect::<Vec<_>>());
    let n = docs.len();
    let one = DocfmIndex::build_with_cap(&docs, 1 << 20);
    let pattern = PreparedPattern::prepare_with("abcdefghi", ci()).unwrap();
    let plan = pattern.fold().expect("plan");
    assert_eq!(one.shards.len(), 1);
    assert!(crate::fold::fold_locate(one.shards[0].cursor(), plan).is_none());
    let spans: [Option<Vec<Range<usize>>>; 3] =
        [None, Some(vec![0..9, 30..41]), Some(vec![500..520, 540..n])];
    for term in ["printk", "abcdefghi", "missing"] {
        let pattern = PreparedPattern::prepare_with(term, ci()).unwrap();
        for cap in [40u64, 400, 1 << 20] {
            let built = DocfmIndex::build_with_cap(&docs, cap);
            let dir = tempfile::tempdir().unwrap();
            for index in [round_trip(&built, dir.path(), &docs), built] {
                for within in &spans {
                    let walk = index
                        .walk(&pattern, within.as_deref())
                        .expect("a fold plan");
                    let walked = index.probe_all(&[Query::Walked(&walk)], within.as_deref());
                    let probed = index.probe_all(&[Query::Regex(&pattern)], within.as_deref());
                    assert_eq!(
                        walked[0].candidates, probed[0].candidates,
                        "{term} cap={cap} {within:?}"
                    );
                    assert!(walked[0].exact.is_empty() && walked[0].counts.is_empty());
                }
            }
        }
    }
}

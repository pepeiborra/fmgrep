//! The docfm shard structure through the index: counts against a naive
//! oracle, persistence, and load refusals.

use super::*;

fn doc(id: usize, path: &str, bytes: &[u8]) -> Document {
    Document {
        id,
        path: path.into(),
        bytes: bytes.to_vec(),
    }
}

fn docs() -> Vec<Document> {
    vec![
        doc(0, "empty.c", b""),
        doc(1, "a.c", b"printk hello printk"),
        doc(2, "b.c", b"aaaa"),
    ]
}

fn hits(idx: &DocfmIndex, term: &[u8]) -> Vec<(usize, u64)> {
    idx.probe_literal(term)
}

fn build(docs: &[Document], cap: u64) -> DocfmIndex {
    DocfmIndex::build_with_cap(docs, cap)
}

fn naive_oracle(docs: &[Document], term: &[u8]) -> Vec<(usize, u64)> {
    if term.is_empty() {
        return Vec::new();
    }
    docs.iter()
        .enumerate()
        .filter_map(|(i, d)| {
            if d.bytes.len() < term.len() {
                return None;
            }
            let n = (0..=d.bytes.len() - term.len())
                .filter(|&w| &d.bytes[w..w + term.len()] == term)
                .count() as u64;
            (n > 0).then_some((i, n))
        })
        .collect()
}

fn reload(index: &DocfmIndex, docs: &[Document]) -> DocfmIndex {
    let dir = tempfile::tempdir().unwrap();
    round_trip(index, dir.path(), docs)
}

#[test]
fn search_matches_oracle_and_partitions_empty_docs() {
    let docs = docs();
    for cap in [1u64 << 20, 1] {
        let idx = build(&docs, cap);
        assert_eq!(idx.blob_slots(), if cap == 1 { 2 } else { 1 });
        assert_eq!(idx.num_files_indexed(), 3);
        for term in [b"printk".as_slice(), b"aa", b"a", b"missing", b"aaaaa"] {
            assert_eq!(hits(&idx, term), naive_oracle(&docs, term), "term {term:?}");
        }
        assert_eq!(hits(&idx, b"aa"), vec![(2, 3)], "cap={cap}");
    }
}

#[test]
fn empty_and_separator_and_out_of_alphabet_terms_match_nothing() {
    let idx = build(&docs(), 1 << 20);
    for term in [b"".as_slice(), b"a\x00b", b"\x00", b"\xfe", b"print\xfek"] {
        assert!(hits(&idx, term).is_empty(), "term {term:?}");
    }
}

#[test]
fn full_byte_alphabet_fits_one_shard() {
    let docs = vec![
        doc(0, "low.bin", &(1..=128u8).collect::<Vec<_>>()),
        doc(1, "high.bin", &(128..=255u8).rev().collect::<Vec<_>>()),
    ];
    let idx = build(&docs, 1 << 20);
    assert_eq!(idx.blob_slots(), 1);
    let back = reload(&idx, &docs);
    for term in [&[1u8, 2][..], &[128], &[255, 254], &[128, 127], &[200]] {
        assert_eq!(hits(&idx, term), naive_oracle(&docs, term), "term {term:?}");
        assert_eq!(
            hits(&back, term),
            naive_oracle(&docs, term),
            "term {term:?}"
        );
    }
}

#[test]
fn uniform_run_builds_exactly() {
    let docs = vec![
        doc(0, "run-a.c", &vec![b'x'; 25_000]),
        doc(
            1,
            "run-b.c",
            &[vec![b'x'; 25_000], b"tail".to_vec()].concat(),
        ),
        doc(
            2,
            "run-c.c",
            &[b"head".to_vec(), vec![b'x'; 25_000]].concat(),
        ),
    ];
    let idx = build(&docs, 1 << 20);
    for term in [b"x".as_slice(), b"xx", b"xxx", b"tail", b"head", b"missing"] {
        assert_eq!(hits(&idx, term), naive_oracle(&docs, term), "term {term:?}");
    }
    assert_eq!(
        hits(&idx, b"xx"),
        vec![(0, 24_999), (1, 24_999), (2, 24_999)]
    );
}

#[test]
#[should_panic(expected = "no indexable bytes")]
fn empty_corpus_fails_loudly() {
    let _ = build(&[], 1 << 20);
}

#[test]
#[should_panic(expected = "no indexable bytes")]
fn all_empty_corpus_fails_loudly() {
    let _ = build(&[doc(0, "empty.c", b"")], 1 << 20);
}

#[test]
#[should_panic(expected = "separator byte")]
fn separator_containing_doc_fails_loudly() {
    let _ = build(&[doc(0, "nul.c", b"has\x00nul")], 1 << 20);
}

#[test]
fn shard_text_limit_is_the_i32_sa_bound() {
    assert_eq!(::docfm::MAX_TEXT_BYTES, i32::MAX as u64);
}

#[test]
fn regex_candidates_prune_via_locate() {
    let docs = docs();
    let idx = build(&docs, 1 << 20);
    let nullable = PreparedPattern::prepare("a*").unwrap();
    assert!(idx.regex_candidates(&nullable).is_none());
    for pattern in [
        "printk",
        "aa",
        "missing",
        "pr[i1]ntk",
        "[ap]+",
        "stg_\\w+_info",
        "printk.*hello",
    ] {
        let prepared = PreparedPattern::prepare(pattern).unwrap();
        let Some(got) = idx.regex_candidates(&prepared) else {
            panic!("{pattern} narrows");
        };
        for (i, d) in docs.iter().enumerate() {
            assert!(
                prepared.count(&d.bytes) == 0 || got.contains(&i),
                "{pattern} pruned matching doc {i}"
            );
        }
    }
    let absent = PreparedPattern::prepare("\u{00fe}+z").unwrap();
    assert_eq!(idx.regex_candidates(&absent), Some(Vec::new()));
}

#[test]
fn fold_candidates_are_exact() {
    // A folded `hscenv` may not match across the separator between `HSC`
    // and `env`.
    let docs = vec![
        doc(0, "empty.c", b""),
        doc(1, "a.c", b"HscEnv hscenv"),
        doc(2, "b.c", b"HSC"),
        doc(3, "c.c", b"env"),
        doc(4, "d.c", "print\u{212A}".as_bytes()),
    ];
    let ci = MatchMode {
        ignore_case: true,
        ..MatchMode::default()
    };
    for cap in [1u64 << 20, 8] {
        let idx = build(&docs, cap);
        for (pattern, want) in [
            ("hscenv", vec![1usize]),
            ("printk", vec![4]),
            ("missing", vec![]),
        ] {
            let prepared = PreparedPattern::prepare_with(pattern, ci).unwrap();
            assert!(prepared.fold().is_some(), "{pattern:?} must plan");
            let Some(got) = idx.regex_candidates(&prepared) else {
                panic!("{pattern} has a fold plan");
            };
            assert_eq!(got, want, "cap={cap} pattern={pattern:?}");
        }
    }
}

#[test]
fn size_is_the_structure_alone() {
    let docs = docs();
    let idx = build(&docs, 1 << 20);
    let text: Vec<u8> = docs
        .iter()
        .filter(|d| !d.bytes.is_empty())
        .flat_map(|d| d.bytes.iter().copied().chain([crate::UNIFIED_SEPARATOR]))
        .collect();
    let structure = ::docfm::FmIndex::build(text, DOC_SAMPLE_RATE).unwrap();
    assert_eq!(idx.index_size_bytes(), structure.heap_bytes());
}

#[test]
fn blob_round_trips_and_load_refusals() {
    let docs = docs();
    let idx = build(&docs, 1 << 20);
    let back = reload(&idx, &docs);
    for term in [b"printk".as_slice(), b"aa", b"missing"] {
        assert_eq!(
            hits(&back, term),
            naive_oracle(&docs, term),
            "term {term:?}"
        );
    }
    let label = "shard-00000000-00000003.bin".to_string();
    for (labels, blob, doc_lens, want) in [
        (
            vec![label.clone()],
            Vec::new(),
            vec![0, 0, 0],
            "no indexable doc",
        ),
        (
            vec![label],
            b"not-a-docfm-blob".to_vec(),
            doc_lens(&docs),
            "corrupt docfm blob",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blobs.shard"), &blob).unwrap();
        let lens = vec![blob.len() as u64];
        let err = load_from(dir.path(), &labels, &lens, &doc_lens)
            .err()
            .expect("must refuse");
        assert!(err.contains(want), "got: {err}");
    }
    // A real blob over a manifest with one non-empty doc fewer.
    let dir = tempfile::tempdir().unwrap();
    let (labels, lens) = persist(&idx, dir.path());
    let err = load_from(dir.path(), &labels, &lens, &[0, 19, 0])
        .err()
        .expect("must refuse");
    assert!(
        err.contains("indexes 2 docs") && err.contains("tampered"),
        "got: {err}"
    );
}

#[test]
fn many_tiny_docs_locate_exactly() {
    let docs: Vec<Document> = (0..2000)
        .map(|i| {
            let bytes: &[u8] = [b"a".as_slice(), b"ab", b"", b"ba", b"bab"][i % 5];
            doc(i, &format!("{i}.c"), bytes)
        })
        .collect();
    for cap in [1u64 << 20, 64] {
        let idx = build(&docs, cap);
        for term in [b"a".as_slice(), b"b", b"ab", b"ba", b"bab", b"aa"] {
            assert_eq!(
                hits(&idx, term),
                naive_oracle(&docs, term),
                "cap={cap} {term:?}"
            );
        }
    }
}

#[test]
fn slot_lens_are_line_multiples() {
    let docs = docs();
    for cap in [1u64 << 20, 1] {
        let dir = tempfile::tempdir().unwrap();
        let (_, lens) = persist(&build(&docs, cap), dir.path());
        assert!(lens.iter().all(|l| l % 64 == 0), "cap={cap} {lens:?}");
    }
}

#[test]
fn built_and_loaded_report_identical_size_and_coverage() {
    let docs = docs();
    for cap in [1u64 << 20, 1] {
        let idx = build(&docs, cap);
        let back = reload(&idx, &docs);
        assert_eq!(back.blob_slots(), idx.blob_slots(), "cap={cap}");
        assert_eq!(back.index_size_bytes(), idx.index_size_bytes(), "cap={cap}");
        assert_eq!(
            back.num_files_indexed(),
            idx.num_files_indexed(),
            "cap={cap}"
        );
    }
}

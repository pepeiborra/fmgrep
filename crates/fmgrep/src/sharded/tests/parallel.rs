//! Locates large enough to walk back in parallel, through the index: a
//! literal's one interval, a walk's many accepted intervals, a fold walk's
//! case variants. Their answers equal a scan of the documents, on one shard
//! and on three.

use super::*;
use crate::pattern::count_overlapping;

/// Rows a shard's locate of `ull` must walk back: three of docfm's grains
/// of 8,192, so that it splits into tasks.
const WALKED: u64 = 3 * 8192;

/// Two hundred documents of lines like `x = 0xA3F1ull;`, from a fixed
/// generator, about 5 MB. By document: `uLL` and `ULL` lines; `ull` after
/// decimal digits only; `ull` after any hex digit; `u` alone.
fn hex_docs() -> Vec<Document> {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let texts: Vec<Vec<u8>> = (0..200)
        .map(|d| {
            let (digits, suffixes): (&[u8], &[&str]) = match d % 4 {
                0 => (b"0123456789ABCDEF", &["uLL", "ULL"]),
                1 => (b"0123456789", &["ull"]),
                2 => (b"0123456789ABCDEF", &["ull"]),
                _ => (b"0123456789ABCDEF", &["u"]),
            };
            let mut text = Vec::new();
            for _ in 0..1200 + next(400) {
                let hex: String = (0..4)
                    .map(|_| digits[next(digits.len() as u64) as usize] as char)
                    .collect();
                let suffix = suffixes[next(suffixes.len() as u64) as usize];
                text.extend(format!("  x = 0x{hex}{suffix};\n").bytes());
            }
            text
        })
        .collect();
    docs_of(&texts.iter().map(Vec::as_slice).collect::<Vec<_>>())
}

/// Docs holding a match of `pattern`, by scanning them.
fn matching(docs: &[Document], pattern: &PreparedPattern) -> Vec<usize> {
    (0..docs.len())
        .filter(|&d| pattern.count(&docs[d].bytes) > 0)
        .collect()
}

/// `term`'s occurrences per doc, by scanning them.
fn scanned(docs: &[Document], term: &str) -> Vec<(usize, u64)> {
    let finder = memchr::memmem::Finder::new(term);
    (0..docs.len())
        .map(|d| (d, count_overlapping(&docs[d].bytes, &finder)))
        .filter(|&(_, n)| n > 0)
        .collect()
}

#[test]
fn large_locates_equal_a_scan() {
    let docs = hex_docs();
    let one = DocfmIndex::build_with_cap(&docs, 1 << 30);
    // Three shards of about a third each.
    let bytes: u64 = doc_lens(&docs).iter().map(|l| l + 1).sum();
    let longest = doc_lens(&docs).into_iter().max().unwrap();
    let three = DocfmIndex::build_with_cap(&docs, bytes / 3 + longest);
    assert_eq!((one.blob_slots(), three.blob_slots()), (1, 3));
    for s in &three.shards {
        let stats = s.cursor().extend_front(b"ull").locate_stats(&mut |_| ());
        let walked = stats.occurrences - stats.zero_step - stats.forward;
        assert!(walked >= WALKED, "{walked} rows of ull walked back");
    }
    for index in [&one, &three] {
        // One interval: a literal's counts.
        for term in ["ull", "uLL", "u;"] {
            assert_eq!(hits(index, term), scanned(&docs, term), "{term}");
        }
        // 6 and 16 intervals a walk accepts, 10 x 10 a deferred walk
        // accepts, and a walk that gives up and locates `ull`.
        let patterns = [
            "[A-F]ull",
            "[0-9A-F]ull",
            "[0-9]{2}ull;",
            "0x[0-9A-F]{4}ull",
        ];
        for p in patterns {
            let pattern = PreparedPattern::prepare(p).unwrap();
            assert_eq!(
                candidates(index, p, docs.len()),
                matching(&docs, &pattern),
                "{p}"
            );
        }
        // The case variants of a fold walk, as a regex and as a counted walk.
        let ull = PreparedPattern::prepare_with("ull;", ci()).unwrap();
        let want = matching(&docs, &ull);
        let got = candidate_docs(
            index.regex_candidates(&ull, None, &Default::default()),
            docs.len(),
        );
        assert_eq!(got, want, "-i ull;");
        let walk = index.walk(&ull, None).expect("a fold plan");
        assert_eq!(walk.docs(None), want, "-i ull; walked");
    }
}

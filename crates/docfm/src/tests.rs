use super::*;
use crate::layout::Gaps;

fn naive_positions(text: &[u8], pattern: &[u8]) -> Vec<usize> {
    if pattern.is_empty() || text.len() < pattern.len() {
        return Vec::new();
    }
    (0..=text.len() - pattern.len())
        .filter(|&i| &text[i..i + pattern.len()] == pattern)
        .collect()
}

/// The doc of each occurrence start: the DOC_END bytes before it.
fn naive_docs(text: &[u8], pattern: &[u8]) -> Vec<usize> {
    let doc_at: Vec<usize> = text
        .iter()
        .scan(0, |docs, &b| {
            let here = *docs;
            *docs += usize::from(b == DOC_END);
            Some(here)
        })
        .collect();
    naive_positions(text, pattern)
        .into_iter()
        .map(|p| doc_at[p])
        .collect()
}

type Built = FmIndex<Vec<u8>>;

fn build_with(text: Vec<u8>, gaps: Gaps) -> Result<Built, Error> {
    FmIndex::view(build::build(
        text,
        layout::Config {
            gaps,
            ..layout::Config::DEFAULT
        },
    )?)
}

/// `f` inside a rayon pool of `threads` threads.
fn in_pool<R: Send>(threads: usize, f: impl FnOnce() -> R + Send) -> R {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap()
        .install(f)
}

fn located<D: AsRef<[u8]>>(index: &FmIndex<D>, pattern: &[u8]) -> Vec<usize> {
    let mut got = Vec::new();
    index.locate(pattern, &mut |p| got.push(p));
    got.sort_unstable();
    got
}

fn count<D: AsRef<[u8]>>(index: &FmIndex<D>, pattern: &[u8]) -> u64 {
    index.cursor().extend_front(pattern).count()
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// Pieces over {1,2,3} each followed by a NUL, the shape of a shard text.
fn joined_texts() -> Vec<Vec<u8>> {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    (0..100)
        .map(|_| {
            let mut text = Vec::new();
            for _ in 0..1 + rng.below(5) {
                for _ in 0..1 + rng.below(40) {
                    text.push(1 + rng.below(3) as u8);
                }
                text.push(0);
            }
            text
        })
        .collect()
}

fn all_bytes() -> Vec<u8> {
    let mut rng = Rng(7);
    let mut text: Vec<u8> = (0..=u8::MAX).collect();
    text.extend((0..4000).map(|_| rng.below(256) as u8));
    text
}

/// Many 1-3 byte documents: doc starts dominate the samples.
fn tiny_docs() -> Vec<u8> {
    let mut rng = Rng(13);
    let mut text = Vec::new();
    for _ in 0..3000 {
        for _ in 0..1 + rng.below(3) {
            text.push(b'a' + rng.below(3) as u8);
        }
        text.push(DOC_END);
    }
    text
}

fn terminated(mut text: Vec<u8>) -> Vec<u8> {
    text.push(DOC_END);
    text
}

/// About 8 KB of code-like documents: identifiers in every case, `_` runs,
/// indentation, blank and CRLF lines, whitespace runs over 64 bytes.
fn code_text() -> Vec<u8> {
    let wide = [b"#define FOO_BAR".as_slice(), &[b' '; 72], b"0x1\n"].concat();
    let tabs = [b"\t".repeat(70), b"}\n".to_vec()].concat();
    let lines: Vec<Vec<u8>> = [
        b"static int kmem_cache_create(struct kmem_cache *cachep, size_t size)\n".as_slice(),
        b"{\n",
        b"\tif (unlikely(!cachep)) {\n",
        b"\t\treturn -ENOMEM;\n",
        b"    HTMLDocumentParser::parseURLRequest(fooBarBaz, kMaxSize_2);\n",
        b"#define BIF_CFG_DEV0_EPF0_VF12_MASK  0x0000FFFFL\n",
        b"\n",
        b"x = __builtin_expect(a__b___c, 0); // TODO: fixMe\r\n",
        b"  \t  \r\n",
        b"\treturn x_y + 12ab_CD;\n",
        &wide,
        &tabs,
    ]
    .map(<[u8]>::to_vec)
    .to_vec();
    let mut rng = Rng(61);
    let mut text = Vec::new();
    while text.len() < 8000 {
        if rng.below(30) == 0 {
            text.push(DOC_END);
        }
        text.extend_from_slice(&lines[rng.below(lines.len() as u64) as usize]);
    }
    terminated(text)
}

fn gaps(min: u32, max: u32, ws: u32) -> Gaps {
    Gaps { min, max, ws }
}

/// The build's gaps, then tight and per-document-rate ones.
fn samplings() -> Vec<Gaps> {
    let texts = [(8, 16, 64), (12, 16, 64), (1, 2, 4), (3, 5, 9)];
    let rates = [1, 3, 4, 32].map(|r| (r, r, r));
    texts
        .into_iter()
        .chain(rates)
        .map(|(min, max, ws)| gaps(min, max, ws))
        .collect()
}

/// Whether each text position is sampled, by definition.
fn naive_marked(text: &[u8], gaps: Gaps) -> Vec<bool> {
    let doc_start = |p: usize| p == 0 || text[p - 1] == DOC_END;
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut marked = vec![false; text.len()];
    let mut last = 0;
    for p in 0..text.len() {
        let (b, prev) = (text[p], p.checked_sub(1).map_or(0, |q| text[q]));
        let mut q = p;
        while !doc_start(q) && matches!(text[q - 1], b' ' | b'\t' | b'\r') {
            q -= 1;
        }
        let line_first = !b.is_ascii_whitespace() && (doc_start(q) || text[q - 1] == b'\n');
        let start = (word(b) && !word(prev))
            || (prev == b'_' && word(b) && b != b'_')
            || (prev.is_ascii_lowercase() && b.is_ascii_uppercase())
            || line_first;
        let force = if b.is_ascii_whitespace() {
            gaps.ws
        } else {
            gaps.max
        };
        let gap = (p - last) as u32;
        if doc_start(p) || gap >= force || (start && gap >= gaps.min) {
            marked[p] = true;
            last = p;
        }
    }
    marked
}

fn texts() -> Vec<Vec<u8>> {
    let mut texts = joined_texts();
    texts.push(b"mississippi\0".to_vec());
    texts.push(b"a\0".to_vec());
    texts.push(b"\0\0a\0".to_vec());
    texts.push(terminated(vec![b'x'; 70_000]));
    texts.push(terminated(all_bytes()));
    texts.push(tiny_docs());
    texts.push(code_text());
    texts
}

/// Every substring of length 1..=3 at a few offsets, plus absent ones.
fn patterns(text: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = vec![b"zz\xff".to_vec(), vec![1, 1, 1, 1, 1, 1]];
    for start in [
        0,
        text.len() / 3,
        text.len() / 2,
        text.len().saturating_sub(4),
    ] {
        for len in 1..=4 {
            if start + len <= text.len() {
                out.push(text[start..start + len].to_vec());
            }
        }
    }
    out
}

#[test]
fn counts_match_naive() {
    for threads in [1, 3] {
        for text in texts() {
            let index = in_pool(threads, || Built::build(text.clone())).unwrap();
            assert_eq!(count(&index, b""), text.len() as u64 + 1);
            for pattern in patterns(&text) {
                let want = naive_positions(&text, &pattern).len() as u64;
                assert_eq!(
                    count(&index, &pattern),
                    want,
                    "text len {} pattern {pattern:?}",
                    text.len()
                );
            }
        }
    }
}

#[test]
fn locate_visits_each_occurrence_doc() {
    for text in texts() {
        let docs = text.iter().filter(|&&b| b == DOC_END).count();
        let want: Vec<(Vec<u8>, Vec<usize>)> = patterns(&text)
            .into_iter()
            .map(|p| (p.clone(), naive_docs(&text, &p)))
            .collect();
        for threads in [1, 3] {
            for sampling in samplings() {
                let index = in_pool(threads, || build_with(text.clone(), sampling)).unwrap();
                assert_eq!(index.num_docs(), docs);
                for (pattern, docs) in &want {
                    assert_eq!(
                        &located(&index, pattern),
                        docs,
                        "threads {threads} {sampling:?} pattern {pattern:?}"
                    );
                }
            }
        }
    }
}

/// About 1 MiB of documents: over several supers.
fn large_text() -> Vec<u8> {
    let mut rng = Rng(17);
    let mut text = Vec::new();
    while text.len() < 1_000_000 {
        let run = rng.below(4) == 0;
        for _ in 0..1 + rng.below(3000) {
            text.push(if run {
                b'r'
            } else {
                b'a' + rng.below(20) as u8
            });
        }
        text.push(DOC_END);
    }
    text
}

#[test]
fn threads_do_not_change_the_blob() {
    for text in texts().into_iter().chain([large_text()]) {
        for sampling in [Gaps::WORDS, gaps(1, 2, 4)] {
            let build = || build_with(text.clone(), sampling).unwrap().into_bytes();
            let outside = build();
            for threads in [1, 3, 8] {
                let inside = in_pool(threads, build);
                assert!(
                    inside == outside,
                    "threads {threads} {sampling:?} len {}",
                    text.len()
                );
            }
        }
    }
}

/// Marks and samples by definition, from a naive suffix sort.
#[test]
fn samples_match_suffix_order() {
    for sampling in samplings() {
        for text in texts().into_iter().filter(|t| t.len() < 10_000) {
            let index = build_with(text.clone(), sampling).unwrap();
            let marked = naive_marked(&text, sampling);
            let mut rows: Vec<usize> = (0..=text.len()).collect();
            rows.sort_by(|&a, &b| text[a..].cmp(&text[b..]));
            let doc_of: Vec<usize> = text
                .iter()
                .scan(0, |docs, &b| {
                    let here = *docs;
                    *docs += usize::from(b == DOC_END);
                    Some(here)
                })
                .collect();
            let want: Vec<Option<usize>> = rows
                .iter()
                .map(|&p| (p < text.len() && marked[p]).then(|| doc_of[p]))
                .collect();
            let view = index.cursor().view;
            let marks = index.header.marks(view.bytes);
            let got: Vec<Option<usize>> = (0..=text.len())
                .map(|row| {
                    let (marked, rank) = marks.get_rank(row);
                    marked.then(|| view.sampled(rank))
                })
                .collect();
            assert_eq!(got, want, "{sampling:?} len {}", text.len());
        }
    }
}

/// One-byte documents: the most doc-start walks per byte.
#[test]
fn one_byte_docs_build() {
    let mut rng = Rng(19);
    let text: Vec<u8> = (0..40_000)
        .flat_map(|_| [b'a' + rng.below(3) as u8, DOC_END])
        .collect();
    let index = Built::build(text.clone()).unwrap();
    for pattern in [b"a".as_slice(), b"b\0c", b"\0a\0", b"c\0c\0c"] {
        assert_eq!(
            located(&index, pattern),
            naive_docs(&text, pattern),
            "pattern {pattern:?}"
        );
    }
    let again = in_pool(4, || Built::build(text)).unwrap();
    assert!(again.into_bytes() == index.into_bytes());
}

#[test]
fn sample_count_matches_the_rule() {
    for sampling in samplings() {
        for text in texts().into_iter().chain([large_text()]) {
            let index = build_with(text.clone(), sampling).unwrap();
            let want = naive_marked(&text, sampling).iter().filter(|&&m| m).count();
            assert_eq!(
                index.header.num_samples,
                want,
                "{sampling:?} len {}",
                text.len()
            );
        }
    }
}

#[test]
fn bwt_matches_naive_rotation_sort() {
    for text in texts().into_iter().filter(|t| t.len() < 10_000) {
        let index = Built::build(text.clone()).unwrap();
        let mut rows: Vec<usize> = (0..=text.len()).collect();
        rows.sort_by(|&a, &b| text[a..].cmp(&text[b..]));
        let want: Vec<Option<u8>> = rows
            .iter()
            .map(|&p| p.checked_sub(1).map(|q| text[q]))
            .collect();
        let mut byte_of = [0u8; 256];
        for (byte, code) in index.header.code_of.iter().enumerate() {
            if let Some(code) = code {
                byte_of[usize::from(*code)] = byte as u8;
            }
        }
        let view = index.cursor().view;
        let got: Vec<Option<u8>> = (0..=text.len())
            .map(|row| {
                (row != index.header.sentinel_row)
                    .then(|| byte_of[usize::from(view.bwt().access_rank(view.bwt_index(row)).0)])
            })
            .collect();
        assert_eq!(got, want, "text {text:?}");
    }
}

#[test]
fn cursor_composes() {
    let index = Built::build(b"abracadabra\0".to_vec()).unwrap();
    let root = index.cursor();
    assert_eq!(root.count(), 13);
    let mut visited = 0;
    root.locate(&mut |_| visited += 1);
    assert_eq!(visited, 0, "the root visits nothing");
    assert_eq!(root.extend_front(b"").count(), 13);
    for (whole, tail, head) in [
        (b"ab", b"b", b"a"),
        (b"ra", b"a", b"r"),
        (b"zb", b"b", b"z"),
    ] {
        let a = root.extend_front(whole);
        let b = root.extend_front(tail).extend_front(head);
        assert_eq!((a.lo, a.hi), (b.lo, b.hi), "{whole:?}");
    }
    assert_eq!(root.extend_front(b"abra").count(), 2);
}

#[test]
fn out_of_alphabet_is_dead() {
    let index = Built::build(b"abc\0".to_vec()).unwrap();
    assert!(!index.contains_symbol(b'z'));
    let dead = index.cursor().extend_front(b"z");
    assert_eq!(dead.count(), 0);
    assert_eq!(dead.extend_front(b"a").count(), 0);
    let mut visited = 0;
    dead.locate(&mut |_| visited += 1);
    assert_eq!(visited, 0);
}

#[test]
fn empty_text_builds_and_matches_nothing() {
    let index = Built::build(Vec::new()).unwrap();
    assert_eq!(count(&index, b""), 1);
    assert_eq!(count(&index, b"a"), 0);
    assert!(located(&index, b"a").is_empty());
    let back = FmIndex::view(encode(&index)).unwrap();
    assert_eq!(count(&back, b"a"), 0);
}

fn encode<D: AsRef<[u8]>>(index: &FmIndex<D>) -> Vec<u8> {
    let mut blob = Vec::new();
    index.encode_into(&mut blob).unwrap();
    blob
}

#[test]
fn view_of_encoded_bytes_matches_built() {
    for text in texts() {
        let index = Built::build(text.clone()).unwrap();
        let blob = encode(&index);
        let mut shifted = vec![0u8; blob.len() + 1];
        shifted[1..].copy_from_slice(&blob);
        let aligned = FmIndex::view(blob.as_slice()).unwrap();
        let unaligned = FmIndex::view(&shifted[1..]).unwrap();
        for back in [&aligned, &unaligned] {
            assert_eq!(back.heap_bytes(), index.heap_bytes());
            assert_eq!(encode(back), blob);
            for pattern in patterns(&text) {
                assert_eq!(count(back, &pattern), count(&index, &pattern));
                assert_eq!(located(back, &pattern), located(&index, &pattern));
            }
        }
        assert_eq!(index.into_bytes(), blob);
    }
}

#[test]
fn foreign_or_truncated_blob_is_refused() {
    let index = Built::build(b"printk hello printk\0".to_vec()).unwrap();
    let blob = encode(&index);
    let refuse = |bytes: &[u8], want: &str| {
        let Err(err) = FmIndex::view(bytes) else {
            panic!("must refuse ({want})");
        };
        let err = err.to_string();
        assert!(
            err.contains("corrupt docfm blob") && err.contains(want),
            "{want}: {err}"
        );
    };
    let patched = |at: usize, bytes: &[u8]| {
        let mut b = blob.clone();
        b[at..at + bytes.len()].copy_from_slice(bytes);
        b
    };
    let h = &index.header;
    refuse(&patched(0, b"NOTDOCF"), "magic");
    refuse(&patched(6, &[1]), "magic/version");
    refuse(&patched(6, &[5]), "magic/version");
    for cut in [
        0,
        7,
        12,
        layout::HEADER_BYTES,
        blob.len() / 2,
        blob.len() - 1,
    ] {
        refuse(&blob[..cut], "truncated");
    }
    refuse(&[blob.as_slice(), &[0]].concat(), "trailing");
    refuse(&patched(8, &u64::MAX.to_le_bytes()), "over the bound");
    refuse(&patched(16, &99u64.to_le_bytes()), "sentinel row");
    refuse(&patched(24, &0u32.to_le_bytes()), "v8 sampling");
    refuse(&patched(28, &(h.sigma as u32 + 1).to_le_bytes()), "sigma");
    // One more alphabet symbol than the header's sigma.
    let mut extra = blob.clone();
    extra[32 + 15] |= 0x80;
    refuse(&extra, "sigma");
    refuse(&patched(96 + 4, &1u32.to_le_bytes()), "not increasing");
    refuse(
        &patched(96 + 4 * h.sigma, &99u32.to_le_bytes()),
        "not increasing",
    );
    for block in [0u32, 128, 768, 8192] {
        refuse(&patched(64, &block.to_le_bytes()), "digits per block");
    }
    for d in [0u32, 3, 8] {
        refuse(&patched(2336, &d.to_le_bytes()), "bits per digit");
    }
    refuse(&patched(2340, &[1]), "reserved");
    let depth = h.wavelet.depth;
    // Level 0's start of digit 1 moves.
    let start = h.wavelet.starts[1];
    refuse(&patched(2176 + 4, &(start + 1).to_le_bytes()), "digits");
    refuse(&patched(2176, &1u32.to_le_bytes()), "level 0");
    refuse(
        &patched(2176 + 4, &(h.n as u32 + 1).to_le_bytes()),
        "level 0",
    );
    // Starts past the depth's digits.
    refuse(&patched(2176 + 8 * depth, &1u32.to_le_bytes()), "level");
    refuse(&patched(2304, &0u32.to_le_bytes()), "level 0");
    refuse(&patched(2304 + 4 * depth, &8u32.to_le_bytes()), "level");
    refuse(&patched(2304, &(1u32 << 30).to_le_bytes()), "level 0");
    // Level 0's first super counts one more digit 1 before it.
    refuse(
        &patched(h.wavelet.levels[0].supers + 4, &1u32.to_le_bytes()),
        "digits",
    );
    let bottom = |k: usize| h.wavelet.bottom[k];
    refuse(
        &patched(1124 + 4, &(bottom(1) + 1).to_le_bytes()),
        "code 1: ",
    );
    refuse(
        &patched(1124 + 4, &(h.n as u32 + 1).to_le_bytes()),
        "code starts",
    );
    refuse(
        &patched(1124 + 4 * h.sigma, &1u32.to_le_bytes()),
        "code starts",
    );
    refuse(&patched(2156, &[1]), "reserved");
    refuse(&patched(2366, &[1]), "reserved");
    let docs = h.num_docs as u32;
    refuse(&patched(2148, &0u32.to_le_bytes()), "docs and");
    refuse(&patched(2148, &(docs + 1).to_le_bytes()), "DOC_END bytes");
    refuse(
        &patched(2152, &(h.n as u32 + 1).to_le_bytes()),
        "samples over",
    );
    refuse(&patched(2152, &(docs - 1).to_le_bytes()), "samples over");
    let row_marks = h.marks(&blob);
    let flipped = |bit: usize| {
        let mut b = blob.clone();
        b[bit / 8] ^= 1 << (bit % 8);
        b
    };
    let sentinel_rank = row_marks.get_rank(h.sentinel_row).1;
    let sample_bit = 8 * h.samples_at + sentinel_rank * h.sample_bits as usize;
    refuse(&flipped(sample_bit), "not in doc 0");
    let (low, highs_at) = marks::layout_of(h.n + 1, h.num_samples);
    // One more high bit set: a mark the header does not count.
    let unset = (0..)
        .map(|p| 8 * (h.marks_at + highs_at) + p)
        .find(|&bit| blob[bit / 8] >> (bit % 8) & 1 == 0)
        .unwrap();
    refuse(&flipped(unset), "marked rows");
    // The sentinel row's mark moves within its bucket.
    assert!(low > 0);
    refuse(
        &flipped(8 * h.marks_at + sentinel_rank * low as usize),
        "sentinel row is not sampled",
    );
    // A flip that still views may miscount, but only our own locate
    // invariant may panic.
    for i in 0..blob.len() {
        let mut b = blob.clone();
        b[i] ^= 0x5a;
        let Ok(index) = FmIndex::view(b.as_slice()) else {
            continue;
        };
        let walk = std::panic::catch_unwind(|| {
            for pattern in [b"printk".as_slice(), b"p", b"\0", b"k p"] {
                let _ = count(&index, pattern);
                index.locate(pattern, &mut |_| {});
            }
        });
        if let Err(panic) = walk {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            assert!(msg.contains("corrupt docfm index"), "flip at {i}: {msg}");
        }
    }
}

#[test]
fn heap_bytes_is_the_blob_plus_header() {
    let mut rng = Rng(11);
    let text: Vec<u8> = (0..200_000).map(|_| b'a' + rng.below(26) as u8).collect();
    let index = Built::build(terminated(text)).unwrap();
    let blob = encode(&index).len();
    assert_eq!(
        index.heap_bytes(),
        (blob + std::mem::size_of::<layout::Header>()) as u64
    );
}

#[test]
fn blob_is_page_aligned() {
    for text in texts() {
        let index = Built::build(text).unwrap();
        let h = &index.header;
        assert_eq!(h.blob_len % layout::PAGE, 0);
        let lines: Vec<usize> = h.wavelet.levels[..h.wavelet.depth]
            .iter()
            .map(|l| l.supers)
            .chain([layout::HEADER_BYTES, h.marks_at, h.samples_at])
            .collect();
        assert!(lines.iter().all(|at| at % 64 == 0), "{lines:?}");
        assert_eq!(encode(&index).len(), h.blob_len);
    }
}

#[test]
fn rank_overhead_is_bounded() {
    let mut rng = Rng(5);
    let text: Vec<u8> = (0..100_000).map(|_| rng.below(256) as u8).collect();
    let text = terminated(text);
    let n = text.len();
    let index = Built::build(text).unwrap();
    let h = &index.header;
    let (d, lb) = (h.wavelet.d, h.wavelet.lb);
    let (s, heads) = digits::index_bytes(n, d, lb);
    let level = s + heads + digits::max_payload(n, d, lb) + 64;
    let levels = h.marks_at - layout::HEADER_BYTES;
    assert!(
        levels <= h.wavelet.depth * level,
        "{levels} vs {level} per level"
    );
    let n = n as f64;
    let marks_bits = 8.0 * (h.samples_at - h.marks_at) as f64;
    let m = h.num_samples as f64;
    // Low bits, a one and at most two zeros per mark, a select sample per
    // 64 zeros.
    let want = m * (((n + 1.0) / m).log2().floor() + 4.0) + 1024.0;
    assert!(marks_bits <= want, "{marks_bits} vs {want}");
}

/// Codes below `sigma`: runs, then uniform noise.
fn occ_codes(n: usize, sigma: usize, rng: &mut Rng) -> Vec<u8> {
    (0..n)
        .map(|i| {
            if i % 3000 < 1500 {
                (i / 3000 % sigma) as u8
            } else {
                rng.below(sigma as u64) as u8
            }
        })
        .collect()
}

#[test]
fn select_in_word_matches_naive() {
    let mut rng = Rng(41);
    let words = [
        0u64,
        1,
        1 << 63,
        u64::MAX,
        0x8000_0001,
        0xff00,
        0x0100_0000_0000_0080,
    ];
    for w in words
        .into_iter()
        .chain((0..2000).map(|_| rng.below(u64::MAX) ^ rng.below(u64::MAX) << 32))
    {
        let ones: Vec<usize> = (0..64).filter(|&b| w >> b & 1 == 1).collect();
        for (r, &want) in ones.iter().enumerate() {
            assert_eq!(marks::select_in_word(w, r), want, "{w:#x} {r}");
        }
    }
}

#[test]
fn sparse_marks_match_naive() {
    let mut rng = Rng(3);
    for len in [0usize, 1, 4095, 4096, 4097, 70_001] {
        for fill in 0..7 {
            let bits: Vec<bool> = (0..len)
                .map(|i| match fill {
                    0 => rng.below(2) == 1,
                    1 => true,
                    2 => false,
                    3 => i % 32 == 0,
                    // Sparse: low bits and empty buckets.
                    4 => rng.below(13) == 0,
                    5 => rng.below(1000) == 0,
                    // Clustered: long gaps between dense runs.
                    _ => i % 20_000 < 300 && rng.below(3) == 0,
                })
                .collect();
            let mut words = vec![0u64; len.div_ceil(64) + 1];
            for (i, _) in bits.iter().enumerate().filter(|&(_, &b)| b) {
                words[i / 64] |= 1 << (i % 64);
            }
            words[len / 64] |= u64::MAX << (len % 64);
            let ones = bits.iter().filter(|&&b| b).count();
            let mut out = vec![0u8; marks::section_bytes(len, ones)];
            assert_eq!(
                marks::write(&mut out, len, &words),
                ones,
                "bits past len are ignored"
            );
            let marks = marks::Marks::new(&out, len, ones);
            assert_eq!(marks.total(), ones);
            let mut before = 0;
            for (i, &bit) in bits.iter().enumerate() {
                assert_eq!(
                    marks.get_rank(i),
                    (bit, before),
                    "len {len} fill {fill} i {i}"
                );
                before += usize::from(bit);
            }
            assert_eq!(marks.get_rank(len), (false, ones), "len {len} fill {fill}");
            assert_eq!(
                marks.get_rank(len + 5000),
                (false, ones),
                "clamped past the end"
            );
        }
    }
}

#[test]
fn text_too_long_is_refused() {
    assert!(build::check_len(i32::MAX as usize).is_ok());
    let err = build::check_len(i32::MAX as usize + 1).unwrap_err();
    assert!(matches!(err, Error::TextTooLong(_)), "{err}");
    assert!(matches!(
        Built::build(b"a".to_vec()),
        Err(Error::Unterminated)
    ));
    assert!(matches!(
        Built::build(b"a\0b".to_vec()),
        Err(Error::Unterminated)
    ));
}

/// Intervals below and above the lane count, tiny docs, and a long run.
#[test]
fn interleaved_locate_equals_rowwise() {
    for sampling in samplings() {
        for text in texts() {
            let index = build_with(text.clone(), sampling).unwrap();
            for pattern in patterns(&text) {
                let cursor = index.cursor().extend_front(&pattern);
                let mut rowwise: Vec<usize> =
                    (cursor.lo..cursor.hi).map(|r| cursor.view.doc(r)).collect();
                let mut got = Vec::new();
                cursor
                    .view
                    .docs::<false>(cursor.lo..cursor.hi, &mut |d| got.push(d));
                rowwise.sort_unstable();
                got.sort_unstable();
                assert_eq!(got, rowwise, "{sampling:?} pattern {pattern:?}");
            }
        }
    }
}

#[test]
fn rows_locates_a_slice_of_the_rows() {
    for text in texts() {
        let index = Built::build(text.clone()).unwrap();
        for pattern in patterns(&text) {
            let cursor = index.cursor().extend_front(&pattern);
            for within in [
                0..0,
                0..1,
                2..7,
                5..17,
                std::ops::Range { start: 3, end: 1 },
                1..u64::MAX,
                u64::MAX..u64::MAX,
            ] {
                let slice = cursor.rows(within.clone());
                let n = cursor.count();
                let want_count = within.end.min(n).saturating_sub(within.start.min(n));
                assert_eq!(slice.count(), want_count);
                assert_eq!(slice.lo, cursor.lo + within.start.min(n) as usize);
                let mut want: Vec<usize> =
                    (slice.lo..slice.hi).map(|r| cursor.view.doc(r)).collect();
                let mut got = Vec::new();
                slice.locate(&mut |d| got.push(d));
                want.sort_unstable();
                got.sort_unstable();
                assert_eq!(got, want, "pattern {pattern:?} rows {within:?}");
            }
        }
    }
    let mut visited = 0;
    let index = Built::build(b"abracadabra\0".to_vec()).unwrap();
    index.cursor().rows(0..5).locate(&mut |_| visited += 1);
    assert_eq!(visited, 0, "the root visits nothing");
}

fn doc_of(text: &[u8]) -> Vec<usize> {
    text.iter()
        .scan(0, |docs, &b| {
            let here = *docs;
            *docs += usize::from(b == DOC_END);
            Some(here)
        })
        .collect()
}

/// LF steps from each text position back to its sample.
fn naive_walks(text: &[u8], gaps: Gaps) -> Vec<usize> {
    let mut last = 0;
    naive_marked(text, gaps)
        .iter()
        .enumerate()
        .map(|(p, &marked)| {
            if marked {
                last = p;
            }
            p - last
        })
        .collect()
}

#[test]
fn text_walks_stay_within_bound() {
    for text in [code_text(), terminated(all_bytes()), tiny_docs()] {
        let mut rows: Vec<usize> = (0..=text.len()).collect();
        rows.sort_by(|&a, &b| text[a..].cmp(&text[b..]));
        let docs = doc_of(&text);
        for sampling in samplings() {
            for (p, walk) in naive_walks(&text, sampling).into_iter().enumerate() {
                let bound = if text[p].is_ascii_whitespace() {
                    sampling.ws
                } else {
                    sampling.max
                };
                assert!(walk < bound as usize, "{sampling:?} at {p}: {walk}");
            }
            let index = build_with(text.clone(), sampling).unwrap();
            let view = index.cursor().view;
            for (row, &p) in rows.iter().enumerate().skip(1) {
                assert_eq!(view.doc(row), docs[p], "{sampling:?} row {row}");
            }
        }
    }
}

fn refuse(bytes: &[u8], want: &str) {
    let Err(err) = FmIndex::view(bytes) else {
        panic!("must refuse ({want})");
    };
    let err = err.to_string();
    assert!(
        err.contains("corrupt docfm blob") && err.contains(want),
        "{want}: {err}"
    );
}

fn patched(blob: &[u8], at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut b = blob.to_vec();
    b[at..at + bytes.len()].copy_from_slice(bytes);
    b
}

#[test]
fn text_blob_header_is_checked() {
    let blob = encode(&Built::build(b"printk hello printk\0".to_vec()).unwrap());
    assert!(FmIndex::view(blob.as_slice()).is_ok());
    let u32s = |v: u32| v.to_le_bytes();
    for (at, value) in [
        (68, 2),
        (72, 0),
        (72, 9),
        (80, 7),
        (84, 30),
        (88, 1),
        (92, 1),
        (24, 16),
    ] {
        refuse(&patched(&blob, at, &u32s(value)), "v8 sampling");
    }
    refuse(&patched(&blob, 6, &[7]), "magic/version");
}

#[test]
fn blobs_are_v8_with_their_gaps() {
    for sampling in samplings() {
        let blob = encode(&build_with(code_text(), sampling).unwrap());
        assert_eq!(&blob[..8], b"DOCFM\0\x08\0", "{sampling:?}");
        let Gaps { min, max, ws } = sampling;
        let got: Vec<u32> = blob[68..96]
            .chunks(4)
            .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
            .collect();
        assert_eq!(got, [1, min, max, ws, ws - 1, 0, 0], "{sampling:?}");
        assert_eq!(blob[24..28], max.to_le_bytes(), "{sampling:?}");
    }
}

/// Whether a forward locate probes the tail `pattern[i..]`: if `pattern[i]`
/// may be sampled as a word, subword or line start.
fn naive_probed(pattern: &[u8], i: usize) -> bool {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let (prev, b) = (pattern[i - 1], pattern[i]);
    (word(b) && !word(prev))
        || (prev == b'_' && word(b) && b != b'_')
        || (prev.is_ascii_lowercase() && b.is_ascii_uppercase())
        || (matches!(prev, b' ' | b'\t' | b'\r' | b'\n') && !b.is_ascii_whitespace())
}

#[test]
fn text_forward_probes_start_tails() {
    let text = code_text();
    let index = Built::build(text.clone()).unwrap();
    for (pattern, starts) in [
        (b"return".as_slice(), [].as_slice()),
        (b"int", &[]),
        (b"\treturn", &[1]),
        (b"_MASK", &[1]),
        (b"fooBarBaz", &[3, 6]),
        (b"x = __b", &[2, 4, 6]),
    ] {
        let cursor = index.cursor().extend_front(pattern);
        let probed: Vec<usize> = (1..pattern.len()).filter(|&i| cursor.probed(i)).collect();
        assert_eq!(probed, starts, "{pattern:?}");
        assert!(cursor.count() > 0, "{pattern:?}");
        assert_eq!(
            located(&index, pattern),
            naive_docs(&text, pattern),
            "{pattern:?}"
        );
    }
}

/// With every position sampled, a locate reads every occurrence's own
/// mark and probes nothing.
#[test]
fn own_marks_resolve_first() {
    for text in texts().into_iter().filter(|t| t.len() < 10_000) {
        let index = build_with(text.clone(), gaps(1, 1, 1)).unwrap();
        for pattern in patterns(&text) {
            let cursor = index.cursor().extend_front(&pattern);
            let mut docs = Vec::new();
            let stats = cursor.locate_stats(&mut |d| docs.push(d));
            docs.sort_unstable();
            assert_eq!(docs, naive_docs(&text, &pattern), "{pattern:?}");
            assert_eq!(cursor.forward_depth(), 0, "{pattern:?}");
            let n = docs.len() as u64;
            assert_eq!(
                (
                    stats.zero_step,
                    stats.forward,
                    stats.probes,
                    stats.walk_steps
                ),
                (n, 0, 0, 0),
                "{pattern:?}"
            );
        }
    }
}

#[test]
fn locate_stats_count_the_walks() {
    let mut forward = 0;
    for text in texts().into_iter().filter(|t| t.len() < 10_000) {
        for sampling in samplings() {
            let walks = naive_walks(&text, sampling);
            let index = build_with(text.clone(), sampling).unwrap();
            for pattern in patterns(&text) {
                let at = naive_positions(&text, &pattern);
                let cursor = index.cursor().extend_front(&pattern);
                let want = LocateStats {
                    occurrences: at.len() as u64,
                    walk_steps: at.iter().map(|&p| walks[p] as u64).sum(),
                    max_walk: at.iter().map(|&p| walks[p] as u32).max().unwrap_or(0),
                    zero_step: at.iter().filter(|&&p| walks[p] == 0).count() as u64,
                    ..LocateStats::default()
                };
                assert_eq!(
                    cursor.locate_at(0, &mut |_| ()),
                    want,
                    "{sampling:?} {pattern:?}"
                );
                let gated = cursor.locate_stats(&mut |_| ());
                let mut rows = at.clone();
                rows.sort_by(|&a, &b| text[a..].cmp(&text[b..]));
                let resolved = match at.len() {
                    0 => 0,
                    _ => cursor
                        .resolve(cursor.forward_depth(), &mut |_| ())
                        .map_or(0, |bits| {
                            (0..at.len())
                                .filter(|&k| bits[k / 64] >> (k % 64) & 1 == 1)
                                .filter(|&k| walks[rows[k]] != 0)
                                .count()
                        }),
                };
                assert_eq!(
                    (gated.occurrences, gated.forward, gated.zero_step),
                    (want.occurrences, resolved as u64, want.zero_step),
                    "{sampling:?} {pattern:?}"
                );
                assert!(gated.walk_steps <= want.walk_steps && gated.max_walk <= want.max_walk);
                forward += gated.forward;
            }
        }
    }
    assert!(forward > 0);
}

#[test]
fn locate_stats_count_the_probes() {
    let mut probes = 0;
    for text in texts().into_iter().filter(|t| t.len() < 10_000) {
        for sampling in samplings() {
            let marked = naive_marked(&text, sampling);
            let index = build_with(text.clone(), sampling).unwrap();
            for pattern in patterns(&text).into_iter().chain([b"\treturn".to_vec()]) {
                let at = naive_positions(&text, &pattern);
                let cursor = index.cursor().extend_front(&pattern);
                let depth = cursor.forward_depth();
                let marks_of = |p: &[u8]| {
                    naive_positions(&text, p)
                        .into_iter()
                        .filter(|&q| marked[q])
                        .count() as u64
                };
                let unresolved = at.iter().any(|&p| !marked[p]);
                let want: u64 = (1..=depth)
                    .filter(|&i| unresolved && naive_probed(&pattern, i))
                    .map(|i| marks_of(&pattern[i..]))
                    .sum();
                let got = cursor.locate_stats(&mut |_| ()).probes;
                assert_eq!(got, want, "{sampling:?} {pattern:?}");
                probes += got;
            }
        }
    }
    assert!(probes > 0);
}

/// The message of a caught panic.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

/// Runs a locate of `cursor`; a panic must be our own.
fn locate_or_corrupt(cursor: &Cursor<'_>, what: &str) {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cursor.locate(&mut |_| {});
    }));
    if let Err(panic) = run {
        let msg = panic_message(&*panic);
        assert!(msg.contains("corrupt docfm index"), "{what}: {msg}");
    }
}

/// Where the select samples of `h`'s marks start in the blob, and how many.
fn select_samples(h: &Header) -> (usize, usize) {
    let (at, len) = marks::samples_of(h.n + 1, h.num_samples);
    (h.marks_at + at, len)
}

#[test]
fn a_corrupt_mark_offset_stays_inside_the_interval() {
    let mut text = vec![DOC_END];
    text.extend([b'a'; 1024]);
    text.push(DOC_END);
    let blob = encode(&Built::build(text).unwrap());
    let h = Header::parse(&blob).unwrap();
    let (samples, num) = select_samples(&h);
    let mut viewed = Vec::new();
    for s in 0..num {
        let b = patched(&blob, samples + 4 * s, &0u32.to_le_bytes());
        let Ok(index) = FmIndex::view(b.as_slice()) else {
            continue;
        };
        viewed.push(s);
        for len in [1, 258, 300, 600] {
            let cursor = index.cursor().extend_front(&vec![b'a'; len]);
            let (lo, hi) = (cursor.lo, cursor.hi);
            let marks = index.header.marks(&b);
            for (row, _) in marks.range(lo, hi) {
                assert!(
                    (lo..hi).contains(&row),
                    "sample {s}, a^{len}: row {row} outside {lo}..{hi}"
                );
            }
            locate_or_corrupt(&cursor, &format!("sample {s}, a^{len}"));
        }
    }
    assert!(viewed.contains(&1), "viewed {viewed:?} of {num}");
}

#[test]
fn decreasing_mark_ranks_panic_as_corrupt() {
    let mut text = vec![DOC_END];
    text.extend([b'a'; 10_000]);
    text.push(DOC_END);
    text.extend([b'b'; 3000]);
    text.push(DOC_END);
    let blob = encode(&Built::build(text).unwrap());
    let h = Header::parse(&blob).unwrap();
    let (samples, _) = select_samples(&h);
    let (low, _) = marks::layout_of(h.n + 1, h.num_samples);
    let index = FmIndex::view(blob.as_slice()).unwrap();
    let (lo, hi) = {
        let cursor = index.cursor().extend_front(&[b'a'; 5000]);
        (cursor.lo, cursor.hi)
    };
    let (s_lo, s_hi) = (((lo >> low) - 1) / 64, ((hi >> low) - 1) / 64);
    assert!(s_lo < s_hi && hi <= h.n, "{lo}..{hi}");
    // The sample before `lo` points past the high bits: `lo` ranks as all.
    let b = patched(&blob, samples + 4 * s_lo, &u32::MAX.to_le_bytes());
    let index = FmIndex::view(b.as_slice()).unwrap();
    let cursor = index.cursor().extend_front(&[b'a'; 5000]);
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cursor.locate(&mut |_| {});
    }));
    let msg = panic_message(&*run.expect_err("decreasing ranks must panic"));
    assert!(msg.contains("corrupt docfm index"), "{msg}");
}

#[test]
fn more_marks_than_rows_panic_as_corrupt() {
    let mut text = vec![DOC_END];
    text.extend([b'a'; 1024]);
    text.push(DOC_END);
    let blob = encode(&Built::build(text).unwrap());
    let h = Header::parse(&blob).unwrap();
    let (samples, _) = select_samples(&h);
    // Sample 1 points past the high bits: one row ranks as 96 marks.
    let b = patched(&blob, samples + 4, &u32::MAX.to_le_bytes());
    let index = FmIndex::view(b.as_slice()).unwrap();
    let cursor = index.cursor().extend_front(b"a").rows(256..257);
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cursor.locate(&mut |_| {});
    }));
    let msg = panic_message(&*run.expect_err("more marks than rows must panic"));
    assert!(msg.contains("corrupt docfm index"), "{msg}");
}

#[path = "forward_tests.rs"]
mod forward_tests;

#[path = "wavelet_tests.rs"]
mod wavelet_tests;

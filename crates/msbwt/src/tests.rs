use std::fmt::Debug;
use std::panic::RefUnwindSafe;

use dicetest::prelude::*;

use super::*;

/// Switch levels the position-sorted levels are checked from.
const SA_LEVELS: [usize; 3] = [1, 2, 3];

const WIDTHS: [usize; 3] = [1, 2, 4];

/// Piece counts the parallel passes are checked at.
const PIECE_COUNTS: [usize; 4] = [1, 2, 3, 7];

/// [`Mode::Forced`]: names at least `width` bytes wide, every parallel pass
/// in `pieces` pieces when given, and levels from `sa_level` up sorted by
/// positions when given.
fn forced(width: usize, pieces: Option<usize>, sa_level: Option<usize>) -> Mode {
    Mode::Forced {
        width,
        pieces,
        sa_level: sa_level.unwrap_or(SA_LEVEL),
    }
}

/// [`bwt`] with names at least `width` bytes wide.
fn bwt_at(width: usize, text: Vec<u8>) -> Result<Vec<u8>, Error> {
    bwt_in(forced(width, None, None), text)
}

/// The BWT by definition: every rotation of every document, sorted by its
/// infinite repetition.
fn naive(docs: &[Vec<u8>]) -> Vec<u8> {
    let cyc: Vec<Vec<u8>> = docs
        .iter()
        .map(|d| d.iter().copied().chain([DOC_END]).collect())
        .collect();
    let at = |(d, i): (usize, usize), k: usize| cyc[d][(i + k) % cyc[d].len()];
    let mut rows: Vec<(usize, usize)> = cyc
        .iter()
        .enumerate()
        .flat_map(|(d, c)| (0..c.len()).map(move |i| (d, i)))
        .collect();
    rows.sort_by(|&a, &b| {
        // Two primitive periods agree forever once they agree this far.
        (0..cyc[a.0].len() + cyc[b.0].len())
            .map(|k| at(a, k).cmp(&at(b, k)))
            .find(|o| o.is_ne())
            .unwrap_or(Ordering::Equal)
    });
    rows.iter()
        .map(|&(d, i)| cyc[d][(i + cyc[d].len() - 1) % cyc[d].len()])
        .collect()
}

/// The documents of a multi-string BWT, sorted, by LF walks from each
/// terminator row; every row must lie on exactly one walk. A string whose
/// walks give back the input documents is their BWT: no other string
/// inverts to the same documents.
fn invert(bwt: &[u8]) -> Vec<Vec<u8>> {
    let mut c = [0usize; 256];
    for &b in bwt {
        c[b as usize] += 1;
    }
    let mut acc = 0;
    for v in &mut c {
        (*v, acc) = (acc, acc + *v);
    }
    let lf: Vec<usize> = bwt
        .iter()
        .map(|&b| {
            c[b as usize] += 1;
            c[b as usize] - 1
        })
        .collect();
    let mut seen = vec![false; bwt.len()];
    let terminators = bwt.iter().filter(|&&b| b == DOC_END).count();
    let mut docs: Vec<Vec<u8>> = (0..terminators)
        .map(|first| {
            let (mut row, mut doc) = (first, Vec::new());
            loop {
                assert!(!seen[row], "row {row} on two walks");
                seen[row] = true;
                if bwt[row] == DOC_END {
                    break;
                }
                doc.push(bwt[row]);
                row = lf[row];
            }
            doc.reverse();
            doc
        })
        .collect();
    assert!(seen.iter().all(|&s| s), "a row on no walk");
    docs.sort();
    docs
}

fn text(docs: &[Vec<u8>]) -> Vec<u8> {
    docs.iter()
        .flat_map(|d| d.iter().copied().chain([DOC_END]))
        .collect()
}

/// Against the definition, at every name width.
fn check(docs: &[Vec<u8>]) {
    let want = naive(docs);
    for width in WIDTHS {
        let got = bwt_at(width, text(docs)).unwrap();
        assert_eq!(got, want, "names of {width} bytes, docs {docs:?}");
    }
}

/// Against the definition, at every name width and piece count.
fn check_pieces(docs: &[Vec<u8>]) {
    let want = naive(docs);
    for width in WIDTHS {
        for pieces in PIECE_COUNTS {
            let got = bwt_in(forced(width, Some(pieces), None), text(docs)).unwrap();
            assert_eq!(
                got, want,
                "names of {width} bytes, {pieces} pieces, docs {docs:?}"
            );
        }
    }
}

/// Against the definition with levels from each switch level up sorted by
/// positions, at every name width.
fn check_sa_levels(docs: &[Vec<u8>]) {
    let want = naive(docs);
    for width in WIDTHS {
        for level in SA_LEVELS {
            let got = bwt_in(forced(width, None, Some(level)), text(docs)).unwrap();
            assert_eq!(
                got, want,
                "names of {width} bytes, positions from level {level}, docs {docs:?}"
            );
        }
    }
}

/// By inversion, for inputs too large for [`naive`]; every width must agree.
fn check_inverts(docs: &[Vec<u8>]) {
    let mut want = docs.to_vec();
    want.sort();
    let first = bwt_at(1, text(docs)).unwrap();
    assert_eq!(invert(&first), want);
    for width in [2, 4] {
        assert!(
            bwt_at(width, text(docs)).unwrap() == first,
            "names of {width} bytes differ from 1"
        );
    }
}

fn docs_die(sigma: u8, max_len: usize, max_docs: usize) -> impl Die<Vec<Vec<u8>>> {
    dice::outer_vec(dice::vec(dice::u8(1..=sigma), 0..=max_len), 1..=max_docs)
}

/// What one document of [`source_die`] is.
#[derive(Clone, Copy, Debug)]
enum SourceDoc {
    /// An exact copy of an earlier document, or for the first document
    /// [`SourceDoc::SharedHeader`].
    CopyOfEarlier,
    /// Bytes that look nothing like source.
    Binary,
    /// The shared header, then words.
    SharedHeader,
    /// Words alone.
    WordsOnly,
}

/// Source-like documents: words from a small vocabulary between
/// punctuation and newlines, a shared header on some, exact copies of
/// earlier documents, and the odd binary-looking one.
fn source_die(max_words: usize, max_docs: usize) -> impl Die<Vec<Vec<u8>>> {
    let kind_die = dice::weighted_one_of().four(
        (1, SourceDoc::CopyOfEarlier),
        (1, SourceDoc::Binary),
        (3, SourceDoc::SharedHeader),
        (3, SourceDoc::WordsOnly),
    );
    dice::from_fn(move |mut fate| {
        let vocab = fate.roll(dice::vec(dice::vec(dice::u8(b'a'..=b'h'), 1..=7), 1..=30));
        let separators = b" \n\n();_{}";
        let words = |fate: &mut Fate, count: usize| {
            let mut out = Vec::new();
            for _ in 0..count {
                out.extend_from_slice(&vocab[fate.roll(dice::uni_usize(0..vocab.len()))]);
                out.push(separators[fate.roll(dice::uni_usize(0..separators.len()))]);
            }
            out
        };
        let header_words = fate.roll(dice::usize(0..=40));
        let header = words(&mut fate, header_words);
        let num_docs = fate.roll(dice::usize(1..=max_docs));
        let mut docs: Vec<Vec<u8>> = Vec::with_capacity(num_docs);
        for i in 0..num_docs {
            let kind = fate.roll(&kind_die);
            hint!("doc {i}: {kind:?}");
            let doc = match kind {
                SourceDoc::CopyOfEarlier if !docs.is_empty() => {
                    docs[fate.roll(dice::uni_usize(0..docs.len()))].clone()
                }
                SourceDoc::Binary => fate.roll(dice::vec(dice::u8(1..=255), 1..=200)),
                SourceDoc::WordsOnly => {
                    let count = fate.roll(dice::usize(0..=max_words));
                    words(&mut fate, count)
                }
                SourceDoc::CopyOfEarlier | SourceDoc::SharedHeader => {
                    let mut doc = header.clone();
                    let count = fate.roll(dice::usize(0..=max_words));
                    doc.extend(words(&mut fate, count));
                    doc
                }
            };
            docs.push(if doc.is_empty() { vec![b'x'] } else { doc });
        }
        docs
    })
}

#[test]
fn matches_naive_small_alphabet() {
    Dicetest::repeatedly()
        .passes(2000)
        .run(|mut fate| check(&fate.roll(docs_die(3, 12, 8))));
}

#[test]
fn matches_naive_binary_long_docs() {
    // Long documents over two symbols recurse deepest.
    Dicetest::repeatedly()
        .passes(200)
        .run(|mut fate| check(&fate.roll(docs_die(2, 400, 6))));
}

#[test]
fn matches_naive_bytes() {
    Dicetest::repeatedly()
        .passes(400)
        .run(|mut fate| check(&fate.roll(docs_die(255, 60, 10))));
}

#[test]
fn matches_naive_source_like() {
    Dicetest::repeatedly()
        .passes(300)
        .run(|mut fate| check(&fate.roll(source_die(40, 6))));
}

#[test]
fn matches_naive_repeats() {
    // Identical documents, periodic documents, long runs.
    Dicetest::repeatedly().passes(1000).run(|mut fate| {
        let unit = fate.roll(dice::vec(dice::u8(1..=3), 1..=4));
        let docs = fate.roll(dice::vec(
            dice::zip().two(dice::u8(1..=8), dice::bool()),
            1..=6,
        ));
        let docs: Vec<Vec<u8>> = docs
            .into_iter()
            .map(|(reps, tail)| {
                let mut d = unit.repeat(reps as usize);
                if tail {
                    d.push(unit[0]);
                }
                d
            })
            .collect();
        check(&docs);
    });
}

#[test]
fn pieces_match_naive_small_alphabet() {
    Dicetest::repeatedly()
        .passes(500)
        .run(|mut fate| check_pieces(&fate.roll(docs_die(3, 40, 8))));
}

#[test]
fn pieces_match_naive_source_like() {
    Dicetest::repeatedly()
        .passes(100)
        .run(|mut fate| check_pieces(&fate.roll(source_die(60, 6))));
}

/// Documents of runs of one symbol, some longer than a bitmap word: types
/// hidden past a piece's end by a run, and pieces inside one run.
fn runs_die(max_runs: usize, max_docs: usize) -> impl Die<Vec<Vec<u8>>> {
    let len = dice::one_of_die().two(dice::usize(1..=4), dice::usize(1..=200));
    let doc = dice::vec(dice::zip().two(dice::u8(1..=3), len), 1..=max_runs).map(|runs| {
        runs.into_iter()
            .flat_map(|(sym, len)| std::iter::repeat_n(sym, len))
            .collect::<Vec<u8>>()
    });
    dice::vec(doc, 1..=max_docs)
}

#[test]
fn pieces_match_naive_long_runs() {
    Dicetest::repeatedly()
        .passes(150)
        .run(|mut fate| check_pieces(&fate.roll(runs_die(8, 4))));
}

#[test]
fn pieces_invert_large() {
    // Large enough that pieces of whole bitmap words split every pass.
    Dicetest::repeatedly().passes(10).run(|mut fate| {
        let docs = fate.roll(source_die(3000, 40));
        let mut want = docs.clone();
        want.sort();
        let first = bwt_at(1, text(&docs)).unwrap();
        assert_eq!(invert(&first), want);
        for width in WIDTHS {
            for pieces in PIECE_COUNTS {
                assert!(
                    bwt_in(forced(width, Some(pieces), None), text(&docs)).unwrap() == first,
                    "names of {width} bytes, {pieces} pieces"
                );
            }
        }
    });
}

#[test]
fn inverts_large_source_like() {
    // Thousands of names per level: wide names for real, not just forced.
    Dicetest::repeatedly()
        .passes(60)
        .run(|mut fate| check_inverts(&fate.roll(source_die(3000, 40))));
}

#[test]
fn inverts_large_random() {
    Dicetest::repeatedly()
        .passes(40)
        .run(|mut fate| check_inverts(&fate.roll(docs_die(255, 20000, 20))));
}

/// Packed keys are equal exactly when the substrings and finals are.
fn packs_injectively(sym: impl Die<u8> + RefUnwindSafe) {
    let sub = dice::zip().two(dice::vec(&sym, 2..=7), &sym);
    Dicetest::repeatedly().passes(5000).run(|mut fate| {
        let (a, fa) = fate.roll(&sub);
        let (b, fb) = fate.roll(&sub);
        if let (Some(ka), Some(kb)) = (pack(&a, fa), pack(&b, fb)) {
            assert_eq!(
                ka == kb,
                (&a, fa) == (&b, fb),
                "{a:?} {fa:?} / {b:?} {fb:?}"
            );
        }
    });
}

#[test]
fn packing_is_injective() {
    packs_injectively(dice::u8(0..=2));
    packs_injectively(dice::one_of_die().two(dice::u8(0..=2), dice::u8(..)));
}

#[test]
fn packing_in_place_is_packing() {
    Dicetest::repeatedly().passes(5000).run(|mut fate| {
        let seq = fate.roll(dice::vec(dice::u8(..), 2..=20));
        let start = fate.roll(dice::uni_usize(0..seq.len() - 1));
        let end = fate.roll(dice::uni_usize(start + 1..=seq.len()));
        let fin = fate.roll(dice::u8(..));
        assert_eq!(pack_in(&seq, start, end, fin), pack(&seq[start..end], fin));
    });
}

fn fields<T>(n: &Named<T>) -> (&[T], &[u32], &[u32], usize, &[u32]) {
    (&n.alphabet, &n.offsets, &n.loopers, n.term_names, &n.next)
}

/// Every level from `seq` up, at least `width` bytes wide: sorting
/// `chunk` records at a time names as the build does, which at a level of
/// bytes hashes.
fn same_names<T: Sym + Debug>(
    seq: Vec<T>,
    num_sym: usize,
    num_term: usize,
    width: usize,
    chunks: &[usize],
) {
    if num_term >= num_sym {
        return;
    }
    let (star, _) = types(&seq, num_sym, num_term, Mode::Normal);
    let want = T::name(&seq, &star, num_sym, num_term);
    for &chunk in chunks {
        let got = name_by_sorting(&seq, &star, num_sym, num_term, chunk);
        assert_eq!(
            fields(&want),
            fields(&got),
            "{chunk} records at a time, seq {seq:?}"
        );
    }
    let (names, term_names) = (want.offsets.len() - 1, want.term_names);
    let next = want.next;
    match forced(width, None, None).width(names) {
        4 => same_names(u32::narrow(next), names, term_names, width, chunks),
        2 => same_names(u16::narrow(next), names, term_names, width, chunks),
        _ => same_names(u8::narrow(next), names, term_names, width, chunks),
    }
}

fn same_names_at_widths(docs: &[Vec<u8>], chunks: &[usize]) {
    for width in WIDTHS {
        let mut seq = text(docs);
        seq.rotate_right(1);
        same_names(seq, 256, 1, width, chunks);
    }
}

#[test]
fn namings_agree() {
    Dicetest::repeatedly().passes(300).run(|mut fate| {
        let chunks = [fate.roll(dice::usize(1..=64)), 1 << 16];
        same_names_at_widths(&fate.roll(source_die(200, 8)), &chunks);
        same_names_at_widths(&fate.roll(docs_die(3, 200, 8)), &chunks);
        same_names_at_widths(&fate.roll(docs_die(255, 60, 8)), &chunks);
    });
    // Substrings longer than many keys.
    let docs = [
        vec![7; 3000],
        vec![7; 2999],
        [vec![7; 1000], vec![6]].concat(),
    ];
    same_names_at_widths(&docs, &[4]);
}

#[test]
fn namings_agree_large() {
    // Thousands of names: tables that grow, buckets of many records.
    Dicetest::repeatedly()
        .passes(10)
        .run(|mut fate| same_names_at_widths(&fate.roll(source_die(3000, 20)), &[1 << 16, 999]));
}

#[test]
fn edge_cases() {
    check(&[vec![1]]);
    check(&[vec![1], vec![1], vec![1]]);
    check(&[vec![5, 5, 5, 5]]);
    check(&[b"banana".to_vec(), b"ananas".to_vec(), b"banana".to_vec()]);
    check_pieces(&[vec![1]]);
    check_pieces(&[vec![2; 300], vec![1; 129], vec![2; 64]]);
    assert_eq!(bwt(Vec::new()), Ok(Vec::new()));
    assert_eq!(bwt(b"ab".to_vec()), Err(Error::Unterminated));
    // Empty documents: alone, first, last, between, several in a row.
    check(&[vec![]]);
    check(&[vec![], vec![1, 2]]);
    check(&[vec![1, 2], vec![]]);
    check(&[vec![1], vec![], vec![], vec![2, 1], vec![]]);
    check(&[vec![], vec![], vec![]]);
    check_sa_levels(&[vec![1], vec![], vec![], vec![2, 1], vec![]]);
}

#[test]
fn sa_levels_match_naive_small_alphabet() {
    Dicetest::repeatedly()
        .passes(1500)
        .run(|mut fate| check_sa_levels(&fate.roll(docs_die(3, 12, 8))));
}

#[test]
fn sa_levels_match_naive_binary_long_docs() {
    Dicetest::repeatedly()
        .passes(150)
        .run(|mut fate| check_sa_levels(&fate.roll(docs_die(2, 400, 6))));
}

#[test]
fn sa_levels_match_naive_source_like() {
    Dicetest::repeatedly()
        .passes(200)
        .run(|mut fate| check_sa_levels(&fate.roll(source_die(40, 6))));
}

#[test]
fn sa_levels_invert_large() {
    // Thousands of names per level; every switch level gives the same BWT.
    Dicetest::repeatedly().passes(40).run(|mut fate| {
        let docs = fate.roll(source_die(3000, 40));
        let mut want = docs.clone();
        want.sort();
        let groups = bwt_at(1, text(&docs)).unwrap();
        assert_eq!(invert(&groups), want);
        for level in SA_LEVELS {
            assert!(
                bwt_in(forced(1, None, Some(level)), text(&docs)).unwrap() == groups,
                "positions from level {level} differ from groups"
            );
        }
    });
}

/// Every rotation of every document, sorted as [`naive`] sorts them: each
/// as (document, offset into the document and its terminator).
fn naive_rows(docs: &[Vec<u8>]) -> Vec<(usize, usize)> {
    let cyc = cyclic(docs);
    let at = |(d, i): (usize, usize), k: usize| cyc[d][(i + k) % cyc[d].len()];
    let mut rows: Vec<(usize, usize)> = cyc
        .iter()
        .enumerate()
        .flat_map(|(d, c)| (0..c.len()).map(move |i| (d, i)))
        .collect();
    rows.sort_by(|&a, &b| {
        (0..cyc[a.0].len() + cyc[b.0].len())
            .map(|k| at(a, k).cmp(&at(b, k)))
            .find(|o| o.is_ne())
            .unwrap_or(Ordering::Equal)
    });
    rows
}

fn cyclic(docs: &[Vec<u8>]) -> Vec<Vec<u8>> {
    docs.iter()
        .map(|d| d.iter().copied().chain([DOC_END]).collect())
        .collect()
}

/// [`bwt_sampled`] at a name width, pieces and switch level.
fn sampled_with(
    width: usize,
    pieces: Option<usize>,
    sa_level: Option<usize>,
    text: Vec<u8>,
    bits: &[u64],
) -> (Vec<u8>, Vec<(u32, u32)>) {
    let (bwt, rows) = bwt_sampled_in(forced(width, pieces, sa_level), text, bits).unwrap();
    let rows = rows.iter().map(|&r| ((r >> 32) as u32, r as u32)).collect();
    (bwt, rows)
}

/// The sampled rows of `docs` against the definition: the BWT as [`naive`],
/// each sampled position reported once, each row at most once, and every
/// pair's rotation the naive row's (identical rotations may trade rows).
fn check_rows(docs: &[Vec<u8>], keep: impl Fn(usize) -> bool) {
    let cyc = cyclic(docs);
    let rows = naive_rows(docs);
    let mut start = Vec::with_capacity(docs.len());
    let mut n = 0;
    for c in &cyc {
        start.push(n);
        n += c.len();
    }
    let rotation = |(d, i): (usize, usize)| -> Vec<u8> {
        cyc[d][i..].iter().chain(&cyc[d][..i]).copied().collect()
    };
    let of = |p: usize| {
        let d = start.partition_point(|&s| s <= p) - 1;
        (d, p - start[d])
    };
    let mut bits = vec![0u64; n.div_ceil(64)];
    let want: Vec<usize> = (0..n).filter(|&p| keep(p)).collect();
    for &p in &want {
        bits[p / 64] |= 1 << (p % 64);
    }
    for width in WIDTHS {
        for (pieces, sa_level) in [
            (None, None),
            (Some(3), None),
            (None, Some(1)),
            (Some(2), Some(3)),
        ] {
            let (bwt, mut found) = sampled_with(width, pieces, sa_level, text(docs), &bits);
            let label =
                format!("width {width}, pieces {pieces:?}, level {sa_level:?}, docs {docs:?}");
            assert_eq!(bwt, naive(docs), "{label}");
            found.sort_unstable_by_key(|&(_, p)| p);
            let positions: Vec<usize> = found.iter().map(|&(_, p)| p as usize).collect();
            assert_eq!(positions, want, "{label}");
            let mut seen = vec![false; n];
            for &(row, p) in &found {
                assert!(
                    !std::mem::replace(&mut seen[row as usize], true),
                    "row {row} twice, {label}"
                );
                assert_eq!(
                    rotation(of(p as usize)),
                    rotation(rows[row as usize]),
                    "position {p} at row {row}, {label}"
                );
            }
        }
    }
}

#[test]
fn every_row_matches_naive() {
    Dicetest::repeatedly()
        .passes(600)
        .run(|mut fate| check_rows(&fate.roll(docs_die(3, 12, 8)), |_| true));
}

#[test]
fn sampled_rows_match_naive() {
    Dicetest::repeatedly().passes(400).run(|mut fate| {
        let docs = fate.roll(source_die(30, 6));
        let seed = fate.roll(dice::u64(..));
        check_rows(&docs, |p| (p as u64).wrapping_mul(seed | 1) >> 61 == 0);
    });
}

#[test]
fn rows_of_long_docs_match_naive() {
    // Deep recursion: positions carried through many levels.
    Dicetest::repeatedly()
        .passes(80)
        .run(|mut fate| check_rows(&fate.roll(docs_die(2, 300, 5)), |_| true));
}

/// Documents of random words from a vocabulary, all different: enough
/// repeats that the parallel passes split, as `words` sets how many names.
fn salad(seed: u64, words: usize, word_len: usize, docs: usize, doc_words: usize) -> Vec<Vec<u8>> {
    let mut x = seed;
    let mut next = move |below: usize| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x >> 16) as usize % below
    };
    let vocab: Vec<Vec<u8>> = (0..words)
        .map(|_| (0..word_len).map(|_| 1 + next(255) as u8).collect())
        .collect();
    (0..docs)
        .map(|_| {
            (0..doc_words)
                .flat_map(|_| vocab[next(words)].iter().copied())
                .collect()
        })
        .collect()
}

/// A build as callers get it, on a pool of four threads: by inversion, and
/// the sampled rows against one thread's.
fn check_on_four_threads(docs: &[Vec<u8>]) {
    let pool = |threads| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
    };
    let (four, one) = (pool(4), pool(1));
    let n = text(docs).len();
    let mut want = docs.to_vec();
    want.sort();
    let got = four.install(|| bwt(text(docs))).unwrap();
    assert!(invert(&got) == want, "the BWT does not invert to the docs");

    let bits = vec![0x0101_0101_0101_0101u64; n.div_ceil(64)];
    let (sampled, mut rows) = four.install(|| bwt_sampled(text(docs), &bits)).unwrap();
    assert!(sampled == got, "bwt_sampled's BWT differs from bwt's");
    let bytes = text(docs);
    for &r in &rows {
        let (row, p) = ((r >> 32) as usize, r as u32 as usize);
        if p > 0 && bytes[p - 1] != DOC_END {
            assert_eq!(got[row], bytes[p - 1], "the symbol before position {p}");
        }
    }
    let (_, mut alone) = one.install(|| bwt_sampled(text(docs), &bits)).unwrap();
    rows.sort_unstable();
    alone.sort_unstable();
    assert!(rows == alone, "four threads' rows differ from one's");
}

#[test]
fn four_threads_wide_names() {
    // More than 256 and at most 65536 names at level 0.
    check_on_four_threads(&salad(0x9e37_79b9_7f4a_7c15, 200, 16, 40, 3000));
}

#[test]
fn four_threads_widest_names() {
    // More than 65536 names at level 0, each several times over.
    check_on_four_threads(&salad(0x1234_5678_9abc_def1, 8000, 16, 40, 3000));
}

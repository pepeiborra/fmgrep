use super::*;

/// Sorted docs of a locate resolving from tails `1..=depth`, and how many
/// resolved that way.
fn at_depth(cursor: &Cursor<'_>, depth: usize) -> (Vec<usize>, usize) {
    let mut got = Vec::new();
    cursor.locate_at(depth, &mut |d| got.push(d));
    got.sort_unstable();
    let resolved = match cursor.count() {
        0 => None,
        _ => cursor.resolve(depth, &mut |_| ()),
    };
    let forward = resolved.map_or(0, |bits| bits.iter().map(|w| w.count_ones() as usize).sum());
    (got, forward)
}

fn gated(cursor: &Cursor<'_>) -> Vec<usize> {
    let mut got = Vec::new();
    cursor.locate(&mut |d| got.push(d));
    got.sort_unstable();
    got
}

/// Every depth on small texts, a few on large ones.
fn depths(text: &[u8]) -> Vec<usize> {
    if text.len() < 10_000 {
        (0..=forward::MAX_DEPTH).collect()
    } else {
        vec![0, 1, 7, forward::MAX_DEPTH]
    }
}

/// [`patterns`] plus random lengths up to 12, doc-start and doc-end ones,
/// separator-spanning, text-prefix tails and the empty pattern.
fn forward_patterns(text: &[u8], rng: &mut Rng) -> Vec<Vec<u8>> {
    let mut out = patterns(text);
    out.push(Vec::new());
    let len = text.len() as u64;
    for _ in 0..12 {
        let start = rng.below(len) as usize;
        let n = 1 + rng.below(12) as usize;
        out.push(text[start..(start + n).min(text.len())].to_vec());
    }
    let ends: Vec<usize> = (0..text.len())
        .filter(|&i| text[i] == DOC_END)
        .take(4)
        .collect();
    for &end in &ends {
        let start = end.saturating_sub(5);
        out.push(text[start..end].to_vec());
        out.push(text[start..(end + 3).min(text.len())].to_vec());
        out.push(text[end + 1..(end + 7).min(text.len())].to_vec());
    }
    for k in 1..=4.min(text.len()) {
        for x in [b'a', 1, b'x'] {
            out.push([&[x], &text[..k]].concat());
        }
    }
    out
}

#[test]
fn forward_depths_equal_naive() {
    let mut rng = Rng(23);
    for text in texts() {
        let want: Vec<(Vec<u8>, Vec<usize>)> = forward_patterns(&text, &mut rng)
            .into_iter()
            .map(|p| {
                let docs = naive_docs(&text, &p);
                (p, docs)
            })
            .collect();
        for sampling in samplings() {
            let index = build_with(text.clone(), sampling).unwrap();
            for (pattern, docs) in &want {
                let cursor = index.cursor().extend_front(pattern);
                for depth in depths(&text) {
                    let (got, _) = at_depth(&cursor, depth);
                    assert_eq!(&got, docs, "{sampling:?} depth {depth} pattern {pattern:?}");
                }
                assert_eq!(&located(&index, pattern), docs, "{sampling:?} {pattern:?}");
            }
        }
    }
}

/// Tails of the pattern that start the text sit on the sentinel row.
#[test]
fn forward_walk_stops_at_the_text_start() {
    let mut rng = Rng(41);
    for _ in 0..200 {
        let text: Vec<u8> = (0..2 + rng.below(10))
            .map(|_| b'a' + rng.below(2) as u8)
            .chain([DOC_END])
            .collect();
        for rate in [1, 2, 3, 4] {
            let index = build_with(text.clone(), gaps(rate, rate, rate)).unwrap();
            for k in 1..text.len() - 1 {
                for x in *b"ab" {
                    let pattern = [&[x], &text[..k]].concat();
                    let (got, _) = at_depth(&index.cursor().extend_front(&pattern), k);
                    assert_eq!(
                        got,
                        naive_docs(&text, &pattern),
                        "rate {rate} {text:?} {pattern:?}"
                    );
                }
            }
        }
    }
}

/// The gate's depth from the text alone: probed tails' marks, its
/// occurrences at a `marked` position, against the unmarked occurrences.
fn naive_depth(text: &[u8], pattern: &[u8], gaps: Gaps, marked: &[bool]) -> usize {
    let known = pattern.len().min(RING);
    let cap = (0..known)
        .find(|&j| pattern[j] == DOC_END)
        .unwrap_or(pattern.len().saturating_sub(1));
    let (num, den) = (text.len(), marked.iter().filter(|&&m| m).count());
    let depth = (gaps.max as usize / 2).saturating_sub(1);
    let limit = cap.min(depth).min(forward::MAX_DEPTH);
    let at = naive_positions(text, pattern);
    let count = at.iter().filter(|&&p| !marked[p]).count();
    (1..=limit)
        .filter(|&i| count > 0 && naive_probed(pattern, i))
        .take_while(|&i| {
            let marks = naive_positions(text, &pattern[i..])
                .into_iter()
                .filter(|&p| marked[p])
                .count();
            marks * num <= 32 * count * den
        })
        .last()
        .unwrap_or(0)
}

#[test]
fn mark_gate_matches_naive() {
    let mut rng = Rng(59);
    for text in texts() {
        let patterns = forward_patterns(&text, &mut rng);
        for sampling in samplings() {
            let index = build_with(text.clone(), sampling).unwrap();
            let marked = naive_marked(&text, sampling);
            for pattern in patterns.iter().filter(|p| count(&index, p) > 0) {
                assert_eq!(
                    index.cursor().extend_front(pattern).forward_depth(),
                    naive_depth(&text, pattern, sampling, &marked),
                    "{sampling:?} {pattern:?}"
                );
            }
        }
    }
    let depth = |text: &[u8], pattern: &[u8], rate| {
        let index = build_with(text.to_vec(), gaps(rate, rate, rate)).unwrap();
        index.cursor().extend_front(pattern).forward_depth()
    };
    let text = [b"x a b c d\0".repeat(4), b" b c d\0".repeat(4)].concat();
    assert_eq!(depth(&text, b"c d\0x a", 32), 2, "capped at the separator");
    assert_eq!(depth(&text, b"\0x a", 32), 0);
    assert_eq!(depth(&text, b"d", 32), 0, "one byte has no tail");
    assert_eq!(depth(&text, b" b c d", 1), 0, "gap 1 has no depth");
    assert_eq!(depth(&text, b" b c d", 4), 1, "max / 2 - 1");
    assert_eq!(depth(&text, b" b c d", 32), 5);
    // Every tiny "b" doc start is a mark: marks fail the gate where the
    // width passes.
    let long = [b" x b".as_slice(), &[b'y'; 200], b"\0"].concat();
    assert_eq!(depth(&[&long[..], b"b\0"].concat(), b"x b", 32), 2);
    assert_eq!(depth(&[&long[..], b"b\0b\0"].concat(), b"x b", 32), 0);
}

/// Some occurrences resolve forward, none twice.
#[test]
fn forward_resolves_occurrences() {
    let mut rng = Rng(29);
    let (mut deepest, mut gate) = (0, 0);
    for text in texts().into_iter().filter(|t| t.len() < 10_000) {
        for sampling in samplings() {
            let index = build_with(text.clone(), sampling).unwrap();
            for pattern in forward_patterns(&text, &mut rng) {
                let cursor = index.cursor().extend_front(&pattern);
                let (docs, forward) = at_depth(&cursor, forward::MAX_DEPTH);
                assert!(forward <= docs.len(), "{sampling:?} {pattern:?}");
                deepest += forward;
                gate += at_depth(&cursor, cursor.forward_depth()).1;
            }
        }
    }
    assert!(deepest > 0 && gate > 0, "{deepest} {gate}");
}

#[test]
fn marks_range_matches_get_rank() {
    let mut rng = Rng(31);
    for len in [0usize, 1, 4095, 4096, 4097, 70_001] {
        for fill in 0..4 {
            let bits: Vec<bool> = (0..len)
                .map(|i| match fill {
                    0 => rng.below(2) == 1,
                    1 => true,
                    2 => false,
                    _ => i % 32 == 0,
                })
                .collect();
            let mut words = vec![0u64; len.div_ceil(64) + 1];
            for (i, _) in bits.iter().enumerate().filter(|&(_, &b)| b) {
                words[i / 64] |= 1 << (i % 64);
            }
            let ones = bits.iter().filter(|&&b| b).count();
            let mut out = vec![0u8; marks::section_bytes(len, ones)];
            marks::write(&mut out, len, &words);
            let marks = marks::Marks::new(&out, len, ones);
            let mut ranges = vec![
                (0, 0),
                (0, 1),
                (0, len),
                (4095, 4097),
                (100, 8000),
                (len.saturating_sub(1), len + 5000),
                (len + 10, len + 20),
                (7, 3),
            ];
            ranges.extend((0..20).map(|_| {
                let lo = rng.below(len as u64 + 1) as usize;
                (lo, lo + rng.below(9000) as usize)
            }));
            for (lo, hi) in ranges {
                let want: Vec<(usize, usize)> = (lo..hi.min(len))
                    .filter(|&r| marks.get_rank(r).0)
                    .map(|r| (r, marks.get_rank(r).1))
                    .collect();
                let got: Vec<(usize, usize)> = marks.range(lo, hi).collect();
                assert_eq!(got, want, "len {len} fill {fill} {lo}..{hi}");
            }
        }
    }
}

#[test]
fn occ_access_matches_access_rank() {
    let mut rng = Rng(37);
    for n in [1, 63, 64, 4095, 4096, 4097, 70_000] {
        for sigma in [1usize, 2, 3, 100, 256] {
            let codes = occ_codes(n, sigma, &mut rng);
            let mut alphabet = [0u64; 4];
            for b in 0..sigma {
                alphabet[b / 64] |= 1 << (b % 64);
            }
            let h = layout::Header::new(n, 0, Gaps::WORDS, alphabet, [0; 257], 0, 0);
            let mut blob = vec![0u8; h.blob_len];
            occ::write_occ(&codes, &h, &mut blob);
            let bwt = occ::Occ::new(&blob, &h);
            for i in 0..n {
                assert_eq!(
                    bwt.access(i),
                    bwt.access_rank(i).0,
                    "n {n} sigma {sigma} i {i}"
                );
            }
        }
    }
}

/// `pattern` prepended one byte at a time, or in random chunks if `rng`.
fn chained<'a>(index: &'a Built, pattern: &[u8], rng: Option<&mut Rng>) -> Cursor<'a> {
    let mut cursor = index.cursor();
    let mut end = pattern.len();
    let mut rng = rng;
    while end > 0 {
        let n = match rng.as_deref_mut() {
            Some(rng) => 1 + rng.below(end as u64) as usize,
            None => 1,
        };
        cursor = cursor.extend_front(&pattern[end - n..end]);
        end -= n;
    }
    cursor
}

/// [`forward_patterns`] plus chains past the ring, separator-spanning
/// ones and dead branches.
fn chain_patterns(text: &[u8], rng: &mut Rng) -> Vec<Vec<u8>> {
    let mut out = forward_patterns(text, rng);
    let len = text.len() as u64;
    for _ in 0..8 {
        let start = rng.below(len) as usize;
        let n = 16 + rng.below(16) as usize;
        let tail = text[start..(start + n).min(text.len())].to_vec();
        out.push([b"\xfe".as_slice(), &tail].concat());
        out.push([&tail[..1], &tail].concat());
        out.push(tail);
    }
    out
}

#[test]
fn cursor_chains_equal_naive() {
    let mut rng = Rng(43);
    for text in texts() {
        let patterns = chain_patterns(&text, &mut rng);
        for sampling in samplings() {
            let index = build_with(text.clone(), sampling).unwrap();
            for pattern in &patterns {
                let docs = naive_docs(&text, pattern);
                for cursor in [
                    chained(&index, pattern, None),
                    chained(&index, pattern, Some(&mut rng)),
                ] {
                    for depth in depths(&text) {
                        let (got, _) = at_depth(&cursor, depth);
                        assert_eq!(got, docs, "{sampling:?} depth {depth} pattern {pattern:?}");
                    }
                    assert_eq!(gated(&cursor), docs, "{sampling:?} {pattern:?}");
                }
            }
        }
    }
}

#[test]
fn cursor_chunking_does_not_change_locates() {
    let mut rng = Rng(47);
    for text in texts().into_iter().filter(|t| t.len() < 10_000) {
        let patterns = chain_patterns(&text, &mut rng);
        for sampling in samplings() {
            let index = build_with(text.clone(), sampling).unwrap();
            for pattern in &patterns {
                let whole = index.cursor().extend_front(pattern);
                for cursor in [
                    chained(&index, pattern, None),
                    chained(&index, pattern, Some(&mut rng)),
                ] {
                    assert_eq!(cursor.forward_depth(), whole.forward_depth(), "{pattern:?}");
                    for depth in [0, 3, forward::MAX_DEPTH] {
                        assert_eq!(
                            at_depth(&cursor, depth),
                            at_depth(&whole, depth),
                            "{sampling:?} depth {depth} {pattern:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn rows_drops_the_ancestors() {
    let mut rng = Rng(53);
    let mut rebuilt = 0;
    for (text, sampling) in texts()
        .into_iter()
        .filter(|t| t.len() < 10_000)
        .flat_map(|t| samplings().into_iter().map(move |s| (t.clone(), s)))
    {
        let index = build_with(text.clone(), sampling).unwrap();
        for pattern in forward_patterns(&text, &mut rng) {
            let cursor = chained(&index, &pattern, None);
            for within in [0..1, 1..3, 2..u64::MAX] {
                let narrowed = cursor.rows(within.clone());
                for extended in [
                    narrowed,
                    narrowed.extend_front(&pattern[..1.min(pattern.len())]),
                ] {
                    let (want, _) = at_depth(&extended, 0);
                    assert_eq!(extended.forward_depth(), 0, "{pattern:?} rows {within:?}");
                    assert_eq!(
                        at_depth(&extended, forward::MAX_DEPTH),
                        (want.clone(), 0),
                        "{pattern:?} rows {within:?}"
                    );
                    assert_eq!(gated(&extended), want, "{pattern:?} rows {within:?}");
                }
                // Tails rebuilt after `rows` resolve forward again.
                let Some(ahead) = text
                    .windows(pattern.len() + 3)
                    .find(|w| w.ends_with(&pattern))
                    .map(|w| &w[..3])
                else {
                    continue;
                };
                let extended = narrowed.extend_front(ahead);
                let (want, _) = at_depth(&extended, 0);
                let (docs, forward) = at_depth(&extended, forward::MAX_DEPTH);
                assert_eq!(docs, want, "{ahead:?} {pattern:?} rows {within:?}");
                assert_eq!(
                    gated(&extended),
                    want,
                    "{ahead:?} {pattern:?} rows {within:?}"
                );
                rebuilt += forward;
            }
        }
    }
    assert!(rebuilt > 0);
}

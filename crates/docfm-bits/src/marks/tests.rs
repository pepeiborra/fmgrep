use dicetest::prelude::*;

use super::*;
use crate::test_util::Rng;

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
            let mut out = vec![0u8; section_bytes(len, ones)];
            assert_eq!(
                write(&mut out, len, &words),
                ones,
                "bits past len are ignored"
            );
            let marks = Marks::new(&out, len, ones);
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
fn marks_range_matches_get_rank() {
    let mut rng = Rng(31);
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
            let ones = bits.iter().filter(|&&b| b).count();
            let mut out = vec![0u8; section_bytes(len, ones)];
            write(&mut out, len, &words);
            let marks = Marks::new(&out, len, ones);
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

/// `get_rank`, `range` and `total` against the bitmap `bits`.
fn check_marks(bits: &[bool], ranges: &[(usize, usize)]) {
    let len = bits.len();
    let ones = bits.iter().filter(|&&b| b).count();
    let mut words = vec![0u64; len.div_ceil(64)];
    for (i, _) in bits.iter().enumerate().filter(|&(_, &b)| b) {
        words[i / 64] |= 1 << (i % 64);
    }
    let mut out = vec![0u8; section_bytes(len, ones)];
    assert_eq!(write(&mut out, len, &words), ones);
    let marks = Marks::new(&out, len, ones);
    assert_eq!(marks.total(), ones);
    let mut naive = Vec::with_capacity(ones);
    let mut before = 0;
    for (i, &bit) in bits.iter().enumerate() {
        assert_eq!(marks.get_rank(i), (bit, before), "row {i}");
        if bit {
            naive.push((i, before));
            before += 1;
        }
    }
    assert_eq!(marks.get_rank(len), (false, ones));
    assert_eq!(marks.get_rank(usize::MAX), (false, ones));
    for &(lo, hi) in ranges {
        let want: Vec<_> = naive
            .iter()
            .copied()
            .filter(|&(row, _)| lo <= row && row < hi)
            .collect();
        assert_eq!(
            marks.range(lo, hi).collect::<Vec<_>>(),
            want,
            "{lo}..{hi} of {len}"
        );
    }
}

#[test]
fn marks_match_a_naive_bitmap() {
    Dicetest::repeatedly().passes(3000).run(|mut fate| {
        let rows = fate.roll(dice::usize(0..=1500));
        let bits: Vec<bool> = match fate.roll(dice::u8(0..=4)) {
            0 => vec![false; rows],
            1 => vec![true; rows],
            2 if rows > 0 => {
                let one = fate.roll(dice::uni_usize(0..rows));
                (0..rows).map(|i| i == one).collect()
            }
            3 => {
                let run = 1 + fate.roll(dice::usize(0..200));
                let on = 1 + fate.roll(dice::usize(0..run));
                (0..rows).map(|i| i % (2 * run) < on).collect()
            }
            _ => {
                let density = fate.roll(dice::u8(..));
                (0..rows)
                    .map(|_| fate.roll(dice::u8(..)) < density)
                    .collect()
            }
        };
        let ranges: Vec<(usize, usize)> = (0..8)
            .map(|_| {
                let lo = fate.roll(dice::usize(0..=rows + 8));
                (lo, lo + fate.roll(dice::usize(0..=rows + 8)))
            })
            .chain([(0, rows), (0, usize::MAX), (rows, rows + 1)])
            .collect();
        hint!("{rows} rows");
        check_marks(&bits, &ranges);
    });
}

/// Empty, full and single-bit sets, at sizes across select samples.
#[test]
fn degenerate_sets_match_naive() {
    for rows in [0usize, 1, 2, 63, 64, 65, 127, 128, 4096, 4097, 20_000] {
        check_marks(&vec![false; rows], &[(0, rows)]);
        check_marks(&vec![true; rows], &[(0, rows), (rows / 2, rows)]);
        for one in [0, rows / 2, rows.saturating_sub(1)] {
            if one < rows {
                let bits: Vec<bool> = (0..rows).map(|i| i == one).collect();
                check_marks(&bits, &[(0, rows), (one, one + 1), (one + 1, rows)]);
            }
        }
    }
}

#[test]
#[should_panic(expected = "assertion `left == right` failed")]
fn write_refuses_a_section_of_the_wrong_size() {
    let mut out = vec![0u8; section_bytes(100, 1) + 64];
    write(&mut out, 100, &[1, 0]);
}

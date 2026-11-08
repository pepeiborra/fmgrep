use dicetest::prelude::*;

use super::*;

/// The last shape of [`bitmap`].
const LAST_FILL: u8 = 9;

/// `rows` bits of one shape.
fn bitmap(fate: &mut Fate, rows: usize, fill: u8) -> Vec<bool> {
    match fill {
        0 => (0..rows).map(|_| fate.roll(dice::bool())).collect(),
        1 => vec![true; rows],
        2 => vec![false; rows],
        3 => (0..rows).map(|i| i % 32 == 0).collect(),
        // Sparse: low bits and empty buckets.
        4 => (0..rows)
            .map(|_| fate.roll(dice::uni_u8(0..13)) == 0)
            .collect(),
        5 => (0..rows)
            .map(|_| fate.roll(dice::uni_u16(0..1000)) == 0)
            .collect(),
        // Clustered: long gaps between dense runs, so that select samples
        // span empty buckets.
        6 => {
            let gap = fate.roll(dice::uni_usize(2000..=30_000));
            let run = fate.roll(dice::uni_usize(200..=600));
            (0..rows)
                .map(|i| i % gap < run && fate.roll(dice::uni_u8(0..3)) == 0)
                .collect()
        }
        7 if rows > 0 => {
            let one = fate.roll(dice::uni_usize(0..rows));
            (0..rows).map(|i| i == one).collect()
        }
        7 => Vec::new(),
        8 => {
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
    }
}

/// Ranges to read `rows` rows by: the whole, the ends, past the end, inverted
/// and around a select sample, then rolled ones.
fn ranges(fate: &mut Fate, rows: usize) -> Vec<(usize, usize)> {
    let width = (rows + 8).min(9000);
    let mut ranges = vec![
        (0, 0),
        (0, 1),
        (0, rows),
        (0, usize::MAX),
        (4095, 4097),
        (100, 8000),
        (rows, rows + 1),
        (rows.saturating_sub(1), rows + 5000),
        (rows + 10, rows + 20),
        (7, 3),
    ];
    ranges.extend((0..28).map(|_| {
        let lo = fate.roll(dice::usize(0..=rows + 8));
        (lo, lo + fate.roll(dice::usize(0..=width)))
    }));
    ranges
}

/// `get_rank`, `range` and `total` against the bitmap `bits`, written with
/// set bits past its end if `junk`.
fn check_marks(bits: &[bool], ranges: &[(usize, usize)], junk: bool) {
    let len = bits.len();
    let ones = bits.iter().filter(|&&b| b).count();
    let mut words = vec![0u64; len.div_ceil(64) + usize::from(junk)];
    for (i, _) in bits.iter().enumerate().filter(|&(_, &b)| b) {
        words[i / 64] |= 1 << (i % 64);
    }
    if junk {
        words[len / 64] |= u64::MAX << (len % 64);
    }
    let mut out = vec![0u8; section_bytes(len, ones)];
    assert_eq!(
        write(&mut out, len, &words),
        ones,
        "bits past len are ignored"
    );
    let marks = Marks::new(&out, len, ones);
    assert_eq!(marks.total(), ones);
    let mut naive = Vec::with_capacity(ones);
    let mut before = 0;
    for (i, &bit) in bits.iter().enumerate() {
        assert_eq!(marks.get_rank(i), (bit, before), "row {i} of {len}");
        if bit {
            naive.push((i, before));
            before += 1;
        }
    }
    assert_eq!(marks.get_rank(len), (false, ones));
    assert_eq!(marks.get_rank(len + 5000), (false, ones), "clamped");
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
        let fill = fate.roll(dice::u8(0..=LAST_FILL));
        let bits = bitmap(&mut fate, rows, fill);
        let ranges = ranges(&mut fate, rows);
        let junk = fate.roll(dice::bool());
        hint!("{rows} rows, fill {fill}, junk {junk}");
        check_marks(&bits, &ranges, junk);
    });
}

/// Every shape at the lengths small dice do not reach: around a 4096-row
/// boundary, and long enough for many select samples and long gaps.
#[test]
fn marks_match_a_naive_bitmap_at_large_sizes() {
    Dicetest::repeatedly().passes(3).run(|mut fate| {
        for rows in [0usize, 1, 4095, 4096, 4097, 70_001] {
            for fill in 0..=LAST_FILL {
                let bits = bitmap(&mut fate, rows, fill);
                let ranges = ranges(&mut fate, rows);
                let junk = fate.roll(dice::bool());
                hint!("{rows} rows, fill {fill}, junk {junk}");
                check_marks(&bits, &ranges, junk);
            }
        }
    });
}

/// Empty, full and single-bit sets, at sizes across select samples.
#[test]
fn degenerate_sets_match_naive() {
    for rows in [0usize, 1, 2, 63, 64, 65, 127, 128, 4096, 4097, 20_000] {
        check_marks(&vec![false; rows], &[(0, rows)], false);
        check_marks(&vec![true; rows], &[(0, rows), (rows / 2, rows)], false);
        for one in [0, rows / 2, rows.saturating_sub(1)] {
            if one < rows {
                let bits: Vec<bool> = (0..rows).map(|i| i == one).collect();
                check_marks(&bits, &[(0, rows), (one, one + 1), (one + 1, rows)], false);
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

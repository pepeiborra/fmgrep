use dicetest::prelude::*;

use super::*;
use crate::test_util::{Rng, edges, in_pool};

/// `len` digits below `1 << d`: stretches of one digit, of two alternating
/// in runs, of sparse noise over one digit, and of uniform noise, so blocks
/// of every kind occur.
fn mixed_digits(len: usize, d: u32, rng: &mut Rng) -> Vec<u8> {
    let k = 1u64 << d;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let stretch = (1 + rng.below(3000) as usize).min(len - out.len());
        let (a, b) = (rng.below(k) as u8, rng.below(k) as u8);
        let style = rng.below(4);
        let run = 1 + stretch / 7;
        for j in 0..stretch {
            out.push(match style {
                0 => a,
                1 => [a, b][j / run % 2],
                2 if rng.below(40) == 0 => rng.below(k) as u8,
                2 => a,
                _ => rng.below(k) as u8,
            });
        }
    }
    out
}

fn digits_of(seq: &[u8], d: u32, lb: u32) -> Vec<u8> {
    let bytes = in_pool(3, || encode(seq, d, 0, lb));
    check_size(&bytes, seq.len(), d, lb);
    bytes
}

fn check_size(bytes: &[u8], len: usize, d: u32, lb: u32) {
    let (s, h) = index_bytes(len, d, lb);
    assert!(
        bytes.len() <= s + h + max_payload(len, d, lb),
        "len {len} d {d} lb {lb}"
    );
}

type RankFn = fn(&Digits, &[u8], u32, usize, usize) -> usize;
type AccessFn = fn(&Digits, &[u8], u32, usize) -> (usize, usize);

fn rank_d<const D: u32>(seq: &Digits, bytes: &[u8], lb: u32, i: usize, x: usize) -> usize {
    seq.rank::<D>(bytes, lb, i, x)
}

fn access_rank_d<const D: u32>(seq: &Digits, bytes: &[u8], lb: u32, i: usize) -> (usize, usize) {
    seq.access_rank::<D>(bytes, lb, i)
}

fn readers(d: u32) -> (RankFn, AccessFn) {
    match d {
        1 => (rank_d::<1>, access_rank_d::<1>),
        2 => (rank_d::<2>, access_rank_d::<2>),
        _ => (rank_d::<4>, access_rank_d::<4>),
    }
}

/// Every rank and access at `positions`, ascending, against a running count.
fn check_positions(seq: &[u8], d: u32, lb: u32, bytes: &[u8], positions: &[usize]) {
    let (len, k) = (seq.len(), 1usize << d);
    let at = Digits::at(0, len, d, lb);
    let (rank, access) = readers(d);
    let mut before = vec![0usize; k];
    let mut done = 0;
    for &i in positions {
        while done < i {
            before[usize::from(seq[done])] += 1;
            done += 1;
        }
        for (x, &want) in before.iter().enumerate() {
            assert_eq!(
                rank(&at, bytes, lb, i, x),
                want,
                "d {d} len {len} lb {lb} i {i} x {x}"
            );
        }
        if i < len {
            let x = usize::from(seq[i]);
            assert_eq!(
                access(&at, bytes, lb, i),
                (x, before[x]),
                "d {d} len {len} lb {lb} i {i}"
            );
        }
    }
}

#[test]
fn digits_rank_matches_naive() {
    let mut rng = Rng(17);
    for d in [1u32, 2, 4] {
        let k = 1usize << d;
        let span = super_len(d);
        for len in [
            1,
            63,
            255,
            256,
            257,
            1024,
            4097,
            span - 1,
            span,
            span + 1,
            300_000,
        ] {
            let all = |x: u8| vec![x; len];
            for seq in [
                mixed_digits(len, d, &mut rng),
                all(0),
                all(k as u8 - 1),
                (0..len).map(|_| rng.below(k as u64) as u8).collect(),
            ] {
                for lb in [8, 9, 10, 12] {
                    let bytes = digits_of(&seq, d, lb);
                    let positions = edges(len, span, lb, &mut rng);
                    check_positions(&seq, d, lb, &bytes, &positions);
                }
            }
        }
    }
}

#[test]
fn digits_use_every_kind_and_shrink_runs() {
    let mut rng = Rng(19);
    let len = 1 << 20;
    for d in [1u32, 2, 4] {
        let seq = mixed_digits(len, d, &mut rng);
        let bytes = digits_of(&seq, d, 10);
        let at = Digits::at(0, len, d, 10);
        let stride = 2 << d;
        let mut kinds = [0usize; 4];
        for h in bytes[at.heads..at.payload].chunks(stride) {
            kinds[usize::from(h[0] & 3)] += 1;
        }
        assert!(
            kinds[0] > 0 && kinds[2] > 0 && kinds[3] > 0,
            "d {d}: {kinds:?}"
        );
        let payload = bytes.len() - at.payload;
        assert!(payload < len * d as usize / 8, "d {d}: {payload} bytes");
    }
}

/// Every digit width and block log the format allows.
fn every_shape() -> impl Iterator<Item = (u32, u32)> {
    [1u32, 2, 4]
        .into_iter()
        .flat_map(|d| (8..=12).map(move |lb| (d, lb)))
}

/// The kind word of block `b`.
fn head_of(bytes: &[u8], len: usize, d: u32, lb: u32, b: usize) -> u16 {
    let at = Digits::at(0, len, d, lb);
    u16_at(bytes, at.heads + (2 << d) * b) as u16
}

/// A block of one digit, of two long runs, of many short runs, or of noise.
fn block_of(fate: &mut Fate, k: u8, block: usize) -> Vec<u8> {
    let a = fate.roll(dice::u8(0..k));
    let b = (a + 1 + fate.roll(dice::u8(0..k - 1))) % k;
    match fate.roll(dice::u8(0..=3)) {
        0 => vec![a; block],
        1 => {
            let split = fate.roll(dice::usize(1..block));
            let mut out = vec![a; split];
            out.resize(block, b);
            out
        }
        2 => {
            let mut out = Vec::with_capacity(block);
            while out.len() < block {
                let digit = if fate.roll(dice::bool()) { a } else { b };
                let run = fate.roll(dice::usize(1..=9));
                out.extend(std::iter::repeat_n(digit, run.min(block - out.len())));
            }
            out
        }
        _ => (0..block).map(|_| fate.roll(dice::u8(0..k))).collect(),
    }
}

/// Plain, runs and one-digit blocks, at lengths on and around the block and
/// super boundaries, for every digit width and block log.
#[test]
fn blocks_of_every_kind_match_naive_across_boundaries() {
    Dicetest::repeatedly().passes(40).run(|mut fate| {
        for (d, lb) in every_shape() {
            let (block, span) = (1usize << lb, super_len(d));
            let k = 1u8 << d;
            let len = match fate.roll(dice::u8(0..=4)) {
                0 => block * (1 + fate.roll(dice::usize(0..8))),
                1 => span,
                2 => span - 1 + fate.roll(dice::usize(0..=2)),
                3 => span + block * (1 + fate.roll(dice::usize(0..4))),
                _ => 1 + fate.roll(dice::usize(0..4 * block)),
            };
            hint!("d {d} lb {lb} len {len}");
            let mut seq = Vec::with_capacity(len + block);
            while seq.len() < len {
                seq.extend(block_of(&mut fate, k, block));
            }
            seq.truncate(len);
            let bytes = encode(&seq, d, 0, lb);
            check_size(&bytes, len, d, lb);
            let mut rng = Rng(fate.roll(dice::u64(1..)));
            let positions = edges(len, span, lb, &mut rng);
            check_positions(&seq, d, lb, &bytes, &positions);
        }
    });
}

/// Every position of a whole block.
fn check_block(seq: &[u8], d: u32, lb: u32, bytes: &[u8], b: usize) {
    let from = b << lb;
    let positions: Vec<usize> = (from..=from + (1 << lb)).collect();
    check_positions(seq, d, lb, bytes, &positions);
}

/// Blocks of two runs whose lengths cross each power of two, the longest
/// included: the gamma codes the encoder writes, read back by rank.
#[test]
fn gamma_lengths_round_trip_through_the_run_encoder() {
    for (d, lb) in every_shape() {
        let block = 1usize << lb;
        let k = 1u8 << d;
        let mut splits = vec![1, 2, block - 2, block - 1];
        for z in 1..lb {
            splits.extend([(1 << z) - 1, 1 << z, (1 << z) + 1]);
        }
        splits.retain(|&s| (1..block).contains(&s));
        splits.sort_unstable();
        splits.dedup();
        for split in splits {
            let mut seq = vec![k - 1; split];
            seq.resize(block, 0);
            // A second block, for the head after it.
            seq.extend(std::iter::repeat_n(1, block));
            let bytes = encode(&seq, d, 0, lb);
            assert_eq!(
                head_of(&bytes, seq.len(), d, lb, 0) & 3,
                RUNS,
                "d {d} lb {lb} split {split}"
            );
            check_block(&seq, d, lb, &bytes, 0);
        }
    }
}

/// Runs of one or two digits, enough of them that the codes cross words.
#[test]
fn many_runs_decode_across_words() {
    Dicetest::repeatedly().passes(300).run(|mut fate| {
        for (d, lb) in every_shape() {
            let block = 1usize << lb;
            let k = 1u8 << d;
            let mut seq = Vec::with_capacity(block);
            let mut digit = fate.roll(dice::u8(0..k));
            while seq.len() < block {
                let run = 16 + fate.roll(dice::usize(0..48));
                seq.extend(std::iter::repeat_n(digit, run.min(block - seq.len())));
                digit = (digit + 1 + fate.roll(dice::u8(0..k - 1))) % k;
            }
            let bytes = encode(&seq, d, 0, lb);
            hint!("d {d} lb {lb}");
            assert_eq!(head_of(&bytes, block, d, lb, 0) & 3, RUNS);
            check_block(&seq, d, lb, &bytes, 0);
        }
    });
}

#[test]
fn one_digit_blocks_hold_their_digit_in_the_head() {
    for (d, lb) in every_shape() {
        let block = 1usize << lb;
        for digit in 0..1u8 << d {
            let mut seq = vec![digit; 3 * block];
            seq[2 * block] = digit ^ 1;
            let bytes = encode(&seq, d, 0, lb);
            for b in 0..2 {
                assert_eq!(
                    head_of(&bytes, seq.len(), d, lb, b),
                    ONE | u16::from(digit) << 2,
                    "d {d} lb {lb} digit {digit}"
                );
                check_block(&seq, d, lb, &bytes, b);
            }
        }
    }
}

#[test]
fn alternating_blocks_are_plain() {
    Dicetest::repeatedly().passes(100).run(|mut fate| {
        for (d, lb) in every_shape() {
            let block = 1usize << lb;
            let k = 1u8 << d;
            let a = fate.roll(dice::u8(0..k));
            let b = (a + 1 + fate.roll(dice::u8(0..k - 1))) % k;
            // Every run is one digit long: runs cannot be smaller.
            let seq: Vec<u8> = (0..block).map(|i| [a, b][i % 2]).collect();
            let bytes = encode(&seq, d, 0, lb);
            hint!("d {d} lb {lb} a {a} b {b}");
            assert_eq!(head_of(&bytes, block, d, lb, 0) & 3, PLAIN);
            check_block(&seq, d, lb, &bytes, 0);
        }
    });
}

/// Encoding a digit field above bit 0 reads the same digits.
#[test]
fn digits_at_a_shift_read_the_same() {
    Dicetest::repeatedly().passes(100).run(|mut fate| {
        for (d, lb) in every_shape() {
            let block = 1usize << lb;
            let k = 1u8 << d;
            let seq = block_of(&mut fate, k, block);
            let shift = 8 - d;
            let shifted: Vec<u8> = seq.iter().map(|&x| x << shift).collect();
            assert_eq!(encode(&shifted, d, shift, lb), encode(&seq, d, 0, lb));
        }
    });
}

#[test]
fn encoding_does_not_depend_on_the_pool() {
    let mut rng = Rng(29);
    for d in [1u32, 2, 4] {
        let seq = mixed_digits(3 * super_len(d) + 5, d, &mut rng);
        let one = in_pool(1, || encode(&seq, d, 0, 8));
        assert!(one == in_pool(3, || encode(&seq, d, 0, 8)), "d {d}");
    }
}

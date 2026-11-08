use dicetest::prelude::*;

use super::*;

/// Slot by slot: one bit at the bottom of each `D`-bit slot holding `x`.
fn equal_by_slots<const D: u32>(w: u64, x: usize) -> u64 {
    let mask = (1u64 << D) - 1;
    (0..64 / D)
        .filter(|s| (w >> (s * D)) & mask == x as u64 & mask)
        .fold(0, |acc, s| acc | 1 << (s * D))
}

fn equal_matches_slots<const D: u32>() {
    Dicetest::repeatedly().passes(20_000).run(|mut fate| {
        let w = if fate.roll(dice::bool()) {
            fate.roll(dice::u64(..))
        } else {
            // Slots from few digits, so that many hold `x`.
            let few = fate.roll(dice::u64(0..4));
            (0..64 / D).fold(0, |w, s| {
                let slot = (few >> (s % 2)) & ((1 << D) - 1);
                w | slot << (s * D)
            })
        };
        let x = fate.roll(dice::usize(..));
        hint!("w {w:#x} x {x}");
        assert_eq!(equal::<D>(w, x), equal_by_slots::<D>(w, x));
    });
}

#[test]
fn equal_matches_slots_d1() {
    equal_matches_slots::<1>();
}

#[test]
fn equal_matches_slots_d2() {
    equal_matches_slots::<2>();
}

#[test]
fn equal_matches_slots_d4() {
    equal_matches_slots::<4>();
}

/// Every length the runs format can hold, alone in the stream.
#[test]
fn gamma_round_trips_every_length() {
    for x in 1..=65535usize {
        let mut out = Vec::new();
        let mut bits = BitWriter::new(&mut out);
        bits.gamma(x);
        bits.finish();
        assert_eq!(gamma_decode(u64_at(&out, 0)), (x, 2 * x.ilog2() + 1), "{x}");
    }
}

#[test]
fn gamma_decodes_at_any_bit_offset_before_any_bits() {
    Dicetest::repeatedly().passes(20_000).run(|mut fate| {
        let offset = fate.roll(dice::u32(0..=7));
        let x = 1 + fate.roll(dice::usize(0..65535));
        let after = fate.roll(dice::u64(0..1 << 32));
        hint!("offset {offset} x {x} after {after:#x}");
        let mut out = Vec::new();
        let mut bits = BitWriter::new(&mut out);
        bits.put(fate.roll(dice::u64(0..1 << offset)), offset);
        bits.gamma(x);
        bits.put(after, 32);
        bits.finish();
        assert_eq!(
            gamma_decode(u64_at(&out, 0) >> offset),
            (x, 2 * x.ilog2() + 1)
        );
    });
}

/// A stream as the runs format writes it, read back as `runs` does.
#[test]
fn gamma_streams_round_trip() {
    Dicetest::repeatedly().passes(3000).run(|mut fate| {
        let items = fate.roll(dice::vec(
            dice::zip().two(dice::u64(0..16), dice::usize(1..=65535)),
            1..=64,
        ));
        let mut out = Vec::new();
        let mut bits = BitWriter::new(&mut out);
        for &(digit, len) in &items {
            bits.put(digit, 4);
            bits.gamma(len);
        }
        bits.finish();
        let mut pos = 0usize;
        for &(digit, len) in &items {
            let window = u64_at(&out, pos / 8) >> (pos % 8);
            assert_eq!(window & 15, digit);
            let (got, used) = gamma_decode(window >> 4);
            assert_eq!(got, len);
            pos += 4 + used as usize;
        }
    });
}

fn check_select(w: u64) {
    for (r, want) in (0..64).filter(|&b| w >> b & 1 == 1).enumerate() {
        assert_eq!(select_in_word(w, r), want, "{w:#x} r {r}");
    }
}

/// The extreme words, then arbitrary ones.
#[test]
fn select_in_word_matches_naive() {
    for w in [
        0u64,
        1,
        1 << 63,
        u64::MAX,
        0x8000_0001,
        0xff00,
        0x0100_0000_0000_0080,
    ] {
        check_select(w);
    }
    Dicetest::repeatedly().passes(50_000).run(|mut fate| {
        let mut w = fate.roll(dice::u64(..));
        // Sparse words too: most select paths want empty bytes.
        for _ in 0..fate.roll(dice::u8(0..=3)) {
            w &= fate.roll(dice::u64(..));
        }
        hint!("w {w:#x}");
        check_select(w);
    });
}

/// `width` bits of `b` from bit `at`, a bit at a time; zero past the end.
fn naive_bits(b: &[u8], at: usize, width: u32) -> u64 {
    (0..width as usize).fold(0, |v, j| {
        let p = at + j;
        let bit = b.get(p / 8).map_or(0, |&x| x >> (p % 8) & 1);
        v | u64::from(bit) << j
    })
}

/// Every width the read serves, at every bit offset, past the end included.
#[test]
fn bits_at_matches_naive() {
    Dicetest::repeatedly().passes(10_000).run(|mut fate| {
        let bytes = fate.roll(dice::vec(dice::u8(..), 0..=32));
        for width in 1..=57 {
            let at = fate.roll(dice::usize(0..=8 * bytes.len() + 16));
            assert_eq!(
                bits_at(&bytes, at, width),
                naive_bits(&bytes, at, width),
                "at {at} width {width}"
            );
        }
    });
}

#[test]
fn pack_round_trips_through_bits_at() {
    Dicetest::repeatedly().passes(2000).run(|mut fate| {
        for width in 1..=57u32 {
            let count = fate.roll(dice::usize(0..=40));
            let max = (1u64 << width) - 1;
            let values: Vec<u64> = (0..count)
                .map(|_| match fate.roll(dice::u8(0..=3)) {
                    0 => max,
                    1 => 0,
                    _ => fate.roll(dice::u64(..)) & max,
                })
                .collect();
            let bits = count * width as usize;
            let mut out = vec![0u8; bits.div_ceil(8)];
            pack(&mut out, values.iter().copied(), width);
            for (k, &v) in values.iter().enumerate() {
                assert_eq!(bits_at(&out, k * width as usize, width), v, "width {width}");
            }
            assert_eq!(naive_bits(&out, bits, 8), 0, "width {width}: tail bits");
        }
    });
}

#[test]
fn pack_of_width_zero_writes_nothing() {
    let mut out = [0xa5u8; 8];
    pack(&mut out, [1u64, 2, 3].into_iter(), 0);
    assert_eq!(out, [0xa5; 8]);
}

/// The bytes from `at`, zero where past the end.
fn naive_le(b: &[u8], at: usize, n: usize) -> u64 {
    (0..n).fold(0, |v, j| {
        let byte = at.checked_add(j).and_then(|p| b.get(p)).copied();
        v | u64::from(byte.unwrap_or(0)) << (8 * j)
    })
}

#[test]
fn readers_clamp_to_the_bytes_given() {
    Dicetest::repeatedly().passes(20_000).run(|mut fate| {
        let bytes = fate.roll(dice::vec(dice::u8(..), 0..=24));
        let len = bytes.len();
        for at in (0..=len + 9).chain([usize::MAX - 7, usize::MAX]) {
            let whole = |n: usize| at.checked_add(n).is_some_and(|end| end <= len);
            let want = |n: usize| {
                if whole(n) {
                    naive_le(&bytes, at, n) as usize
                } else {
                    0
                }
            };
            assert_eq!(u16_at(&bytes, at), want(2), "at {at}");
            assert_eq!(u32_at(&bytes, at), want(4), "at {at}");
            assert_eq!(u64_at(&bytes, at), naive_le(&bytes, at, 8), "at {at}");
        }
    });
}

use dicetest::prelude::*;

use super::*;

#[test]
fn width_covers_the_values() {
    let cases = [
        (0, 1),
        (1, 1),
        (2, 1),
        (3, 2),
        (256, 8),
        (257, 9),
        (1 << 31, 31),
        ((1 << 31) + 1, 32),
        (u32::MAX, 32),
    ];
    for (count, width) in cases {
        assert_eq!(Packed::for_values(count).width(), width, "{count}");
    }
}

/// The widths the constructor gives, with a count that gives each.
fn every_width() -> impl Iterator<Item = (u32, u32)> {
    (1..=32).map(|w| (w, if w == 32 { u32::MAX } else { 1 << w }))
}

#[test]
fn values_round_trip_at_any_start() {
    Dicetest::repeatedly().passes(2000).run(|mut fate| {
        for (width, count) in every_width() {
            let packed = Packed::for_values(count);
            assert_eq!(packed.width(), width);
            let from = fate.roll(dice::usize(0..=8));
            let n = fate.roll(dice::usize(0..=40));
            let mask = (1u64 << width) - 1;
            let values: Vec<u64> = (0..n).map(|_| fate.roll(dice::u64(..)) & mask).collect();
            let mut out = vec![0u8; from + packed.section_bytes(n)];
            packed.write(&mut out[from..], values.iter().copied());
            for (k, &v) in values.iter().enumerate() {
                assert_eq!(packed.get(&out, from, k), v, "width {width} k {k}");
            }
        }
    });
}

#[test]
fn values_are_cut_to_the_width() {
    let packed = Packed::for_values(16);
    let mut out = [0u8; 8];
    packed.write(&mut out, [0xf3u64, 0x1, u64::MAX].into_iter());
    assert_eq!(
        (0..3).map(|k| packed.get(&out, 0, k)).collect::<Vec<_>>(),
        [3, 1, 15]
    );
}

#[test]
fn reads_past_the_end_are_zero() {
    let packed = Packed::for_values(1 << 16);
    let bytes = [0xffu8; 4];
    assert_eq!(packed.get(&bytes, 0, 1), 0xffff);
    assert_eq!(packed.get(&bytes, 0, 2), 0);
    assert_eq!(packed.get(&bytes, 3, 0), 0xff);
    assert_eq!(packed.get(&bytes, 100, 7), 0);
}

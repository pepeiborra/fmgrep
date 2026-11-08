use dicetest::prelude::*;

use super::*;
use crate::test_util::{edges, in_pool};

fn params(d: u32, lb: u32) -> Params {
    Params::new(d, lb).unwrap()
}

/// The levels of `codes` laid out from offset 0.
fn tree_of(codes: &[u8], sigma: usize, params: Params) -> (Vec<u8>, Shape) {
    let encoded = encode(codes, sigma, params);
    let (shape, end) = Shape::new(
        codes.len(),
        sigma,
        params,
        *encoded.starts(),
        *encoded.bottom(),
        &encoded.payloads(),
        0,
    );
    let mut bytes = vec![0u8; end];
    encoded.write(&shape, &mut bytes);
    (bytes, shape)
}

/// Codes below `sigma`: runs, then uniform noise.
fn occ_codes(n: usize, sigma: usize, fate: &mut Fate) -> Vec<u8> {
    (0..n)
        .map(|i| {
            if i % 3000 < 1500 {
                (i / 3000 % sigma) as u8
            } else {
                fate.roll(dice::uni_usize(0..sigma)) as u8
            }
        })
        .collect()
}

/// Codes below `sigma`: long runs, then noise.
fn run_codes(n: usize, sigma: usize, fate: &mut Fate) -> Vec<u8> {
    let mut codes = Vec::with_capacity(n);
    while codes.len() < n {
        let code = fate.roll(dice::uni_usize(0..sigma)) as u8;
        let run = if fate.roll(dice::bool()) {
            1
        } else {
            fate.roll(dice::uni_usize(0..500))
        };
        codes.extend(std::iter::repeat_n(code, run.min(n - codes.len())));
    }
    codes
}

/// Every rank and access-rank at `positions`, ascending past the end too,
/// against a running count.
fn check_tree(tree: &WaveletMatrix<'_>, codes: &[u8], sigma: usize, positions: &[usize]) {
    let n = codes.len();
    let mut before = vec![0usize; sigma];
    let mut at = 0;
    for &i in positions {
        while at < i.min(n) {
            before[usize::from(codes[at])] += 1;
            at += 1;
        }
        for (code, &want) in before.iter().enumerate() {
            assert_eq!(
                tree.rank(code as u8, i),
                want,
                "n {n} sigma {sigma} code {code} i {i}"
            );
        }
        if i < n {
            let code = codes[i];
            assert_eq!(
                tree.access_rank(i),
                (code, before[usize::from(code)]),
                "n {n} sigma {sigma} i {i}"
            );
        }
    }
}

/// Every length and alphabet below at once: the sizes around a level's
/// blocks and supers, up to several supers, that small dice do not reach.
#[test]
fn wavelet_rank_matches_naive() {
    Dicetest::repeatedly().passes(1).run(|mut fate| {
        for n in [0, 1, 63, 64, 1023, 1024, 1025, 65536, 65537, 200_000] {
            for sigma in [1usize, 2, 3, 5, 17, 100, 200, 256] {
                if n == 0 && sigma > 1 {
                    continue;
                }
                let seqs = [
                    occ_codes(n, sigma, &mut fate),
                    run_codes(n, sigma, &mut fate),
                ];
                for codes in &seqs {
                    for (d, lb) in [(1, 8), (1, 10), (2, 9), (2, 12), (4, 8), (4, 10)] {
                        hint!("n {n} sigma {sigma} d {d} lb {lb}");
                        let p = params(d, lb);
                        let (bytes, shape) = in_pool(3, || tree_of(codes, sigma, p));
                        assert_eq!(shape.depth(), depth_for(sigma, p));
                        let tree = WaveletMatrix::new(&bytes, &shape);
                        let positions: Vec<usize> =
                            edges(n.max(1), digits::super_len(d), lb, &mut fate)
                                .into_iter()
                                .chain([n + 1, usize::MAX])
                                .collect();
                        check_tree(&tree, codes, sigma, &positions);
                    }
                }
            }
        }
    });
}

/// Runs and noise over `sigma` codes, every code at least once when it fits.
fn rolled_codes(fate: &mut Fate, n: usize, sigma: usize) -> Vec<u8> {
    let single = dice::just(1);
    let short = dice::usize(1..=8);
    let long = dice::usize(1..=600);
    let run_die = dice::one_of_die().three(single, short, long);
    let mut codes: Vec<u8> = Vec::with_capacity(n);
    while codes.len() < n {
        let code = fate.roll(dice::uni_usize(0..sigma)) as u8;
        let run = fate.roll(&run_die);
        codes.extend(std::iter::repeat_n(code, run.min(n - codes.len())));
    }
    codes
}

fn wavelet_matches_naive(sigma: usize) {
    Dicetest::repeatedly().passes(150).run(|mut fate| {
        let d = [1u32, 2, 4][fate.roll(dice::uni_usize(0..3))];
        let lb = fate.roll(dice::u32(8..=12));
        let n = 1 + fate.roll(dice::usize(0..=20_000));
        hint!("sigma {sigma} d {d} lb {lb} n {n}");
        let codes = rolled_codes(&mut fate, n, sigma);
        let (bytes, shape) = tree_of(&codes, sigma, params(d, lb));
        let tree = WaveletMatrix::new(&bytes, &shape);
        let positions: Vec<usize> = edges(n, digits::super_len(d), lb, &mut fate)
            .into_iter()
            .chain([n + 1, usize::MAX])
            .collect();
        check_tree(&tree, &codes, sigma, &positions);
    });
}

#[test]
fn wavelet_matches_naive_sigma_1() {
    wavelet_matches_naive(1);
}

#[test]
fn wavelet_matches_naive_sigma_2() {
    wavelet_matches_naive(2);
}

#[test]
fn wavelet_matches_naive_sigma_16() {
    wavelet_matches_naive(16);
}

#[test]
fn wavelet_matches_naive_sigma_17() {
    wavelet_matches_naive(17);
}

#[test]
fn wavelet_matches_naive_sigma_256() {
    wavelet_matches_naive(256);
}

#[test]
fn params_hold_only_what_the_format_allows() {
    for d in 0..=9u32 {
        for lb in 0..=20u32 {
            let valid = matches!(d, 1 | 2 | 4) && (8..=12).contains(&lb);
            assert_eq!(Params::new(d, lb).is_some(), valid, "d {d} lb {lb}");
        }
    }
}

#[test]
#[should_panic(expected = "257 codes")]
fn a_shape_refuses_more_codes_than_bytes() {
    let _ = Shape::new(
        0,
        257,
        params(4, 8),
        [0; MAX_STARTS],
        [0; 256],
        &[0; MAX_DEPTH],
        0,
    );
}

#[test]
#[should_panic(expected = "a code at or past 2")]
fn the_encoder_refuses_a_code_past_sigma() {
    let _ = encode(&[0, 1, 2], 2, params(4, 8));
}

#[test]
#[should_panic(expected = "a shape for other levels")]
fn levels_refuse_another_shape() {
    let codes = [0u8, 1, 2, 3];
    let encoded = encode(&codes, 4, params(2, 8));
    let (shape, end) = Shape::new(
        codes.len(),
        4,
        params(4, 8),
        *encoded.starts(),
        *encoded.bottom(),
        &encoded.payloads(),
        0,
    );
    encoded.write(&shape, &mut vec![0; end]);
}

#[test]
fn level_rank_past_the_last_level_is_zero() {
    let (bytes, shape) = tree_of(&[0, 1, 1, 0], 2, params(1, 8));
    let tree = WaveletMatrix::new(&bytes, &shape);
    let one = params(1, 8).digit(1).unwrap();
    assert_eq!(tree.level_rank(0, 4, one), 2);
    assert_eq!(tree.level_rank(shape.depth(), 4, one), 0);
    assert_eq!(tree.level_rank(usize::MAX, 4, one), 0);
}

#[test]
fn a_digit_is_below_its_base() {
    assert_eq!(params(1, 8).digit(2), None);
    assert_eq!(params(2, 8).digit(4), None);
    assert_eq!(params(4, 8).digit(16), None);
    assert!(params(4, 8).digit(15).is_some());
}

use super::*;
use crate::digits::{self, Digits};
use crate::wavelet::{self, Shape, WaveletMatrix};

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
    let bytes = in_pool(3, || digits::encode(seq, d, 0, lb));
    let (s, h) = digits::index_bytes(seq.len(), d, lb);
    assert!(
        bytes.len() <= s + h + digits::max_payload(seq.len(), d, lb),
        "len {} d {d} lb {lb}",
        seq.len()
    );
    bytes
}

/// The levels of `codes` in `d`-bit digits laid out from offset 0.
fn tree_of(codes: &[u8], sigma: usize, d: u32, lb: u32) -> (Vec<u8>, Shape) {
    let n = codes.len();
    let (levels, starts, bottom) = in_pool(3, || wavelet::encode(codes, sigma, d, lb));
    let payload = wavelet::payloads(&levels, n, d, lb);
    let (shape, end) = Shape::new(n, levels.len(), d, lb, &payload, 0);
    let shape = Shape {
        starts,
        bottom,
        ..shape
    };
    let mut bytes = vec![0u8; end];
    wavelet::write(&levels, &shape, &mut bytes);
    (bytes, shape)
}

/// Positions around every block and super boundary, plus random ones.
fn edges(len: usize, d: u32, lb: u32, rng: &mut Rng) -> Vec<usize> {
    let block = 1usize << lb;
    let mut at: Vec<usize> = (0..=len / block + 1)
        .flat_map(|k| [k * block, k * block + block / 2])
        .chain([digits::super_len(d), len])
        .flat_map(|e| [e.saturating_sub(1), e, e + 1])
        .chain((0..200).map(|_| rng.below(len as u64 + 1) as usize))
        .filter(|&i| i <= len)
        .collect();
    at.sort_unstable();
    at.dedup();
    at
}

type RankFn = fn(&Digits, &[u8], u32, usize, usize) -> usize;
type AccessFn = fn(&Digits, &[u8], u32, usize) -> (usize, usize);

fn rank_d<const D: u32>(seq: &Digits, bytes: &[u8], lb: u32, i: usize, x: usize) -> usize {
    seq.rank::<D>(bytes, lb, i, x)
}

fn access_rank_d<const D: u32>(seq: &Digits, bytes: &[u8], lb: u32, i: usize) -> (usize, usize) {
    seq.access_rank::<D>(bytes, lb, i)
}

#[test]
fn digits_rank_matches_naive() {
    let mut rng = Rng(17);
    for d in [1u32, 2, 4] {
        let k = 1usize << d;
        let span = digits::super_len(d);
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
                    let at = Digits::at(0, len, d, lb);
                    let (rank, access): (RankFn, AccessFn) = match d {
                        1 => (rank_d::<1>, access_rank_d::<1>),
                        2 => (rank_d::<2>, access_rank_d::<2>),
                        _ => (rank_d::<4>, access_rank_d::<4>),
                    };
                    let mut before = vec![0usize; k];
                    let mut done = 0;
                    for i in edges(len, d, lb, &mut rng) {
                        while done < i {
                            before[usize::from(seq[done])] += 1;
                            done += 1;
                        }
                        for (x, &want) in before.iter().enumerate() {
                            assert_eq!(
                                rank(&at, &bytes, lb, i, x),
                                want,
                                "d {d} len {len} lb {lb} i {i} x {x}"
                            );
                        }
                        if i < len {
                            let x = usize::from(seq[i]);
                            assert_eq!(
                                access(&at, &bytes, lb, i),
                                (x, before[x]),
                                "d {d} len {len} lb {lb} i {i}"
                            );
                        }
                    }
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

/// Codes below `sigma`: long runs, then noise.
fn run_codes(n: usize, sigma: usize, rng: &mut Rng) -> Vec<u8> {
    let mut codes = Vec::with_capacity(n);
    while codes.len() < n {
        let code = rng.below(sigma as u64) as u8;
        let run = if rng.below(2) == 0 {
            1
        } else {
            rng.below(500) as usize
        };
        codes.extend(std::iter::repeat_n(code, run.min(n - codes.len())));
    }
    codes
}

#[test]
fn wavelet_rank_matches_naive() {
    let mut rng = Rng(23);
    for n in [0, 1, 63, 64, 1023, 1024, 1025, 65536, 65537, 200_000] {
        for sigma in [1usize, 2, 3, 5, 17, 100, 200, 256] {
            if n == 0 && sigma > 1 {
                continue;
            }
            for codes in [occ_codes(n, sigma, &mut rng), run_codes(n, sigma, &mut rng)] {
                for (d, lb) in [(1, 8), (1, 10), (2, 9), (2, 12), (4, 8), (4, 10)] {
                    let (bytes, shape) = tree_of(&codes, sigma, d, lb);
                    assert_eq!(shape.depth, wavelet::depth_for(sigma, d));
                    let tree = WaveletMatrix::new(&bytes, &shape);
                    let mut before = vec![0usize; sigma];
                    let mut at = 0;
                    for i in edges(n.max(1), d, lb, &mut rng)
                        .into_iter()
                        .chain([n + 1, usize::MAX])
                    {
                        while at < i.min(n) {
                            before[usize::from(codes[at])] += 1;
                            at += 1;
                        }
                        for (code, &want) in before.iter().enumerate() {
                            assert_eq!(
                                tree.rank(code as u8, i),
                                want,
                                "n {n} sigma {sigma} d {d} lb {lb} code {code} i {i}"
                            );
                        }
                        if i < n {
                            let code = codes[i];
                            assert_eq!(
                                tree.access_rank(i),
                                (code, before[usize::from(code)]),
                                "n {n} sigma {sigma} d {d} lb {lb} i {i}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn every_shape_builds_the_same_index() {
    let text = code_text();
    let base = FmIndex::build(text.clone()).unwrap();
    for d in [1u32, 2, 4] {
        for lb in [8, 9, 12] {
            let config = layout::Config {
                d,
                lb,
                ..layout::Config::DEFAULT
            };
            let index = FmIndex::view(build::build(text.clone(), config).unwrap()).unwrap();
            assert_eq!(index.header.wavelet.d, d);
            let blob = encode(&index);
            let back = FmIndex::view(blob.as_slice()).unwrap();
            for pattern in patterns(&text) {
                assert_eq!(
                    count(&back, &pattern),
                    count(&base, &pattern),
                    "d {d} lb {lb}"
                );
                assert_eq!(
                    located(&back, &pattern),
                    located(&base, &pattern),
                    "d {d} lb {lb}"
                );
            }
        }
    }
}

#[test]
fn tampered_kary_blobs_never_panic_outside_locate() {
    for d in [2u32, 4] {
        let config = layout::Config {
            d,
            ..layout::Config::DEFAULT
        };
        let blob = build::build(b"printk hello printk\0".to_vec(), config).unwrap();
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
                assert!(
                    msg.contains("corrupt docfm index"),
                    "d {d} flip at {i}: {msg}"
                );
            }
        }
    }
}

/// FNV-1a 64 of `bytes`.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Digests of the blobs over msbwt's BWT, as the encoder first wrote them:
/// it must reproduce them byte for byte, on any number of threads.
#[test]
fn blobs_match_the_pinned_encoder() {
    const PINNED: [u64; 24] = [
        0x224929b4ed10b659,
        0xe0216694c5ca67b5,
        0x995109b1fbf246fb,
        0x36d6ec4e73be6cdf,
        0x2b3504897049f37d,
        0x24da22dda009292d,
        0x7d6b2b988c21fad7,
        0x2677ff9075e37083,
        0xda40a28d46b4b6da,
        0xebcb12e019d7cda3,
        0x628b16e82b403ab0,
        0xce16e934b3f18175,
        0xc3704b655c04495f,
        0x4f150f1ad72e491a,
        0xae392c828b1a9288,
        0x9e2a33d9087bf775,
        0x399f7191fee6a2e5,
        0x959a9ce0617681a0,
        0x0c538c0ae8e4ac77,
        0xd29fa43c28c89dda,
        0xa2113aef4a63e3d2,
        0x3bdb84af2c8439c3,
        0x1970fa607847d7d9,
        0x6921a45676216b58,
    ];
    let mut rng = Rng(53);
    let runs: Vec<u8> = (0..300_000)
        .map(|i| {
            if i % 7000 < 3000 {
                b'a' + (i / 7000 % 20) as u8
            } else {
                rng.below(256) as u8
            }
        })
        .collect();
    let texts = [code_text(), large_text(), terminated(runs)];
    let mut got = Vec::new();
    for text in &texts {
        for (d, lb) in [(1, 9), (2, 8), (4, 8), (4, 12)] {
            for gaps in [Gaps::WORDS, gaps(8, 16, 64)] {
                let config = layout::Config { gaps, d, lb };
                let blob = in_pool(1, || build::build(text.clone(), config).unwrap());
                let threaded = in_pool(3, || build::build(text.clone(), config).unwrap());
                assert!(blob == threaded, "{config:?}");
                got.push(fnv(&blob));
            }
        }
    }
    assert_eq!(got, PINNED, "{got:#x?}");
}

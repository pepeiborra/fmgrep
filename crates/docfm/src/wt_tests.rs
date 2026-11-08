use super::*;
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
            assert_eq!(index.header.wt.params().d(), d);
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

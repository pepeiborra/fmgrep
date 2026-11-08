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
            assert_eq!(index.header.wavelet.params().d(), d);
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
/// it must reproduce them byte for byte, on any number of threads. They
/// depend on dicetest's generator too, which rolls the texts, so a dicetest
/// upgrade can change them with no encoder change.
#[test]
fn blobs_match_the_pinned_encoder() {
    const PINNED: [u64; 24] = [
        0x16a5c187440ac58f,
        0xda237f3ce29779a9,
        0x2a48ac705bae37ed,
        0x3499f1e50d67e32f,
        0x7ea99bf23fb5fc83,
        0x2550a47e48d6885d,
        0x233ef7f32af0e412,
        0x0ca373e6fcec06f4,
        0xc18bedc94b454959,
        0x0657038826e54216,
        0x5bb81d10bb3fe29c,
        0x32aee2b285395cb7,
        0xd81e19963157afc3,
        0x8abd525197a5a854,
        0x3772823fe84d6d36,
        0x199fde98d24f8915,
        0xd25a04803ddb139a,
        0x38fe7609f39040ff,
        0x30251d7a1ab45a35,
        0x111f47d47937e02c,
        0x194f833f67d8080b,
        0x351131cfa0d5999a,
        0xa23e9e6b883738d7,
        0x063cf7152ee2da3e,
    ];
    let runs: Vec<u8> = pinned(53, |fate| {
        (0..300_000)
            .map(|i| {
                if i % 7000 < 3000 {
                    b'a' + (i / 7000 % 20) as u8
                } else {
                    fate.roll(dice::uni_u8(..))
                }
            })
            .collect()
    });
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

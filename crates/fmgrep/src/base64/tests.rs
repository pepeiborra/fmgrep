use super::*;

#[test]
fn encode_matches_known_vectors() {
    // RFC 4648 section 10.
    for (raw, want) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(encode(raw.as_bytes()), want, "raw={raw:?}");
        assert_eq!(decode(want).unwrap(), raw.as_bytes(), "want={want:?}");
    }
    assert_eq!(encode(&[0xfb, 0xff, 0xbe]), "+/++");
}

#[test]
fn decode_inverts_encode() {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut bytes = Vec::new();
    for _ in 0..300 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        bytes.push(x as u8);
    }
    for len in (0..=7).chain([64, 299, 300]) {
        let b = &bytes[..len];
        assert_eq!(decode(&encode(b)).unwrap(), b, "len={len}");
    }
}

#[test]
fn decode_refuses_malformed_input() {
    for bad in [
        "Zg=",
        "Zm9v Zg==",
        "Z===",
        "Zg==Zm9v",
        "Zm9*",
        "=Zm9",
        "Zh==",
        "Zm9=",
        "Zm+=",
    ] {
        assert!(decode(bad).is_err(), "{bad:?}");
    }
}

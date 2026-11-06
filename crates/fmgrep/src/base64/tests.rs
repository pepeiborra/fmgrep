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
    }
    assert_eq!(encode(&[0xfb, 0xff, 0xbe]), "+/++");
}

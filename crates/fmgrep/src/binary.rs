//! The index-time binary predicate: NUL rule, native object magic, then a
//! portable control-density fallback.

use crate::unified::UNIFIED_SEPARATOR;

/// Prefix window (bytes) the portable control-density sniff inspects; the NUL
/// rule still covers the whole buffer, so no late NUL slips through.
const BINARY_SNIFF_PREFIX: usize = 8192;

/// True when `bytes` look binary. Empty is text; any NUL is binary (the
/// [`UNIFIED_SEPARATOR`] contract); merely-non-UTF-8 text passes.
pub fn is_binary_doc(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    if bytes.contains(&UNIFIED_SEPARATOR) {
        return true;
    }
    if platform_is_binary(bytes).unwrap_or(false) {
        return true;
    }
    portable_is_binary(bytes)
}

/// [`is_binary_doc`] of the file's first [`BINARY_SNIFF_PREFIX`] bytes: true
/// implies it of the whole file.
pub fn sniff_binary(path: &std::path::Path) -> std::io::Result<bool> {
    use std::io::Read as _;
    let mut prefix = Vec::with_capacity(BINARY_SNIFF_PREFIX);
    std::fs::File::open(path)?
        .take(BINARY_SNIFF_PREFIX as u64)
        .read_to_end(&mut prefix)?;
    Ok(is_binary_doc(&prefix))
}

/// Native object magic: `Some(true)` on a hit, `None` = inconclusive, which
/// defers to [`portable_is_binary`] and never means "text".
#[cfg(target_os = "macos")]
fn platform_is_binary(bytes: &[u8]) -> Option<bool> {
    // Mach-O 32/64-bit, either endianness. (`CAFEBABE` fat/class lives in the
    // portable table — it is cross-platform.)
    const MACHO: &[&[u8]] = &[
        b"\xfe\xed\xfa\xce",
        b"\xfe\xed\xfa\xcf",
        b"\xce\xfa\xed\xfe",
        b"\xcf\xfa\xed\xfe",
    ];
    if MACHO.iter().any(|m| bytes.starts_with(m)) {
        Some(true)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn platform_is_binary(bytes: &[u8]) -> Option<bool> {
    if bytes.starts_with(b"\x7fELF") {
        Some(true)
    } else {
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_is_binary(_bytes: &[u8]) -> Option<bool> {
    None
}

/// Magic prefixes, else control density. `bytes` is non-empty and NUL-free.
fn portable_is_binary(bytes: &[u8]) -> bool {
    const MAGIC: &[&[u8]] = &[
        b"\xff\xfe", // UTF-16LE BOM
        b"\xfe\xff", // UTF-16BE BOM
        b"\x1f\x8b", // gzip
        // zip (+ empty archive + spanned).
        b"PK\x03\x04",
        b"PK\x05\x06",
        b"PK\x07\x08",
        b"\x89PNG\r\n\x1a\n", // PNG
        b"\xff\xd8\xff",      // JPEG
        b"GIF87a",
        b"GIF89a",           // GIF
        b"%PDF-",            // PDF
        b"\xca\xfe\xba\xbe", // CAFEBABE: Java class or Mach-O fat binary
    ];
    if MAGIC.iter().any(|m| bytes.starts_with(m)) {
        return true;
    }
    let window = &bytes[..bytes.len().min(BINARY_SNIFF_PREFIX)];
    let suspicious = window
        .iter()
        .filter(|&&b| (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r')) || b == 0x7f)
        .count();
    // Over 5% control bytes: binary.
    suspicious * 20 > window.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_predicate_keeps_text_and_empties() {
        // Empty docs are structural zeros, not binary — they must survive.
        assert!(!is_binary_doc(b""));
        // Plain ASCII / UTF-8 (incl. CJK + emoji), form feeds, and
        // high-byte-but-control-free Latin-1 prose: all text.
        assert!(!is_binary_doc(b"printk(KERN_INFO \"hi\");\n"));
        assert!(!is_binary_doc("kmalloc — 日本語テスト 🎉\n".as_bytes()));
        assert!(!is_binary_doc(b"int x;\x0c\n"));
        assert!(!is_binary_doc(b"caf\xe9 na\xefve soup\xe7on\n"));
        // All-high-byte files are merely non-UTF-8, not control soup: text.
        assert!(!is_binary_doc(&[0x80u8; 64]));
        // A lone stray control byte stays text (the density rule needs >5%).
        let mut mostly_text = vec![b'x'; 9000];
        mostly_text.extend_from_slice(b"printk(ctx);\n");
        mostly_text[8500] = 0x07;
        assert!(!is_binary_doc(&mostly_text));
    }

    #[test]
    fn binary_predicate_drops_nul_and_magic_and_soup() {
        // NUL anywhere (incl. late, past the prefix window) → binary.
        let mut late = vec![b'x'; 9000];
        late.extend_from_slice(b"printk(ctx);\n");
        late[8500] = 0;
        assert!(is_binary_doc(&late));
        // Portable magics (NUL-free) → binary.
        assert!(is_binary_doc(b"\xff\xfeH\x00i\x00")); // UTF-16LE BOM
        assert!(is_binary_doc(b"\xfe\xff\x00H")); // UTF-16BE BOM
        assert!(is_binary_doc(b"\x1f\x8b\x08text")); // gzip
        assert!(is_binary_doc(b"PK\x03\x04text")); // zip
        assert!(is_binary_doc(b"%PDF-1.4 binary")); // PDF
        assert!(is_binary_doc(b"\xca\xfe\xba\xbeABCD")); // CAFEBABE, NUL-free
        // NUL-free control soup → binary (11 suspicious of 14 ≈ 79%).
        assert!(is_binary_doc(
            b"\x01\x02\x03\x04\x05\x06\x07\x08\x0b\x0e\x7fabc"
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn platform_branch_catches_macho_without_nul_or_soup() {
        // Mach-O magic + printable padding is NUL-free and control-free, so
        // only the macOS platform branch catches this.
        let mut header = b"\xcf\xfa\xed\xfe".to_vec();
        header.extend(vec![b'A'; 64]);
        assert!(!header.contains(&0));
        assert!(!portable_is_binary(&header));
        assert_eq!(platform_is_binary(&header), Some(true));
        assert!(is_binary_doc(&header));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn platform_branch_catches_elf_without_nul_or_soup() {
        // ELF magic + printable padding is NUL-free and under the density
        // threshold, so only the Linux platform branch catches this.
        let mut header = b"\x7fELF".to_vec();
        header.extend(vec![b'A'; 64]);
        assert!(!header.contains(&0));
        assert!(!portable_is_binary(&header));
        assert_eq!(platform_is_binary(&header), Some(true));
        assert!(is_binary_doc(&header));
    }

    #[test]
    fn sniff_binary_implies_a_binary_whole() {
        let dir = tempfile::tempdir().unwrap();
        let sniff = |name: &str, bytes: &[u8]| {
            std::fs::write(dir.path().join(name), bytes).unwrap();
            sniff_binary(&dir.path().join(name)).unwrap()
        };
        let mut late = vec![b'x'; BINARY_SNIFF_PREFIX + 10];
        late[BINARY_SNIFF_PREFIX + 1] = 0;
        assert!(
            !sniff("late.c", &late) && is_binary_doc(&late),
            "a NUL past the prefix is found on read"
        );
        assert!(sniff("nul.c", b"alpha\0beta"));
        assert!(sniff("gz.c", b"\x1f\x8btext"));
        assert!(!sniff("text.c", b"alpha\n") && !sniff("empty.c", b""));
        assert!(sniff_binary(&dir.path().join("missing.c")).is_err());
    }
}

//! Which files are searchable text, and their text.

use std::io::Read as _;
use std::path::Path;

use crate::unified::UNIFIED_SEPARATOR;

/// Leading bytes the control-byte rule inspects.
const SNIFF_BYTES: usize = 8192;

/// Longest document: its separator must fit docfm's text bound.
pub const MAX_DOC_BYTES: u64 = docfm::MAX_TEXT_BYTES - 1;

/// A searchable file's text.
#[derive(Debug, PartialEq, Eq)]
pub struct Text {
    pub bytes: Vec<u8>,
    /// Decoded from UTF-16: its length is not the file's.
    pub utf16: bool,
}

/// Why a file is not searched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    Binary,
    TooLarge,
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Skip::Binary => f.write_str("looks binary"),
            Skip::TooLarge => write!(f, "over {MAX_DOC_BYTES} bytes"),
        }
    }
}

/// A file's text: UTF-16 with a BOM decoded to UTF-8 as rg decodes it, else
/// the bytes as they are.
pub fn text(raw: Vec<u8>) -> Result<Text, Skip> {
    if raw.len() as u64 > MAX_DOC_BYTES {
        return Err(Skip::TooLarge);
    }
    let text = match utf16(&raw) {
        Some(bytes) => Text { bytes, utf16: true },
        None => Text {
            bytes: raw,
            utf16: false,
        },
    };
    if text.bytes.len() as u64 > MAX_DOC_BYTES {
        return Err(Skip::TooLarge);
    }
    if is_binary(&text.bytes) {
        return Err(Skip::Binary);
    }
    Ok(text)
}

/// Whether the file's first bytes allow it to be text: false implies
/// [`text`] of the whole file is `Err`. An unreadable file is not text.
pub fn sniff_text(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    if !file.metadata().is_ok_and(|m| m.len() <= MAX_DOC_BYTES) {
        return false;
    }
    // Enough UTF-16 to decode to the control-byte window.
    let mut head = Vec::with_capacity(2 * SNIFF_BYTES + 2);
    if file
        .take(2 * SNIFF_BYTES as u64 + 2)
        .read_to_end(&mut head)
        .is_err()
    {
        return false;
    }
    !is_binary(&utf16(&head).unwrap_or(head))
}

/// The UTF-8 of UTF-16 text after its BOM; `None` without a BOM. Invalid
/// units become U+FFFD as in WHATWG's decoder, which rg uses.
fn utf16(raw: &[u8]) -> Option<Vec<u8>> {
    let unit: fn([u8; 2]) -> u16 = match raw.get(..2)? {
        [0xff, 0xfe] => u16::from_le_bytes,
        [0xfe, 0xff] => u16::from_be_bytes,
        _ => return None,
    };
    let (units, odd) = raw[2..].as_chunks::<2>();
    let mut text = String::with_capacity(raw.len());
    let chars = char::decode_utf16(units.iter().map(|&u| unit(u)));
    text.extend(chars.map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)));
    // An odd last byte is one more U+FFFD, unless it follows an unpaired
    // lead surrogate, which already got one.
    let lead = units
        .last()
        .is_some_and(|&u| (0xd800..0xdc00).contains(&unit(u)));
    if !odd.is_empty() && !lead {
        text.push(char::REPLACEMENT_CHARACTER);
    }
    Some(text.into_bytes())
}

/// A NUL anywhere, a PDF, or over 5% control bytes in the first
/// [`SNIFF_BYTES`].
fn is_binary(text: &[u8]) -> bool {
    if text.contains(&UNIFIED_SEPARATOR) || text.starts_with(b"%PDF-") {
        return true;
    }
    let window = &text[..text.len().min(SNIFF_BYTES)];
    let control = window
        .iter()
        .filter(|&&b| (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r')) || b == 0x7f)
        .count();
    control * 20 > window.len()
}

/// `s` as UTF-16LE with a BOM.
#[cfg(test)]
pub fn utf16le(s: &str) -> Vec<u8> {
    let mut raw = vec![0xff, 0xfe];
    raw.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
    raw
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_text(bytes: &[u8]) -> bool {
        text(bytes.to_vec()).is_ok()
    }

    #[test]
    fn binary_predicate_keeps_text_and_empties() {
        assert!(is_text(b""));
        assert!(is_text(b"printk(KERN_INFO \"hi\");\n"));
        assert!(is_text("kmalloc — 日本語テスト 🎉\n".as_bytes()));
        assert!(is_text(b"int x;\x0c\n"));
        assert!(is_text(b"caf\xe9 na\xefve soup\xe7on\n"));
        assert!(is_text(&[0x80u8; 64]));
        assert!(is_text(b"\xef\xbb\xbf%PDF-1.4"), "a PDF starts the file");
        let mut mostly_text = vec![b'x'; 9000];
        mostly_text.extend_from_slice(b"printk(ctx);\n");
        mostly_text[8500] = 0x07;
        assert!(is_text(&mostly_text));
    }

    #[test]
    fn binary_predicate_drops_nul_pdf_and_soup() {
        let mut late = vec![b'x'; 9000];
        late.extend_from_slice(b"printk(ctx);\n");
        late[8500] = 0;
        assert_eq!(text(late), Err(Skip::Binary));
        assert!(!is_text(b"%PDF-1.4 binary"));
        assert!(!is_text(b"\x1f\x8b\x08text"), "gzip, as control bytes");
        assert!(!is_text(b"PK\x03\x04text"), "zip, as control bytes");
        assert!(!is_text(b"\x01\x02\x03\x04\x05\x06\x07\x08\x0b\x0e\x7fabc"));
    }

    fn decoded(raw: &[u8]) -> Result<Text, Skip> {
        text(raw.to_vec())
    }

    #[test]
    fn utf16_with_a_bom_reads_as_rg_decodes_it() {
        let utf8 = |s: &str| {
            Ok(Text {
                bytes: s.as_bytes().to_vec(),
                utf16: true,
            })
        };
        assert_eq!(
            decoded(&utf16le("hi\r\nCLASS ü 🎉\n")),
            utf8("hi\r\nCLASS ü 🎉\n")
        );
        let mut be = vec![0xfe, 0xff];
        be.extend("hi ü".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(decoded(&be), utf8("hi ü"));
        assert_eq!(decoded(&utf16le("")), utf8(""));
        assert_eq!(decoded(b"\xff\xfe"), utf8(""));
        // The searcher, not the decoder, strips one more BOM, as for UTF-8.
        assert_eq!(
            decoded(&utf16le("\u{feff}hi\u{feff}")),
            utf8("\u{feff}hi\u{feff}")
        );
        let mut odd = utf16le("ab");
        odd.push(b'Z');
        assert_eq!(decoded(&odd), utf8("ab\u{fffd}"));
        let lone = [&utf16le("x")[..], b"\x00\xd8y\x00", b"\x00\xdc"].concat();
        assert_eq!(decoded(&lone), utf8("x\u{fffd}y\u{fffd}"));
        let lead_then_odd = [&utf16le("ab")[..], b"\x00\xd8Z"].concat();
        assert_eq!(decoded(&lead_then_odd), utf8("ab\u{fffd}"));
        assert_eq!(decoded(&utf16le("a\0b")), Err(Skip::Binary));
        assert_eq!(
            decoded(b"\xff\xfe\x00\x00h\x00"),
            Err(Skip::Binary),
            "UTF-32LE"
        );
        assert_eq!(decoded(&utf16le("\x01\x02\x03")), Err(Skip::Binary));
        assert_eq!(decoded(&utf16le("%PDF-1.4")), Err(Skip::Binary));
        assert_eq!(
            decoded(b"\xffhi"),
            Ok(Text {
                bytes: b"\xffhi".to_vec(),
                utf16: false
            })
        );
    }

    #[test]
    fn sniff_text_false_implies_a_skipped_whole() {
        let dir = tempfile::tempdir().unwrap();
        let sniff = |name: &str, bytes: &[u8]| {
            std::fs::write(dir.path().join(name), bytes).unwrap();
            sniff_text(&dir.path().join(name))
        };
        let mut late = vec![b'x'; 2 * SNIFF_BYTES + 10];
        late[2 * SNIFF_BYTES + 3] = 0;
        assert!(sniff("late.c", &late) && !is_text(&late), "found on read");
        assert!(!sniff("nul.c", b"alpha\0beta"));
        assert!(!sniff("gz.c", b"\x1f\x8btext"));
        assert!(sniff("text.c", b"alpha\n") && sniff("empty.c", b""));
        assert!(sniff("le.c", &utf16le("alpha\n")));
        assert!(!sniff("le-nul.c", &utf16le("alpha\0")));
        // A pair cut at the head's end decodes to U+FFFD, never to control.
        let cut = utf16le(&format!("a{}", "🎉".repeat(SNIFF_BYTES)));
        assert!(sniff("cut.c", &cut) && is_text(&cut));
        let soup = utf16le(&format!(
            "{}{}",
            "x".repeat(SNIFF_BYTES - 1),
            "\x01".repeat(1000)
        ));
        assert!(sniff("soup.c", &soup) && is_text(&soup), "past the window");
        assert!(!sniff_text(&dir.path().join("missing.c")));
    }

    #[test]
    fn a_file_over_the_doc_bound_is_not_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.txt");
        std::fs::write(&path, vec![b'x'; 2 * SNIFF_BYTES + 2]).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_len(MAX_DOC_BYTES).unwrap();
        assert!(sniff_text(&path));
        file.set_len(MAX_DOC_BYTES + 1).unwrap();
        assert!(!sniff_text(&path));
        assert_eq!(Skip::TooLarge.to_string(), "over 2147483646 bytes");
    }
}

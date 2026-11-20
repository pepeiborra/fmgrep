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

/// Whether the file's first bytes allow it to be text: `Ok(false)` implies
/// [`text`] of the whole file is `Err`. A file that is gone is not text,
/// as rg would not see it; one that cannot be read, or is over
/// [`MAX_DOC_BYTES`], is an `Err` that says why, for the caller to name.
pub fn sniff_text(path: &Path) -> Result<bool, String> {
    let head = match head(path) {
        Ok(Some(head)) => head,
        Ok(None) => return Err(Skip::TooLarge.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("cannot read ({e})")),
    };
    Ok(!is_binary(&utf16(&head).unwrap_or(head)))
}

/// The text of the file at `path`, or why it is left out: `Err(None)` for a
/// file that is gone or binary, which is no news. The rest say why: it
/// cannot be read, or is over [`MAX_DOC_BYTES`].
pub fn read_text(path: &Path) -> Result<Text, Option<String>> {
    let unread = |e: std::io::Error| {
        (e.kind() != std::io::ErrorKind::NotFound).then(|| format!("cannot read ({e})"))
    };
    if std::fs::metadata(path).map_err(unread)?.len() > MAX_DOC_BYTES {
        return Err(Some(Skip::TooLarge.to_string()));
    }
    let raw = std::fs::read(path).map_err(unread)?;
    text(raw).map_err(|why| (why == Skip::TooLarge).then(|| why.to_string()))
}

/// Bytes read at a time where a NUL is searched for past a file's head.
const CHUNK: usize = 64 * 1024;

/// Whether the whole file at `path` is text, as [`text`] of all of it
/// would say, with at most [`CHUNK`] bytes of it in memory:
/// [`sniff_text`] of its head, then a NUL searched for in the rest. Errs
/// as [`sniff_text`] does.
pub fn whole_text(path: &Path) -> Result<bool, String> {
    if !sniff_text(path)? {
        return Ok(false);
    }
    match holds_nul(path) {
        Ok(nul) => Ok(!nul),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot read ({e})")),
    }
}

/// Whether the text of the file at `path` holds a NUL: a zero byte, or a
/// zero unit after a UTF-16 BOM. [`CHUNK`] is even: a unit never spans two.
fn holds_nul(path: &Path) -> std::io::Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let mut chunk = Vec::with_capacity(CHUNK);
    let mut wide = None;
    loop {
        chunk.clear();
        (&mut file).take(CHUNK as u64).read_to_end(&mut chunk)?;
        if chunk.is_empty() {
            return Ok(false);
        }
        let wide = *wide.get_or_insert_with(|| utf16(&chunk[..chunk.len().min(2)]).is_some());
        let nul = match wide {
            true => chunk.as_chunks::<2>().0.contains(&[0, 0]),
            false => memchr::memchr(0, &chunk).is_some(),
        };
        if nul {
            return Ok(true);
        }
    }
}

/// The leading bytes of the file at `path` that [`sniff_text`] inspects:
/// enough UTF-16 to decode to the control-byte window. None when it is
/// over [`MAX_DOC_BYTES`].
fn head(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_DOC_BYTES {
        return Ok(None);
    }
    let mut head = Vec::with_capacity(2 * SNIFF_BYTES + 2);
    file.take(2 * SNIFF_BYTES as u64 + 2)
        .read_to_end(&mut head)?;
    Ok(Some(head))
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
            sniff_text(&dir.path().join(name)).unwrap()
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
        assert_eq!(sniff_text(&dir.path().join("missing.c")), Ok(false), "gone");
    }

    /// A file that is gone or binary is no news; one that cannot be read, or is
    /// too large, says why.
    #[cfg(unix)]
    #[test]
    fn read_text_says_why_it_cannot_read_a_file() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for (name, bytes) in [
            ("ok.c", &b"printk"[..]),
            ("bin.c", b"printk\0"),
            ("shut.c", b"printk"),
        ] {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        std::fs::File::create(dir.join("huge.log"))
            .unwrap()
            .set_len(1 << 31)
            .unwrap();
        let shut = dir.join("shut.c");
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
        // The size is checked before the read: a file too large is said to be
        // so, though it could not be read either, and is never read whole.
        let huge_shut = dir.join("huge-shut.log");
        std::fs::File::create(&huge_shut)
            .unwrap()
            .set_len(1 << 31)
            .unwrap();
        std::fs::set_permissions(&huge_shut, std::fs::Permissions::from_mode(0o000)).unwrap();
        let read = |name: &str| read_text(&dir.join(name)).map(|t| t.bytes);
        assert_eq!(
            read("huge-shut.log"),
            Err(Some("over 2147483646 bytes".to_owned()))
        );
        assert_eq!(read("ok.c"), Ok(b"printk".to_vec()));
        assert_eq!(read("missing.c"), Err(None));
        assert_eq!(read("bin.c"), Err(None));
        assert_eq!(
            read("huge.log"),
            Err(Some("over 2147483646 bytes".to_owned()))
        );
        if std::fs::read(&shut).is_ok() {
            println!("skipped: the file can be read (running as root)");
            return;
        }
        let why = read("shut.c").unwrap_err().unwrap();
        assert!(why.starts_with("cannot read (Permission denied"), "{why}");
    }

    #[cfg(unix)]
    #[test]
    fn sniff_text_says_why_it_cannot_read() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shut.c");
        std::fs::write(&path, b"printk\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            println!("skipped: the file can be read (running as root)");
            return;
        }
        let why = sniff_text(&path).unwrap_err();
        assert!(why.starts_with("cannot read (Permission denied"), "{why}");
        // A directory opens and then fails to read.
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert!(sniff_text(&sub).unwrap_err().starts_with("cannot read ("));
    }

    #[test]
    fn a_file_over_the_doc_bound_is_not_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.txt");
        std::fs::write(&path, vec![b'x'; 2 * SNIFF_BYTES + 2]).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_len(MAX_DOC_BYTES).unwrap();
        assert_eq!(sniff_text(&path), Ok(true));
        file.set_len(MAX_DOC_BYTES + 1).unwrap();
        assert_eq!(sniff_text(&path), Err("over 2147483646 bytes".to_owned()));
        assert_eq!(Skip::TooLarge.to_string(), "over 2147483646 bytes");
    }

    /// [`whole_text`] says what [`text`] of the whole file says.
    #[test]
    fn whole_text_agrees_with_text_of_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let whole = |name: &str, bytes: &[u8]| {
            std::fs::write(dir.path().join(name), bytes).unwrap();
            let seen = whole_text(&dir.path().join(name)).unwrap();
            assert_eq!(seen, is_text(bytes), "{name}");
            seen
        };
        let plain = vec![b'x'; 3 * CHUNK + 10];
        assert!(whole("plain.c", &plain));
        // A NUL past the head and past a chunk, at a chunk's first byte and
        // at the file's last.
        for at in [2 * CHUNK + 5, CHUNK, plain.len() - 1] {
            let mut late = plain.clone();
            late[at] = 0;
            assert!(!whole(&format!("late{at}.c"), &late), "{at}");
        }
        // UTF-16: its bytes are full of zeros, its text is none; a zero
        // unit past a chunk is a NUL.
        assert!(whole("le.c", &utf16le(&"x".repeat(2 * CHUNK))));
        assert!(!whole(
            "le-nul.c",
            &utf16le(&format!("{}\0", "x".repeat(CHUNK)))
        ));
        let cut = utf16le(&format!("a{}", "🎉".repeat(SNIFF_BYTES)));
        assert!(whole("cut.c", &cut));
        let soup = utf16le(&format!(
            "{}{}",
            "x".repeat(SNIFF_BYTES - 1),
            "\x01".repeat(1000)
        ));
        assert!(whole("soup.c", &soup));
        assert!(!whole("gz.c", b"\x1f\x8btext") && whole("empty.c", b""));
        assert_eq!(whole_text(&dir.path().join("missing.c")), Ok(false));
    }

    /// A file that cannot be read says why, as [`sniff_text`] does.
    #[cfg(unix)]
    #[test]
    fn whole_text_says_why_it_cannot_read() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shut.c");
        std::fs::write(&path, b"printk\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            println!("skipped: the file can be read (running as root)");
            return;
        }
        let why = whole_text(&path).unwrap_err();
        assert!(why.starts_with("cannot read (Permission denied"), "{why}");
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert!(whole_text(&sub).unwrap_err().starts_with("cannot read ("));
    }
}

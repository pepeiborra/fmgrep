//! The manifest's binary form, written and read by hand as the other
//! binary files of the index are: little-endian integers of fixed widths,
//! no alignment, no padding.
//!
//! Header, 88 bytes:
//!
//! | offset | width | field |
//! |---|---|---|
//! | 0 | 8 | magic `b"fmmanif\0"` |
//! | 8 | 4 | `format`, u32 |
//! | 12 | 4 | flags, u32: bit 0 is `store_docs`; every other bit is 0 |
//! | 16 | 8 | `num_docs`, u64 |
//! | 24 | 8 | `num_bytes`, u64 |
//! | 32 | 8 | `bom_bytes`, u64 |
//! | 40 | 8 | `index_size_bytes`, u64 |
//! | 48 | 8 | `build_time_us`, u64 |
//! | 56 | 8 | `index_started_ns`, i64 |
//! | 64 | 8 | shards: the number of blobs, u64 |
//! | 72 | 8 | bytes of `corpus_root`, u64; 0 when absent |
//! | 80 | 8 | bytes of the served commit, u64; 0 |
//!
//! Then the sections, in this order, each right after the one before:
//!
//! 1. `doc_lens`: one u64 per document;
//! 2. path ends: one u64 per document; path `i` is the path text from the
//!    end of path `i - 1` (0 for the first) to its own end;
//! 3. `blob_lens`: one u64 per shard;
//! 4. label ends: one u64 per shard, the same rule over the label text;
//! 5. `corpus_root`, UTF-8;
//! 6. the served commit, UTF-8: kept for a later fmgrep, which fills it;
//!    this one writes none and reads past it;
//! 7. the label text: the `blobs` labels, joined;
//! 8. the path text: the `files` paths, joined, by id.
//!
//! The file ends where the path text ends. The reader checks the magic,
//! then the format, before anything else; it checks every count and length
//! against the file size before it sizes a buffer from it.

use std::io::{self, Read, Write};
use std::path::Path;

use super::{Manifest, refuse_foreign};

const MAGIC: &[u8; 8] = b"fmmanif\0";

/// Bytes of the header.
const HEADER: u64 = 88;

/// The magic and the format: what is read before anything else.
const PREFIX: usize = 12;

/// The flag bit of `store_docs`.
const STORE_DOCS: u32 = 1;

/// Write `m` as `manifest.bin`. Refuses a manifest that this layout cannot
/// hold: tables of different lengths, or an empty `corpus_root`.
pub fn encode(m: &Manifest, out: &mut impl Write) -> io::Result<()> {
    let n = m.files.len();
    if m.num_docs != n || m.doc_lens.len() != n || m.blob_lens.len() != m.blobs.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "manifest tables skew (num_docs={} files={n} doc_lens={} blobs={} blob_lens={})",
                m.num_docs,
                m.doc_lens.len(),
                m.blobs.len(),
                m.blob_lens.len(),
            ),
        ));
    }
    if m.corpus_root.as_deref() == Some("") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an empty corpus_root",
        ));
    }
    let root = m.corpus_root.as_deref().unwrap_or_default();
    out.write_all(MAGIC)?;
    out.write_all(&m.format.to_le_bytes())?;
    out.write_all(&u32::from(m.store_docs).to_le_bytes())?;
    for v in [
        n as u64,
        m.num_bytes,
        m.bom_bytes,
        m.index_size_bytes,
        m.build_time_us,
    ] {
        out.write_all(&v.to_le_bytes())?;
    }
    out.write_all(&m.index_started_ns.to_le_bytes())?;
    for v in [m.blobs.len() as u64, root.len() as u64, 0] {
        out.write_all(&v.to_le_bytes())?;
    }
    for &len in &m.doc_lens {
        out.write_all(&len.to_le_bytes())?;
    }
    write_ends(out, m.files.iter())?;
    for &len in &m.blob_lens {
        out.write_all(&len.to_le_bytes())?;
    }
    write_ends(out, m.blobs.iter())?;
    out.write_all(root.as_bytes())?;
    for label in &m.blobs {
        out.write_all(label.as_bytes())?;
    }
    for path in m.files.iter() {
        out.write_all(path.as_bytes())?;
    }
    Ok(())
}

/// The end of each of `texts` in their concatenation, as u64 words.
fn write_ends<S: AsRef<str>>(
    out: &mut impl Write,
    texts: impl Iterator<Item = S>,
) -> io::Result<()> {
    let mut end = 0u64;
    for text in texts {
        end += text.as_ref().len() as u64;
        out.write_all(&end.to_le_bytes())?;
    }
    Ok(())
}

/// Read the `len` bytes of `manifest.bin` at `path` from `input`, front to
/// back, into owned buffers. A failure that the bytes show reads
/// "error: `path` ...; re-run `index`". The checks of the values are the
/// caller's ([`super::validate`]).
pub fn decode(input: &mut impl Read, len: u64, path: &Path) -> Result<Manifest, String> {
    let bad = |why: String| format!("error: {} {why}; re-run `index`", path.display());
    let short = |need: u128| {
        bad(format!(
            "is truncated: holds {len} byte(s), its sections need {need}"
        ))
    };
    // The next `n` bytes; `at` counts the bytes read so far.
    let mut at = 0u64;
    let mut take = |n: u64| -> Result<Vec<u8>, String> {
        let want = usize::try_from(n).map_err(|_| {
            bad(format!(
                "records a section of {n} byte(s), more than this host holds"
            ))
        })?;
        let mut buf = Vec::with_capacity(want);
        input
            .by_ref()
            .take(n)
            .read_to_end(&mut buf)
            .map_err(|e| format!("error: cannot read {}: {e}", path.display()))?;
        at += buf.len() as u64;
        if buf.len() < want {
            return Err(bad(format!(
                "is truncated: its size is {len} byte(s), but a read ends at byte {at}"
            )));
        }
        Ok(buf)
    };

    let head = take(len.min(HEADER))?;
    if head.len() < PREFIX {
        return Err(short(HEADER.into()));
    }
    if head[..8] != MAGIC[..] {
        return Err(bad("is not an fmgrep manifest".into()));
    }
    refuse_foreign(path, u32_at(&head, 8))?;
    if head.len() < HEADER as usize {
        return Err(short(HEADER.into()));
    }
    let flags = u32_at(&head, 12);
    if flags & !STORE_DOCS != 0 {
        return Err(bad(format!(
            "sets unknown flag bits {:#x}",
            flags & !STORE_DOCS
        )));
    }
    let num_docs = u64_at(&head, 16);
    let num_shards = u64_at(&head, 64);
    let (root_len, commit_len) = (u64_at(&head, 72), u64_at(&head, 80));
    let count = |n: u64, what: &str| {
        usize::try_from(n)
            .map_err(|_| bad(format!("records {n} {what}, more than this host holds")))
    };
    let (docs, shards) = (count(num_docs, "documents")?, count(num_shards, "shards")?);

    let fixed = 16 * u128::from(num_docs)
        + 16 * u128::from(num_shards)
        + u128::from(root_len)
        + u128::from(commit_len);
    if u128::from(HEADER) + fixed > u128::from(len) {
        return Err(short(u128::from(HEADER) + fixed));
    }
    let fixed = take(fixed as u64)?;
    let mut rest = &fixed[..];
    let doc_lens: Vec<u64> = words(split(&mut rest, 8 * docs)).collect();
    let path_ends = ends(split(&mut rest, 8 * docs), "path", &bad)?;
    let blob_lens: Vec<u64> = words(split(&mut rest, 8 * shards)).collect();
    let label_ends = ends(split(&mut rest, 8 * shards), "shard label", &bad)?;
    let root = split(&mut rest, root_len as usize);
    let commit = rest;

    let (labels_len, paths_len) = (label_ends[shards], path_ends[docs]);
    let need = u128::from(HEADER) + fixed.len() as u128 + labels_len as u128 + paths_len as u128;
    if need > u128::from(len) {
        return Err(short(need));
    }
    if need < u128::from(len) {
        return Err(bad(format!(
            "holds {} byte(s) past its last section",
            u128::from(len) - need
        )));
    }
    let utf8 = |bytes: Vec<u8>, what: &str| {
        String::from_utf8(bytes).map_err(|_| bad(format!("holds {what} that is not UTF-8")))
    };
    let corpus_root = match root {
        [] => None,
        _ => Some(utf8(root.to_vec(), "a corpus_root")?),
    };
    std::str::from_utf8(commit)
        .map_err(|_| bad("holds a served commit that is not UTF-8".into()))?;
    let labels = utf8(take(labels_len as u64)?, "a shard label")?;
    cuts(&labels, &label_ends, "shard label", &bad)?;
    let text = utf8(take(paths_len as u64)?, "a path")?;
    cuts(&text, &path_ends, "path", &bad)?;
    Ok(Manifest {
        format: u32_at(&head, 8),
        store_docs: flags & STORE_DOCS != 0,
        num_docs: docs,
        num_bytes: u64_at(&head, 24),
        bom_bytes: u64_at(&head, 32),
        index_size_bytes: u64_at(&head, 40),
        build_time_us: u64_at(&head, 48),
        index_started_ns: u64_at(&head, 56) as i64,
        files: pieces(&text, &path_ends),
        blobs: pieces(&labels, &label_ends),
        doc_lens,
        blob_lens,
        corpus_root,
    })
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut w = [0; 4];
    w.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(w)
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut w = [0; 8];
    w.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(w)
}

/// The first `n` bytes of `rest`, which then holds the bytes after them.
fn split<'a>(rest: &mut &'a [u8], n: usize) -> &'a [u8] {
    let (head, tail) = rest.split_at(n);
    *rest = tail;
    head
}

fn words(b: &[u8]) -> impl Iterator<Item = u64> + '_ {
    (0..b.len() / 8).map(|i| u64_at(b, 8 * i))
}

/// The ends in `b`, after a leading 0: each one at or past the one before.
fn ends(b: &[u8], what: &str, bad: &impl Fn(String) -> String) -> Result<Vec<usize>, String> {
    let mut out = Vec::with_capacity(b.len() / 8 + 1);
    out.push(0);
    for (i, end) in words(b).enumerate() {
        let Ok(end) = usize::try_from(end) else {
            return Err(bad(format!(
                "records the end of {what} {i} at byte {end}, more than this host holds"
            )));
        };
        // The leading 0: the first end cannot go back.
        if end < out[i] {
            return Err(bad(format!(
                "records the end of {what} {i} before the end of {what} {}",
                i - 1
            )));
        }
        out.push(end);
    }
    Ok(out)
}

/// The pieces of `text` between consecutive `ends`.
fn pieces(text: &str, ends: &[usize]) -> Vec<String> {
    ends.windows(2)
        .map(|w| text[w[0]..w[1]].to_owned())
        .collect()
}

/// Every end of `ends` falls between two characters of `text`.
fn cuts(
    text: &str,
    ends: &[usize],
    what: &str,
    bad: &impl Fn(String) -> String,
) -> Result<(), String> {
    match ends.iter().position(|&e| !text.is_char_boundary(e)) {
        None => Ok(()),
        Some(i) => Err(bad(format!(
            "cuts {what} {} inside a character",
            i.saturating_sub(1)
        ))),
    }
}

#[cfg(test)]
mod tests;

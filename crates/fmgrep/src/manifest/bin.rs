//! The manifest's binary form. The header is one `repr(C)` struct of
//! zerocopy's little-endian integers and byte arrays, so the struct is the
//! layout, as in docfm's blob header. The tables are little-endian u64
//! words, viewed in place over the bytes of the file. No field is aligned
//! and there is no padding.
//!
//! Header, 96 bytes ([`OnDisk`]):
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
//! | 80 | 8 | bytes of the served commit, u64; 0 when absent |
//! | 88 | 8 | bytes of `why_no_commit`, u64; 0 when absent |
//!
//! Then the sections, in this order, each right after the one before:
//!
//! 1. `doc_lens`: one u64 per document;
//! 2. path ends: one u64 per document; path `i` is the path text from the
//!    end of path `i - 1` (0 for the first) to its own end;
//! 3. `blob_lens`: one u64 per shard;
//! 4. label ends: one u64 per shard, the same rule over the label text;
//! 5. `corpus_root`, UTF-8;
//! 6. the served commit, UTF-8: the VCS, one space, then the commit id in
//!    lowercase hex (`git 1a2b...`);
//! 7. `why_no_commit`, UTF-8: why the index records no served commit;
//! 8. the label text: the `blobs` labels, joined;
//! 9. the path text: the `files` paths, joined, by id.
//!
//! The file ends where the path text ends. The reader checks the magic,
//! then the format, before anything else. zerocopy checks that each table
//! fits in the bytes left before it makes the view, so no count sizes a
//! buffer before the file is known to hold what it counts. The path text
//! is not read here: a path's UTF-8 is checked when the path is read.

use std::io::{self, Write};
use std::path::Path;

use zerocopy::little_endian::{I64, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes};

use super::files::{Bytes, Lens, Store};
use super::{Commit, FileTable, Manifest, refuse_foreign};

const MAGIC: [u8; 8] = *b"fmmanif\0";

/// The flag bit of `store_docs`.
const STORE_DOCS: u32 = 1;

/// What is read before anything else: the magic, then the format.
#[derive(FromBytes, IntoBytes, Immutable)]
#[repr(C)]
struct Prefix {
    magic: [u8; 8],
    format: U32,
}

/// The header as it lies on disk; the table above gives each field.
#[derive(FromBytes, IntoBytes, Immutable)]
#[repr(C)]
struct OnDisk {
    prefix: Prefix,
    flags: U32,
    num_docs: U64,
    num_bytes: U64,
    bom_bytes: U64,
    index_size_bytes: U64,
    build_time_us: U64,
    index_started_ns: I64,
    num_shards: U64,
    root_len: U64,
    commit_len: U64,
    why_len: U64,
}

const _: () = assert!(size_of::<OnDisk>() == 96);

/// Whether `vcs` and `id` make a served commit this layout holds: a VCS
/// name of lowercase ASCII letters and digits, and an id of lowercase hex
/// digits. Nothing else is safe to hand to the VCS as a commit, nor to
/// print or cut as one.
pub fn commit_ok(vcs: &str, id: &str) -> bool {
    let name = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let hex = |c: u8| c.is_ascii_digit() || (b'a'..=b'f').contains(&c);
    !vcs.is_empty() && vcs.bytes().all(name) && !id.is_empty() && id.bytes().all(hex)
}

/// Write `m` as `manifest.bin`. Refuses a manifest that this layout cannot
/// hold: tables of different lengths, an empty `corpus_root`, or a served
/// commit that [`commit_ok`] refuses.
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
    let commit = match &m.commit {
        None => String::new(),
        Some(c) if !commit_ok(&c.vcs, &c.id) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a served commit of vcs {:?} and id {:?}", c.vcs, c.id),
            ));
        }
        Some(c) => format!("{} {}", c.vcs, c.id),
    };
    let why = m.why_no_commit.as_deref().unwrap_or_default();
    let root = m.corpus_root.as_deref().unwrap_or_default();
    let head = OnDisk {
        prefix: Prefix {
            magic: MAGIC,
            format: U32::new(m.format),
        },
        flags: U32::new(u32::from(m.store_docs)),
        num_docs: U64::new(n as u64),
        num_bytes: U64::new(m.num_bytes),
        bom_bytes: U64::new(m.bom_bytes),
        index_size_bytes: U64::new(m.index_size_bytes),
        build_time_us: U64::new(m.build_time_us),
        index_started_ns: I64::new(m.index_started_ns),
        num_shards: U64::new(m.blobs.len() as u64),
        root_len: U64::new(root.len() as u64),
        commit_len: U64::new(commit.len() as u64),
        why_len: U64::new(why.len() as u64),
    };
    out.write_all(head.as_bytes())?;
    write_words(out, m.doc_lens.iter().copied())?;
    write_words(out, ends_of(paths(&m.files)))?;
    write_words(out, m.blob_lens.iter().copied())?;
    write_words(out, ends_of(m.blobs.iter()))?;
    out.write_all(root.as_bytes())?;
    out.write_all(commit.as_bytes())?;
    out.write_all(why.as_bytes())?;
    for label in &m.blobs {
        out.write_all(label.as_bytes())?;
    }
    for path in paths(&m.files) {
        out.write_all(path)?;
    }
    Ok(())
}

/// `values` as one table of little-endian u64 words.
fn write_words(out: &mut impl Write, values: impl Iterator<Item = u64>) -> io::Result<()> {
    for value in values {
        out.write_all(U64::new(value).as_bytes())?;
    }
    Ok(())
}

/// The bytes of each path of `files`, as they were read.
fn paths(files: &FileTable) -> impl Iterator<Item = &[u8]> {
    (0..files.len()).map(|id| files.raw(id).unwrap_or_default())
}

/// The end of each of `texts` in their concatenation.
fn ends_of<S: AsRef<[u8]>>(texts: impl Iterator<Item = S>) -> impl Iterator<Item = u64> {
    texts.scan(0, |end, text| {
        *end += text.as_ref().len() as u64;
        Some(*end)
    })
}

/// How many bytes from the start of `bytes` hold the header and the four
/// tables, as the header counts them: what [`decode`] reads of a file
/// before the text. 0 unless `bytes` start with the magic and this format
/// and hold every table the header counts: a file of another kind or
/// format, or one too short for its counts, is refused from its header
/// alone, and no count of it sizes a read.
pub fn tables_len(bytes: &[u8]) -> usize {
    if check_prefix(bytes, Path::new("")).is_err() {
        return 0;
    }
    let Ok((head, _)) = OnDisk::read_from_prefix(bytes) else {
        return 0;
    };
    let words = head.num_docs.get().checked_add(head.num_shards.get());
    let len = words
        .and_then(|w| w.checked_mul(16))
        .and_then(|b| b.checked_add(size_of::<OnDisk>() as u64))
        .and_then(|len| usize::try_from(len).ok());
    len.filter(|&len| len <= bytes.len()).unwrap_or(0)
}

/// Refuse `bytes` unless they start with the magic, then this format.
fn check_prefix(bytes: &[u8], path: &Path) -> Result<(), String> {
    match Prefix::read_from_prefix(bytes) {
        Err(_) => Err(truncated(path, bytes.len(), "header")),
        Ok((prefix, _)) if prefix.magic != MAGIC => Err(refuse(path, "is not an fmgrep manifest")),
        Ok((prefix, _)) => refuse_foreign(path, prefix.format.get()),
    }
}

/// The refusal of what the file at `path` holds.
fn refuse(path: &Path, why: impl std::fmt::Display) -> String {
    format!("error: {} {why}; re-run `index`", path.display())
}

/// The refusal of a file of `len` bytes that ends inside its `what`.
fn truncated(path: &Path, len: usize, what: &str) -> String {
    refuse(
        path,
        format!("is truncated: its {len} byte(s) end inside its {what}"),
    )
}

/// Read `bytes`, the whole of the `manifest.bin` at `path`: its map, or
/// its bytes. The path table and `doc_lens` view their tables where they
/// lie in `bytes`. Every check but that of the path text is made here:
/// the header, the sizes, the ends ascending, the labels and the root. A
/// failure that the bytes show reads "error: `path` ...; re-run `index`".
/// The checks of the values are the caller's ([`super::validate`]).
pub fn decode(bytes: impl Bytes + 'static, path: &Path) -> Result<Manifest, String> {
    let store = Store::new(bytes);
    let bytes: &[u8] = &store;
    let bad = |why: String| refuse(path, why);
    let len = bytes.len();
    let short = |what: &str| truncated(path, len, what);

    check_prefix(bytes, path)?;
    let Ok((head, rest)) = OnDisk::read_from_prefix(bytes) else {
        return Err(short("header"));
    };
    let flags = head.flags.get();
    if flags & !STORE_DOCS != 0 {
        return Err(bad(format!(
            "sets unknown flag bits {:#x}",
            flags & !STORE_DOCS
        )));
    }
    let (doc_lens, rest) = words(rest, head.num_docs).ok_or_else(|| short("doc_lens"))?;
    let (path_ends, rest) = words(rest, head.num_docs).ok_or_else(|| short("path ends"))?;
    let (blob_lens, rest) = words(rest, head.num_shards).ok_or_else(|| short("blob_lens"))?;
    let (label_ends, rest) = words(rest, head.num_shards).ok_or_else(|| short("label ends"))?;
    let (root, rest) = split(rest, head.root_len).ok_or_else(|| short("corpus_root"))?;
    let (commit, rest) = split(rest, head.commit_len).ok_or_else(|| short("served commit"))?;
    let (why, rest) = split(rest, head.why_len).ok_or_else(|| short("why_no_commit"))?;
    let labels_len = last_end(label_ends, "shard label").map_err(&bad)?;
    let paths_len = last_end(path_ends, "path").map_err(&bad)?;
    let (labels, paths) = split(rest, labels_len).ok_or_else(|| short("shard labels"))?;
    match split(paths, paths_len) {
        None => return Err(short("paths")),
        Some((_, [])) => {}
        Some((_, past)) => {
            return Err(bad(format!(
                "holds {} byte(s) past its last section",
                past.len()
            )));
        }
    }

    let utf8 = |b, what: &str| {
        std::str::from_utf8(b).map_err(|_| bad(format!("holds {what} that is not UTF-8")))
    };
    let root = utf8(root, "a corpus_root")?;
    let commit = match utf8(commit, "a served commit")? {
        "" => None,
        text => match text.split_once(' ') {
            Some((vcs, id)) if commit_ok(vcs, id) => Some(Commit {
                vcs: vcs.to_owned(),
                id: id.to_owned(),
            }),
            _ => {
                return Err(bad(
                    "holds a served commit that is not a VCS, a space and a hex id".into(),
                ));
            }
        },
    };
    let why = utf8(why, "a why_no_commit")?;
    let labels = utf8(labels, "a shard label")?;
    let label_ends = with_zero(label_ends);
    cuts(labels, &label_ends, "shard label").map_err(&bad)?;
    let files = FileTable::from_parts(
        store.clone(),
        offset(bytes, path_ends.as_bytes()),
        len - paths.len(),
        path_ends.len(),
        path,
    );
    let doc_lens = Lens::of(&store, offset(bytes, doc_lens.as_bytes()), doc_lens);
    Ok(Manifest {
        format: head.prefix.format.get(),
        store_docs: flags & STORE_DOCS != 0,
        num_docs: files.len(),
        num_bytes: head.num_bytes.get(),
        bom_bytes: head.bom_bytes.get(),
        index_size_bytes: head.index_size_bytes.get(),
        build_time_us: head.build_time_us.get(),
        index_started_ns: head.index_started_ns.get(),
        blobs: pieces(labels, &label_ends),
        doc_lens,
        blob_lens: blob_lens.iter().map(|w| w.get()).collect(),
        corpus_root: (!root.is_empty()).then(|| root.to_owned()),
        files,
        commit,
        why_no_commit: (!why.is_empty()).then(|| why.to_owned()),
    })
}

/// Where `part`, a slice of `whole`, starts in it.
fn offset(whole: &[u8], part: &[u8]) -> usize {
    part.as_ptr() as usize - whole.as_ptr() as usize
}

/// The `n` words at the front of `rest`, and the bytes after them; `None`
/// if `rest` holds fewer.
fn words(rest: &[u8], n: U64) -> Option<(&[U64], &[u8])> {
    <[U64]>::ref_from_prefix_with_elems(rest, usize::try_from(n.get()).ok()?).ok()
}

/// The `n` bytes at the front of `rest`, and the bytes after them; `None`
/// if `rest` holds fewer.
fn split(rest: &[u8], n: impl Into<u64>) -> Option<(&[u8], &[u8])> {
    rest.split_at_checked(usize::try_from(n.into()).ok()?)
}

/// The last of `ends` (0 if none), once each is known to be at or past the
/// one before it.
fn last_end(ends: &[U64], what: &str) -> Result<u64, String> {
    let mut last = 0;
    for (i, end) in ends.iter().map(|w| w.get()).enumerate() {
        // The first end cannot go back: ends are not below 0.
        if end < last {
            return Err(format!(
                "records the end of {what} {i} before the end of {what} {}",
                i - 1
            ));
        }
        last = end;
    }
    Ok(last)
}

/// `ends` after a leading 0. Each end is at most the last one, which the
/// caller has found to be the length of a text in memory, so it fits a
/// usize.
fn with_zero(ends: &[U64]) -> Vec<usize> {
    std::iter::once(0)
        .chain(ends.iter().map(|w| w.get() as usize))
        .collect()
}

/// Every end of `ends` falls between two characters of `text`.
fn cuts(text: &str, ends: &[usize], what: &str) -> Result<(), String> {
    match ends.iter().position(|&e| !text.is_char_boundary(e)) {
        None => Ok(()),
        Some(i) => Err(format!(
            "cuts {what} {} inside a character",
            i.saturating_sub(1)
        )),
    }
}

/// The pieces of `text` between consecutive `ends`.
fn pieces(text: &str, ends: &[usize]) -> Vec<String> {
    ends.windows(2)
        .map(|w| text[w[0]..w[1]].to_owned())
        .collect()
}

#[cfg(test)]
mod tests;

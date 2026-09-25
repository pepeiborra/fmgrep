//! Indexable-document discovery: the one enumeration shared by `index` and
//! the bench harness.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::binary::is_binary_doc;
use crate::unified::UNIFIED_SEPARATOR;
use crate::verbose::verbose;

/// Documents one read batch holds: bounded by count AND by bytes, so a batch
/// of huge files cannot raise the walk's peak.
const READ_BATCH_DOCS: usize = 64;
const READ_BATCH_BYTES: u64 = 32 << 20;
/// Reads are latency-bound, not CPU-bound, so the default is capped low.
const MAX_READ_THREADS: usize = 8;
const READ_THREADS_VAR: &str = "FMGREP_READ_THREADS";

/// Document reads in flight, from `FMGREP_READ_THREADS`; junk or zero takes
/// the default.
fn read_threads() -> usize {
    let machine = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::env::var(READ_THREADS_VAR)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| machine.min(MAX_READ_THREADS))
}

/// An indexable document and its bytes.
#[derive(Debug, Clone)]
pub struct Document {
    /// Positional id: the document's index in the enumeration.
    pub id: usize,
    /// `/`-separated path relative to the corpus root, lossy-decoded.
    pub path: String,
    pub bytes: Vec<u8>,
}

/// A file's `(size, mtime)` as the walk read it: what a files-source index
/// checks the indexed file against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub len: u64,
    /// Nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
}

impl Stamp {
    pub fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            mtime_ns: mtime_ns(meta),
        }
    }
}

#[cfg(unix)]
fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt as _;
    meta.mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.mtime_nsec())
}

#[cfg(not(unix))]
fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    let at = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
    match at.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i64,
        Err(e) => -(e.duration().as_nanos() as i64),
    }
}

/// Bytes-free descriptor of one indexable document; its id is its position
/// in [`Corpus::docs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocMeta {
    /// `/`-separated path relative to the corpus root, lossy-decoded. The
    /// enumeration reopens documents through it, so a non-UTF-8 or
    /// backslash-bearing file name is not addressable.
    pub path: String,
    pub len: u64,
}

/// A corpus the runner describes but never holds: root plus descriptors.
#[derive(Debug, Clone, Copy)]
pub struct Corpus<'a> {
    pub root: &'a Path,
    pub docs: &'a [DocMeta],
}

impl Corpus<'_> {
    pub fn num_docs(&self) -> usize {
        self.docs.len()
    }

    /// Sum of [`DocMeta::len`]: the corpus-size line and the throughput
    /// denominator.
    pub fn total_bytes(&self) -> u64 {
        self.docs.iter().map(|d| d.len).sum()
    }

    /// Absolute path of document `id`; `None` out of range.
    pub fn doc_path(&self, id: usize) -> Option<PathBuf> {
        self.docs.get(id).map(|d| self.root.join(&d.path))
    }
}

/// The definition of "indexable": every file under `root`, sorted by relative
/// path, positional ids. Symlinked dirs are never descended, symlinked files
/// are included, `.`-prefixed names are pruned, NUL-bearing and
/// [`is_binary_doc`] files are skipped: one [`skip_summary`] line by default,
/// a `warning:` each under [`verbose`]. An unreadable root or an all-skipped
/// tree is `Err`. `f` takes `(id, rel path, bytes, stamp)`, so the
/// bytes-holding and bytes-free enumerations cannot diverge; its `Err` stops
/// the walk at once and is returned verbatim.
///
/// `exclude` is a directory that is never descended: an index dir written
/// under `root` is output, not input.
///
/// Documents are read in batches on `FMGREP_READ_THREADS` threads and handed
/// to `f` in path order, so parallelism changes neither the ids nor the order
/// of the skip lines.
fn for_each_indexable(
    root: &Path,
    exclude: Option<&Path>,
    mut f: impl FnMut(usize, &str, Vec<u8>, Stamp) -> Result<(), String>,
) -> Result<usize, String> {
    let exclude = exclude.and_then(|p| std::fs::canonicalize(p).ok());
    let mut stack = vec![root.to_path_buf()];
    // Length per document, for the batch byte bound alone.
    let mut rels: Vec<(String, u64)> = Vec::new();
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("error: cannot list folder {}: {e}", dir.display()))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("error: cannot list folder {}: {e}", dir.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if file_type.is_symlink() {
                let meta = std::fs::metadata(&path).ok().filter(|m| m.is_file());
                if let Some(meta) = meta {
                    rels.push((relative_path(root, &path), meta.len()));
                }
                continue;
            }
            if file_type.is_dir() {
                let excluded = exclude
                    .as_ref()
                    .is_some_and(|x| std::fs::canonicalize(&path).is_ok_and(|p| &p == x));
                if !excluded {
                    stack.push(path);
                }
                continue;
            }
            if file_type.is_file() {
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                rels.push((relative_path(root, &path), len));
            }
        }
    }
    rels.sort();
    if rels.is_empty() {
        return Err(format!(
            "error: no indexable files in {} (want regular non-hidden files)",
            root.display()
        ));
    }
    let threads = read_threads();
    let mut kept = 0usize;
    let mut skipped = 0usize;
    let mut at = 0usize;
    while at < rels.len() {
        let end = batch_end(&rels, at);
        let batch = &rels[at..end];
        let read = read_batch(
            root,
            batch,
            &|(rel, _): &(String, u64)| rel.as_str(),
            threads,
        )?;
        for ((rel, _), (bytes, stamp)) in batch.iter().zip(read) {
            if bytes.contains(&UNIFIED_SEPARATOR) {
                skipped += 1;
                if verbose() {
                    eprintln!(
                        "warning: skipping {rel}: contains NUL (reserved as the unified-corpus separator)"
                    );
                }
                continue;
            }
            if is_binary_doc(&bytes) {
                skipped += 1;
                if verbose() {
                    eprintln!("warning: skipping {rel}: looks binary (not searchable text)");
                }
                continue;
            }
            f(kept, rel, bytes, stamp)?;
            kept += 1;
        }
        at = end;
    }
    if kept == 0 {
        return Err(format!(
            "error: no indexable files in {} (every file skipped as NUL-containing or binary)",
            root.display()
        ));
    }
    if skipped > 0 {
        eprintln!("{}", skip_summary(skipped));
    }
    Ok(kept)
}

/// The walk's one default-verbosity line: what the per-file `warning:` lines
/// collapse to.
fn skip_summary(skipped: usize) -> String {
    format!(
        "skipped {skipped} non-text file(s) (binary or NUL); set FMGREP_VERBOSE=1 \
         or pass --verbose to list them"
    )
}

/// End of the batch starting at `at`: at least one document, then up to
/// [`READ_BATCH_DOCS`] of them or [`READ_BATCH_BYTES`], whichever comes first.
fn batch_end(rels: &[(String, u64)], at: usize) -> usize {
    let mut bytes = 0u64;
    let mut end = at;
    while end < rels.len() && end - at < READ_BATCH_DOCS {
        bytes = bytes.saturating_add(rels[end].1);
        end += 1;
        if bytes >= READ_BATCH_BYTES {
            break;
        }
    }
    end
}

/// A document's bytes and the stamp its handle had before the read.
type Read = (Vec<u8>, Stamp);

/// One batch's reads, each tagged with its position in the batch.
type BatchReads = Vec<(usize, Result<Read, String>)>;

/// `fs::read`'s syscalls, with the fstat kept as the stamp.
fn read_doc(root: &Path, rel: &str) -> Result<Read, String> {
    use std::io::Read as _;
    let path = root.join(rel);
    let failed = |e: std::io::Error| format!("error: cannot read {}: {e}", path.display());
    let mut file = std::fs::File::open(&path).map_err(failed)?;
    let stamp = Stamp::of(&file.metadata().map_err(failed)?);
    let mut bytes = Vec::with_capacity(stamp.len as usize);
    file.read_to_end(&mut bytes).map_err(failed)?;
    Ok((bytes, stamp))
}

/// `batch`'s documents in `batch` order, read on up to `threads` threads.
///
/// Slots are handed out ascending and every handed-out read completes, so the
/// lowest-index failure is reported — the one the sequential walk hit first.
fn read_batch<T: Sync>(
    root: &Path,
    batch: &[T],
    rel: &(dyn Fn(&T) -> &str + Sync),
    threads: usize,
) -> Result<Vec<Read>, String> {
    if threads <= 1 || batch.len() <= 1 {
        return batch.iter().map(|t| read_doc(root, rel(t))).collect();
    }
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let done: Mutex<BatchReads> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads.min(batch.len()) {
            scope.spawn(|| {
                loop {
                    // Check before claiming: a claimed slot is always read, so
                    // the lowest failing slot cannot be skipped.
                    if failed.load(Ordering::Relaxed) {
                        break;
                    }
                    let at = next.fetch_add(1, Ordering::Relaxed);
                    if at >= batch.len() {
                        break;
                    }
                    let got = read_doc(root, rel(&batch[at]));
                    failed.fetch_or(got.is_err(), Ordering::Relaxed);
                    done.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((at, got));
                }
            });
        }
    });
    let mut done = done.into_inner().unwrap_or_else(|e| e.into_inner());
    done.sort_by_key(|&(at, _)| at);
    done.into_iter().map(|(_, bytes)| bytes).collect()
}

/// Hand every indexable document under `root` to `f`, one at a time, and
/// return the count: the streaming form of [`discover_index_docs`], so an
/// indexer holds one document instead of the corpus. `f`'s `Err` stops the
/// walk. `exclude` is the [`for_each_indexable`] one. The stamp is the read
/// handle's fstat, taken before the read.
pub fn for_each_index_doc(
    root: &Path,
    exclude: Option<&Path>,
    mut f: impl FnMut(Document, Stamp) -> Result<(), String>,
) -> Result<usize, String> {
    for_each_indexable(root, exclude, |id, path, bytes, stamp| {
        let doc = Document {
            id,
            path: path.to_owned(),
            bytes,
        };
        f(doc, stamp)
    })
}

/// `rels` under `root` with their stamps, in order, read as the walk reads
/// (`FMGREP_READ_THREADS`); the lowest failing read is the error.
pub fn read_stamped(root: &Path, rels: &[String]) -> Result<Vec<(Vec<u8>, Stamp)>, String> {
    read_batch(root, rels, &|rel: &String| rel.as_str(), read_threads())
}

/// The indexable documents under `root`, bytes included, as
/// `for_each_indexable` defines them.
pub fn discover_index_docs(root: &Path) -> Result<Vec<Document>, String> {
    let mut docs = Vec::new();
    for_each_index_doc(root, None, |doc, _| {
        docs.push(doc);
        Ok(())
    })?;
    Ok(docs)
}

/// Bytes-free twin of [`discover_index_docs`]: same walk, same skips, same
/// order, peak memory one file instead of the corpus.
pub fn discover_index_meta(root: &Path) -> Result<Vec<DocMeta>, String> {
    let mut metas = Vec::new();
    for_each_indexable(root, None, |_id, path, bytes, _| {
        metas.push(DocMeta {
            path: path.to_owned(),
            len: bytes.len() as u64,
        });
        Ok(())
    })?;
    Ok(metas)
}

/// Path of `path` relative to `root`, lossy-decoded for [`Document::path`].
/// `path` is always built by joining under `root`, so the prefix strip
/// succeeds; the file name is the fallback (never a panic).
fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .replace('\\', "/")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir.
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fmgrep-discover-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn skip_summary_names_the_count_and_both_switches() {
        let line = skip_summary(70);
        assert!(line.starts_with("skipped 70 non-text file(s)"), "{line}");
        assert!(line.contains("FMGREP_VERBOSE=1"), "{line}");
        assert!(line.contains("--verbose"), "{line}");
        assert_eq!(line.lines().count(), 1, "{line}");
    }

    #[test]
    fn discover_recurses_and_sorts_by_relative_path() {
        let dir = scratch_dir("nested");
        std::fs::write(dir.join("b.c"), b"second").unwrap();
        std::fs::write(dir.join("a.c"), b"first").unwrap();
        std::fs::create_dir_all(dir.join("sub").join("nested")).unwrap();
        std::fs::write(dir.join("sub").join("c.c"), b"third").unwrap();
        std::fs::write(dir.join("sub").join("nested").join("d.c"), b"fourth").unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["a.c", "b.c", "sub/c.c", "sub/nested/d.c"]);
        for (i, d) in docs.iter().enumerate() {
            assert_eq!(d.id, i);
        }
        assert_eq!(docs[0].bytes, b"first");
        assert_eq!(docs[3].bytes, b"fourth");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_prunes_hidden_files_and_dirs() {
        let dir = scratch_dir("hidden");
        std::fs::write(dir.join("a.c"), b"keep").unwrap();
        std::fs::write(dir.join(".hidden"), b"prune").unwrap();
        std::fs::create_dir_all(dir.join(".hdir")).unwrap();
        std::fs::write(dir.join(".hdir").join("x.c"), b"pruned-subtree").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join(".hidden2"), b"prune").unwrap();
        std::fs::write(dir.join("sub").join("b.c"), b"keep").unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["a.c", "sub/b.c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_keeps_equal_basenames_in_different_subdirs() {
        let dir = scratch_dir("collision");
        std::fs::create_dir_all(dir.join("sub1")).unwrap();
        std::fs::create_dir_all(dir.join("sub2")).unwrap();
        std::fs::write(dir.join("sub1").join("a.c"), b"one").unwrap();
        std::fs::write(dir.join("sub2").join("a.c"), b"two").unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].path, "sub1/a.c");
        assert_eq!(docs[0].bytes, b"one");
        assert_eq!(docs[1].path, "sub2/a.c");
        assert_eq!(docs[1].bytes, b"two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_skips_nul_files_and_rejects_empty_or_missing() {
        let dir = scratch_dir("nul");
        std::fs::write(dir.join("a.c"), b"keep").unwrap();
        std::fs::write(dir.join("nul.bin"), b"has\x00nul").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("nul2.bin"), b"also\x00nul").unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].path, "a.c");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(discover_index_docs(std::path::Path::new("/nonexistent-fmgrep-dir-xyz")).is_err());
        let empty = scratch_dir("empty");
        assert!(discover_index_docs(&empty).is_err());
        let _ = std::fs::remove_dir_all(&empty);

        // A tree whose every file is skipped (NUL or binary) is the
        // all-skipped Err branch.
        let all_nul = scratch_dir("all-nul");
        std::fs::write(all_nul.join("x.bin"), b"has\x00nul").unwrap();
        std::fs::write(all_nul.join("y.bin"), b"\x00").unwrap();
        assert!(discover_index_docs(&all_nul).is_err());
        let _ = std::fs::remove_dir_all(&all_nul);
        // Same when every file is binary-but-NUL-free (density soup).
        let all_bin = scratch_dir("all-bin");
        std::fs::write(
            all_bin.join("x.bin"),
            b"\x01\x02\x03\x04\x05\x06\x07\x08\x0b\x0e\x7fabc",
        )
        .unwrap();
        assert!(discover_index_docs(&all_bin).is_err());
        let _ = std::fs::remove_dir_all(&all_bin);
    }

    #[test]
    fn discover_skips_binary_fixtures_but_keeps_text_and_empties() {
        let dir = scratch_dir("binary-filter");
        std::fs::write(dir.join("a.c"), "printk — 日本語\n").unwrap();
        std::fs::write(dir.join("empty.c"), b"").unwrap();
        std::fs::write(dir.join("formfeed.c"), b"int x;\x0c\n").unwrap();
        // ELF-ish object (magic + NULs, like a real .o).
        let mut elf = b"\x7fELF\x02\x01\x01".to_vec();
        elf.extend(vec![0u8; 64]);
        elf.extend_from_slice(b".text.symtab");
        std::fs::write(dir.join("o.o"), &elf).unwrap();
        // NUL-dense blob.
        std::fs::write(dir.join("blob.bin"), b"\x01binary\x00blob\x00dense").unwrap();
        // UTF-16LE file (BOM + interleaved NULs).
        std::fs::write(
            dir.join("utf16.c"),
            b"\xff\xfe#\x00i\x00n\x00c\x00l\x00u\x00d\x00e\x00",
        )
        .unwrap();
        // NUL-free control soup.
        std::fs::write(
            dir.join("soup.bin"),
            b"\x01\x02\x03\x04\x05\x06\x07\x08\x0b\x0e\x7fabc",
        )
        .unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["a.c", "empty.c", "formfeed.c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn discover_excludes_native_macho_object_from_index_list() {
        // A Mach-O object with no NUL and no control soup is excluded via the
        // macOS platform branch.
        let dir = scratch_dir("binary-filter-macho");
        std::fs::write(dir.join("keep.c"), b"printk here\n").unwrap();
        let mut header = b"\xcf\xfa\xed\xfe".to_vec();
        header.extend(vec![b'A'; 64]);
        std::fs::write(dir.join("macho.o"), &header).unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["keep.c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn discover_excludes_native_elf_object_from_index_list() {
        // An ELF object with no NUL and sub-threshold density is excluded via
        // the Linux platform branch.
        let dir = scratch_dir("binary-filter-elf");
        std::fs::write(dir.join("keep.c"), b"printk here\n").unwrap();
        let mut header = b"\x7fELF".to_vec();
        header.extend(vec![b'A'; 64]);
        std::fs::write(dir.join("elf.o"), &header).unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["keep.c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn discover_never_descends_symlink_dirs_but_follows_symlink_files() {
        use std::os::unix::fs::symlink;
        let dir = scratch_dir("symlink");
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real").join("a.c"), b"keep").unwrap();
        std::fs::write(dir.join("real").join("b.c"), b"keep").unwrap();
        symlink(dir.join("real"), dir.join("linkdir")).unwrap();
        symlink(dir.join("real").join("a.c"), dir.join("alias.c")).unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        let mut paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        paths.sort();
        // The symlinked dir contributes nothing (never descended); the
        // symlinked file counts (harness p.is_file() follows links).
        assert_eq!(paths, vec!["alias.c", "real/a.c", "real/b.c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Anti-divergence gate: the bytes-free enumeration must name exactly the
    /// same documents, in the same order, on every fixture shape above.
    #[test]
    fn meta_enumeration_equals_docs_enumeration() {
        let dir = scratch_dir("meta-vs-docs");
        std::fs::write(dir.join("a.c"), b"printk hello").unwrap();
        std::fs::write(dir.join("empty.c"), b"").unwrap();
        std::fs::write(dir.join("nul.bin"), b"has\x00nul").unwrap();
        std::fs::write(
            dir.join("soup.bin"),
            b"\x01\x02\x03\x04\x05\x06\x07\x08\x0b\x0e\x7fabc",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("b.c"), b"kmalloc").unwrap();
        std::fs::write(dir.join(".hidden.c"), b"printk").unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        let metas = discover_index_meta(&dir).unwrap();
        assert_eq!(docs.len(), metas.len());
        for (d, m) in docs.iter().zip(metas.iter()) {
            assert_eq!(d.path, m.path);
            assert_eq!(d.bytes.len() as u64, m.len);
        }
        assert_eq!(
            metas.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(),
            vec!["a.c", "empty.c", "sub/b.c"]
        );
        // Both enumerations reject the same degenerate trees.
        let empty = scratch_dir("meta-empty");
        assert!(discover_index_meta(&empty).is_err());
        assert!(discover_index_docs(&empty).is_err());
        let _ = std::fs::remove_dir_all(&empty);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Parallelism is a read strategy, not a walk: the documents, their ids
    /// and their bytes must be the sequential walk's whatever the thread
    /// count, over more than two batches.
    #[test]
    fn parallel_reads_match_the_sequential_walk() {
        let dir = scratch_dir("parallel-reads");
        let n = READ_BATCH_DOCS * 2 + 3;
        for i in 0..n {
            std::fs::write(dir.join(format!("f{i:04}.c")), format!("printk {i}\n")).unwrap();
        }
        let mut rels: Vec<(String, u64)> = (0..n)
            .map(|i| {
                let rel = format!("f{i:04}.c");
                let len = std::fs::metadata(dir.join(&rel)).unwrap().len();
                (rel, len)
            })
            .collect();
        rels.sort();
        assert!(batch_end(&rels, 0) < rels.len());
        fn rel((r, _): &(String, u64)) -> &str {
            r
        }
        let one = read_batch(&dir, &rels, &rel, 1).unwrap();
        let many = read_batch(&dir, &rels, &rel, 4).unwrap();
        assert_eq!(one, many);
        let docs = discover_index_docs(&dir).unwrap();
        assert_eq!(docs.len(), n);
        for (i, doc) in docs.iter().enumerate() {
            assert_eq!(doc.id, i);
            assert_eq!(doc.path, rels[i].0);
            assert_eq!(doc.bytes, one[i].0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reads race, the reported failure does not: it is the lowest-index one,
    /// the failure the sequential walk hit first.
    #[test]
    fn a_batch_reports_its_lowest_read_failure() {
        let dir = scratch_dir("read-failure");
        std::fs::write(dir.join("a.c"), b"fine").unwrap();
        let rels: Vec<(String, u64)> = ["a.c", "gone-1.c", "also-gone-2.c", "gone-3.c"]
            .iter()
            .map(|r| ((*r).to_string(), 4))
            .collect();
        for threads in [1usize, 4] {
            let err = read_batch(&dir, &rels, &|(r, _): &(String, u64)| r.as_str(), threads)
                .expect_err("missing files must fail");
            assert!(err.contains("gone-1.c"), "threads={threads} got: {err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stamp_matches_metadata_and_read_stamped_agrees_with_walk() {
        let dir = scratch_dir("stamps");
        std::fs::write(dir.join("a.c"), b"printk").unwrap();
        std::fs::write(dir.join("b.c"), b"").unwrap();
        let mut walked = Vec::new();
        for_each_index_doc(&dir, None, |doc, stamp| {
            let meta = std::fs::metadata(dir.join(&doc.path)).unwrap();
            assert_eq!(stamp, Stamp::of(&meta), "{}", doc.path);
            assert_eq!(stamp.len, doc.bytes.len() as u64);
            walked.push((doc.path, doc.bytes, stamp));
            Ok(())
        })
        .unwrap();
        let rels: Vec<String> = walked.iter().map(|(p, _, _)| p.clone()).collect();
        let reread = read_stamped(&dir, &rels).unwrap();
        for ((_, bytes, stamp), got) in walked.iter().zip(&reread) {
            assert_eq!((bytes, stamp), (&got.0, &got.1));
        }
        let err = read_stamped(&dir, &["gone.c".to_string()]).unwrap_err();
        assert!(err.contains("gone.c"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn meta_len_matches_filesystem_metadata() {
        let dir = scratch_dir("meta-len");
        std::fs::write(dir.join("a.c"), vec![b'x'; 4096]).unwrap();
        std::fs::write(dir.join("b.c"), b"").unwrap();
        let metas = discover_index_meta(&dir).unwrap();
        let corpus = Corpus {
            root: &dir,
            docs: &metas,
        };
        for m in &metas {
            let on_disk = std::fs::metadata(dir.join(&m.path)).unwrap().len();
            assert_eq!(m.len, on_disk, "{}", m.path);
        }
        assert_eq!(corpus.num_docs(), 2);
        assert_eq!(corpus.total_bytes(), 4096);
        assert_eq!(corpus.doc_path(0), Some(dir.join("a.c")));
        assert_eq!(corpus.doc_path(2), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

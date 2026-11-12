//! Indexable-document discovery: the one enumeration shared by `index` and
//! the bench harness.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use rayon::prelude::*;

use crate::binary::{MAX_DOC_BYTES, Skip, Text, text};
use crate::verbose::verbose;

/// Documents one read batch holds: bounded by count AND by bytes, so a batch
/// of huge files cannot raise the walk's peak.
const READ_BATCH_DOCS: usize = 64;
const READ_BATCH_BYTES: u64 = 32 << 20;
/// Reads are latency-bound, not CPU-bound, so the count is capped low.
const MAX_READ_THREADS: usize = 8;

/// A pool of up to [`MAX_READ_THREADS`] threads for document reads.
pub fn read_pool() -> Result<rayon::ThreadPool, String> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(MAX_READ_THREADS));
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| format!("error: cannot start read threads: {e}"))
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
/// in [`discover_index_meta`]'s list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocMeta {
    /// `/`-separated path relative to the corpus root, lossy-decoded. The
    /// enumeration reopens documents through it, so a non-UTF-8 file name is
    /// not addressable.
    pub path: String,
    pub len: u64,
}

/// What the walk saw besides documents; every list ascending.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkNotes {
    /// Every listed directory, root-relative, `""` the root, stamped before
    /// listing; `None` when unstattable.
    pub dirs: Vec<(String, Option<Stamp>)>,
    /// Every ignore file below the root, root-relative, stamped before it is
    /// read; then, `None` when absent, those of the root and its ancestors
    /// (`../`-prefixed) and the global excludes file (absolute).
    pub ignores: Vec<(String, Option<Stamp>)>,
}

/// The ignore files a directory may hold, besides [`GIT_EXCLUDE`].
const IGNORE_FILES: [&str; 3] = [".gitignore", ".ignore", ".rgignore"];
const GIT_EXCLUDE: &str = ".git/info/exclude";

/// rg's default walk: ignore files honoured, hidden names and symlinks skipped.
fn builder(root: &Path) -> ignore::WalkBuilder {
    let mut builder = ignore::WalkBuilder::new(root);
    builder.add_custom_ignore_filename(".rgignore");
    builder
}

/// The walk's rule for one root-relative path at a time.
pub fn rules(root: &Path) -> ignore::IncrementalIgnore {
    builder(root).build_matchers().remove(0)
}

/// The directory whose rules ignore file `rel` changes, if it is one.
pub fn ignore_file_dir(rel: &str) -> Option<&str> {
    if let Some(dir) = rel.strip_suffix(GIT_EXCLUDE) {
        return (dir.is_empty() || dir.ends_with('/')).then(|| dir.trim_end_matches('/'));
    }
    let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    IGNORE_FILES.contains(&name).then_some(dir)
}

/// `path`'s stamp, symlinks followed; `None` when absent.
pub fn stamp(path: &Path) -> Option<Stamp> {
    std::fs::metadata(path).ok().map(|m| Stamp::of(&m))
}

/// `dir`'s ignore files, each stamped.
pub fn ignore_files(dir: &Path) -> impl Iterator<Item = (&'static str, Option<Stamp>)> + '_ {
    IGNORE_FILES
        .into_iter()
        .chain([GIT_EXCLUDE])
        .map(|name| (name, stamp(&dir.join(name))))
}

/// `rel/name`, `name` alone at the root.
pub fn join_rel(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_owned()
    } else {
        format!("{rel}/{name}")
    }
}

/// The ignore inputs from above the root: those of the root and its
/// ancestors, then the global excludes file.
fn outer_ignores(root: &Path) -> Vec<(String, Option<Stamp>)> {
    let mut rows = Vec::new();
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    for (up, dir) in canonical.ancestors().enumerate() {
        let prefix = "../".repeat(up);
        rows.extend(ignore_files(dir).map(|(name, s)| (format!("{prefix}{name}"), s)));
    }
    if let Some(global) = ignore::gitignore::gitconfig_excludes_path() {
        rows.push((global.to_string_lossy().into_owned(), stamp(&global)));
    }
    rows
}

/// The definition of "indexable": every file `rg --files` lists under
/// `root`, sorted by relative path, positional ids. A file [`text`] refuses
/// is skipped: a binary one in one [`skip_summary`] line by default, a
/// `warning:` each under [`verbose`]; a too-large one with a `warning:`
/// always. An unlistable directory or an all-skipped tree is `Err`. `f` takes
/// `(rel path, text, stamp, bytes read)`, the stamp being the read handle's
/// fstat before the read; its `Err` stops the walk at once and is returned
/// verbatim.
///
/// `exclude` is a directory that is never descended: an index dir written
/// under `root` is output, not input.
///
/// Documents are read in parallel batches and handed to `f` in path order, so
/// parallelism changes neither the ids nor the order of the skip lines.
pub fn for_each_index_doc(
    root: &Path,
    exclude: Option<&Path>,
    f: impl FnMut(&str, Text, Stamp, u64) -> Result<(), String>,
) -> Result<WalkNotes, String> {
    for_each_listed_doc(root, exclude, |_| {}, f)
}

/// [`for_each_index_doc`], calling `listed` with the number of files the
/// walk listed, the ones it will skip included, once it has listed them
/// all and before it reads the first.
pub fn for_each_listed_doc(
    root: &Path,
    exclude: Option<&Path>,
    listed: impl FnOnce(usize),
    mut f: impl FnMut(&str, Text, Stamp, u64) -> Result<(), String>,
) -> Result<WalkNotes, String> {
    let (rels, notes) = list(root, exclude)?;
    if rels.is_empty() {
        return Err(format!(
            "error: no indexable files in {} (every file is ignored, hidden or a symlink)",
            root.display()
        ));
    }
    listed(rels.len());
    let pool = read_pool()?;
    let mut kept = 0usize;
    let mut skipped = 0usize;
    let mut at = 0usize;
    while at < rels.len() {
        let end = batch_end(&rels, at);
        let batch = &rels[at..end];
        let read = read_batch(&pool, root, batch, &|(rel, _): &(String, u64)| rel.as_str())?;
        for ((rel, _), (raw, stamp)) in batch.iter().zip(read) {
            let got = raw.len() as u64;
            let text = if stamp.len > MAX_DOC_BYTES {
                Err(Skip::TooLarge)
            } else {
                text(raw)
            };
            let text = match text {
                Ok(text) => text,
                Err(why) => {
                    let binary = why == Skip::Binary;
                    skipped += usize::from(binary);
                    if !binary || verbose() {
                        eprintln!("warning: skipping {rel}: {why}");
                    }
                    continue;
                }
            };
            f(rel, text, stamp, got)?;
            kept += 1;
        }
        at = end;
    }
    if kept == 0 {
        return Err(format!(
            "error: no indexable files in {} (every file skipped as binary or too large)",
            root.display()
        ));
    }
    if skipped > 0 {
        eprintln!("{}", skip_summary(skipped));
    }
    Ok(notes)
}

/// Whether `path` is the canonical `exclude`.
fn is_excluded(path: &Path, exclude: Option<&Path>) -> bool {
    exclude.is_some_and(|x| {
        x.file_name() == path.file_name() && std::fs::canonicalize(path).is_ok_and(|p| p == x)
    })
}

fn is_dir(entry: &ignore::DirEntry) -> bool {
    entry.file_type().is_some_and(|t| t.is_dir())
}

/// The walk's files, sorted, each with its length, and what it saw besides.
fn list(root: &Path, exclude: Option<&Path>) -> Result<(Vec<(String, u64)>, WalkNotes), String> {
    let exclude = exclude.and_then(|p| std::fs::canonicalize(p).ok());
    let mut ignores = outer_ignores(root);
    let notes = Arc::new(Mutex::new(WalkNotes {
        dirs: vec![(String::new(), stamp(root))],
        ignores: Vec::new(),
    }));
    let mut walk = builder(root);
    let (seen, base) = (Arc::clone(&notes), root.to_path_buf());
    walk.filter_entry(move |entry| {
        if !is_dir(entry) {
            return true;
        }
        if is_excluded(entry.path(), exclude.as_deref()) {
            return false;
        }
        let dir = entry.metadata().ok().map(|m| Stamp::of(&m));
        let rel = relative_path(&base, entry.path());
        let found: Vec<_> = ignore_files(entry.path())
            .filter_map(|(name, s)| Some((join_rel(&rel, name), Some(s?))))
            .collect();
        let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
        seen.dirs.push((rel, dir));
        seen.ignores.extend(found);
        true
    });
    let files = Mutex::new(Vec::new());
    let failed = Mutex::new(None);
    walk.build_parallel().run(|| {
        Box::new(|entry| {
            let entry = match entry {
                Ok(entry) => entry,
                // Only a listing error has a depth; an ignore file's is skipped, as rg does.
                Err(e) if e.depth().is_some() => {
                    let mut failed = failed.lock().unwrap_or_else(PoisonError::into_inner);
                    failed.get_or_insert(format!("error: cannot list folder {e}"));
                    return ignore::WalkState::Quit;
                }
                Err(e) => {
                    if verbose() {
                        eprintln!("warning: {e}");
                    }
                    return ignore::WalkState::Continue;
                }
            };
            if let Some(e) = entry.error()
                && verbose()
            {
                eprintln!("warning: {e}");
            }
            if entry.file_type().is_some_and(|t| t.is_file()) {
                let len = entry.metadata().map_or(0, |m| m.len());
                files
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((relative_path(root, entry.path()), len));
            }
            ignore::WalkState::Continue
        })
    });
    if let Some(e) = failed.into_inner().unwrap_or_else(PoisonError::into_inner) {
        return Err(e);
    }
    let mut rels = files.into_inner().unwrap_or_else(PoisonError::into_inner);
    rels.sort();
    let mut notes = std::mem::take(&mut *notes.lock().unwrap_or_else(PoisonError::into_inner));
    notes.dirs.sort_by(|a, b| a.0.cmp(&b.0));
    ignores.append(&mut notes.ignores);
    ignores.sort_by(|a, b| a.0.cmp(&b.0));
    notes.ignores = ignores;
    Ok((rels, notes))
}

/// Every dir at or under `top` that the index walk of `root` enters, plus each `.git/info` beside one.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn walk_dirs(root: &Path, top: &Path) -> Vec<PathBuf> {
    let rel = relative_path(root, top);
    if !rel.is_empty() && rules(root).matched(&rel, true).is_ignore() {
        return Vec::new();
    }
    let found = Mutex::new(Vec::new());
    builder(top).build_parallel().run(|| {
        Box::new(|entry| {
            if let Ok(entry) = entry
                && is_dir(&entry)
            {
                let info = entry.path().join(".git/info");
                let mut found = found.lock().unwrap_or_else(PoisonError::into_inner);
                if info.is_dir() {
                    found.push(info);
                }
                found.push(entry.into_path());
            }
            ignore::WalkState::Continue
        })
    });
    found.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// Paths relative to `root`, sorted, of every file under `under` the index
/// walk lists before its text check. An unreadable directory is skipped.
pub fn walk_paths(root: &Path, under: &Path, exclude: Option<&Path>) -> Vec<String> {
    walk_changed(root, vec![under.to_path_buf()], exclude, |_| false)
}

/// [`walk_paths`] under each of `dirs`, never descending into a directory
/// whose relative path `prune` holds.
pub fn walk_changed(
    root: &Path,
    dirs: Vec<PathBuf>,
    exclude: Option<&Path>,
    prune: impl Fn(&str) -> bool + Send + Sync + 'static,
) -> Vec<String> {
    // The walker never filters its roots.
    let mut matcher = rules(root);
    let mut seeds = dirs.into_iter().filter(|dir| {
        let rel = relative_path(root, dir);
        rel.is_empty() || !matcher.matched(&rel, true).is_ignore()
    });
    let Some(first) = seeds.next() else {
        return Vec::new();
    };
    let mut walk = builder(&first);
    for seed in seeds {
        walk.add(seed);
    }
    let exclude = exclude.and_then(|p| std::fs::canonicalize(p).ok());
    let base = root.to_path_buf();
    walk.filter_entry(move |entry| {
        !is_dir(entry)
            || !is_excluded(entry.path(), exclude.as_deref())
                && !prune(&relative_path(&base, entry.path()))
    });
    let found = Mutex::new(Vec::new());
    walk.build_parallel().run(|| {
        Box::new(|entry| {
            if let Ok(entry) = entry
                && entry.file_type().is_some_and(|t| t.is_file())
            {
                found
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(relative_path(root, entry.path()));
            }
            ignore::WalkState::Continue
        })
    });
    let mut found = found.into_inner().unwrap_or_else(PoisonError::into_inner);
    found.sort_unstable();
    found
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

/// `fs::read`'s syscalls, with the fstat kept as the stamp. A file over
/// [`MAX_DOC_BYTES`] is not read: its bytes are empty.
fn read_doc(root: &Path, rel: &str) -> Result<Read, String> {
    use std::io::Read as _;
    let path = root.join(rel);
    let failed = |e: std::io::Error| format!("error: cannot read {}: {e}", path.display());
    let mut file = std::fs::File::open(&path).map_err(failed)?;
    let stamp = Stamp::of(&file.metadata().map_err(failed)?);
    if stamp.len > MAX_DOC_BYTES {
        return Ok((Vec::new(), stamp));
    }
    let mut bytes = Vec::with_capacity(stamp.len as usize);
    file.read_to_end(&mut bytes).map_err(failed)?;
    Ok((bytes, stamp))
}

/// `batch`'s documents in `batch` order, read on `pool`; the lowest-index
/// failure wins.
pub fn read_batch<T: Sync>(
    pool: &rayon::ThreadPool,
    root: &Path,
    batch: &[T],
    rel: &(dyn Fn(&T) -> &str + Sync),
) -> Result<Vec<Read>, String> {
    let read: Vec<_> = pool.install(|| batch.par_iter().map(|t| read_doc(root, rel(t))).collect());
    read.into_iter().collect()
}

/// The indexable documents under `root`, bytes included.
#[cfg(test)]
pub fn discover_index_docs(root: &Path) -> Result<Vec<Document>, String> {
    let mut docs = Vec::new();
    for_each_index_doc(root, None, |path, text, _, _| {
        docs.push(Document {
            id: docs.len(),
            path: path.to_owned(),
            bytes: text.bytes,
        });
        Ok(())
    })?;
    Ok(docs)
}

/// The indexable documents under `root` without their bytes: the
/// [`for_each_index_doc`] walk, peak memory one file instead of the corpus.
pub fn discover_index_meta(root: &Path) -> Result<Vec<DocMeta>, String> {
    let mut metas = Vec::new();
    for_each_index_doc(root, None, |path, text, _, _| {
        metas.push(DocMeta {
            path: path.to_owned(),
            len: text.bytes.len() as u64,
        });
        Ok(())
    })?;
    Ok(metas)
}

/// Path of `path` relative to `root`, lossy-decoded for [`Document::path`].
/// `path` is always built by joining under `root`, so the prefix strip
/// succeeds; the file name is the fallback (never a panic).
fn relative_path(root: &Path, path: &Path) -> String {
    let file_name = Path::new(path.file_name().unwrap_or_default());
    let rel = path
        .strip_prefix(root)
        .unwrap_or(file_name)
        .to_string_lossy();
    // Only Windows separates with `\`; elsewhere it is part of a name.
    if cfg!(windows) {
        rel.replace('\\', "/")
    } else {
        rel.into_owned()
    }
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

    fn pool(threads: usize) -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
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

    #[cfg(unix)]
    #[test]
    fn discover_keeps_a_backslash_in_a_file_name() {
        let dir = scratch_dir("backslash");
        std::fs::write(dir.join("str\\\\escape.rs"), b"keep").unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["str\\\\escape.rs"]);
        assert_eq!(discover_index_meta(&dir).unwrap()[0].path, paths[0]);
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
        assert_eq!(paths, vec!["a.c", "empty.c", "formfeed.c", "utf16.c"]);
        assert_eq!(docs[3].bytes, b"#include");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_skips_a_file_over_the_doc_bound_unread() {
        let dir = scratch_dir("too-large");
        std::fs::write(dir.join("a.c"), b"keep").unwrap();
        let file = std::fs::File::create(dir.join("huge.log")).unwrap();
        file.set_len(MAX_DOC_BYTES + 1).unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["a.c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn discover_skips_symlinks_like_rg() {
        use std::os::unix::fs::symlink;
        let dir = scratch_dir("symlink");
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real").join("a.c"), b"keep").unwrap();
        std::fs::write(dir.join("real").join("b.c"), b"keep").unwrap();
        symlink(dir.join("real"), dir.join("linkdir")).unwrap();
        symlink(dir.join("real").join("a.c"), dir.join("alias.c")).unwrap();
        let docs = discover_index_docs(&dir).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["real/a.c", "real/b.c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Whether a directory above the scratch dirs holds `.git`, which makes
    /// every scratch tree part of a repository.
    fn scratch_in_a_repo() -> bool {
        let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        tmp.ancestors().any(|d| d.join(".git").exists())
    }

    fn put(dir: &Path, files: &[(&str, &str)]) {
        for (rel, text) in files {
            std::fs::create_dir_all(dir.join(rel).parent().unwrap()).unwrap();
            std::fs::write(dir.join(rel), text).unwrap();
        }
    }

    fn indexed(dir: &Path) -> Vec<String> {
        discover_index_meta(dir)
            .unwrap()
            .into_iter()
            .map(|m| m.path)
            .collect()
    }

    #[test]
    fn discover_follows_ignore_files_like_rg() {
        let dir = scratch_dir("ignores");
        std::fs::create_dir_all(dir.join(".git/info")).unwrap();
        put(
            &dir,
            &[
                (".gitignore", "ig/\n*.log\n!keep.log\np.c\n"),
                (".ignore", "!p.c\n!.wl.c\n"),
                (".rgignore", "rg.c\n"),
                (".git/info/exclude", "ex.c\n"),
                ("a.c", "keep"),
                ("ig/x.c", "ignored dir"),
                ("x.log", "ignored"),
                ("keep.log", "whitelisted"),
                ("p.c", ".ignore over .gitignore"),
                ("rg.c", "ignored"),
                ("ex.c", "ignored"),
                (".wl.c", "whitelisted hidden file"),
                (".hidden.c", "hidden"),
                ("sub/.gitignore", "n.c\n"),
                ("sub/n.c", "ignored"),
                ("sub/m.c", "keep"),
            ],
        );
        assert_eq!(
            indexed(&dir),
            [".wl.c", "a.c", "keep.log", "p.c", "sub/m.c"]
        );
        let notes = for_each_index_doc(&dir, None, |_, _, _, _| Ok(())).unwrap();
        let dirs: Vec<&str> = notes.dirs.iter().map(|(rel, _)| rel.as_str()).collect();
        assert_eq!(dirs, ["", "sub"], "ignored dirs are never listed");
        assert_eq!(walk_paths(&dir, &dir, None), indexed(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_reads_ignore_files_above_the_root() {
        let dir = scratch_dir("parent-ignores");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        put(
            &dir,
            &[
                (".gitignore", "*.gen\n/sub/top.c\n"),
                (".ignore", "*.tmp\n"),
                ("sub/a.c", "keep"),
                ("sub/b.gen", "ignored"),
                ("sub/c.tmp", "ignored"),
                ("sub/top.c", "ignored"),
            ],
        );
        assert_eq!(indexed(&dir.join("sub")), ["a.c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_applies_gitignore_only_in_a_repo() {
        if scratch_in_a_repo() {
            return;
        }
        let dir = scratch_dir("no-repo");
        put(
            &dir,
            &[
                (".gitignore", "*.gen\n"),
                (".ignore", "*.tmp\n"),
                ("a.c", "keep"),
                ("b.gen", "kept"),
                ("c.tmp", "ignored"),
            ],
        );
        assert_eq!(indexed(&dir), ["a.c", "b.gen"]);
        std::fs::create_dir(dir.join(".git")).unwrap();
        assert_eq!(indexed(&dir), ["a.c"]);
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
        let one = read_batch(&pool(1), &dir, &rels, &rel).unwrap();
        let many = read_batch(&pool(4), &dir, &rels, &rel).unwrap();
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
        std::fs::write(dir.join("big.c"), vec![b'x'; 8 << 20]).unwrap();
        let mut rels = vec!["big.c".to_string()];
        rels.extend((1..64).map(|i| format!("gone-{i:02}.c")));
        for threads in [1usize, 4] {
            let err = read_batch(&pool(threads), &dir, &rels, &String::as_str)
                .expect_err("missing files must fail");
            assert!(err.contains("gone-01.c"), "threads={threads} got: {err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stamp_matches_metadata_and_a_reread_agrees_with_walk() {
        let dir = scratch_dir("stamps");
        std::fs::write(dir.join("a.c"), b"printk").unwrap();
        std::fs::write(dir.join("b.c"), b"").unwrap();
        let mut walked = Vec::new();
        for_each_index_doc(&dir, None, |path, text, stamp, read| {
            let meta = std::fs::metadata(dir.join(path)).unwrap();
            assert_eq!(stamp, Stamp::of(&meta), "{path}");
            assert_eq!((stamp.len, read), (text.bytes.len() as u64, stamp.len));
            walked.push((path.to_owned(), text.bytes, stamp));
            Ok(())
        })
        .unwrap();
        let rels: Vec<String> = walked.iter().map(|(p, _, _)| p.clone()).collect();
        let reread = read_batch(&read_pool().unwrap(), &dir, &rels, &String::as_str).unwrap();
        for ((_, bytes, stamp), got) in walked.iter().zip(&reread) {
            assert_eq!((bytes, stamp), (&got.0, &got.1));
        }
        let gone = ["gone.c".to_string()];
        let err = read_batch(&read_pool().unwrap(), &dir, &gone, &String::as_str).unwrap_err();
        assert!(err.contains("gone.c"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn meta_len_matches_filesystem_metadata() {
        let dir = scratch_dir("meta-len");
        std::fs::write(dir.join("a.c"), vec![b'x'; 4096]).unwrap();
        std::fs::write(dir.join("b.c"), b"").unwrap();
        let metas = discover_index_meta(&dir).unwrap();
        assert_eq!(metas.len(), 2);
        for m in &metas {
            let on_disk = std::fs::metadata(dir.join(&m.path)).unwrap().len();
            assert_eq!(m.len, on_disk, "{}", m.path);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Minus its binary files, the free walk lists what the index walk
    /// indexes, in order, whatever the thread count.
    #[test]
    fn walk_paths_equals_the_index_walk_before_its_text_check() {
        let dir = scratch_dir("walk-paths");
        std::fs::write(dir.join("a.c"), b"printk").unwrap();
        std::fs::write(dir.join("nul.bin"), b"has\x00nul").unwrap();
        std::fs::write(dir.join(".hidden.c"), b"printk").unwrap();
        std::fs::create_dir_all(dir.join(".hdir")).unwrap();
        std::fs::write(dir.join(".hdir").join("x.c"), b"pruned").unwrap();
        std::fs::create_dir_all(dir.join("sub").join("nested")).unwrap();
        std::fs::write(dir.join("sub").join("b.c"), b"kmalloc").unwrap();
        std::fs::write(dir.join("sub").join("nested").join("d.c"), b"deep").unwrap();
        std::fs::create_dir_all(dir.join("idx")).unwrap();
        std::fs::write(dir.join("idx").join("manifest.json"), b"{}").unwrap();
        std::fs::write(dir.join(".ignore"), b"*.skip\n").unwrap();
        std::fs::write(dir.join("sub").join("x.skip"), b"ignored").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("sub"), dir.join("linkdir")).unwrap();
            std::os::unix::fs::symlink(dir.join("a.c"), dir.join("alias.c")).unwrap();
        }
        let exclude = dir.join("idx");
        let mut indexed = Vec::new();
        for_each_index_doc(&dir, Some(&exclude), |path, _, _, _| {
            indexed.push(path.to_owned());
            Ok(())
        })
        .unwrap();
        let walked = walk_paths(&dir, &dir, Some(&exclude));
        let text: Vec<String> = walked
            .iter()
            .filter(|p| !p.ends_with(".bin"))
            .cloned()
            .collect();
        assert_eq!(text, indexed);
        assert!(walked.contains(&"nul.bin".to_string()));
        assert_eq!(
            walk_paths(&dir, &dir.join("sub"), None),
            ["sub/b.c", "sub/nested/d.c"]
        );
        assert!(walk_paths(&dir, &dir.join("missing"), None).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn walk_notes_record_dirs_and_skip_non_text_files() {
        use std::os::unix::fs::symlink;
        let dir = scratch_dir("walk-dirs");
        for (rel, bytes) in [
            ("a.c", &b"keep"[..]),
            ("nul.bin", b"has\x00nul"),
            (
                "soup.bin",
                b"\x01\x02\x03\x04\x05\x06\x07\x08\x0b\x0e\x7fabc",
            ),
            ("sub/b.c", b"keep"),
            ("sub/deep/c.c", b"keep"),
            (".hdir/x.c", b"pruned"),
            ("idx/manifest.json", b"{}"),
            ("links/l.c", b"keep"),
        ] {
            std::fs::create_dir_all(dir.join(rel).parent().unwrap()).unwrap();
            std::fs::write(dir.join(rel), bytes).unwrap();
        }
        symlink(dir.join("sub"), dir.join("links/linkdir")).unwrap();
        symlink(dir.join("nul.bin"), dir.join("bin-alias.o")).unwrap();
        let mut kept = Vec::new();
        let notes = for_each_index_doc(&dir, Some(&dir.join("idx")), |rel, _, _, _| {
            kept.push(rel.to_owned());
            Ok(())
        })
        .unwrap();
        let stamp = |rel: &str| {
            Some(Stamp::of(
                &std::fs::symlink_metadata(dir.join(rel)).unwrap(),
            ))
        };
        let want = [
            ("", stamp("")),
            ("links", stamp("links")),
            ("sub", stamp("sub")),
            ("sub/deep", stamp("sub/deep")),
        ];
        assert_eq!(notes.dirs, want.map(|(r, s)| (r.to_string(), s)));
        assert_eq!(kept, ["a.c", "links/l.c", "sub/b.c", "sub/deep/c.c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_notes_record_ignore_files() {
        let dir = scratch_dir("walk-ignores");
        put(
            &dir,
            &[
                ("a.c", "keep"),
                ("sub/.rgignore", "*.gen\n"),
                ("sub/b.c", "keep"),
                ("skip/.gitignore", "x\n"),
                ("skip/c.c", "keep"),
            ],
        );
        std::fs::write(dir.join(".ignore"), b"*.tmp\nskip/\n").unwrap();
        let notes = for_each_index_doc(&dir, None, |_, _, _, _| Ok(())).unwrap();
        let stamp = |rel: &str| Some(Stamp::of(&std::fs::metadata(dir.join(rel)).unwrap()));
        let rows: Vec<&(String, Option<Stamp>)> = notes
            .ignores
            .iter()
            .filter(|(rel, _)| !rel.starts_with("../") && !rel.starts_with('/'))
            .collect();
        let want = [
            (".git/info/exclude", None),
            (".gitignore", None),
            (".ignore", stamp(".ignore")),
            (".rgignore", None),
            ("sub/.rgignore", stamp("sub/.rgignore")),
        ]
        .map(|(rel, s)| (rel.to_string(), s));
        assert_eq!(rows, want.iter().collect::<Vec<_>>());
        let depth = std::fs::canonicalize(&dir).unwrap().ancestors().count();
        let above = notes
            .ignores
            .iter()
            .filter(|(rel, _)| rel.starts_with("../"))
            .count();
        assert_eq!(above, 4 * (depth - 1), "each ancestor's, present or not");
        assert!(notes.ignores.is_sorted_by(|a, b| a.0 < b.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_changed_lists_nothing_under_an_ignored_seed() {
        let dir = scratch_dir("ignored-seed");
        put(
            &dir,
            &[
                (".ignore", "ig/\n"),
                ("ig/deep/a.c", "ignored"),
                (".hdir/b.c", "hidden"),
                ("ok/c.c", "keep"),
            ],
        );
        let seeds = vec![
            dir.join("ig"),
            dir.join("ig/deep"),
            dir.join(".hdir"),
            dir.join("ok"),
        ];
        assert_eq!(walk_changed(&dir, seeds, None, |_| false), ["ok/c.c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_changed_descends_only_unrecorded_dirs() {
        let dir = scratch_dir("walk-changed");
        for rel in [
            "top.c",
            "sub/a.c",
            "sub/old/o.c",
            "sub/new/n.c",
            "sub/new/deep/d.c",
            "sub/idx/m.json",
        ] {
            std::fs::create_dir_all(dir.join(rel).parent().unwrap()).unwrap();
            std::fs::write(dir.join(rel), b"text").unwrap();
        }
        let old = |rel: &str| rel == "sub/old";
        let exclude = dir.join("sub/idx");
        let got = walk_changed(&dir, vec![dir.join("sub")], Some(&exclude), old);
        assert_eq!(got, ["sub/a.c", "sub/new/deep/d.c", "sub/new/n.c"]);
        let seeds = vec![dir.join("sub/old"), dir.join("sub")];
        let got = walk_changed(&dir, seeds, Some(&exclude), old);
        assert_eq!(
            got,
            ["sub/a.c", "sub/new/deep/d.c", "sub/new/n.c", "sub/old/o.c"]
        );
        let all = walk_changed(&dir, vec![dir.clone()], None, |_| false);
        assert_eq!(all, walk_paths(&dir, &dir, None));
        assert_eq!(all.len(), 6);
        assert!(walk_changed(&dir, Vec::new(), None, |_| false).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

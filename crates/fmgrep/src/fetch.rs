use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::manifest::{DOCS_SHARD, Manifest, shard_offsets};
use crate::source::{Files, Stamps, Watch};

/// One positional `read` at `off`, leaving the handle's cursor untouched:
/// `read_at` on Unix, `seek_read` on Windows, a cloned-handle seek+read
/// elsewhere (same bytes, one extra fd).
fn pread_once(file: &std::fs::File, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt as _;
        file.read_at(buf, off)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt as _;
        file.seek_read(buf, off)
    }
    #[cfg(not(any(unix, windows)))]
    {
        use std::io::{Read as _, Seek as _};
        let mut clone = file.try_clone()?;
        clone.seek(std::io::SeekFrom::Start(off))?;
        clone.read(buf)
    }
}

/// Positional `read_exact` at `off`: one handle serves any thread and any
/// sequence with no seek syscall and no cursor cross-talk. Short reads
/// (truncated shard) surface as `UnexpectedEof`, never a silent short buffer.
pub fn pread_exact(file: &std::fs::File, mut buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        match pread_once(file, buf, off)? {
            0 => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                ));
            }
            n => {
                off += n as u64;
                buf = &mut buf[n..];
            }
        }
    }
    Ok(())
}

/// A zeroed `len`-byte buffer, recycled from `pool` when one is big enough:
/// no previous doc's bytes stay observable.
fn fetch_buffer(pool: &mut Vec<Vec<u8>>, pool_bytes: &mut u64, len: usize) -> Vec<u8> {
    if let Some(i) = pool.iter().rposition(|b| b.capacity() >= len) {
        let mut buf = pool.swap_remove(i);
        *pool_bytes -= buf.capacity() as u64;
        buf.clear();
        buf.resize(len, 0);
        buf
    } else {
        vec![0u8; len]
    }
}

/// Ceiling on buffer bytes pinned per fetch-worker pool: returns past this are
/// dropped instead of pooled, so pools stay O(batch), not O(candidate set).
const MAX_POOL_BYTES_PER_WORKER: u64 = 1 << 18;

/// One reused parallel-fetch worker: the shard handle (shard source only)
/// plus a free-list of fetch buffers, both reused across every streaming
/// batch of one term. Reads are positional, so workers share no cursor.
struct FetchWorker {
    file: Option<std::fs::File>,
    pool: Vec<Vec<u8>>,
    pool_bytes: u64,
}

enum Source {
    Shard { path: PathBuf, offsets: Vec<u64> },
    Files(Files),
}

/// The query's one read path for document bytes, from `docs.shard` or the
/// indexed files, with a cross-term cache cleared per query. Nothing is
/// opened until a batch has a non-empty read.
pub struct DocStore {
    source: Source,
    lens: Vec<u64>,
    cache: HashMap<usize, Vec<u8>>,
    cached_bytes: u64,
    max_cached_bytes: u64,
    /// Stale bytes pinned this query, outside `cached_bytes`.
    stale_bytes: u64,
    /// The stale docs, and the stale plus gone ones, ascending, as of
    /// `epoch`.
    stale: Vec<usize>,
    changed: Vec<usize>,
    epoch: usize,
    fetches: u64,
    hits: u64,
}

/// Ceiling on bytes pinned by the cross-term cache: small overlaps (the usual
/// shape) dedup fully, larger candidate unions bypass and re-read per term.
const MAX_CACHED_BYTES: u64 = 1 << 20;

/// Ceiling on stale docs' bytes pinned for every term of a query.
const STALE_CACHE_BYTES: u64 = 64 << 20;

impl DocStore {
    /// The manifest's layout. Stored docs ignore `corpus_root`; a missing
    /// `docs.shard` surfaces only on the first non-empty fetch. The files
    /// layout maps `docs.stamps` and checks `corpus_root` (else the
    /// manifest's) is a directory.
    pub fn open(
        index_dir: &Path,
        manifest: &Manifest,
        corpus_root: Option<&Path>,
    ) -> Result<Self, String> {
        let source = match (manifest.store_docs, corpus_root) {
            (true, _) => Source::Shard {
                path: index_dir.join(DOCS_SHARD),
                offsets: shard_offsets(&manifest.doc_lens)?,
            },
            (false, root) => {
                let root = match (root, &manifest.corpus_root) {
                    (Some(root), _) => root.to_path_buf(),
                    (None, Some(root)) => PathBuf::from(root),
                    (None, None) => {
                        return Err("error: manifest records no corpus_root".to_string());
                    }
                };
                let stamps = Stamps::open(index_dir, manifest.files.len())?;
                Source::Files(Files::open(root, stamps, manifest.files.len())?)
            }
        };
        Ok(Self::of(source, &manifest.doc_lens))
    }

    fn of(source: Source, lens: &[u64]) -> Self {
        Self {
            source,
            lens: lens.to_vec(),
            cache: HashMap::new(),
            cached_bytes: 0,
            max_cached_bytes: MAX_CACHED_BYTES,
            stale_bytes: 0,
            stale: Vec::new(),
            changed: Vec::new(),
            epoch: 0,
            fetches: 0,
            hits: 0,
        }
    }

    #[cfg(test)]
    fn shard(path: PathBuf, doc_lens: &[u64]) -> Result<Self, String> {
        let offsets = shard_offsets(doc_lens)?;
        Ok(Self::of(Source::Shard { path, offsets }, doc_lens))
    }

    #[cfg(test)]
    fn files(files: Files, doc_lens: &[u64]) -> Self {
        Self::of(Source::Files(files), doc_lens)
    }

    /// Joined before it returns: the stale set is known on return.
    pub fn stat_all(&self, files: &[String]) {
        if let Source::Files(f) = &self.source {
            f.stat_all(files, &self.lens);
        }
    }

    /// `None` for a shard source.
    pub fn watch(&self) -> Option<Result<Watch, String>> {
        match &self.source {
            Source::Shard { .. } => None,
            Source::Files(f) => Some(f.watch()),
        }
    }

    /// Start a query: drop the cache and its counters, and learn which docs
    /// are stale or gone.
    pub fn begin(&mut self) {
        self.cache.clear();
        self.cached_bytes = 0;
        self.stale_bytes = 0;
        self.fetches = 0;
        self.hits = 0;
        self.refresh(true);
    }

    fn refresh(&mut self, force: bool) {
        let Source::Files(f) = &self.source else {
            return;
        };
        let epoch = f.epoch();
        if force || epoch != self.epoch {
            self.epoch = epoch;
            (self.stale, self.changed) = f.snapshot();
            self.changed.extend_from_slice(&self.stale);
            self.changed.sort_unstable();
        }
    }

    /// The stale docs, ascending, as of the last [`Self::begin`] or
    /// [`Self::screen`]: every term scans them in full.
    pub fn stale(&self) -> &[usize] {
        &self.stale
    }

    /// The stale and gone docs, ascending, as of the last [`Self::begin`] or
    /// [`Self::screen`]: what the screen drops.
    pub fn changed(&self) -> &[usize] {
        &self.changed
    }

    /// Drop the stale and gone docs from one term's answer, first catching
    /// up with changes seen since the last call.
    pub fn screen(&mut self, exact: &mut Vec<(usize, u64)>, candidates: &mut Vec<usize>) {
        self.refresh(false);
        if self.changed.is_empty() {
            return;
        }
        let keep = |id: usize| self.changed.binary_search(&id).is_err();
        exact.retain(|&(id, _)| keep(id));
        candidates.retain(|&id| keep(id));
    }

    fn is_stale(&self, id: usize) -> bool {
        self.stale.binary_search(&id).is_ok()
    }

    /// Physical reads performed and cache hits served without a read.
    #[cfg(test)]
    fn fetches(&self) -> u64 {
        self.fetches
    }

    #[cfg(test)]
    fn hits(&self) -> u64 {
        self.hits
    }

    /// Byte range `(offset, len)` of doc `doc_idx` in its source: pure range
    /// math, resolved single-threaded up front so worker threads only read.
    fn range_of(&self, doc_idx: usize) -> Result<(u64, usize), String> {
        let Some(&len) = self.lens.get(doc_idx) else {
            return Err(format!(
                "error: doc id {doc_idx} out of range for index with {} doc(s)",
                self.lens.len()
            ));
        };
        let lo = match &self.source {
            Source::Shard { offsets, .. } => offsets[doc_idx],
            Source::Files(_) => 0,
        };
        let len: usize = len.try_into().map_err(|_| {
            format!("error: doc id {doc_idx} holds {len} bytes, past addressable memory")
        })?;
        Ok((lo, len))
    }
}

impl Source {
    fn worker(&self) -> Result<FetchWorker, String> {
        let file = match self {
            Source::Shard { path, .. } => Some(
                std::fs::File::open(path)
                    .map_err(|e| format!("error: cannot read {}: {e}", path.display()))?,
            ),
            Source::Files(_) => None,
        };
        Ok(FetchWorker {
            file,
            pool: Vec::new(),
            pool_bytes: 0,
        })
    }

    /// Doc `doc_idx`'s `len` bytes at `lo`, into a pooled buffer; `None`
    /// for an excluded file.
    fn read(
        &self,
        worker: &mut FetchWorker,
        files: &[String],
        doc_idx: usize,
        lo: u64,
        len: usize,
    ) -> Result<Option<Vec<u8>>, String> {
        let mut buf = fetch_buffer(&mut worker.pool, &mut worker.pool_bytes, len);
        match self {
            Source::Shard { path, .. } => {
                let file = worker
                    .file
                    .as_ref()
                    .expect("a shard worker holds the shard");
                pread_exact(file, &mut buf, lo)
                    .map_err(|e| format!("error: cannot read {}: {e}", path.display()))?;
                Ok(Some(buf))
            }
            Source::Files(f) => Ok(f.read(doc_idx, &files[doc_idx], buf)),
        }
    }
}

/// Worker count for per-term parallel fetch: machine parallelism, clamped to
/// the batch's read count inside [`fetch_stream_parallel`].
pub fn fetch_threads() -> usize {
    std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1)
}

/// Reads in flight from the files source: open(2) contends past this.
const FILES_READ_THREADS: usize = 4;

/// Docs whose bytes may be resident at once per streaming batch.
const FETCH_STREAM_BATCH: usize = 32;

/// How one planned candidate is served: from the cross-term cache, from
/// nothing (a zero-length doc), or by a read of `(offset, len)`.
#[derive(Clone, Copy)]
enum Slot {
    Cached,
    Empty,
    Range(u64, usize),
}

/// One read doc: its id, and its bytes with their mapped value unless its
/// file was excluded.
type Fetched<T> = (usize, Option<(Vec<u8>, T)>);

/// Read one batch's ranges in parallel and map each over its bytes, ascending
/// by doc id. Worker handles open only for a non-empty batch.
fn read_batch<T: Send>(
    source: &Source,
    files: &[String],
    ranges: &[(usize, u64, usize)],
    max_threads: usize,
    map: &(dyn Fn(usize, &[u8]) -> T + Sync),
    workers: &mut Vec<FetchWorker>,
) -> Result<Vec<Fetched<T>>, String> {
    if ranges.is_empty() {
        return Ok(Vec::new());
    }
    let workers_n = max_threads.min(ranges.len()).max(1);
    while workers.len() < workers_n {
        workers.push(source.worker()?);
    }
    let read_part = |worker: &mut FetchWorker,
                     part: &[(usize, u64, usize)]|
     -> Result<Vec<Fetched<T>>, String> {
        let mut out = Vec::with_capacity(part.len());
        for &(doc_idx, lo, len) in part {
            let read = source.read(worker, files, doc_idx, lo, len)?.map(|bytes| {
                let v = map(doc_idx, &bytes);
                (bytes, v)
            });
            out.push((doc_idx, read));
        }
        Ok(out)
    };
    if workers_n == 1 {
        return read_part(&mut workers[0], ranges);
    }
    // `chunks_mut(1)` lends each thread a disjoint worker slot.
    let chunk = ranges.len().div_ceil(workers_n);
    let mut fetched: Vec<Fetched<T>> = Vec::with_capacity(ranges.len());
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for (slot, part) in workers[..workers_n].chunks_mut(1).zip(ranges.chunks(chunk)) {
            let worker = &mut slot[0];
            let read_part = &read_part;
            handles.push(s.spawn(move || read_part(worker, part)));
        }
        for h in handles {
            match h.join() {
                Ok(Ok(part)) => fetched.extend(part),
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err("error: parallel fetch worker panicked".to_string()),
            }
        }
        Ok(())
    })?;
    fetched.sort_by_key(|&(doc_idx, _)| doc_idx);
    Ok(fetched)
}

/// Pin `bytes` in the cross-term cache under its byte budget (a stale doc's
/// under the stale one), else return it to a worker pool round-robin
/// (deterministic).
fn store_or_pool(
    docs: &mut DocStore,
    workers: &mut [FetchWorker],
    cursor: &mut usize,
    doc_idx: usize,
    bytes: Vec<u8>,
) {
    if docs.is_stale(doc_idx) && docs.stale_bytes + bytes.len() as u64 <= STALE_CACHE_BYTES {
        docs.stale_bytes += bytes.len() as u64;
        docs.cache.insert(doc_idx, bytes);
    } else if docs.cached_bytes + bytes.len() as u64 <= docs.max_cached_bytes {
        docs.cached_bytes += bytes.len() as u64;
        docs.cache.insert(doc_idx, bytes);
        debug_assert!(docs.cache.len() <= docs.lens.len());
    } else if !workers.is_empty() {
        let w = *cursor % workers.len();
        *cursor += 1;
        if workers[w].pool_bytes + bytes.capacity() as u64 <= MAX_POOL_BYTES_PER_WORKER {
            workers[w].pool_bytes += bytes.capacity() as u64;
            workers[w].pool.push(bytes);
        }
    }
}

/// Sink for one streamed document: `(doc id, its bytes, the mapped value)`.
type Emit<'a, T> = dyn FnMut(usize, &[u8], T) -> Result<(), String> + 'a;

/// Fetch every candidate doc, map it over its bytes in parallel, and emit the
/// result single-threaded in ascending doc order as each batch completes.
///
/// `doc_ids` ascending unique. Cache state, counters and mapped values do not
/// depend on the thread count; one batch is resident at a time. Empty docs
/// (not stale) map over `&[]` with no I/O. An excluded file is skipped; a
/// missing or short shard, or an `emit` error, aborts loudly.
pub fn fetch_stream_parallel<T: Send>(
    docs: &mut DocStore,
    files: &[String],
    doc_ids: &[usize],
    max_threads: usize,
    map: &(dyn Fn(usize, &[u8]) -> T + Sync),
    emit: &mut Emit<'_, T>,
) -> Result<(), String> {
    debug_assert!(doc_ids.windows(2).all(|w| w[0] < w[1]));
    let max_threads = match docs.source {
        Source::Shard { .. } => max_threads,
        Source::Files(_) => max_threads.min(FILES_READ_THREADS),
    };
    // One ascending plan: every range resolves before a thread spawns, so a
    // range error never races a read.
    let mut plan: Vec<(usize, Slot)> = Vec::with_capacity(doc_ids.len());
    for &doc_idx in doc_ids {
        let slot = if docs.cache.contains_key(&doc_idx) {
            Slot::Cached
        } else {
            match docs.range_of(doc_idx)? {
                (_, 0) if !docs.is_stale(doc_idx) => Slot::Empty,
                (lo, len) => Slot::Range(lo, len),
            }
        };
        plan.push((doc_idx, slot));
    }
    // Workers are created lazily and reused across every batch of this term.
    let mut workers: Vec<FetchWorker> = Vec::new();
    for batch in plan.chunks(FETCH_STREAM_BATCH) {
        let ranges: Vec<(usize, u64, usize)> = batch
            .iter()
            .filter_map(|&(doc_idx, slot)| match slot {
                Slot::Range(lo, len) => Some((doc_idx, lo, len)),
                _ => None,
            })
            .collect();
        let fetched = read_batch(&docs.source, files, &ranges, max_threads, map, &mut workers)?;
        docs.fetches += ranges.len() as u64;
        let mut read = fetched.into_iter();
        let mut cursor = 0usize;
        for &(doc_idx, slot) in batch {
            match slot {
                Slot::Cached => {
                    docs.hits += 1;
                    let raw = &docs.cache[&doc_idx];
                    let v = map(doc_idx, raw);
                    emit(doc_idx, raw, v)?;
                }
                Slot::Empty => {
                    docs.fetches += 1;
                    emit(doc_idx, &[], map(doc_idx, &[]))?;
                }
                Slot::Range(..) => {
                    let (id, got) = read.next().expect("one read per planned range");
                    debug_assert_eq!(id, doc_idx);
                    if let Some((bytes, v)) = got {
                        emit(doc_idx, &bytes, v)?;
                        store_or_pool(docs, &mut workers, &mut cursor, doc_idx, bytes);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

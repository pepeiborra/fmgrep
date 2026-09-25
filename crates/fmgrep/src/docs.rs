//! The index's documents, streamed in one read pass over the corpus and read
//! back a blob slot at a time: from `docs.shard` (no separators, the length
//! table recovers every boundary) or from the files themselves. The stream
//! announces a slot as a [`Cut`] the moment its range closes, so construction
//! overlaps the read pass; [`Docs`] is the manifest's tables.

mod feed;
mod read;

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::manifest::{DOCS_SHARD, SHARD_WRITE_BUFFER, shard_offsets};
use crate::plan::Cutter;

use feed::CutSource;
pub use feed::{Cut, Cuts, Shape};

/// One index's per-document manifest tables; `Cuts` holds the documents.
pub struct Docs {
    /// Corpus-relative path per doc, kept so a refusal can name the document.
    paths: Vec<String>,
    /// Raw stored byte length per doc, ascending by id.
    lens: Vec<u64>,
    /// Prefix sums of `lens`; the last entry is the shard's byte count.
    offsets: Vec<u64>,
    /// Leading UTF-8 BOM bytes over all docs, the manifest's `bom_bytes`.
    bom_bytes: u64,
    mtimes: Vec<i64>,
    /// Canonical corpus root; files layout only.
    root: Option<String>,
}

/// Stream every indexable document under `folder` while `build` consumes the
/// shards of at most `cap` text bytes it closes; `store_docs` also copies
/// them into `<output>/docs.shard`. `build` runs on the caller's thread, so
/// its refusal keeps the caller's unwind path, and a stream error is reported
/// over a build error: it is the earlier cause.
pub fn stream_while<R>(
    folder: &Path,
    output: &Path,
    cap: u64,
    store_docs: bool,
    build: impl FnOnce(&Cuts) -> Result<R, String>,
) -> Result<(Docs, R), String> {
    let (source, root) = if store_docs {
        (CutSource::Shard(output.join(DOCS_SHARD)), None)
    } else {
        let root = corpus_root(folder)?;
        let utf8 = root.to_str().map(str::to_owned);
        (CutSource::Files(root), utf8)
    };
    let (cuts, writer) = feed::feed(source.clone());
    let (streamed, built) = std::thread::scope(|scope| {
        let worker = scope.spawn(|| stream_into(folder, output, &source, cap, writer));
        let built = {
            // Scoped, so a build that gives up stops the stream before the
            // join below waits for it.
            let guard = AbortOnDrop(&cuts);
            let built = build(&cuts);
            if built.is_ok() {
                guard.disarm();
            }
            built
        };
        (worker.join(), built)
    });
    let streamed = match streamed {
        Ok(streamed) => streamed,
        Err(payload) => std::panic::resume_unwind(payload),
    };
    // The stream's error wins: it is why the build saw a short plan.
    if let Err(e) = &streamed
        && e.as_str() != ABORTED
    {
        return Err(e.clone());
    }
    let built = built?;
    let mut docs = streamed.expect("the stream only aborts once the build has failed");
    docs.root = root;
    Ok((docs, built))
}

/// `folder` canonicalized; a files-layout manifest records it as UTF-8.
fn corpus_root(folder: &Path) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(folder)
        .map_err(|e| format!("error: cannot resolve {}: {e}", folder.display()))?;
    if root.to_str().is_none() {
        return Err(format!(
            "error: corpus root {} is not UTF-8; the index records it verbatim",
            root.display()
        ));
    }
    Ok(root)
}

/// Stops the stream when the build gives up, however it gives up.
struct AbortOnDrop<'a>(&'a Cuts);

impl AbortOnDrop<'_> {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for AbortOnDrop<'_> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The read pass: one document at a time, copied into `docs.shard` (flushed
/// before every cut it announces) when the index stores the documents.
/// `output` is the walk's excluded directory.
fn stream_into(
    folder: &Path,
    output: &Path,
    source: &CutSource,
    cap: u64,
    mut writer: feed::Writer,
) -> Result<Docs, String> {
    let mut out = match source {
        CutSource::Files(_) => None,
        CutSource::Shard(shard) => match std::fs::File::create(shard) {
            Ok(file) => Some((BufWriter::with_capacity(SHARD_WRITE_BUFFER, file), shard)),
            Err(e) => {
                // Before the first cut the feed holds nothing: report through
                // it, or the build sees an empty plan instead of this error.
                let e = format!("error: cannot write {}: {e}", shard.display());
                writer.end(None, Some(e.clone()));
                return Err(e);
            }
        },
    };
    let failed =
        |shard: &Path, e: std::io::Error| format!("error: cannot write {}: {e}", shard.display());
    let mut cutter = Cutter::of(cap);
    let mut bom_bytes = 0u64;
    let walked = crate::for_each_index_doc(folder, Some(output), |doc, stamp| {
        if writer.aborted() {
            return Err(ABORTED.to_string());
        }
        if matches!(source, CutSource::Files(_)) && doc.bytes.len() as u64 != stamp.len {
            return Err(read::changed_while_indexing(&folder.join(&doc.path)));
        }
        let stamp = crate::Stamp {
            len: doc.bytes.len() as u64,
            ..stamp
        };
        bom_bytes += (doc.bytes.len() - crate::matching::strip_bom(&doc.bytes).len()) as u64;
        let cut = cutter.push(doc.bytes.len() as u64);
        let handed = match out.as_mut() {
            Some((w, shard)) => {
                w.write_all(&doc.bytes).map_err(|e| failed(shard, e))?;
                if cut.is_some() {
                    w.flush().map_err(|e| failed(shard, e))?;
                }
                None
            }
            None => Some(doc.bytes),
        };
        writer.pushed(doc.path, stamp, handed, cut);
        Ok(())
    });
    let flushed = match out.as_mut() {
        Some((w, shard)) => w.flush().map_err(|e| failed(shard, e)),
        None => Ok(()),
    };
    let error = walked.and(flushed).err();
    // A short stream announces no trailing cut: its error is the answer.
    let trailing = error.is_none().then(|| cutter.finish()).flatten();
    let tables = writer.end(trailing, error.clone());
    match error {
        Some(e) => Err(e),
        None => Ok(Docs {
            offsets: shard_offsets(&tables.lens)?,
            paths: tables.paths,
            lens: tables.lens,
            bom_bytes,
            mtimes: tables.mtimes,
            root: None,
        }),
    }
}

/// Not reported: the build gave up first, so its own error is the answer.
const ABORTED: &str = "the document stream was aborted by the build";

/// Corpus-relative path per doc, the manifest's `files`.
pub fn paths(docs: &Docs) -> &[String] {
    &docs.paths
}

/// Raw byte length per doc, ascending by id: the shard plan's only input.
pub fn lens(docs: &Docs) -> &[u64] {
    &docs.lens
}

/// Bytes indexed, the manifest's `num_bytes`.
pub fn total_bytes(docs: &Docs) -> u64 {
    docs.offsets.last().copied().unwrap_or(0)
}

/// Stored bytes rg's searcher would skip: the leading BOMs.
pub fn bom_bytes(docs: &Docs) -> u64 {
    docs.bom_bytes
}

/// The walk's mtime per doc, ascending by id.
pub fn mtimes(docs: &Docs) -> &[i64] {
    &docs.mtimes
}

/// The canonical corpus root of a files-layout stream.
pub fn root(docs: &Docs) -> Option<&str> {
    docs.root.as_deref()
}

impl Docs {
    pub fn num_docs(&self) -> usize {
        self.lens.len()
    }
}

#[cfg(test)]
mod tests;

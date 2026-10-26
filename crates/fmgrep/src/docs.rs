//! The index's documents, streamed in one read pass over the corpus and read
//! back a blob slot at a time: from `docs.shard` (no separators, the length
//! table recovers every boundary) or from the files themselves. The stream
//! announces a slot as a [`Cut`] the moment its range closes, so construction
//! overlaps the read pass; [`Docs`] is the manifest's tables.

mod feed;
mod read;

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::manifest::{DOCS_SHARD, SHARD_WRITE_BUFFER};
use crate::plan::Cutter;
use crate::walk::WalkNotes;

use feed::CutSource;
pub use feed::{Cut, Cuts};

/// One index's per-document manifest tables; `Cuts` holds the documents.
pub struct Docs {
    /// Corpus-relative path per doc, the manifest's `files`.
    pub paths: Vec<String>,
    /// Raw stored byte length per doc, ascending by id.
    pub lens: Vec<u64>,
    /// The manifest's `num_bytes`.
    pub num_bytes: u64,
    /// Leading UTF-8 BOM bytes over all docs, the manifest's `bom_bytes`.
    pub bom_bytes: u64,
    /// The walk's mtime per doc, ascending by id.
    pub mtimes: Vec<i64>,
    /// Canonical corpus root; files layout only.
    pub root: Option<String>,
    /// What the walk saw besides documents.
    pub walk: WalkNotes,
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
    let source = if store_docs {
        CutSource::Shard(output.join(DOCS_SHARD))
    } else {
        CutSource::Files(corpus_root(folder)?)
    };
    let (cuts, writer) = feed::feed(source.clone());
    // A plain thread: the stream blocks for the whole build and must never run a shard build.
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
    let docs = streamed.expect("the stream only aborts once the build has failed");
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
    let walked = crate::for_each_index_doc(folder, Some(output), |path, bytes, stamp| {
        if writer.aborted() {
            return Err(ABORTED.to_string());
        }
        if matches!(source, CutSource::Files(_)) && bytes.len() as u64 != stamp.len {
            return Err(read::changed_while_indexing(&folder.join(path)));
        }
        let stamp = crate::Stamp {
            len: bytes.len() as u64,
            ..stamp
        };
        bom_bytes += (bytes.len() - crate::matching::strip_bom(&bytes).len()) as u64;
        let cut = cutter.push(bytes.len() as u64);
        let handed = match out.as_mut() {
            Some((w, shard)) => {
                w.write_all(&bytes).map_err(|e| failed(shard, e))?;
                if cut.is_some() {
                    w.flush().map_err(|e| failed(shard, e))?;
                }
                None
            }
            None => Some(bytes),
        };
        writer.pushed(path.to_owned(), stamp, handed, cut);
        Ok(())
    });
    let flushed = match out.as_mut() {
        Some((w, shard)) => w.flush().map_err(|e| failed(shard, e)),
        None => Ok(()),
    };
    let (walk, walked) = match walked {
        Ok(walk) => (walk, Ok(())),
        Err(e) => (WalkNotes::default(), Err(e)),
    };
    let error = walked.and(flushed).err();
    // A short stream announces no trailing cut: its error is the answer.
    let trailing = error.is_none().then(|| cutter.finish()).flatten();
    let tables = writer.end(trailing, error.clone());
    match error {
        Some(e) => Err(e),
        None => Ok(Docs {
            num_bytes: tables.lens.iter().sum(),
            paths: tables.paths,
            lens: tables.lens,
            bom_bytes,
            mtimes: tables.mtimes,
            root: match source {
                CutSource::Files(root) => root.to_str().map(str::to_owned),
                CutSource::Shard(_) => None,
            },
            walk,
        }),
    }
}

/// Not reported: the build gave up first, so its own error is the answer.
const ABORTED: &str = "the document stream was aborted by the build";

impl Docs {
    pub fn num_docs(&self) -> usize {
        self.lens.len()
    }
}

#[cfg(test)]
mod tests;

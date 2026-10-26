use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use crate::blobs::{create_sink, finish_sink};
use crate::docs;
use crate::manifest::{MANIFEST_FORMAT, Manifest, create_output_dir, write_manifest};
use crate::plan::shard_bytes;
use crate::sharded::DocfmIndex;
use crate::source::{Journal, Knob, marks, write_stamps, write_walk};
use crate::wire::IndexSummary;

/// The default index directory, under the indexed folder.
pub const INDEX_DIR: &str = ".fmgrep";

/// What `index` builds.
pub struct IndexRequest<'a> {
    pub folder: &'a Path,
    pub output: Option<&'a Path>,
    /// Keep a copy of the documents in `docs.shard`.
    pub store_docs: bool,
}

/// Build, persist and summarize an index. Printing is the caller's job.
///
/// `build_time_us` is the index wall — the document stream, construction and
/// the blob flush, all of it, so the number keeps its meaning however many
/// shards are built at once; the journal marks and the manifest write are
/// outside it. Any failure, error or panic, removes the output dir, so a
/// half-written index is never queryable.
pub fn run_index(req: &IndexRequest<'_>) -> Result<IndexSummary, String> {
    std::fs::read_dir(req.folder)
        .map_err(|e| format!("error: cannot list folder {}: {e}", req.folder.display()))?;
    let output: PathBuf = match req.output {
        Some(p) => {
            refuse_foreign_dotdir(req.folder, p)?;
            p.to_path_buf()
        }
        None => req.folder.join(INDEX_DIR),
    };
    create_output_dir(&output)?;
    let persist = |output: &Path| -> Result<Manifest, String> {
        let marks = match req.store_docs {
            true => Vec::new(),
            false => marks(req.folder, Knob::read()?),
        };
        let start = Instant::now();
        let started = SystemTime::now();
        let mut sink = create_sink(output)?;
        let cap = shard_bytes()?;
        let (docs, index_size_bytes) =
            docs::stream_while(req.folder, output, cap, req.store_docs, |cuts| {
                DocfmIndex::build_into(cuts, cap, None, req.store_docs, &mut sink)
            })?;
        let (blobs, blob_lens) = finish_sink(sink)?;
        if !req.store_docs {
            write_stamps(output, &docs.mtimes)?;
            write_walk(output, &docs.walk, started)?;
        }
        let build_time_us = start.elapsed().as_micros() as u64;
        if let Some(root) = docs.root.as_deref().filter(|_| !req.store_docs) {
            Journal::write_new(output, root, marks, &docs.walk)?;
        }
        let manifest = Manifest {
            format: MANIFEST_FORMAT,
            store_docs: req.store_docs,
            num_docs: docs.num_docs(),
            num_bytes: docs.num_bytes,
            bom_bytes: docs.bom_bytes,
            index_size_bytes,
            build_time_us,
            files: docs.paths,
            blobs,
            doc_lens: docs.lens,
            blob_lens,
            corpus_root: docs.root,
        };
        write_manifest(output, &manifest)?;
        Ok(manifest)
    };
    let manifest = match std::panic::catch_unwind(AssertUnwindSafe(|| persist(&output))) {
        Ok(Ok(manifest)) => manifest,
        Ok(Err(e)) => {
            let _ = std::fs::remove_dir_all(&output);
            return Err(e);
        }
        Err(payload) => {
            let _ = std::fs::remove_dir_all(&output);
            std::panic::resume_unwind(payload);
        }
    };
    Ok(IndexSummary {
        store_docs: manifest.store_docs,
        path: output.display().to_string(),
        num_files: manifest.num_docs,
        num_bytes: manifest.num_bytes,
        index_size_bytes: manifest.index_size_bytes,
        build_time_us: manifest.build_time_us,
    })
}

/// A search reads a files-layout [`INDEX_DIR`] from its parent directory, so
/// one anywhere but directly under `folder` would read the wrong tree.
fn refuse_foreign_dotdir(folder: &Path, output: &Path) -> Result<(), String> {
    if output.file_name() != Some(INDEX_DIR.as_ref()) {
        return Ok(());
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let same = matches!(
        (std::fs::canonicalize(parent), std::fs::canonicalize(folder)),
        (Ok(a), Ok(b)) if a == b
    );
    if same {
        return Ok(());
    }
    Err(format!(
        "error: --output {} is a {INDEX_DIR} outside {}; omit --output or name it otherwise",
        output.display(),
        folder.display()
    ))
}

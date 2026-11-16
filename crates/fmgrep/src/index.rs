use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::blobs::{create_sink, finish_sink};
use crate::docs;
use crate::manifest::{MANIFEST_FORMAT, Manifest, create_output_dir, write_manifest};
use crate::plan::shard_bytes;
use crate::sharded::DocfmIndex;
use crate::source::{request_watch, write_stamps, write_walk};
use crate::vcs::served;
use crate::wire::IndexSummary;

/// The default index directory, under the indexed folder.
pub const INDEX_DIR: &str = ".fmgrep";

/// What `index` builds.
pub struct IndexRequest<'a> {
    pub folder: &'a Path,
    pub output: Option<&'a Path>,
    /// Keep a copy of the documents in `docs.shard`.
    pub store_docs: bool,
    /// Fail unless the index records its served commit; `store_docs` only.
    pub require_commit: bool,
}

/// Build, persist and summarize an index. Printing is the caller's job.
///
/// `build_time_us` is the index wall — the document stream, construction,
/// the blob flush and the check of the served commit, all of it, so the
/// number keeps its meaning however many shards are built at once; the
/// manifest write is outside it. Once the
/// walk has listed the files, before it reads them, the stream asks
/// watchman to watch the tree, so that watchman reads the tree while the
/// index reads and builds. It records when it started. Any failure, error
/// or panic, removes the output dir, so a half-written index is never
/// queryable.
///
/// With `require_commit`, an index that would record no served commit is
/// a failure, in the words of the note it would print: the checks that
/// need no walk run before the output dir is made, so they fail before a
/// file is read, and an index already at the output stays.
pub fn run_index(req: &IndexRequest<'_>) -> Result<IndexSummary, String> {
    if req.require_commit && !req.store_docs {
        return Err(
            "error: --require-served-commit needs --store-docs: only a --store-docs index \
             records a served commit"
                .to_owned(),
        );
    }
    std::fs::read_dir(req.folder)
        .map_err(|e| format!("error: cannot list folder {}: {e}", req.folder.display()))?;
    let output: PathBuf = match req.output {
        Some(p) => {
            refuse_foreign_dotdir(req.folder, p)?;
            p.to_path_buf()
        }
        None => req.folder.join(INDEX_DIR),
    };
    let start = Instant::now();
    let started = SystemTime::now();
    let early = match req.require_commit {
        true => Some(served::early(req.folder).map_err(|why| no_commit(&why))?),
        false => None,
    };
    create_output_dir(&output)?;
    let persist = |output: &Path| -> Result<Manifest, String> {
        let before = req.store_docs.then(|| match &early {
            Some(head) => Ok(head.clone()),
            None => served::before(req.folder),
        });
        let mut sink = create_sink(output)?;
        let cap = shard_bytes()?;
        #[cfg(test)]
        let hooks = crate::tests::Hooks::here();
        // On the stream's thread, once the walk has listed the files: the
        // reads wait for the request, which has its own time limits, and
        // this thread's test hooks go with it.
        let listed = |files: usize| {
            #[cfg(test)]
            hooks.apply();
            if !req.store_docs {
                request_watch(req.folder, files);
            }
        };
        let (docs, index_size_bytes) =
            docs::stream_while(req.folder, output, cap, req.store_docs, listed, |cuts| {
                DocfmIndex::build_into(cuts, cap, None, req.store_docs, &mut sink)
            })?;
        let (blobs, blob_lens) = finish_sink(sink)?;
        if !req.store_docs {
            write_stamps(output, &docs.mtimes, &docs.file_lens)?;
            write_walk(output, &docs.walk, started)?;
        }
        let commit = before
            .map(|b| served::after(b, req.folder, output, &docs.paths, &docs.lens, &docs.walk));
        let build_time_us = start.elapsed().as_micros() as u64;
        match &commit {
            Some(Err(why)) if req.require_commit => return Err(no_commit(why)),
            Some(commit) => served::note(commit),
            None => {}
        }
        let since_epoch = started.duration_since(UNIX_EPOCH).unwrap_or_default();
        let manifest = Manifest {
            format: MANIFEST_FORMAT,
            store_docs: req.store_docs,
            num_docs: docs.num_docs(),
            num_bytes: docs.num_bytes,
            bom_bytes: docs.bom_bytes,
            index_size_bytes,
            build_time_us,
            index_started_ns: i64::try_from(since_epoch.as_nanos()).unwrap_or(i64::MAX),
            files: docs.paths.into_iter().collect(),
            blobs,
            doc_lens: docs.lens.into(),
            blob_lens,
            corpus_root: docs.root,
            commit: commit.and_then(Result::ok),
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

/// Why an index that must record its served commit fails: `why` is the
/// reason that the note gives without `--require-served-commit`.
fn no_commit(why: &str) -> String {
    format!("error: the index cannot record its served commit: {why}")
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

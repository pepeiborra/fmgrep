//! Staging helpers and the drive tests over the real CLI spec: children, so
//! the kit needs no public surface.

mod drive;
mod expr;
mod index;
mod json;
mod matches;
mod occurrences;
mod output;
mod serve;

use std::path::{Path, PathBuf};

use crate::cli::{Outcome, dispatch};
use crate::index::INDEX_DIR;
use crate::manifest::{BLOBS_SHARD, DOCS_SHARD, Manifest, read_manifest, shard_offsets};
use crate::matching::QueryOutput;
use crate::pattern::{PreparedPattern, count_occurrences};
use crate::query::{QueryRequest, run_query};
use crate::registry::cli_spec;
use crate::wire::{IndexSummary, OutputReport};

/// Both index layouts: the files source, and `--store-docs`.
const LAYOUTS: [bool; 2] = [false, true];

fn slice(bytes: &[u8], lens: &[u64]) -> Vec<Vec<u8>> {
    let offsets = shard_offsets(lens).unwrap();
    (0..lens.len())
        .map(|i| bytes[offsets[i] as usize..offsets[i + 1] as usize].to_vec())
        .collect()
}

/// Manifest, per-doc bytes sliced out of `docs.shard`, per-slot blobs sliced
/// out of `blobs.shard`. Stored-docs layout only.
fn read_back(dir: &Path) -> (Manifest, Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let manifest = read_manifest(dir).unwrap();
    let docs = slice(
        &std::fs::read(dir.join(DOCS_SHARD)).unwrap(),
        &manifest.doc_lens,
    );
    let blobs = slice(
        &std::fs::read(dir.join(BLOBS_SHARD)).unwrap(),
        &manifest.blob_lens,
    );
    (manifest, docs, blobs)
}

/// JSON lines with every wall clock dropped (report or rg event stream).
fn timeless_lines(out: &str) -> Vec<serde_json::Value> {
    out.lines()
        .map(|l| {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            for (at, key) in [
                ("/data/stats", "elapsed"),
                ("/data", "elapsed_total"),
                ("", "index_time_us"),
            ] {
                v.pointer_mut(at)
                    .and_then(|o| o.as_object_mut())
                    .map(|o| o.remove(key));
            }
            for q in v
                .get_mut("queries")
                .and_then(|q| q.as_array_mut())
                .into_iter()
                .flatten()
            {
                q.as_object_mut().unwrap().remove("latency_us");
            }
            v
        })
        .collect()
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

fn stage_corpus(tmp: &Path, files: &[(&str, &[u8])]) -> PathBuf {
    let folder = tmp.join("corpus");
    std::fs::create_dir_all(&folder).unwrap();
    for (name, bytes) in files {
        std::fs::write(folder.join(name), bytes).unwrap();
    }
    folder
}

/// `index <folder> [--output PATH]` in the `store_docs` layout.
fn index_words(folder: &Path, output: Option<&Path>, store_docs: bool) -> Vec<String> {
    let mut words = argv(&["index", &folder.display().to_string()]);
    if let Some(output) = output {
        words.extend(argv(&["--output", &output.display().to_string()]));
    }
    if store_docs {
        words.push("--store-docs".into());
    }
    words
}

fn index_it(folder: &Path, output: &Path, store_docs: bool) -> IndexSummary {
    match dispatch(&cli_spec(), &index_words(folder, Some(output), store_docs)).unwrap() {
        Outcome::Index(s) => s,
        _ => panic!("expected an index summary"),
    }
}

/// A request over `manifest` with every flag at its default.
fn request<'a>(
    index: &'a Path,
    manifest: &'a Manifest,
    terms: &'a [String],
    output: QueryOutput,
) -> QueryRequest<'a> {
    QueryRequest {
        index_dir: index,
        manifest,
        corpus_root: None,
        regex: false,
        mode: crate::matching::MatchMode::default(),
        terms,
        expr: None,
        output,
    }
}

fn query_argv(index: &Path, regex: bool, terms: &[&str]) -> Vec<String> {
    let mut v = argv(&["--index", &index.display().to_string()]);
    if regex {
        v.push("--regex".into());
    }
    v.push("--".into());
    v.extend(terms.iter().map(|t| t.to_string()));
    v
}

fn query_it(index: &Path, regex: bool, terms: &[&str]) -> OutputReport {
    match dispatch(&cli_spec(), &query_argv(index, regex, terms)).unwrap() {
        Outcome::Query(r) => r,
        _ => panic!("expected a query report"),
    }
}

fn query_it_err(index: &Path, regex: bool, terms: &[&str]) -> String {
    match dispatch(&cli_spec(), &query_argv(index, regex, terms)) {
        Err(e) => e,
        Ok(_) => panic!("query must fail for {terms:?}"),
    }
}

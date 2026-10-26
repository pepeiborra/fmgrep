use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::cli::{Outcome, dispatch, dispatch_to};
use crate::expr::Expr;
use crate::manifest::{BLOBS_SHARD, DOCS_SHARD, Manifest, read_manifest, shard_offsets};
use crate::matching::QueryOutput;
use crate::query::QueryRequest;
use crate::wire::{IndexSummary, OutputReport};

/// Both index layouts: the files source, and `--store-docs`.
pub(crate) const LAYOUTS: [bool; 2] = [false, true];

pub(crate) const TWO_FILES: [(&str, &[u8]); 2] = [
    ("a.c", b"printk one\nnothing\nprintk two printk\n"),
    ("b.c", b"aaaa\ntail printk"),
];

/// [`TWO_FILES`] in `tmp/corpus`, indexed at `tmp/idx-<store_docs>`.
pub(crate) fn staged(tmp: &Path, store_docs: bool) -> PathBuf {
    let folder = stage_corpus(tmp, &TWO_FILES);
    let output = tmp.join(format!("idx-{store_docs}"));
    index_it(&folder, &output, store_docs);
    output
}

pub(crate) fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// `head flags [-- terms]`.
pub(crate) fn words(head: &[&str], flags: &[&str], terms: &[&str]) -> Vec<String> {
    let mut words = argv(&[head, flags].concat());
    if !terms.is_empty() {
        words.push("--".into());
        words.extend(argv(terms));
    }
    words
}

/// What `words` prints, or the refusal.
pub(crate) fn output(words: &[String], tty: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    dispatch_to(words, &mut &b""[..], &mut out, tty)?;
    Ok(out)
}

/// What `--index <index> <flags> -- <terms>` prints.
pub(crate) fn printed(index: &Path, flags: &[&str], terms: &[&str]) -> String {
    let words = words(&["--index", &index.display().to_string()], flags, terms);
    let out = output(&words, false).unwrap_or_else(|e| panic!("{words:?}: {e}"));
    String::from_utf8(out).unwrap()
}

/// The report of `--index <index> <flags> -- <terms>`.
pub(crate) fn query(index: &Path, flags: &[&str], terms: &[&str]) -> Result<OutputReport, String> {
    let index = index.display().to_string();
    match dispatch(words(&["--index", &index], flags, terms))? {
        Outcome::Query(r) => Ok(r),
        _ => panic!("expected a query report"),
    }
}

fn regex_flag(regex: bool) -> &'static [&'static str] {
    if regex { &["--regex"] } else { &[] }
}

pub(crate) fn query_it(index: &Path, regex: bool, terms: &[&str]) -> OutputReport {
    query(index, regex_flag(regex), terms).unwrap()
}

pub(crate) fn query_it_err(index: &Path, regex: bool, terms: &[&str]) -> String {
    match query(index, regex_flag(regex), terms) {
        Err(e) => e,
        Ok(_) => panic!("query must fail for {terms:?}"),
    }
}

pub(crate) fn stage_corpus(tmp: &Path, files: &[(&str, &[u8])]) -> PathBuf {
    let folder = tmp.join("corpus");
    std::fs::create_dir_all(&folder).unwrap();
    for (name, bytes) in files {
        std::fs::write(folder.join(name), bytes).unwrap();
    }
    folder
}

/// `index <folder> [--output PATH]` in the `store_docs` layout.
pub(crate) fn index_words(folder: &Path, output: Option<&Path>, store_docs: bool) -> Vec<String> {
    let mut words = argv(&["index", &folder.display().to_string()]);
    if let Some(output) = output {
        words.extend(argv(&["--output", &output.display().to_string()]));
    }
    if store_docs {
        words.push("--store-docs".into());
    }
    words
}

pub(crate) fn index_it(folder: &Path, output: &Path, store_docs: bool) -> IndexSummary {
    match dispatch(index_words(folder, Some(output), store_docs)).unwrap() {
        Outcome::Index(s) => s,
        _ => panic!("expected an index summary"),
    }
}

/// A request over `manifest` with every flag at its default.
pub(crate) fn request<'a>(
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
        filter: None,
    }
}

pub(crate) fn slice(bytes: &[u8], lens: &[u64]) -> Vec<Vec<u8>> {
    let offsets = shard_offsets(lens).unwrap();
    (0..lens.len())
        .map(|i| bytes[offsets[i] as usize..offsets[i + 1] as usize].to_vec())
        .collect()
}

/// Manifest, per-doc bytes sliced out of `docs.shard`, per-slot blobs sliced
/// out of `blobs.shard`. Stored-docs layout only.
pub(crate) fn read_back(dir: &Path) -> (Manifest, Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let manifest = read_manifest(dir).unwrap();
    let read = |f: &str| std::fs::read(dir.join(f)).unwrap();
    let docs = slice(&read(DOCS_SHARD), &manifest.doc_lens);
    let blobs = slice(&read(BLOBS_SHARD), &manifest.blob_lens);
    (manifest, docs, blobs)
}

/// `v` as JSON with every wall clock dropped, at any depth.
pub(crate) fn timeless(v: impl serde::Serialize) -> Value {
    fn scrub(v: &mut Value) {
        match v {
            Value::Object(o) => {
                o.retain(|k, _| {
                    !matches!(
                        k.as_str(),
                        "latency_us"
                            | "index_time_us"
                            | "load_us"
                            | "probe_us"
                            | "elapsed"
                            | "elapsed_total"
                    )
                });
                o.values_mut().for_each(scrub);
            }
            Value::Array(a) => a.iter_mut().for_each(scrub),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(v).unwrap();
    scrub(&mut v);
    v
}

/// [`timeless`] per JSON line.
pub(crate) fn timeless_lines(out: &str) -> Vec<Value> {
    out.lines()
        .map(|l| timeless(serde_json::from_str::<Value>(l).unwrap()))
        .collect()
}

/// `out` with its JSON lines' wall clocks dropped, if `flags` ask for JSON.
pub(crate) fn untimed(flags: &[&str], out: &str) -> String {
    if flags.contains(&"--json") {
        format!("{:?}", timeless_lines(out))
    } else {
        out.to_owned()
    }
}

pub(crate) fn t(s: &str) -> Expr {
    Expr::Term(s.to_owned())
}

pub(crate) fn re(s: &str) -> Expr {
    Expr::Regex(s.to_owned())
}

pub(crate) fn and(v: Vec<Expr>) -> Expr {
    Expr::And(v)
}

pub(crate) fn or(v: Vec<Expr>) -> Expr {
    Expr::Or(v)
}

/// Deterministic xorshift, so failures replay.
pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// Long before any test runs: never racy.
#[cfg(unix)]
pub(crate) const T0: std::time::SystemTime = std::time::SystemTime::UNIX_EPOCH;

#[cfg(unix)]
pub(crate) fn set_mtime(path: &Path, at: std::time::SystemTime) {
    std::fs::File::open(path).unwrap().set_modified(at).unwrap();
}

/// Every file and directory under `path` set to [`T0`], symlinks aside.
#[cfg(unix)]
pub(crate) fn backdate(path: &Path) {
    let meta = std::fs::symlink_metadata(path).unwrap();
    if meta.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            backdate(&entry.unwrap().path());
        }
    }
    if !meta.is_symlink() {
        set_mtime(path, T0);
    }
}

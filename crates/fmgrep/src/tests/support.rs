use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::cli::{Outcome, dispatch, dispatch_to};
use crate::expr::Expr;
use crate::manifest::{BLOBS_SHARD, DOCS_SHARD, Manifest, read_manifest, shard_offsets};
use crate::matching::{DocSearch, MatchMode, QueryOutput, matcher};
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

/// Whether `flags` combine several patterns into one expression.
fn combines(flags: &[&str]) -> bool {
    flags.iter().any(|f| matches!(*f, "--and" | "--or"))
}

/// The PATTERNs of `terms`, then its PATHs: the trailing words that name a
/// file or directory, but not the first word.
pub(crate) fn patterns_and_paths<'a>(terms: &'a [&'a str]) -> (&'a [&'a str], &'a [&'a str]) {
    let paths = (terms.iter().skip(1).rev())
        .take_while(|t| **t == "-" || Path::new(t).exists())
        .count();
    terms.split_at(terms.len() - paths)
}

/// `head flags [-- pattern PATH...]`: the command line has one PATTERN. With
/// `--and` or `--or`, each of the patterns of `terms` is an `-e` PATTERN.
pub(crate) fn words(head: &[&str], flags: &[&str], terms: &[&str]) -> Vec<String> {
    let mut words = argv(&[head, flags].concat());
    let (patterns, paths) = patterns_and_paths(terms);
    match patterns {
        [] => {}
        _ if combines(flags) => {
            for t in patterns {
                words.extend(argv(&["-e", t]));
            }
            words.push("--".into());
        }
        [_] => words.push("--".into()),
        _ => panic!("one search has one PATTERN: {patterns:?}"),
    }
    if combines(flags) {
        words.extend(argv(paths));
    } else {
        words.extend(argv(patterns));
        words.extend(argv(paths));
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

/// The report of `--index <index> <flags> -- <terms>`. A search of the
/// command line has one PATTERN, or one expression: several `terms` that
/// `flags` do not combine are searched one after the other, and their
/// queries reported in turn, as a request of several terms reports them.
pub(crate) fn query(index: &Path, flags: &[&str], terms: &[&str]) -> Result<OutputReport, String> {
    let (patterns, paths) = patterns_and_paths(terms);
    if patterns.len() > 1 && !combines(flags) {
        let mut all: Option<OutputReport> = None;
        for pattern in patterns {
            let report = query(index, flags, &[&[*pattern], paths].concat())?;
            match &mut all {
                None => all = Some(report),
                Some(all) => all.queries.extend(report.queries),
            }
        }
        return Ok(all.expect("several terms"));
    }
    let index = index.display().to_string();
    match dispatch(words(&["--index", &index], flags, terms))? {
        Outcome::Query(r) => Ok(r),
        _ => panic!("expected a query report"),
    }
}

fn fixed_flag(fixed: bool) -> &'static [&'static str] {
    if fixed { &["-F"] } else { &[] }
}

pub(crate) fn query_it(index: &Path, fixed: bool, terms: &[&str]) -> OutputReport {
    query(index, fixed_flag(fixed), terms).unwrap()
}

pub(crate) fn query_it_err(index: &Path, fixed: bool, terms: &[&str]) -> String {
    match query(index, fixed_flag(fixed), terms) {
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
        report_none: false,
        plans: false,
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

/// `term`'s search: a pattern, as on the CLI.
pub(crate) fn search(term: &str, output: QueryOutput, mode: MatchMode) -> DocSearch {
    let m = matcher("", Some(term), mode);
    DocSearch::new(m.unwrap().unwrap(), output, mode)
}

/// The system allocator, counting the bytes each thread holds, for tests
/// that bound what a call allocates.
struct Counted;

thread_local! {
    /// The bytes this thread holds, and the most it held since [`held_at_most`] began.
    static HELD: std::cell::Cell<(isize, isize)> = const { std::cell::Cell::new((0, 0)) };
}

fn held(delta: isize) {
    let _ = HELD.try_with(|h| {
        let (now, top) = h.get();
        h.set((now + delta, top.max(now + delta)));
    });
}

// SAFETY: every call is the system allocator's, with its own arguments.
unsafe impl std::alloc::GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        held(layout.size() as isize);
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        held(-(layout.size() as isize));
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        held(size as isize - layout.size() as isize);
        unsafe { std::alloc::System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static COUNTED: Counted = Counted;

/// `f`'s value, and the most bytes that this thread held at once during it,
/// beyond what it held before.
pub(crate) fn held_at_most<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let start = HELD.with(|h| {
        let (now, _) = h.get();
        h.set((now, now));
        now
    });
    let value = f();
    let top = HELD.with(|h| h.get().1);
    (value, usize::try_from(top - start).unwrap_or(0))
}

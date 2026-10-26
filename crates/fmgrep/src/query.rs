use std::borrow::Cow;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::regex_pattern;

use crate::blobs::BlobSlots;
use crate::expr::Expr;
use crate::fetch::{DocStore, Paths, fetch_stream_parallel, fetch_threads};
use crate::filter::PathFilter;
use crate::manifest::Manifest;
use crate::matching::{self, DocOutcome, LineMatcher, MatchMode, QueryOutput};
use crate::pattern::{PreparedPattern, count_overlapping};
use crate::probe::{Query, candidate_docs};
use crate::sharded::DocfmIndex;
use crate::sink::{DocMatches, MatchSink};
use crate::source::{Discovery, Watch};
use crate::wire::{OutputReport, QueryReport};

/// What a search answers. The manifest is read once by the dispatcher and
/// threaded through.
pub struct QueryRequest<'a> {
    pub index_dir: &'a Path,
    pub manifest: &'a Manifest,
    /// Overrides a files-layout manifest's root.
    pub corpus_root: Option<&'a Path>,
    pub regex: bool,
    /// rg `-U` / `--multiline-dotall` / `-i`, as the CLI parsed them.
    pub mode: MatchMode,
    pub terms: &'a [String],
    /// A document-level expression, answered as one query; exclusive with
    /// `terms` and `regex`.
    pub expr: Option<&'a Expr>,
    /// What each term must produce.
    pub output: QueryOutput,
    /// The files searched, of those indexed or added.
    pub filter: Option<PathFilter>,
}

mod boolean;

/// The document state a query answers from.
pub struct Loaded {
    docs: DocStore,
    pub load_wall: Duration,
    watch: Option<Watch>,
    discovery: Option<Discovery>,
}

impl Loaded {
    /// The watch [`load`] started, if asked to.
    pub fn take_watch(&mut self) -> Option<Watch> {
        self.watch.take()
    }

    /// The discovery [`load`] deferred; [`answer`] waits for it.
    pub fn take_discovery(&mut self) -> Option<Discovery> {
        self.discovery.take()
    }

    /// Indexed files missing at load.
    pub fn gone(&self) -> usize {
        self.docs.gone()
    }

    /// Over half of the `n` indexed files missing: how many.
    pub fn mostly_gone(&self, n: usize) -> Option<String> {
        let gone = self.gone();
        (gone * 2 > n).then(|| format!("{gone} of {n} indexed files are missing"))
    }
}

fn covers(index: usize, manifest: &Manifest) -> Result<(), String> {
    if index != manifest.num_docs {
        return Err(format!(
            "error: loaded index covers {index} files but the manifest records {}; refusing to report skew",
            manifest.num_docs
        ));
    }
    Ok(())
}

/// Load the persisted index and its document source, and check coverage
/// against the manifest. The files layout stats every doc and discovers the
/// added files alongside the index load, or defers discovery under `defer`;
/// under `watch` it watches the root from before both.
pub fn load(
    index_dir: &Path,
    manifest: &Manifest,
    corpus_root: Option<&Path>,
    watch: bool,
    defer: bool,
) -> Result<(DocfmIndex, Loaded), String> {
    let load_start = Instant::now();
    let docs = DocStore::open(index_dir, manifest, corpus_root)?;
    let watch = match watch.then(|| docs.watch()).flatten() {
        Some(Err(e)) => {
            eprintln!("warning: {e}; a changed file is seen only when a query reads it");
            None
        }
        w => w.and_then(Result::ok),
    };
    let blobs = BlobSlots::open(index_dir, &manifest.blobs, &manifest.blob_lens)?;
    let mut discovery = docs.discovery();
    let now = (!defer).then(|| discovery.take()).flatten();
    let running = AtomicBool::new(false);
    // Plain threads: two fixed blocking passes that overlap the load.
    let index = std::thread::scope(|scope| {
        scope.spawn(|| docs.stat_all(&manifest.files));
        if let Some(d) = now {
            scope.spawn(|| d.run(&manifest.files, &running));
        }
        DocfmIndex::load(&blobs, &manifest.doc_lens)
    })
    .map_err(|e| format!("error: {e}"))?;
    let loaded = Loaded {
        docs,
        load_wall: load_start.elapsed(),
        watch,
        discovery,
    };
    let loaded = finish(loaded, manifest);
    covers(index.num_files_indexed(), manifest)?;
    Ok((index, loaded))
}

/// `loaded` once its stale docs are noted.
fn finish(mut loaded: Loaded, manifest: &Manifest) -> Loaded {
    loaded.docs.begin();
    let stale = loaded
        .docs
        .stale()
        .partition_point(|&id| id < manifest.files.len());
    if stale > 0 {
        eprintln!(
            "note: {stale} file(s) changed since indexing are scanned in full by every query; re-run index"
        );
    }
    loaded
}

/// [`load`] then [`answer`].
pub fn run_query(req: &QueryRequest<'_>, sink: &mut dyn MatchSink) -> Result<OutputReport, String> {
    let (index, mut loaded) = load(req.index_dir, req.manifest, req.corpus_root, false, false)?;
    answer(&index, &mut loaded, req, sink)
}

/// Answer every term, or the expression, from `index` and `loaded`, once
/// discovery is done: a wait counts in `index_time_us`.
pub fn answer(
    index: &DocfmIndex,
    loaded: &mut Loaded,
    req: &QueryRequest<'_>,
    sink: &mut dyn MatchSink,
) -> Result<OutputReport, String> {
    let wait = Instant::now();
    if let Some(d) = loaded.discovery.take() {
        d.run(&req.manifest.files, &AtomicBool::new(false));
    }
    loaded.docs.wait_discovered();
    let waited = wait.elapsed().as_micros() as u64;
    let mut report = answer_discovered(index, loaded, req, sink)?;
    report.index_time_us += waited;
    let matched = |q: &QueryReport| q.total_matches > 0 || q.docs_matched > 0;
    if let Some(f) = &req.filter
        && !report.queries.iter().any(matched)
        && f.excludes_all(&loaded.docs.paths(&req.manifest.files), |id| {
            loaded.docs.is_gone(id)
        })
    {
        return Err("error: no files were searched: the globs exclude every file".to_owned());
    }
    Ok(report)
}

fn answer_discovered(
    index: &DocfmIndex,
    loaded: &mut Loaded,
    req: &QueryRequest<'_>,
    sink: &mut dyn MatchSink,
) -> Result<OutputReport, String> {
    if let Some(expr) = req.expr {
        return boolean::answer_expr(index, loaded, req, expr, sink);
    }
    let terms: Vec<Term<'_>> = req
        .terms
        .iter()
        .map(|t| Term {
            name: Cow::Borrowed(t),
            literal: t.as_bytes(),
            pattern: spelled(t, req.regex, req.mode.ignore_case),
        })
        .collect();
    answer_terms(index, loaded, req, &terms, sink)
}

/// One per-term query: the name it reports under, its literal bytes, and
/// its pattern when it takes the pattern path.
struct Term<'a> {
    name: Cow<'a, str>,
    literal: &'a [u8],
    pattern: Option<Cow<'a, str>>,
}

/// Under `--regex` every term is a pattern verbatim (slashes included);
/// otherwise a `/x/` term is the pattern `x`, and under `-i` a bare term
/// becomes the escaped pattern of itself, so none stays a literal. The empty
/// bare term keeps the literal path's "matches nothing" under `-i` too,
/// rather than the empty-pattern refusal.
fn spelled(term: &str, regex: bool, ignore_case: bool) -> Option<Cow<'_, str>> {
    match (regex, regex_pattern(term), ignore_case) {
        (true, _, _) => Some(Cow::Borrowed(term)),
        (false, Some(inner), _) => Some(Cow::Borrowed(inner)),
        (false, None, true) if !term.is_empty() => Some(Cow::Owned(regex::escape(term))),
        (false, None, _) => None,
    }
}

/// An invalid pattern is fatal, never a silent 0.
fn prepare(pattern: Option<&str>, mode: MatchMode) -> Result<Option<PreparedPattern>, String> {
    pattern
        .map(|p| PreparedPattern::prepare_with(p, mode).map_err(|e| format!("error: {e}")))
        .transpose()
}

/// The matcher the matching pass scans `literal`, or its pattern, with.
fn line_matcher(
    literal: &[u8],
    pattern: Option<(&str, &PreparedPattern)>,
    mode: MatchMode,
) -> Result<LineMatcher, String> {
    match pattern {
        Some((src, p)) => LineMatcher::regex(src, crate::pattern::anchor(p), mode)
            .map_err(|e| format!("error: {e}")),
        None => LineMatcher::literal(literal, mode).map_err(|e| format!("error: {e}")),
    }
}

/// Answer `terms` from `index` and `loaded` with one probe pass, then fetch
/// each term's candidates plus every stale and added doc, which the index
/// does not describe. Outside the counting modes `latency_us` covers
/// emission too.
fn answer_terms(
    index: &DocfmIndex,
    loaded: &mut Loaded,
    req: &QueryRequest<'_>,
    terms: &[Term<'_>],
    sink: &mut dyn MatchSink,
) -> Result<OutputReport, String> {
    let manifest = req.manifest;
    let num_docs = manifest.files.len();
    loaded.docs.begin();
    let docs = &mut loaded.docs;

    // Every pattern is prepared before any probing.
    let mut prepared: Vec<Option<PreparedPattern>> = Vec::with_capacity(terms.len());
    let mut matchers: Vec<Option<LineMatcher>> = Vec::with_capacity(terms.len());
    let emit = req.output != QueryOutput::Counts;
    for t in terms {
        let pattern = t.pattern.as_deref();
        let prep = prepare(pattern, req.mode)?;
        // A line matcher exists only outside counts mode: its newline refusal
        // must not change what the harness path accepts. Built before any
        // fetch, so a refused pattern costs no I/O.
        matchers.push(if emit {
            Some(line_matcher(
                t.literal,
                pattern.zip(prep.as_ref()),
                req.mode,
            )?)
        } else {
            None
        });
        prepared.push(prep);
    }
    let probes: Vec<Query<'_>> = terms
        .iter()
        .zip(&prepared)
        .map(|(t, pattern)| {
            match (pattern, pattern.as_ref().and_then(crate::pattern::literal)) {
                // Counts mode counts a literal pattern from the FM-index rather
                // than open a file per candidate; stored documents keep being
                // read from the live docs.shard.
                (Some(_), Some(lit)) if !emit && !manifest.store_docs => Query::Literal(lit),
                (Some(p), _) => Query::Regex(p),
                (None, _) => Query::Literal(t.literal),
            }
        })
        .collect();
    let answers = index.probe_all(&probes);
    drop(probes);
    debug_assert_eq!(answers.len(), terms.len());

    if emit {
        // rg's bytes_searched excludes the BOMs its searcher strips.
        sink.corpus(
            manifest.files.len(),
            manifest.num_bytes - manifest.bom_bytes,
        )?;
    }
    let mut queries = Vec::with_capacity(terms.len());
    let membership = req.output == QueryOutput::FilesWithMatches;
    for (((t, pattern), answer), matcher) in terms.iter().zip(&prepared).zip(answers).zip(&matchers)
    {
        let (term, literal) = (&*t.name, t.literal);
        let fetch_start = Instant::now();
        let mut exact = answer.exact;
        let mut candidates = candidate_docs(answer.candidates, num_docs);
        in_index(&exact, &candidates, num_docs)?;
        docs.screen(&mut exact, &mut candidates);
        debug_assert!(exact.windows(2).all(|w| w[0].0 < w[1].0));
        // Counts mode counts a fetched document with the pattern: a literal
        // never has candidates.
        debug_assert!(emit || pattern.is_some() || candidates.is_empty());
        let paths = docs.paths(&manifest.files);
        let mut stale = docs.stale().to_vec();
        if let Some(f) = &req.filter {
            f.retain(&paths, &mut exact, |e| e.0);
            f.retain(&paths, &mut candidates, |&id| id);
            f.retain(&paths, &mut stale, |&id| id);
        }
        let mut scan = scan_ids(req.output, literal, &candidates, &exact);
        if !stale.is_empty() {
            scan.extend_from_slice(&stale);
            scan.sort_unstable();
        }
        paths.sort(&mut scan);
        let mut counts = if emit {
            TermCounts::from_matching(req.output)
        } else {
            TermCounts::from_probe(literal, pattern.as_ref())
        };
        if emit {
            sink.term_start(term)?;
        } else {
            for &(_, c) in &exact {
                counts.add(c);
            }
        }
        // A matcher exists exactly in the emit modes, which own their own
        // numbers; counts mode fetches regex candidates and stale docs, and
        // counts them as the index would. Every other path sees the document
        // as rg's searcher does, BOM stripped.
        let needle = memchr::memmem::Finder::new(literal);
        let map = |_: usize, raw: &[u8]| {
            let bytes = matching::strip_bom(raw);
            match (matcher.as_ref(), pattern.as_ref()) {
                (Some(m), _) => (0, Some(m.scan(bytes, req.output))),
                (None, Some(p)) => (p.count(bytes), None),
                (None, None) => (count_overlapping(raw, &needle), None),
            }
        };
        // Two-pointer merge of the probe-exact documents this mode did not
        // fetch into the path-ordered stream; a fetched doc wins its id.
        let mut next_exact = 0usize;
        fetch_stream_parallel(
            docs,
            &manifest.files,
            &scan,
            fetch_threads(),
            &map,
            &mut |doc, bytes, (count, outcome)| {
                if !emit {
                    counts.add(count);
                    return Ok(());
                }
                while next_exact < exact.len() && paths.cmp(exact[next_exact].0, doc).is_lt() {
                    let (i, c) = exact[next_exact];
                    next_exact += 1;
                    counts.add(emit_probe_exact(sink, &paths, req.output, literal, i, c)?);
                }
                if exact.get(next_exact).is_some_and(|&(i, _)| i == doc) {
                    next_exact += 1;
                }
                let Some(mut o) = outcome else { return Ok(()) };
                if o.matched_lines == 0 {
                    return Ok(());
                }
                counts.add(o.count_matches());
                if membership {
                    // Membership is the whole answer under rg `-l`.
                    o.matched_lines = 0;
                    o.matched_spans = 0;
                    o.spanless_lines = 0;
                }
                sink.doc(DocMatches {
                    path: path_of(&paths, doc)?,
                    raw: matching::strip_bom(bytes),
                    outcome: &o,
                })
            },
        )?;
        if emit {
            for &(i, c) in &exact[next_exact..] {
                counts.add(emit_probe_exact(sink, &paths, req.output, literal, i, c)?);
            }
        }
        let report = QueryReport {
            term: term.to_owned(),
            latency_us: answer.probe_us + fetch_start.elapsed().as_micros() as u64,
            total_matches: counts.total,
            docs_matched: counts.docs,
            counts_exact: counts.exact,
            leaves: Vec::new(),
        };
        if emit {
            sink.term_end(&report)?;
        }
        queries.push(report);
    }
    Ok(OutputReport {
        index_time_us: loaded.load_wall.as_micros() as u64,
        index_size_bytes: manifest.index_size_bytes,
        num_files_indexed: manifest.num_docs,
        queries,
    })
}

/// A term's reported counts and the provenance that decides `counts_exact`.
struct TermCounts {
    total: u64,
    docs: usize,
    exact: bool,
}

impl TermCounts {
    /// Counts mode: FM document-wide numbers, rg's only for a literal that
    /// can neither overlap itself nor cross a line.
    fn from_probe(term: &[u8], pattern: Option<&PreparedPattern>) -> Self {
        Self {
            total: 0,
            docs: 0,
            exact: pattern.is_none() && matching::literal_counts_are_exact(term),
        }
    }

    /// Emit modes: rg's own numbers, except under `-l`, which establishes
    /// membership only.
    fn from_matching(output: QueryOutput) -> Self {
        Self {
            total: 0,
            docs: 0,
            exact: output != QueryOutput::FilesWithMatches,
        }
    }

    fn add(&mut self, hits: u64) {
        if hits > 0 {
            self.docs += 1;
            self.total += hits;
        }
    }
}

/// Whether a probe-exact literal count already is rg's, so a mode can report
/// it without fetching the document.
fn probe_count_is_rgs(output: QueryOutput, term: &[u8]) -> bool {
    output == QueryOutput::CountMatches && matching::literal_counts_are_exact(term)
}

/// Documents whose bytes this mode must see: the index's candidate set,
/// widened with the probe-exact documents whose lines or rg-exact counts the
/// mode still needs.
fn scan_ids(
    output: QueryOutput,
    term: &[u8],
    candidates: &[usize],
    exact: &[(usize, u64)],
) -> Vec<usize> {
    let widen = match output {
        QueryOutput::Counts | QueryOutput::FilesWithMatches => false,
        QueryOutput::CountMatches => !probe_count_is_rgs(output, term),
        QueryOutput::CountLines | QueryOutput::Matches { .. } => true,
    };
    if !widen {
        return candidates.to_vec();
    }
    let mut ids: Vec<usize> = candidates
        .iter()
        .copied()
        .chain(exact.iter().filter(|&&(_, c)| c > 0).map(|&(i, _)| i))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Emit a probe-exact document this mode did not fetch and return its
/// contribution to `total_matches`: rg's span count where the probe count is
/// rg's, else 1 for the membership the probe proves.
fn emit_probe_exact(
    sink: &mut dyn MatchSink,
    paths: &Paths<'_>,
    output: QueryOutput,
    term: &[u8],
    doc: usize,
    count: u64,
) -> Result<u64, String> {
    if count == 0 {
        return Ok(0);
    }
    let spans = if probe_count_is_rgs(output, term) {
        count
    } else {
        0
    };
    let outcome = DocOutcome {
        matched_lines: 0,
        matched_spans: spans,
        spanless_lines: 0,
        multiline: false,
        lines: Vec::new(),
        spans: Vec::new(),
    };
    sink.doc(DocMatches {
        path: path_of(paths, doc)?,
        raw: &[],
        outcome: &outcome,
    })?;
    Ok(spans.max(1))
}

fn path_of<'a>(paths: &'a Paths<'_>, doc: usize) -> Result<&'a str, String> {
    paths
        .path(doc)
        .ok_or_else(|| format!("error: doc {doc} is neither indexed nor added"))
}

/// Refuse an index answer past its documents: those ids are the added docs'.
fn in_index(exact: &[(usize, u64)], candidates: &[usize], num_docs: usize) -> Result<(), String> {
    match exact
        .last()
        .map(|&(d, _)| d)
        .max(candidates.last().copied())
    {
        Some(doc) if doc >= num_docs => Err(format!(
            "error: the index reported doc {doc} but holds {num_docs} doc(s)"
        )),
        _ => Ok(()),
    }
}

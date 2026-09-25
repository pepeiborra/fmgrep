use std::borrow::Cow;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::regex_pattern;

use crate::blobs::BlobSlots;
use crate::expr::Expr;
use crate::fetch::{DocStore, fetch_stream_parallel, fetch_threads};
use crate::manifest::Manifest;
use crate::matching::{self, DocOutcome, LineMatcher, MatchMode, QueryOutput};
use crate::pattern::{PreparedPattern, count_overlapping};
use crate::probe::{Query, candidate_docs};
use crate::sharded::DocfmIndex;
use crate::sink::{DocMatches, MatchSink};
use crate::source::Watch;
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
}

mod boolean;

/// A loaded index plus its doc-fetch state: what a query answers from.
pub struct Loaded {
    pub index: DocfmIndex,
    docs: DocStore,
    pub load_wall: Duration,
    watch: Option<Watch>,
}

impl Loaded {
    /// The watch [`load`] started, if asked to.
    pub fn take_watch(&mut self) -> Option<Watch> {
        self.watch.take()
    }
}

/// Load the persisted index and its document source, and check coverage
/// against the manifest. The files layout stats every doc, alongside the
/// index load; under `watch` it watches the root from before that stat.
pub fn load(
    index_dir: &Path,
    manifest: &Manifest,
    corpus_root: Option<&Path>,
    watch: bool,
) -> Result<Loaded, String> {
    let load_start = Instant::now();
    let mut docs = DocStore::open(index_dir, manifest, corpus_root)?;
    let watch = match watch.then(|| docs.watch()).flatten() {
        Some(Err(e)) => {
            eprintln!("warning: {e}; a changed file is seen only when a query reads it");
            None
        }
        w => w.and_then(Result::ok),
    };
    let blobs = BlobSlots::open(index_dir, &manifest.blobs, &manifest.blob_lens)?;
    let index = std::thread::scope(|scope| {
        scope.spawn(|| docs.stat_all(&manifest.files));
        DocfmIndex::load(&blobs, &manifest.doc_lens, manifest.store_docs)
    })
    .map_err(|e| format!("error: {e}"))?;
    let load_wall = load_start.elapsed();
    docs.begin();
    if !docs.stale().is_empty() {
        eprintln!(
            "note: {} file(s) changed since indexing are scanned in full by every query; re-run index",
            docs.stale().len()
        );
    }
    if index.num_files_indexed() != manifest.num_files_indexed {
        return Err(format!(
            "error: loaded index covers {} files but the manifest records {}; refusing to report skew",
            index.num_files_indexed(),
            manifest.num_files_indexed
        ));
    }
    Ok(Loaded {
        index,
        docs,
        load_wall,
        watch,
    })
}

/// [`load`] then [`answer`].
pub fn run_query(req: &QueryRequest<'_>, sink: &mut dyn MatchSink) -> Result<OutputReport, String> {
    answer(
        &mut load(req.index_dir, req.manifest, req.corpus_root, false)?,
        req,
        sink,
    )
}

/// Answer every term, or the expression, from `loaded`.
pub fn answer(
    loaded: &mut Loaded,
    req: &QueryRequest<'_>,
    sink: &mut dyn MatchSink,
) -> Result<OutputReport, String> {
    if let Some(expr) = req.expr {
        return boolean::answer_expr(loaded, req, expr, sink);
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
    answer_terms(loaded, req, &terms, sink)
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

/// Answer `terms` from `loaded` with one probe pass, then fetch each term's
/// candidates, plus every stale doc: the index does not describe their bytes,
/// so each term scans them in full. Outside the counting modes `latency_us`
/// covers emission too.
fn answer_terms(
    loaded: &mut Loaded,
    req: &QueryRequest<'_>,
    terms: &[Term<'_>],
    sink: &mut dyn MatchSink,
) -> Result<OutputReport, String> {
    let manifest = req.manifest;
    let num_docs = manifest.files.len();
    loaded.docs.begin();
    let (index, docs) = (&loaded.index, &mut loaded.docs);

    // Every pattern is prepared before any probing.
    let mut prepared: Vec<Option<PreparedPattern>> = Vec::with_capacity(terms.len());
    let mut matchers: Vec<Option<LineMatcher>> = Vec::with_capacity(terms.len());
    let emit = !matches!(req.output, QueryOutput::Counts | QueryOutput::Occurrences);
    let occurrences = req.output == QueryOutput::Occurrences;
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
    // The literal an occurrences pass counts without a locate.
    let counted: Vec<Option<&[u8]>> = terms
        .iter()
        .zip(&prepared)
        .map(|(t, pattern)| match pattern {
            _ if !occurrences => None,
            Some(p) => crate::pattern::literal(p),
            None => Some(t.literal),
        })
        .collect();
    // Changed docs are located, so the screen drops their indexed text.
    let mut place = docs.changed().to_vec();
    let probes: Vec<Query<'_>> = terms
        .iter()
        .zip(&prepared)
        .zip(&counted)
        .map(|((t, pattern), &counted)| {
            match (
                counted,
                pattern,
                pattern.as_ref().and_then(crate::pattern::literal),
            ) {
                (Some(term), _, _) => Query::Occurrences {
                    term,
                    place: &place,
                },
                // Counts mode counts a literal pattern from the FM-index rather
                // than open a file per candidate; stored documents keep being
                // read from the live docs.shard.
                (None, Some(_), Some(lit)) if !emit && !manifest.store_docs => Query::Literal(lit),
                (None, Some(p), _) => Query::Regex(p),
                (None, None, _) => Query::Literal(t.literal),
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
    for ((((t, pattern), mut answer), matcher), &counted) in terms
        .iter()
        .zip(&prepared)
        .zip(answers)
        .zip(&matchers)
        .zip(&counted)
    {
        let (term, literal) = (&*t.name, t.literal);
        let fetch_start = Instant::now();
        let mut exact = answer.exact;
        let mut candidates = candidate_docs(answer.candidates, num_docs);
        docs.screen(&mut exact, &mut candidates);
        // A doc that changed mid-query may hide among the unlocated
        // occurrences: count the term again with it located.
        while let Some(term) = counted.filter(|_| docs.changed() != place) {
            place = docs.changed().to_vec();
            let a = index
                .probe_all(&[Query::Occurrences {
                    term,
                    place: &place,
                }])
                .remove(0);
            (exact, candidates) = (a.exact, candidate_docs(a.candidates, num_docs));
            (answer.probe_us, answer.occurrences) = (answer.probe_us + a.probe_us, a.occurrences);
            docs.screen(&mut exact, &mut candidates);
        }
        debug_assert!(exact.windows(2).all(|w| w[0].0 < w[1].0));
        // Counts mode counts a fetched document with the pattern: a literal
        // never has candidates.
        debug_assert!(emit || (pattern.is_some() && counted.is_none()) || candidates.is_empty());
        let mut scan = scan_ids(req.output, literal, &candidates, &exact);
        if !docs.stale().is_empty() {
            scan.extend_from_slice(docs.stale());
            scan.sort_unstable();
        }
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
            counts.total += answer.occurrences;
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
        // fetch into the ascending stream; a fetched doc wins its id.
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
                while next_exact < exact.len() && exact[next_exact].0 < doc {
                    let (i, c) = exact[next_exact];
                    next_exact += 1;
                    counts.add(emit_probe_exact(sink, manifest, req.output, literal, i, c)?);
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
                    doc,
                    path: path_of(manifest, doc)?,
                    raw: matching::strip_bom(bytes),
                    outcome: &o,
                })
            },
        )?;
        if emit {
            for &(i, c) in &exact[next_exact..] {
                counts.add(emit_probe_exact(sink, manifest, req.output, literal, i, c)?);
            }
        }
        let report = QueryReport {
            term: term.to_owned(),
            latency_us: answer.probe_us + fetch_start.elapsed().as_micros() as u64,
            total_matches: counts.total,
            docs_matched: (!occurrences).then_some(counts.docs),
            counts_exact: counts.exact,
            leaves: Vec::new(),
        };
        if emit {
            sink.term_end(&report)?;
        }
        queries.push(report);
    }
    Ok(OutputReport {
        index_name: crate::manifest::NAME.to_owned(),
        index_time_us: loaded.load_wall.as_micros() as u64,
        index_size_bytes: index.index_size_bytes(),
        num_files_indexed: index.num_files_indexed(),
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
        QueryOutput::Counts | QueryOutput::Occurrences | QueryOutput::FilesWithMatches => false,
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
    manifest: &Manifest,
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
        doc,
        path: path_of(manifest, doc)?,
        raw: &[],
        outcome: &outcome,
    })?;
    Ok(spans.max(1))
}

fn path_of(manifest: &Manifest, doc: usize) -> Result<&str, String> {
    manifest.files.get(doc).map(String::as_str).ok_or_else(|| {
        format!(
            "error: the index reported doc {doc} but holds {} doc(s)",
            manifest.files.len()
        )
    })
}

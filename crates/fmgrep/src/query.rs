use std::borrow::Cow;
use std::path::Path;
use std::time::{Duration, Instant};

use grep_printer::Stats;

use crate::blobs::BlobSlots;
use crate::diagnostic::Diagnostic;
use crate::expr::Expr;
use crate::fetch::{Ask, DocStore, Paths, Skip, fetch_stream_parallel, fetch_threads};
use crate::filter::PathFilter;
use crate::manifest::{Checked, FileTable, Manifest};
use crate::matching::{self, DocSearch, Found, MatchMode, QueryOutput};
use crate::pattern::{PreparedPattern, count_overlapping, plain_literal};
use crate::probe::{Answer, Query, candidate_docs};
use crate::sharded::{DocfmIndex, Walk};
use crate::sink::{DocMatches, MatchSink};
use crate::source::Learned;
use crate::wire::{OutputReport, QueryReport};

/// What a search answers. The manifest is read once by the dispatcher and
/// threaded through.
pub struct QueryRequest<'a> {
    pub index_dir: &'a Path,
    pub manifest: &'a Manifest,
    /// Overrides a files-layout manifest's root.
    pub corpus_root: Option<&'a Path>,
    /// Every term is a regex, not a literal.
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
    /// A search that `filter` keeps from every file reports it in
    /// [`OutputReport::none_searched`] instead of failing: its client
    /// searches other files too.
    pub report_none: bool,
    /// Whether the search says how it plans each term (`FMGREP_VERBOSE=2`).
    pub plans: bool,
}

mod boolean;
mod read;

/// The document state a query answers from.
pub struct Loaded {
    docs: DocStore,
    pub load_wall: Duration,
    band: boolean::ReadBand,
    locate_us: f64,
}

impl Loaded {
    /// Plan ANDs from this machine's warm read cost, measured now.
    pub fn measure_band(&mut self, manifest: &Manifest, left_out: &mut Vec<Diagnostic>) {
        self.band = boolean::warm_band(&mut self.docs, manifest, left_out);
    }

    /// Price a locate from this machine's warm locates, measured now; under
    /// verbose, note both costs.
    pub fn measure_locate(&mut self, index: &DocfmIndex, left_out: &mut Vec<Diagnostic>) {
        let start = Instant::now();
        self.locate_us = read::locate_us(index.locate_us());
        if crate::verbose::verbose() {
            left_out.push(Diagnostic::Costs {
                read_us: self.band.min_us,
                locate_us: self.locate_us,
                sampled_us: start.elapsed().as_micros(),
            });
        }
    }

    /// Why a server cannot use watchman at all, if it cannot.
    pub fn refusal(&self) -> Option<String> {
        self.docs.refusal()
    }

    /// Before a served query, re-check what changed since the last one.
    /// Whether it re-checked.
    pub fn recheck(&self, files: Checked<'_>, left_out: &mut Vec<Diagnostic>) -> bool {
        self.docs.recheck(files, left_out)
    }

    /// Under verbose, note the added docs, after what a recheck left out is
    /// printed.
    pub fn note_added(&self, left_out: &mut Vec<Diagnostic>) {
        self.docs.note_added(left_out);
    }

    /// Note the added docs, begin the first query and note the stale docs:
    /// how many of the indexed `files` are stale. After the load, once its
    /// warnings are printed.
    pub fn note_stale(&mut self, files: &FileTable, left_out: &mut Vec<Diagnostic>) -> usize {
        note_stale(&mut self.docs, files, left_out)
    }

    /// Leave out `skip`'s files until the next call.
    pub fn skip(&mut self, skip: Skip) {
        self.docs.set_skip(skip);
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

/// Load the persisted index and its documents, and check coverage. Alongside
/// the index load the files layout re-checks what watchman names, else
/// stats every doc and discovers added files; `ask` says when watchman is
/// asked, at load and in [`Loaded::recheck`]. `checked` is the manifest's
/// paths when the caller has checked them already, as a server has. What the
/// check leaves out goes to `left_out`, also when the load fails. The caller
/// then calls [`note_stale`], after it prints those.
pub fn load(
    index_dir: &Path,
    manifest: &Manifest,
    corpus_root: Option<&Path>,
    ask: Ask,
    checked: Option<Checked<'_>>,
    left_out: &mut Vec<Diagnostic>,
) -> Result<(DocfmIndex, Loaded), String> {
    let load_start = Instant::now();
    let load = tracing::info_span!("load").entered();
    let (docs, blobs) =
        tracing::info_span!("open").in_scope(|| open(index_dir, manifest, corpus_root, ask))?;
    let (files, docs_ref) = (&manifest.files, &docs);
    #[cfg(test)]
    let hooks = crate::tests::Hooks::here();
    let load_id = load.id();
    // Plain threads: fixed blocking passes that overlap the load.
    let (index, fresh, wait, found) = std::thread::scope(|scope| {
        let fresh = scope.spawn(move || {
            // A new thread is in no span: name the parent.
            let check = tracing::info_span!(parent: load_id, "check");
            let _span = check.enter();
            #[cfg(test)]
            hooks.apply();
            let mut found = Vec::new();
            let fresh = check_changes(scope, &check, docs_ref, files, checked, &mut found);
            (fresh, found)
        });
        let index =
            tracing::info_span!("index").in_scope(|| DocfmIndex::load(&blobs, &manifest.doc_lens));
        let wait = tracing::info_span!("wait").entered();
        let (fresh, found) = fresh.join().expect("the stat pass panicked");
        (index, fresh, wait, found)
    });
    drop(wait);
    left_out.extend(found);
    fresh?;
    let index = index.map_err(|e| format!("error: {e}"))?;
    let loaded = Loaded {
        docs,
        load_wall: load_start.elapsed(),
        band: boolean::ReadBand::DEFAULT,
        locate_us: read::locate_us(None),
    };
    covers(index.num_files_indexed(), manifest)?;
    Ok((index, loaded))
}

/// The check of what changed since the index, as [`load`] runs it beside
/// the index load: the re-check of what watchman names, else the stat
/// pass, with discovery on a thread of `scope`, in `span`. `checked` is
/// the manifest's paths when the caller has checked them already. What the
/// check leaves out goes to `left_out`: the replay's, then discovery's.
pub fn check_changes<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    span: &tracing::Span,
    docs: &'env DocStore,
    files: &'env FileTable,
    checked: Option<Checked<'env>>,
    left_out: &mut Vec<Diagnostic>,
) -> Result<Learned, String> {
    // Stored docs need no path here.
    if docs.stored() {
        return Ok(Learned::Stored);
    }
    // The files layout reads every path: one check of them all.
    let files = match checked {
        Some(files) => files,
        None => files.checked()?,
    };
    // A server that refuses to serve needs no stat pass.
    let learned = docs.replay(files, left_out);
    if matches!(learned, Learned::Answered { .. }) || docs.refusal().is_some() {
        return Ok(learned);
    }
    let span = span.clone();
    let discovery = scope.spawn(move || {
        let mut found = Vec::new();
        span.in_scope(|| docs.discover(files, &mut found));
        found
    });
    docs.stat_all(files, left_out);
    left_out.extend(
        discovery
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e)),
    );
    Ok(learned)
}

/// Open the documents and the blobs of the index at `index_dir`, as
/// [`load`] opens them before the index loads: what a search refuses
/// there is refused here.
pub fn open<'m>(
    index_dir: &Path,
    manifest: &'m Manifest,
    corpus_root: Option<&Path>,
    ask: Ask,
) -> Result<(DocStore, BlobSlots<'m>), String> {
    let docs = DocStore::open(index_dir, manifest, corpus_root, ask)?;
    let blobs = BlobSlots::open(index_dir, &manifest.blobs, &manifest.blob_lens)?;
    Ok((docs, blobs))
}

/// After the check of what changed, as [`load`] ends: note the added docs,
/// begin the first query of `docs` and note its stale docs. How many of the
/// indexed `files` are stale.
pub fn note_stale(docs: &mut DocStore, files: &FileTable, left_out: &mut Vec<Diagnostic>) -> usize {
    docs.note_added(left_out);
    docs.begin();
    let stale = docs.stale().partition_point(|&id| id < files.len());
    if stale > 0 {
        left_out.push(Diagnostic::Stale { n: stale });
    }
    stale
}

/// The index of no file and the documents `files` under `root`, which no
/// index describes: [`answer`] scans every one in full.
pub fn listed(root: &Path, files: &[String]) -> Result<(DocfmIndex, Loaded), String> {
    let loaded = Loaded {
        docs: DocStore::listed(root.to_path_buf(), files)?,
        load_wall: Duration::ZERO,
        band: boolean::ReadBand::DEFAULT,
        locate_us: read::locate_us(None),
    };
    Ok((DocfmIndex::empty(), loaded))
}

/// [`load`], [`note_stale`], then [`answer`]; what they leave out is dropped.
#[cfg(test)]
pub fn run_query(req: &QueryRequest<'_>, sink: &mut dyn MatchSink) -> Result<OutputReport, String> {
    let mut left_out = Vec::new();
    let (index, mut loaded) = load(
        req.index_dir,
        req.manifest,
        req.corpus_root,
        Ask::Load,
        None,
        &mut left_out,
    )?;
    loaded.note_stale(&req.manifest.files, &mut left_out);
    let report = answer(&index, &mut loaded, req, sink, &mut left_out);
    tracing::info_span!("drop").in_scope(|| drop((index, loaded)));
    report
}

/// Answer every term, or the expression, from `index` and `loaded`; then
/// save the watchman token and what differs.
pub fn answer(
    index: &DocfmIndex,
    loaded: &mut Loaded,
    req: &QueryRequest<'_>,
    sink: &mut dyn MatchSink,
    left_out: &mut Vec<Diagnostic>,
) -> Result<OutputReport, String> {
    let query = tracing::info_span!("query").entered();
    let mut report = match req.expr {
        Some(expr) => boolean::answer_expr(index, loaded, req, expr, sink, left_out)?,
        None => {
            let terms: Vec<Term<'_>> = req.terms.iter().map(|t| Term::of(t, req)).collect();
            answer_terms(index, loaded, req, &terms, sink, left_out)?
        }
    };
    drop(query);
    tracing::info_span!("save")
        .in_scope(|| loaded.docs.checkpoint(&req.manifest.files, left_out))?;
    if let Some(f) = &req.filter {
        report.unindexed = f.unindexed(|p| loaded.docs.holds(&req.manifest.files, p))?;
    }
    let matched = |q: &QueryReport| q.total_matches > 0 || q.docs_matched > 0;
    if let Some(f) = &req.filter
        && !report.queries.iter().any(matched)
        && f.excludes_all(&loaded.docs.paths(&req.manifest.files), |id| {
            loaded.docs.is_gone(id)
        })?
    {
        if req.report_none {
            report.none_searched = true;
            return Ok(report);
        }
        return Err(none_searched(f));
    }
    Ok(report)
}

/// The error of a search that `f` keeps from every file.
pub fn none_searched(f: &PathFilter) -> String {
    let why = match f.globs() {
        true => "the globs exclude every file",
        false => "no indexed file is below the cwd",
    };
    format!("error: no files were searched: {why}")
}

/// One per-term query: the name it reports under, the text it searches for,
/// and its pattern when it takes the pattern path.
struct Term<'a> {
    name: Cow<'a, str>,
    /// The literal, or for a pattern its source.
    text: Cow<'a, str>,
    pattern: Option<Cow<'a, str>>,
    /// A literal that `-i` turned into the escaped pattern of itself, so
    /// that none stays a literal: the index walks its case variants.
    folded: bool,
}

impl<'a> Term<'a> {
    /// The term of `name`: a pattern verbatim when the request reads its
    /// terms as regexes, else a literal. A pattern that is nothing but a
    /// literal (`plain_literal`) is answered as that literal: from the index
    /// alone, with no read of the files it names.
    fn of(name: &'a str, req: &QueryRequest<'_>) -> Self {
        // The empty pattern matches every line, whatever the flags; the
        // empty literal matches the same.
        if name.is_empty() {
            return Term {
                name: Cow::Borrowed(name),
                text: Cow::Borrowed(name),
                pattern: Some(Cow::Borrowed(name)),
                folded: false,
            };
        }
        let source = req.regex.then_some(name);
        let bare = |text: Cow<'a, str>| {
            let folded = req.mode.ignore_case && !text.is_empty();
            Term {
                name: Cow::Borrowed(name),
                pattern: folded.then(|| Cow::Owned(regex::escape(&text))),
                text,
                folded,
            }
        };
        match source.map(|p| (p, plain_literal(p))) {
            None => bare(Cow::Borrowed(name)),
            Some((_, Some(literal))) => bare(Cow::Owned(literal)),
            Some((pattern, None)) => Term {
                name: Cow::Borrowed(name),
                text: Cow::Borrowed(name),
                pattern: Some(Cow::Borrowed(pattern)),
                folded: false,
            },
        }
    }

    fn literal(&self) -> &[u8] {
        self.text.as_bytes()
    }
}

/// An invalid pattern is fatal, never a silent 0.
fn prepare(pattern: Option<&str>, mode: MatchMode) -> Result<Option<PreparedPattern>, String> {
    pattern
        .map(|p| PreparedPattern::prepare_with(p, mode).map_err(|e| format!("error: {e}")))
        .transpose()
}

/// `path` as `filter` prints it.
fn printed<'a>(
    paths: &'a Paths<'_>,
    filter: Option<&PathFilter>,
    doc: usize,
) -> Result<Cow<'a, str>, String> {
    let path = path_of(paths, doc)?;
    Ok(match filter {
        Some(f) => Cow::Owned(f.printed(path).into_owned()),
        None => Cow::Borrowed(path),
    })
}

/// rg's files and bytes searched: the index's less the skip list's files,
/// BOMs left out as rg's searcher strips them where the output prints the
/// bytes (rg's JSON summary, `--stats`). Under `--stats`, the indexed files
/// that a PATH, a glob or the cwd keeps and the skip list does not hold,
/// and their bytes, BOMs included: the index does not record which file
/// has one. That reads the path of every file the filter may keep.
fn corpus(
    req: &QueryRequest<'_>,
    docs: &DocStore,
    paths: &Paths<'_>,
) -> Result<(usize, u64), String> {
    let m = req.manifest;
    let stats = matches!(
        req.output,
        QueryOutput::Matches(matching::Print { stats: true, .. })
    );
    let whole = docs.corpus(m, req.output.json() || stats)?;
    let (Some(f), true) = (&req.filter, stats) else {
        return Ok(whole);
    };
    let ranges =
        (f.cover(&m.files)?).unwrap_or_else(|| std::iter::once(0..m.files.len()).collect());
    let ids = ranges.into_iter().flatten().filter(|&id| !docs.skipped(id));
    let (mut failed, mut kept) = (None, (0, 0));
    for id in f.kept(paths, ids, &mut failed) {
        kept = (kept.0 + 1, kept.1 + m.doc_lens[id]);
    }
    match (failed, kept.0 == whole.0) {
        (Some(e), _) => Err(e),
        (None, true) => Ok(whole),
        (None, false) => Ok(kept),
    }
}

/// `doc` searched by `s`, with the path it prints under when it matches.
fn searched<'a>(
    s: &DocSearch,
    paths: &'a Paths<'_>,
    filter: Option<&PathFilter>,
    output: QueryOutput,
    doc: usize,
    bytes: &[u8],
) -> Result<Option<(Cow<'a, str>, Found)>, String> {
    if output.names_lines() {
        let path = printed(paths, filter, doc)?;
        return Ok(s.doc(&path, bytes)?.map(|f| (path, f)));
    }
    (s.doc("", bytes)?)
        .map(|f| Ok((printed(paths, filter, doc)?, f)))
        .transpose()
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
    left_out: &mut Vec<Diagnostic>,
) -> Result<OutputReport, String> {
    let manifest = req.manifest;
    let num_docs = manifest.files.len();
    loaded.docs.begin();
    let docs = &mut loaded.docs;

    // Every pattern is prepared before any probing.
    let mut prepared: Vec<Option<PreparedPattern>> = Vec::with_capacity(terms.len());
    let mut searches: Vec<Option<DocSearch>> = Vec::with_capacity(terms.len());
    let emit = req.output != QueryOutput::Counts;
    for t in terms {
        let pattern = t.pattern.as_deref();
        let prep = prepare(pattern, req.mode)?;
        // Outside counts mode only, so its newline refusal does not change
        // what the harness path accepts; built before any fetch.
        if emit {
            let m = matching::matcher(&t.text, pattern, req.mode)?;
            searches.push(m.map(|m| DocSearch::new(m, req.output, req.mode)));
        }
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
                (None, _) => Query::Literal(t.literal()),
            }
        })
        .collect();
    let cover = match &req.filter {
        Some(f) => f.cover(&manifest.files)?,
        None => None,
    };
    // A filtered folded term is counted by its walk, which a locate reuses.
    let walks: Vec<Option<Walk<'_>>> = (probes.iter().zip(terms))
        .map(|(q, t)| match (*q, cover.as_deref()) {
            (Query::Regex(p), Some(c)) if t.folded && boolean::demotable(t.literal()) => {
                index.walk(p, Some(c))
            }
            _ => None,
        })
        .collect();
    let reads = match (&req.filter, cover.as_deref()) {
        (Some(f), Some(c)) => {
            let readable: Vec<Option<read::Readable<'_>>> = (probes.iter().zip(terms))
                .zip(&walks)
                .map(|((q, t), w)| match (*q, w) {
                    (Query::Literal(l), _) if t.pattern.is_none() && boolean::demotable(l) => {
                        Some(read::Readable {
                            count: read::Count::Literal(l),
                            text: l,
                            fetched: reads_exact(req.output, l),
                        })
                    }
                    (_, Some(walk)) => Some(read::Readable {
                        count: read::Count::Walked(walk),
                        text: t.literal(),
                        fetched: true,
                    }),
                    _ => None,
                })
                .collect();
            let paths = docs.paths(&manifest.files);
            let ids = c.iter().cloned().flatten();
            let mut failed = None;
            let kept = f.kept(
                &paths,
                ids.filter(|&i| manifest.doc_lens[i] > 0),
                &mut failed,
            );
            let file_us = boolean::read_band(loaded.band).min_us;
            let costs = read::Costs {
                file_us,
                locate_us: loaded.locate_us,
            };
            let (reads, plan) = read::read_instead(
                index,
                &readable,
                c,
                kept,
                &manifest.doc_lens,
                costs,
                req.plans,
            );
            left_out.extend(plan.map(|text| Diagnostic::Plan { text }));
            failed.map_or(Ok(reads), Err)?
        }
        _ => probes.iter().map(|_| None).collect(),
    };
    let located: Vec<Query<'_>> = (probes.iter().zip(&reads).zip(&walks))
        .filter(|((_, r), _)| r.is_none())
        .map(|((q, _), w)| w.as_ref().map_or(*q, Query::Walked))
        .collect();
    let mut got = match located.is_empty() {
        true => Vec::new(),
        false => index.probe_all(&located, cover.as_deref()),
    }
    .into_iter();
    let reading: Vec<bool> = reads.iter().map(Option::is_some).collect();
    // A term located from its walk took the walk's time too.
    let answers: Vec<Answer> = (reads.into_iter().zip(&walks))
        .map(|(r, w)| {
            r.unwrap_or_else(|| {
                let mut a = got.next().expect("probe_all answers every query");
                a.probe_us += w.as_ref().map_or(0, Walk::us);
                a
            })
        })
        .collect();
    let plans: Vec<String> = match req.plans {
        true => probes.iter().map(plan_of).collect(),
        false => Vec::new(),
    };
    drop(probes);
    debug_assert_eq!(answers.len(), terms.len());

    if emit {
        let (files, bytes) = corpus(req, docs, &docs.paths(&manifest.files))?;
        sink.corpus(files, bytes)?;
    }
    let mut queries = Vec::with_capacity(terms.len());
    let filter = req.filter.as_ref();
    let each = terms.iter().zip(&prepared).zip(answers).zip(reading);
    for (i, (((t, pattern), answer), read)) in each.enumerate() {
        let search = searches.get(i).and_then(Option::as_ref);
        let (term, literal) = (&*t.name, t.literal());
        let fetch_start = Instant::now();
        let mut exact = answer.exact;
        let mut candidates = candidate_docs(answer.candidates, num_docs);
        in_index(&exact, &candidates, num_docs)?;
        docs.screen(&mut exact, &mut candidates);
        debug_assert!(exact.windows(2).all(|w| w[0].0 < w[1].0));
        // Counts mode counts a fetched document with the pattern, or a read
        // literal's occurrences; a located literal has no candidates.
        debug_assert!(emit || pattern.is_some() || read || candidates.is_empty());
        let paths = docs.paths(&manifest.files);
        let mut stale = docs.stale().to_vec();
        if let Some(f) = &req.filter {
            f.retain(&paths, &mut exact, |e| e.0)?;
            f.retain(&paths, &mut candidates, |&id| id)?;
            f.retain(&paths, &mut stale, |&id| id)?;
        }
        let mut scan = scan_ids(req.output, literal, &candidates, &exact);
        if !stale.is_empty() {
            scan.extend_from_slice(&stale);
            scan.sort_unstable();
        }
        paths.sort(&mut scan);
        if let Some(plan) = plans.get(i) {
            left_out.push(Diagnostic::Plan {
                text: format!(
                    "plan: term {term:?}: {plan}; {} files located, {} candidates, {} files read\n",
                    exact.len(),
                    candidates.len(),
                    scan.len()
                ),
            });
        }
        // Every path the term may print is checked before its first line.
        if emit {
            paths.check(exact.iter().map(|e| e.0).chain(scan.iter().copied()))?;
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
        }
        // Counts mode counts regex candidates and stale docs as the index
        // would; the emit modes search them as rg does, BOM stripped.
        let needle = memchr::memmem::Finder::new(literal);
        let map = |doc: usize, raw: &[u8]| {
            let bytes = matching::strip_bom(raw);
            Ok::<_, String>(match (emit, search, pattern.as_ref()) {
                (true, Some(s), _) => (0, searched(s, &paths, filter, req.output, doc, bytes)?),
                (true, None, _) => (0, None),
                (false, _, Some(p)) => (p.count(bytes), None),
                (false, _, None) => (count_overlapping(raw, &needle), None),
            })
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
            &mut |doc, _, scanned| {
                let (count, found) = scanned?;
                if !emit {
                    counts.add(count);
                    return Ok(());
                }
                while next_exact < exact.len() && paths.cmp(exact[next_exact].0, doc).is_lt() {
                    let (i, c) = exact[next_exact];
                    next_exact += 1;
                    counts.add(emit_probe_exact(
                        sink, &paths, filter, req.output, literal, i, c,
                    )?);
                }
                if exact.get(next_exact).is_some_and(|&(i, _)| i == doc) {
                    next_exact += 1;
                }
                let Some((path, f)) = found else {
                    return Ok(());
                };
                counts.add(f.count.max(1));
                sink.doc(DocMatches {
                    rel: path_of(&paths, doc)?,
                    path: &path,
                    printed: &f.printed,
                    stats: &f.stats,
                })
            },
            left_out,
        )?;
        if emit {
            for &(i, c) in &exact[next_exact..] {
                counts.add(emit_probe_exact(
                    sink, &paths, filter, req.output, literal, i, c,
                )?);
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
        none_searched: false,
        unindexed: Vec::new(),
    })
}

/// How the index answers a term, as `FMGREP_VERBOSE=2` says it.
fn plan_of(query: &Query<'_>) -> String {
    match query {
        Query::Literal(_) | Query::Count(_) | Query::LiteralWithin { .. } => {
            "literal, located".to_owned()
        }
        Query::Walked(_) => "folded literal, walked".to_owned(),
        Query::Regex(p) if crate::pattern::fold_planned(p) => "folded literal, walked".to_owned(),
        Query::Regex(p) => match crate::pattern::anchor(p) {
            Some(a) => format!("regex, anchor {:?}", String::from_utf8_lossy(a)),
            None if crate::pattern::narrows(p) => "regex, walked as an automaton".to_owned(),
            None => "regex, no anchor".to_owned(),
        },
    }
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

/// Whether this mode reads the probe-exact documents of `term`.
fn reads_exact(output: QueryOutput, term: &[u8]) -> bool {
    match output {
        QueryOutput::Counts | QueryOutput::FilesWithMatches => false,
        QueryOutput::CountMatches => !probe_count_is_rgs(output, term),
        QueryOutput::CountLines | QueryOutput::Matches(_) => true,
    }
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
    if !reads_exact(output, term) {
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
    filter: Option<&PathFilter>,
    output: QueryOutput,
    term: &[u8],
    doc: usize,
    count: u64,
) -> Result<u64, String> {
    if count == 0 {
        return Ok(0);
    }
    let exact = probe_count_is_rgs(output, term);
    let printed_count = match exact {
        true => format!("{count}\n"),
        false => String::new(),
    };
    let mut stats = Stats::new();
    if exact {
        stats.add_matches(count);
    }
    sink.doc(DocMatches {
        rel: path_of(paths, doc)?,
        path: &printed(paths, filter, doc)?,
        printed: printed_count.as_bytes(),
        stats: &stats,
    })?;
    Ok(if exact { count } else { 1 })
}

fn path_of<'a>(paths: &'a Paths<'_>, doc: usize) -> Result<&'a str, String> {
    paths
        .path(doc)?
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

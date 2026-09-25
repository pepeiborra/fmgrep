//! Document-level AND / OR: one probe pass, a set-algebra prune, then
//! verification of the undecided documents only.

use std::borrow::Cow;
use std::time::Instant;

use super::{Loaded, QueryRequest, Term, answer_terms, line_matcher, path_of, prepare};
use crate::expr::{Expr, REGEX_REFUSAL, TERMS_REFUSAL};
use crate::fetch::{fetch_stream_parallel, fetch_threads};
use crate::matching::{self, DocOutcome, LineMatcher, QueryOutput};
use crate::pattern::{PreparedPattern, count_overlapping};
use crate::probe::{Query, candidate_docs};
use crate::sink::{DocMatches, MatchSink};
use crate::verify::{self, LeafSet};
use crate::wire::{LeafReport, OutputReport, QueryReport};

/// How the FM-index answers a leaf.
enum Probe {
    /// The empty term: no document, no probe.
    Never,
    /// Exact per-document counts.
    Literal(Vec<u8>),
    /// A candidate superset.
    Regex,
}

struct Leaf<'a> {
    name: String,
    src: &'a str,
    pattern: Option<Cow<'a, str>>,
    prep: Option<PreparedPattern>,
    matcher: LineMatcher,
    probe: Probe,
}

/// Leaves spelled and compiled before any I/O: an invalid one is refused
/// whatever the output mode.
fn leaves<'a>(expr: &'a Expr, req: &QueryRequest<'_>) -> Result<Vec<Leaf<'a>>, String> {
    let mut out = Vec::new();
    for leaf in expr.leaves() {
        let (src, pattern) = match leaf {
            Expr::Term(t) if req.mode.ignore_case && !t.is_empty() => {
                (t.as_str(), Some(Cow::Owned(regex::escape(t))))
            }
            Expr::Term(t) => (t.as_str(), None),
            Expr::Regex(r) => (r.as_str(), Some(Cow::Borrowed(r.as_str()))),
            Expr::And(_) | Expr::Or(_) => unreachable!("leaves() yields leaves"),
        };
        let prep = prepare(pattern.as_deref(), req.mode)?;
        let matcher = line_matcher(
            src.as_bytes(),
            pattern.as_deref().zip(prep.as_ref()),
            req.mode,
        )?;
        let probe = match prep.as_ref() {
            None if src.is_empty() => Probe::Never,
            None => Probe::Literal(src.as_bytes().to_vec()),
            Some(p) => {
                crate::pattern::literal(p).map_or(Probe::Regex, |l| Probe::Literal(l.to_vec()))
            }
        };
        out.push(Leaf {
            name: leaf.to_string(),
            src,
            pattern,
            prep,
            matcher,
            probe,
        });
    }
    Ok(out)
}

/// A leaf's exact counts and candidates, screened.
type Hits = (Vec<(usize, u64)>, Vec<usize>);

/// A node's documents: `yes` satisfy it for sure, `maybe` need a scan, and
/// every other document fails it. Both ascending, disjoint.
#[derive(Clone, Debug, Default, PartialEq)]
struct Tri {
    yes: Vec<usize>,
    maybe: Vec<usize>,
}

fn union(a: &[usize], b: &[usize]) -> Vec<usize> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => (out.push(a[i]), i += 1),
            std::cmp::Ordering::Greater => (out.push(b[j]), j += 1),
            std::cmp::Ordering::Equal => (out.push(a[i]), (i, j) = (i + 1, j + 1)),
        };
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

fn intersect(a: &[usize], b: &[usize]) -> Vec<usize> {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    small
        .iter()
        .copied()
        .filter(|x| large.binary_search(x).is_ok())
        .collect()
}

fn difference(a: &[usize], b: &[usize]) -> Vec<usize> {
    a.iter()
        .copied()
        .filter(|x| b.binary_search(x).is_err())
        .collect()
}

/// `expr`'s documents from its leaves', consumed in leaf order.
fn prune(expr: &Expr, tris: &[Tri], next: &mut usize) -> Tri {
    let parts = match expr {
        Expr::Term(_) | Expr::Regex(_) => {
            *next += 1;
            return tris[*next - 1].clone();
        }
        Expr::And(v) | Expr::Or(v) => v.iter().map(|e| prune(e, tris, next)).collect::<Vec<_>>(),
    };
    let fold = |f: fn(&[usize], &[usize]) -> Vec<usize>, sets: Vec<Vec<usize>>| {
        sets.into_iter()
            .reduce(|a, b| f(&a, &b))
            .unwrap_or_default()
    };
    let yes_sets = parts.iter().map(|p| p.yes.clone()).collect();
    if matches!(expr, Expr::And(_)) {
        let yes = fold(intersect, yes_sets);
        let all = fold(
            intersect,
            parts.iter().map(|p| union(&p.yes, &p.maybe)).collect(),
        );
        Tri {
            maybe: difference(&all, &yes),
            yes,
        }
    } else {
        let yes = fold(union, yes_sets);
        let maybe = fold(union, parts.iter().map(|p| p.maybe.clone()).collect());
        Tri {
            maybe: difference(&maybe, &yes),
            yes,
        }
    }
}

/// Whether some occurrence of `a` can overlap one of `b`.
fn can_overlap(a: &[u8], b: &[u8]) -> bool {
    memchr::memmem::find(a, b).is_some()
        || memchr::memmem::find(b, a).is_some()
        || (1..a.len().min(b.len())).any(|k| a.ends_with(&b[..k]) || b.ends_with(&a[..k]))
}

/// Summed leaf counts are rg's alternation count only for plain literals
/// that are each exact and cannot overlap one another.
fn sum_is_exact(leaves: &[Leaf<'_>]) -> bool {
    let plain: Option<Vec<&[u8]>> = leaves
        .iter()
        .map(|l| {
            (l.pattern.is_none() && matching::literal_counts_are_exact(l.src.as_bytes()))
                .then_some(l.src.as_bytes())
        })
        .collect();
    plain.is_some_and(|p| {
        p.iter()
            .enumerate()
            .all(|(i, a)| p[i + 1..].iter().all(|b| !can_overlap(a, b)))
    })
}

fn refuse(req: &QueryRequest<'_>, expr: &Expr) -> Result<(), String> {
    if !req.terms.is_empty() {
        return Err(format!("error: {TERMS_REFUSAL}"));
    }
    if req.regex {
        return Err(format!("error: {REGEX_REFUSAL}"));
    }
    expr.check().map_err(|e| format!("error: {e}"))?;
    if req.output == QueryOutput::Occurrences && expr.has_and() {
        return Err(
            "error: --occurrences places no match in a document, so it cannot answer AND"
                .to_owned(),
        );
    }
    Ok(())
}

/// `--occurrences` over a pure OR: each leaf's occurrences, summed.
fn occurrences(
    loaded: &mut Loaded,
    req: &QueryRequest<'_>,
    expr: &Expr,
    leaves: &[Leaf<'_>],
    sink: &mut dyn MatchSink,
) -> Result<OutputReport, String> {
    let terms: Vec<Term<'_>> = leaves
        .iter()
        .map(|l| Term {
            name: Cow::Borrowed(&l.name),
            literal: l.src.as_bytes(),
            pattern: l.pattern.clone(),
        })
        .collect();
    let mut out = answer_terms(loaded, req, &terms, sink)?;
    let report = QueryReport {
        term: expr.to_string(),
        latency_us: out.queries.iter().map(|q| q.latency_us).sum(),
        total_matches: out.queries.iter().map(|q| q.total_matches).sum(),
        docs_matched: None,
        counts_exact: sum_is_exact(leaves),
        leaves: out
            .queries
            .iter()
            .map(|q| LeafReport {
                term: q.term.clone(),
                total_matches: q.total_matches,
            })
            .collect(),
    };
    out.queries = vec![report];
    Ok(out)
}

/// Answer `expr` as one query. The documents satisfying it are decided by
/// the FM-index where they can be; only the rest are fetched and scanned.
pub fn answer_expr(
    loaded: &mut Loaded,
    req: &QueryRequest<'_>,
    expr: &Expr,
    sink: &mut dyn MatchSink,
) -> Result<OutputReport, String> {
    refuse(req, expr)?;
    let leaves = leaves(expr, req)?;
    if req.output == QueryOutput::Occurrences {
        return occurrences(loaded, req, expr, &leaves, sink);
    }
    let emit = req.output != QueryOutput::Counts;
    // Lines print and count by the alternation of every leaf, as rg -e a -e b.
    let alternation = match req.output {
        QueryOutput::Counts | QueryOutput::FilesWithMatches => None,
        _ => alternation_of(&leaves, req)?,
    };
    let manifest = req.manifest;
    let num_docs = manifest.files.len();
    loaded.docs.begin();
    let (index, docs) = (&loaded.index, &mut loaded.docs);

    let probes: Vec<Query<'_>> = leaves
        .iter()
        .filter_map(|l| match (&l.probe, &l.prep) {
            (Probe::Never, _) => None,
            (Probe::Literal(lit), _) => Some(Query::Literal(lit)),
            (Probe::Regex, Some(p)) => Some(Query::Regex(p)),
            (Probe::Regex, None) => unreachable!("a regex leaf holds its pattern"),
        })
        .collect();
    let answers = index.probe_all(&probes);
    debug_assert_eq!(answers.len(), probes.len());
    drop(probes);
    let start = Instant::now();
    let mut probe_us = 0;
    let mut answers = answers.into_iter();
    let mut hits: Vec<Hits> = Vec::with_capacity(leaves.len());
    for l in &leaves {
        if matches!(l.probe, Probe::Never) {
            hits.push((Vec::new(), Vec::new()));
            continue;
        }
        let a = answers.next().expect("one answer per probe");
        probe_us += a.probe_us;
        hits.push((a.exact, candidate_docs(a.candidates, num_docs)));
    }
    // One snapshot of the changed docs for every leaf.
    loop {
        let seen = docs.changed().to_vec();
        for (exact, candidates) in &mut hits {
            docs.screen(exact, candidates);
        }
        if docs.changed() == seen {
            break;
        }
    }
    let stale = docs.stale().to_vec();
    let mut tris = Vec::with_capacity(leaves.len());
    for (l, (exact, candidates)) in leaves.iter().zip(&hits) {
        tris.push(match l.probe {
            Probe::Never => Tri::default(),
            Probe::Literal(_) => {
                debug_assert!(candidates.is_empty(), "a literal has no candidates");
                Tri {
                    yes: exact
                        .iter()
                        .filter(|&&(_, c)| c > 0)
                        .map(|&(d, _)| d)
                        .collect(),
                    maybe: stale.clone(),
                }
            }
            Probe::Regex => Tri {
                yes: Vec::new(),
                maybe: union(candidates, &stale),
            },
        });
    }
    let root = prune(expr, &tris, &mut 0);

    let vleaves: Vec<verify::Leaf<'_>> = leaves
        .iter()
        .map(|l| match (&l.probe, &l.pattern) {
            (Probe::Literal(lit), _) => verify::Leaf::Literal(lit),
            (Probe::Regex, Some(src)) => verify::Leaf::Pattern {
                src,
                matcher: &l.matcher,
            },
            _ => verify::Leaf::Never,
        })
        .collect();
    let set = LeafSet::new(&vleaves, req.mode);
    let exact: Vec<&[(usize, u64)]> = hits.iter().map(|(e, _)| e.as_slice()).collect();
    let judge = Judge {
        expr,
        leaves: &leaves,
        tris: &tris,
        exact: &exact,
        stale: &stale,
        set: &set,
        yes: &root.yes,
    };

    let mut report = QueryReport {
        term: expr.to_string(),
        latency_us: 0,
        total_matches: 0,
        docs_matched: Some(0),
        counts_exact: req.output != QueryOutput::FilesWithMatches,
        leaves: Vec::new(),
    };
    if emit {
        sink.corpus(num_docs, manifest.num_bytes - manifest.bom_bytes)?;
        sink.term_start(&report.term)?;
    }
    let mut docs_matched = 0usize;
    let mut leaf_totals = vec![0u64; leaves.len()];
    match (req.output, alternation.as_ref()) {
        (QueryOutput::Counts, _) => {
            // Members a regex leaf still has to count are fetched too.
            let regex_maybe = tris
                .iter()
                .zip(&leaves)
                .filter(|(_, l)| matches!(l.probe, Probe::Regex))
                .fold(Vec::new(), |acc, (t, _)| union(&acc, &t.maybe));
            let fetch = union(&root.maybe, &intersect(&root.yes, &regex_maybe));
            let map =
                |doc: usize, raw: &[u8]| judge.member(doc, raw).then(|| judge.counts(doc, raw));
            fetch_stream_parallel(
                docs,
                &manifest.files,
                &fetch,
                fetch_threads(),
                &map,
                &mut |_, _, counts| {
                    if let Some(counts) = counts {
                        docs_matched += 1;
                        leaf_totals
                            .iter_mut()
                            .zip(counts)
                            .for_each(|(t, c)| *t += c);
                    }
                    Ok(())
                },
            )?;
            // Decided members left unfetched: every regex leaf counts 0 there.
            let rest = difference(&root.yes, &fetch);
            docs_matched += rest.len();
            for ((l, (exact, _)), total) in leaves.iter().zip(&hits).zip(&mut leaf_totals) {
                if matches!(l.probe, Probe::Literal(_)) {
                    *total += exact
                        .iter()
                        .filter(|(d, _)| rest.binary_search(d).is_ok())
                        .map(|&(_, c)| c)
                        .sum::<u64>();
                }
            }
            report.counts_exact = sum_is_exact(&leaves);
        }
        (QueryOutput::FilesWithMatches, _) => {
            let map = |doc: usize, raw: &[u8]| judge.member(doc, raw);
            let mut next_yes = 0usize;
            let mut emit_yes =
                |sink: &mut dyn MatchSink, below: usize, n: &mut usize| -> Result<(), String> {
                    while *n < root.yes.len() && root.yes[*n] < below {
                        listed(sink, manifest, root.yes[*n], &[])?;
                        *n += 1;
                        docs_matched += 1;
                    }
                    Ok(())
                };
            let mut members = 0usize;
            fetch_stream_parallel(
                docs,
                &manifest.files,
                &root.maybe,
                fetch_threads(),
                &map,
                &mut |doc, bytes, member| {
                    emit_yes(sink, doc, &mut next_yes)?;
                    if member {
                        members += 1;
                        listed(sink, manifest, doc, matching::strip_bom(bytes))?;
                    }
                    Ok(())
                },
            )?;
            emit_yes(sink, usize::MAX, &mut next_yes)?;
            docs_matched += members;
        }
        (_, None) => {}
        (_, Some(alt)) => {
            let per_leaf = matches!(
                req.output,
                QueryOutput::CountLines | QueryOutput::CountMatches
            );
            let fetch = union(&root.yes, &root.maybe);
            let output = req.output;
            let map = |doc: usize, raw: &[u8]| {
                if !judge.member(doc, raw) {
                    return None;
                }
                let bytes = matching::strip_bom(raw);
                let counts: Vec<u64> = match per_leaf {
                    true => leaves
                        .iter()
                        .map(|l| {
                            l.matcher
                                .scan(bytes, QueryOutput::CountMatches)
                                .count_matches()
                        })
                        .collect(),
                    false => Vec::new(),
                };
                Some((alt.scan(bytes, output), counts))
            };
            fetch_stream_parallel(
                docs,
                &manifest.files,
                &fetch,
                fetch_threads(),
                &map,
                &mut |doc, bytes, scanned| {
                    let Some((o, counts)) = scanned.filter(|(o, _)| o.matched_lines > 0) else {
                        return Ok(());
                    };
                    docs_matched += 1;
                    report.total_matches += o.count_matches();
                    leaf_totals
                        .iter_mut()
                        .zip(counts)
                        .for_each(|(t, c)| *t += c);
                    sink.doc(DocMatches {
                        doc,
                        path: path_of(manifest, doc)?,
                        raw: matching::strip_bom(bytes),
                        outcome: &o,
                    })
                },
            )?;
        }
    }
    report.docs_matched = Some(docs_matched);
    report.total_matches = match req.output {
        QueryOutput::Counts => leaf_totals.iter().sum(),
        QueryOutput::FilesWithMatches => docs_matched as u64,
        _ => report.total_matches,
    };
    if matches!(
        req.output,
        QueryOutput::Counts | QueryOutput::CountLines | QueryOutput::CountMatches
    ) {
        report.leaves = leaves
            .iter()
            .zip(leaf_totals)
            .map(|(l, total_matches)| LeafReport {
                term: l.name.clone(),
                total_matches,
            })
            .collect();
    }
    report.latency_us = probe_us + start.elapsed().as_micros() as u64;
    if emit {
        sink.term_end(&report)?;
    }
    Ok(OutputReport {
        index_name: crate::manifest::NAME.to_owned(),
        index_time_us: loaded.load_wall.as_micros() as u64,
        index_size_bytes: loaded.index.index_size_bytes(),
        num_files_indexed: loaded.index.num_files_indexed(),
        queries: vec![report],
    })
}

/// `(?:p1)|(?:p2)|..` over the leaves that can match; `None` when none can.
fn alternation_of(
    leaves: &[Leaf<'_>],
    req: &QueryRequest<'_>,
) -> Result<Option<LineMatcher>, String> {
    let parts: Vec<String> = leaves
        .iter()
        .filter(|l| !matches!(l.probe, Probe::Never))
        .map(|l| match &l.pattern {
            Some(p) => format!("(?:{p})"),
            None => regex::escape(l.src),
        })
        .collect();
    if parts.is_empty() {
        return Ok(None);
    }
    LineMatcher::regex(&parts.join("|"), None, req.mode)
        .map(Some)
        .map_err(|e| format!("error: {e}"))
}

/// A `-l` line: membership only.
fn listed(
    sink: &mut dyn MatchSink,
    manifest: &crate::manifest::Manifest,
    doc: usize,
    raw: &[u8],
) -> Result<(), String> {
    let outcome = DocOutcome {
        matched_lines: 0,
        matched_spans: 0,
        spanless_lines: 0,
        multiline: false,
        lines: Vec::new(),
        spans: Vec::new(),
    };
    sink.doc(DocMatches {
        doc,
        path: path_of(manifest, doc)?,
        raw,
        outcome: &outcome,
    })
}

/// Everything a fetched document is judged by.
struct Judge<'a> {
    expr: &'a Expr,
    leaves: &'a [Leaf<'a>],
    tris: &'a [Tri],
    /// Each leaf's screened exact counts.
    exact: &'a [&'a [(usize, u64)]],
    stale: &'a [usize],
    set: &'a LeafSet<'a>,
    /// The root's decided members.
    yes: &'a [usize],
}

impl Judge<'_> {
    fn is_stale(&self, doc: usize) -> bool {
        self.stale.binary_search(&doc).is_ok()
    }

    /// Whether `doc` satisfies the expression: the index decides a fresh
    /// document's literal leaves, its bytes everything else.
    fn member(&self, doc: usize, raw: &[u8]) -> bool {
        if self.yes.binary_search(&doc).is_ok() {
            return true;
        }
        let stale = self.is_stale(doc);
        let mut known: Vec<Option<bool>> = self
            .leaves
            .iter()
            .zip(self.tris)
            .map(|(l, t)| match l.probe {
                Probe::Never => Some(false),
                Probe::Literal(_) if !stale => Some(t.yes.binary_search(&doc).is_ok()),
                Probe::Regex if t.maybe.binary_search(&doc).is_err() => Some(false),
                _ => None,
            })
            .collect();
        self.set.fill(matching::strip_bom(raw), &mut known);
        self.expr.eval(&mut |i| known[i] == Some(true))
    }

    /// Each leaf's counts-mode number in member `doc`, as the per-term
    /// counts drive computes it.
    fn counts(&self, doc: usize, raw: &[u8]) -> Vec<u64> {
        let stale = self.is_stale(doc);
        let bytes = matching::strip_bom(raw);
        self.leaves
            .iter()
            .zip(self.tris)
            .zip(self.exact)
            .map(|((l, t), exact)| match (&l.probe, &l.prep) {
                (Probe::Never, _) => 0,
                (Probe::Literal(_), Some(p)) if stale => p.count(bytes),
                (Probe::Literal(lit), None) if stale => {
                    count_overlapping(raw, &memchr::memmem::Finder::new(lit))
                }
                (Probe::Literal(_), _) => exact
                    .binary_search_by_key(&doc, |&(d, _)| d)
                    .map_or(0, |k| exact[k].1),
                (Probe::Regex, Some(p)) if t.maybe.binary_search(&doc).is_ok() => p.count(bytes),
                (Probe::Regex, _) => 0,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;

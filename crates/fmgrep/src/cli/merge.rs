//! A search through a server from a checkout: the client's own files
//! searched here, then put between the server's in path order, so that the
//! output is what a local index of the checkout would print.

use std::path::Path;

use grep_printer::Stats;

use super::checkout::Local;
use super::params::QueryParams;
use crate::manifest::Manifest;
use crate::query::{answer, listed};
use crate::sink::{DocMatches, MatchSink};
use crate::wire::{OutputReport, QueryReport};

/// One matching file of the client's part, kept until its turn.
struct Doc {
    rel: String,
    path: String,
    printed: Vec<u8>,
    stats: Stats,
}

/// The client's part of a search: each term's matching files, in path
/// order, and the report.
pub struct Part {
    terms: Vec<Vec<Doc>>,
    pub report: OutputReport,
    /// The shape of a full scan of the client's files.
    corpus: (usize, u64),
}

/// Keeps each term's files.
#[derive(Default)]
struct Keep {
    terms: Vec<Vec<Doc>>,
}

impl MatchSink for Keep {
    fn term_start(&mut self, _: &str) -> Result<(), String> {
        self.terms.push(Vec::new());
        Ok(())
    }

    fn doc(&mut self, m: DocMatches<'_>) -> Result<(), String> {
        let mut stats = Stats::new();
        stats.add_matches(m.stats.matches());
        stats.add_matched_lines(m.stats.matched_lines());
        stats.add_bytes_printed(m.stats.bytes_printed());
        let term = self
            .terms
            .last_mut()
            .ok_or("error: a file before its term")?;
        term.push(Doc {
            rel: m.rel.to_owned(),
            path: m.path.to_owned(),
            printed: m.printed.to_vec(),
            stats,
        });
        Ok(())
    }
}

/// `p` answered over the client's files `local` under `top`, which no
/// index describes.
pub fn part(p: &QueryParams, top: &Path, local: &Local) -> Result<Part, String> {
    let manifest = Manifest::empty(top);
    let (index, mut loaded) = listed(top, &local.files)?;
    let req = p.request(Path::new(""), &manifest, Some(top))?;
    let mut keep = Keep::default();
    let report = answer(&index, &mut loaded, &req, &mut keep)?;
    Ok(Part {
        terms: keep.terms,
        report,
        corpus: (local.files.len(), local.bytes),
    })
}

/// Puts the client's files of `part` between the server's, which come in
/// path order, on their way to `out`.
pub struct Merge<'a> {
    out: &'a mut dyn MatchSink,
    part: &'a Part,
    /// The term under way, and its next file of the client's.
    term: Option<usize>,
    next: usize,
}

impl<'a> Merge<'a> {
    pub fn new(out: &'a mut dyn MatchSink, part: &'a Part) -> Self {
        Self {
            out,
            part,
            term: None,
            next: 0,
        }
    }

    /// The client's files of the term under way that come before `rel`
    /// (all of them when `None`), to `out`.
    fn until(&mut self, rel: Option<&str>) -> Result<(), String> {
        let Some(docs) = self.term.and_then(|t| self.part.terms.get(t)) else {
            return Ok(());
        };
        while let Some(d) = docs.get(self.next) {
            if rel.is_some_and(|rel| d.rel.as_str() >= rel) {
                break;
            }
            self.next += 1;
            self.out.doc(DocMatches {
                rel: &d.rel,
                path: &d.path,
                printed: &d.printed,
                stats: &d.stats,
            })?;
        }
        Ok(())
    }
}

impl MatchSink for Merge<'_> {
    fn corpus(&mut self, docs: usize, bytes: u64) -> Result<(), String> {
        let (d, b) = self.part.corpus;
        self.out.corpus(docs + d, bytes + b)
    }

    fn term_start(&mut self, term: &str) -> Result<(), String> {
        self.term = Some(self.term.map_or(0, |t| t + 1));
        self.next = 0;
        self.out.term_start(term)
    }

    fn doc(&mut self, m: DocMatches<'_>) -> Result<(), String> {
        self.until(Some(m.rel))?;
        self.out.doc(m)
    }

    fn term_end(&mut self, report: &QueryReport) -> Result<(), String> {
        self.until(None)?;
        let local = self.term.and_then(|t| self.part.report.queries.get(t));
        match local {
            Some(local) => self.out.term_end(&sum(report, local)),
            None => self.out.term_end(report),
        }
    }
}

/// One term's report over both parts.
fn sum(server: &QueryReport, local: &QueryReport) -> QueryReport {
    QueryReport {
        term: server.term.clone(),
        latency_us: server.latency_us + local.latency_us,
        total_matches: server.total_matches + local.total_matches,
        docs_matched: server.docs_matched + local.docs_matched,
        counts_exact: server.counts_exact && local.counts_exact,
        leaves: server
            .leaves
            .iter()
            .zip(local.leaves.iter().map(Some).chain(std::iter::repeat(None)))
            .map(|(s, l)| crate::wire::LeafReport {
                term: s.term.clone(),
                total_matches: s.total_matches + l.map_or(0, |l| l.total_matches),
            })
            .collect(),
    }
}

/// The search's report over both parts: the server's index, both parts'
/// counts. It searched no file only when neither part did.
pub fn report(server: OutputReport, local: &OutputReport) -> OutputReport {
    let queries = server
        .queries
        .iter()
        .zip(
            local
                .queries
                .iter()
                .map(Some)
                .chain(std::iter::repeat(None)),
        )
        .map(|(s, l)| match l {
            Some(l) => sum(s, l),
            None => sum(s, &zero(s)),
        })
        .collect();
    OutputReport {
        queries,
        none_searched: server.none_searched && local.none_searched,
        ..server
    }
}

fn zero(like: &QueryReport) -> QueryReport {
    QueryReport {
        term: like.term.clone(),
        latency_us: 0,
        total_matches: 0,
        docs_matched: 0,
        counts_exact: true,
        leaves: Vec::new(),
    }
}

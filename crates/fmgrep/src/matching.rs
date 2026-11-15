//! Per-document search: grep-searcher finds what rg finds, grep-printer
//! prints it as rg does.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::{Arc, Mutex, PoisonError};

use grep_matcher::Matcher;
use grep_printer::{
    ColorSpecs, JSON, JSONBuilder, Standard, StandardBuilder, Stats, Summary, SummaryBuilder,
    SummaryKind,
};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{
    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkFinish, SinkMatch,
};
use termcolor::{Buffer, ColorSpec, WriteColor};

use crate::RegexError;
use crate::fetch::fetch_threads;
use crate::output::path_field;

/// Leading bytes rg's BOM sniffing removes before searching.
const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";

/// A document as rg searches it: without its leading UTF-8 BOM, if any.
pub fn strip_bom(raw: &[u8]) -> &[u8] {
    raw.strip_prefix(UTF8_BOM).unwrap_or(raw)
}

/// How a term matches: rg's line scan, or `-U` over whole documents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchMode {
    /// rg `-U`.
    pub multiline: bool,
    /// rg `--multiline-dotall`, read only under `multiline`.
    pub dotall: bool,
    /// rg `-i`.
    pub ignore_case: bool,
}

/// What the drive must produce per query; the CLI maps flags onto it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryOutput {
    /// Wire counts only: the harness path.
    Counts,
    /// Matched lines, printed.
    Matches(Print),
    /// rg `-c`.
    CountLines,
    /// rg `--count-matches`.
    CountMatches,
    /// rg `-l`.
    FilesWithMatches,
}

impl QueryOutput {
    /// Whether the printed lines carry the document's path.
    pub fn names_lines(self) -> bool {
        matches!(self, Self::Matches(p) if p.path || p.json)
    }
}

/// How matched lines print.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Print {
    pub before: usize,
    pub after: usize,
    pub line_numbers: bool,
    /// rg `-o`.
    pub only_matching: bool,
    pub color: bool,
    /// Every line starts with its path: rg `--no-heading` over several files.
    pub path: bool,
    /// rg `--json`.
    pub json: bool,
    /// rg `--stats`: count each match of every printed line.
    pub stats: bool,
}

/// rg's matcher for `pattern`, else for the literal; `None` for the empty
/// literal, which matches nothing.
pub fn matcher(
    literal: &str,
    pattern: Option<&str>,
    mode: MatchMode,
) -> Result<Option<RegexMatcher>, String> {
    let mut b = RegexMatcherBuilder::new();
    b.multi_line(true).case_insensitive(mode.ignore_case);
    if mode.multiline {
        b.dot_matches_new_line(mode.dotall);
    } else {
        b.line_terminator(Some(b'\n'));
    }
    match pattern {
        Some(p) => b
            .build(p)
            .map(Some)
            .map_err(|e| format!("error: {}", RegexError::new(p, e.to_string()))),
        None if literal.is_empty() => Ok(None),
        None => b
            .build(&regex::escape(literal))
            .map(Some)
            .map_err(|e| format!("error: invalid literal term {literal:?}: {e}")),
    }
}

/// Whether rg searches `m` line by line rather than over whole documents.
pub fn line_scan(m: &RegexMatcher, mode: MatchMode) -> bool {
    !mode.multiline || m.non_matching_bytes().is_some_and(|b| b.contains(b'\n'))
}

/// A matching document's printed bytes and the stats its printer kept.
pub struct Found {
    pub printed: Vec<u8>,
    /// rg's stats; none from the lines printer without `--stats` or `-o`.
    pub stats: Stats,
    /// What the report counts for it: rg's matches, the number `-c` or
    /// `--count-matches` printed, or the matched lines of a lines search
    /// without `--stats`; 0 under `-l`.
    pub count: u64,
}

/// One matcher searched over documents and printed as `output` asks, from
/// any number of threads.
pub struct DocSearch {
    matcher: RegexMatcher,
    output: QueryOutput,
    searcher: SearcherBuilder,
    /// One per fetch thread, and one for the calling thread.
    slots: Vec<Mutex<Option<Slot>>>,
}

/// A searcher, printer and matcher, reused across documents; the matcher's
/// own clone keeps its regex caches off the other threads' lock.
struct Slot {
    matcher: RegexMatcher,
    searcher: Searcher,
    printer: Printer,
    sep: Arc<AtomicU8>,
}

enum Printer {
    Lines(Standard<Prefixed>),
    Json(JSON<Vec<u8>>),
    Summary(Summary<Buffer>),
}

impl DocSearch {
    pub fn new(matcher: RegexMatcher, output: QueryOutput, mode: MatchMode) -> Self {
        let print = match output {
            QueryOutput::Matches(p) => p,
            _ => Print::default(),
        };
        let mut searcher = SearcherBuilder::new();
        searcher
            .line_number(print.line_numbers)
            .multi_line(mode.multiline)
            .before_context(print.before)
            .after_context(print.after)
            .binary_detection(BinaryDetection::none())
            .bom_sniffing(false);
        Self {
            matcher,
            output,
            searcher,
            slots: (0..=fetch_threads()).map(|_| Mutex::default()).collect(),
        }
    }

    fn slot(&self) -> Slot {
        let sep = Arc::default();
        let summary = |kind| {
            let mut b = SummaryBuilder::new();
            b.kind(kind).path(false).stats(false);
            Printer::Summary(b.build(Buffer::no_color()))
        };
        let printer = match self.output {
            QueryOutput::Matches(p) if p.json => {
                Printer::Json(JSONBuilder::new().build(Vec::new()))
            }
            QueryOutput::Matches(p) => Printer::Lines(
                StandardBuilder::new()
                    .color_specs(ColorSpecs::default_with_color())
                    .path(false)
                    .only_matching(p.only_matching)
                    // These counts find each match of each line again;
                    // `-o` finds them anyway.
                    .stats(p.stats || p.only_matching)
                    .build(Prefixed::new(p.color, Arc::clone(&sep))),
            ),
            QueryOutput::CountLines => summary(SummaryKind::Count),
            QueryOutput::CountMatches => summary(SummaryKind::CountMatches),
            QueryOutput::FilesWithMatches => summary(SummaryKind::QuietWithMatch),
            QueryOutput::Counts => unreachable!("counts mode never searches a document"),
        };
        Slot {
            matcher: self.matcher.clone(),
            searcher: self.searcher.build(),
            printer,
            sep,
        }
    }

    /// `doc`, BOM stripped, searched and printed under `path`; `None` when
    /// nothing matches.
    pub fn doc(&self, path: &str, doc: &[u8]) -> Result<Option<Found>, String> {
        let i = rayon::current_thread_index().map_or(0, |i| i + 1) % self.slots.len();
        let mut slot = self.slots[i].lock().unwrap_or_else(PoisonError::into_inner);
        let slot = slot.get_or_insert_with(|| self.slot());
        self.search(slot, path, doc)
            .map_err(|e| format!("error: {e}"))
    }

    fn search(&self, slot: &mut Slot, path: &str, doc: &[u8]) -> io::Result<Option<Found>> {
        let (m, searcher) = (&slot.matcher, &mut slot.searcher);
        let (matched, stats, printed, count) = match &mut slot.printer {
            Printer::Lines(p) => {
                let named = matches!(self.output, QueryOutput::Matches(Print { path: true, .. }));
                p.get_mut().start(named.then_some(path));
                let mut sink = Tagged {
                    inner: p.sink(m),
                    sep: &slot.sep,
                    multi_line: searcher.multi_line_with_matcher(m),
                    lines: 0,
                };
                searcher.search_slice(m, doc, &mut sink)?;
                let (matched, mut stats) = (sink.inner.has_match(), sink.inner.stats().cloned());
                let count = stats.as_ref().map_or(sink.lines, Stats::matches);
                let w = p.get_mut();
                // rg's bytes printed hold the path on each line.
                if let Some(s) = &mut stats {
                    s.add_bytes_printed(std::mem::take(&mut w.prefixed));
                }
                (matched, stats, w.take(), count)
            }
            Printer::Json(p) => {
                let mut sink = p.sink_with_path(m, path);
                searcher.search_slice(m, doc, &mut sink)?;
                let (matched, stats) = (sink.has_match(), sink.stats().clone());
                let count = stats.matches();
                (matched, Some(stats), std::mem::take(p.get_mut()), count)
            }
            Printer::Summary(p) => {
                let mut sink = p.sink(m);
                searcher.search_slice(m, doc, &mut sink)?;
                let (matched, stats) = (sink.has_match(), sink.stats().cloned());
                let buf = p.get_mut();
                let printed = buf.as_slice().to_vec();
                buf.clear();
                let stats = stats.or_else(|| counted(&printed));
                let count = stats.as_ref().map_or(0, Stats::matches);
                (matched, stats, printed, count)
            }
        };
        Ok(matched.then(|| Found {
            printed,
            stats: stats.unwrap_or_default(),
            count,
        }))
    }
}

/// `-c`'s stats: the count it printed, as matched lines and matches.
fn counted(printed: &[u8]) -> Option<Stats> {
    let n = std::str::from_utf8(printed).ok()?.trim_end().parse().ok()?;
    let mut stats = Stats::new();
    stats.add_matched_lines(n);
    stats.add_matches(n);
    Some(stats)
}

/// The lines printer's writer. It starts every line but a `--` with rg's
/// `path:` or `path-`, since grep-printer resolves the real path of every
/// file it names.
struct Prefixed {
    buf: Buffer,
    path: Vec<u8>,
    /// The path's bytes without colour codes.
    path_len: usize,
    /// `:` on matched lines, `-` on context lines, 0 on a `--`.
    sep: Arc<AtomicU8>,
    fresh: bool,
    /// Bytes of the paths and separators written, without colour codes.
    prefixed: u64,
}

impl Prefixed {
    fn new(color: bool, sep: Arc<AtomicU8>) -> Self {
        Self {
            buf: if color {
                Buffer::ansi()
            } else {
                Buffer::no_color()
            },
            path: Vec::new(),
            path_len: 0,
            sep,
            fresh: true,
            prefixed: 0,
        }
    }

    fn start(&mut self, path: Option<&str>) {
        self.path.clear();
        self.path_len = path.map_or(0, str::len);
        if let Some(p) = path {
            path_field(&mut self.path, p, self.buf.supports_color());
        }
        self.fresh = true;
        self.prefixed = 0;
    }

    fn take(&mut self) -> Vec<u8> {
        let printed = self.buf.as_slice().to_vec();
        self.buf.clear();
        printed
    }

    fn prefix(&mut self) -> io::Result<()> {
        if !std::mem::take(&mut self.fresh) || self.path.is_empty() {
            return Ok(());
        }
        match self.sep.load(Relaxed) {
            0 => Ok(()),
            sep => {
                self.prefixed += self.path_len as u64 + 1;
                self.buf.write_all(&self.path)?;
                self.buf.write_all(&[sep])
            }
        }
    }
}

impl Write for Prefixed {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for line in bytes.split_inclusive(|&b| b == b'\n') {
            self.prefix()?;
            self.buf.write_all(line)?;
            self.fresh = line.ends_with(b"\n");
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl WriteColor for Prefixed {
    fn supports_color(&self) -> bool {
        self.buf.supports_color()
    }

    fn set_color(&mut self, spec: &ColorSpec) -> io::Result<()> {
        self.prefix()?;
        self.buf.set_color(spec)
    }

    fn reset(&mut self) -> io::Result<()> {
        self.buf.reset()
    }
}

/// Tells [`Prefixed`] what kind of line each call prints, and counts the
/// matched lines.
struct Tagged<'a, S> {
    inner: S,
    sep: &'a AtomicU8,
    /// A call may hold several matched lines.
    multi_line: bool,
    lines: u64,
}

impl<S: Sink<Error = io::Error>> Sink for Tagged<'_, S> {
    type Error = io::Error;

    fn matched(&mut self, s: &Searcher, m: &SinkMatch<'_>) -> io::Result<bool> {
        self.sep.store(b':', Relaxed);
        self.lines += match self.multi_line {
            true => m.lines().count() as u64,
            false => 1,
        };
        self.inner.matched(s, m)
    }

    fn context(&mut self, s: &Searcher, c: &SinkContext<'_>) -> io::Result<bool> {
        self.sep.store(b'-', Relaxed);
        self.inner.context(s, c)
    }

    fn context_break(&mut self, s: &Searcher) -> io::Result<bool> {
        self.sep.store(0, Relaxed);
        self.inner.context_break(s)
    }

    fn begin(&mut self, s: &Searcher) -> io::Result<bool> {
        self.inner.begin(s)
    }

    fn finish(&mut self, s: &Searcher, f: &SinkFinish) -> io::Result<()> {
        self.inner.finish(s, f)
    }
}

/// Whether `term` has a non-empty proper border, i.e. can overlap itself. A
/// term that cannot overlap itself has the same overlapping and
/// non-overlapping occurrence count, so rg's `--count-matches` can be served
/// from the literal probe with no fetch.
fn self_overlapping(term: &[u8]) -> bool {
    let n = term.len();
    if n < 2 {
        return false;
    }
    let mut fail = vec![0usize; n];
    let mut k = 0usize;
    for i in 1..n {
        while k > 0 && term[i] != term[k] {
            k = fail[k - 1];
        }
        if term[i] == term[k] {
            k += 1;
        }
        fail[i] = k;
    }
    fail[n - 1] > 0
}

/// Whether a literal's FM document-wide occurrence count equals ripgrep's:
/// no `\n` means no match crosses a line, and no self-overlap means the
/// overlapping count already is the non-overlapping one.
pub fn literal_counts_are_exact(term: &[u8]) -> bool {
    !term.is_empty() && !term.contains(&b'\n') && !self_overlapping(term)
}

#[cfg(test)]
mod tests;

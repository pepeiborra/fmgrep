//! rg's human output around each document's printed bytes: banners,
//! headings, file separators, and the count and `-l` lines.

use std::io::Write;
use std::time::Instant;

pub mod writer;

use crate::sink::{DocMatches, MatchSink};
use crate::wire::QueryReport;
use writer::Pipe;

/// Prefix of every IO failure raised while printing, so [`crate::run`] keeps
/// a write failure at exit 1 instead of the argv exit 2.
pub const WRITE_FAILED: &str = "error: failed to write";

/// What each matching document prints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// Its lines, as printed: rg's default and `-o`.
    Lines,
    /// `path:` and the printed count: rg `-c` and `--count-matches`.
    Count,
    /// rg `-l`.
    FilesWithMatches,
}

#[derive(Clone, Copy, Debug)]
pub struct OutputConfig {
    pub format: Format,
    pub heading: bool,
    /// Context lines are on: files are separated by `--` without headings.
    pub context: bool,
    pub color: bool,
    /// Print a `== term ==` banner: set when the query has several terms.
    pub banner: bool,
    /// False: rg's one file PATH, printed without its path but under `-l`.
    pub filename: bool,
    /// rg `--stats`: its statistics after each term's lines.
    pub stats: bool,
}

/// `path` in rg's path colour.
pub fn path_field(out: &mut Vec<u8>, path: &str, color: bool) {
    if color {
        out.extend_from_slice(b"\x1b[0m\x1b[35m");
    }
    out.extend_from_slice(path.as_bytes());
    if color {
        out.extend_from_slice(b"\x1b[0m");
    }
}

/// Streams one query's matches in rg's formats.
pub struct MatchWriter<W: Write> {
    pipe: Pipe<W>,
    cfg: OutputConfig,
    /// Anything written for this term yet.
    wrote: bool,
    terms: usize,
    field: Vec<u8>,
    /// `(searches, bytes_searched)`: the files the search keeps.
    corpus: (u64, u64),
    tally: Tally,
    /// When the query started, for rg's total time.
    started: Instant,
}

/// One term's numbers for `--stats`, counted as rg counts them.
#[derive(Default)]
struct Tally {
    matches: u64,
    matched_lines: u64,
    docs: u64,
    /// Every byte printed for the term but colour codes and the banner.
    bytes_printed: u64,
}

impl<W: Write> MatchWriter<W> {
    pub fn new(out: W, cfg: OutputConfig) -> Self {
        Self {
            pipe: Pipe::new(out),
            cfg,
            wrote: false,
            terms: 0,
            field: Vec::new(),
            corpus: (0, 0),
            tally: Tally::default(),
            started: Instant::now(),
        }
    }

    pub fn finish(&mut self) -> Result<(), String> {
        self.pipe.finish()
    }

    fn path(&mut self, path: &str, then: &[u8]) -> Result<(), String> {
        self.field.clear();
        path_field(&mut self.field, path, self.cfg.color);
        self.field.extend_from_slice(then);
        self.pipe.raw(&self.field)
    }
}

impl<W: Write> MatchSink for MatchWriter<W> {
    fn corpus(&mut self, docs: usize, bytes: u64) -> Result<(), String> {
        self.corpus = (docs as u64, bytes);
        Ok(())
    }

    fn term_start(&mut self, term: &str) -> Result<(), String> {
        self.wrote = false;
        self.tally = Tally::default();
        if self.cfg.banner {
            if self.terms > 0 {
                self.pipe.raw(b"\n")?;
            }
            self.pipe.raw(format!("== {term} ==\n").as_bytes())?;
        }
        self.terms += 1;
        Ok(())
    }

    fn doc(&mut self, m: DocMatches<'_>) -> Result<(), String> {
        match self.cfg.format {
            Format::FilesWithMatches => self.path(m.path, b"\n")?,
            Format::Count if self.cfg.filename => self.path(m.path, b":")?,
            Format::Count => {}
            Format::Lines if self.cfg.heading && self.cfg.filename => {
                if self.wrote {
                    self.pipe.raw(b"\n")?;
                    self.tally.bytes_printed += 1;
                }
                self.path(m.path, b"\n")?;
                self.tally.bytes_printed += m.path.len() as u64 + 1;
            }
            Format::Lines if self.cfg.context && self.wrote => {
                self.pipe.raw(b"--\n")?;
                self.tally.bytes_printed += 3;
            }
            Format::Lines => {}
        }
        self.wrote = true;
        self.tally.matches += m.stats.matches();
        self.tally.matched_lines += m.stats.matched_lines();
        self.tally.docs += 1;
        self.tally.bytes_printed += m.stats.bytes_printed();
        self.pipe.raw(m.printed)
    }

    /// Under `--stats`, rg's statistics. Files and bytes searched are the
    /// indexed files the search keeps; the total time runs from the start
    /// of the query.
    fn term_end(&mut self, report: &QueryReport) -> Result<(), String> {
        if !self.cfg.stats {
            return Ok(());
        }
        let t = &self.tally;
        let block = format!(
            "\n{} matches\n{} matched lines\n{} files contained matches\n{} files searched\n{} bytes printed\n{} bytes searched\n{:.6} seconds spent searching\n{:.6} seconds total\n",
            t.matches,
            t.matched_lines,
            t.docs,
            self.corpus.0,
            t.bytes_printed,
            self.corpus.1,
            report.latency_us as f64 / 1e6,
            self.started.elapsed().as_secs_f64(),
        );
        self.pipe.raw(block.as_bytes())
    }
}

#[cfg(test)]
mod tests;

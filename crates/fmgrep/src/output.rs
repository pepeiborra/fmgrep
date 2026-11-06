//! rg's human output around each document's printed bytes: banners,
//! headings, file separators, and the count and `-l` lines.

use std::io::Write;

pub mod writer;

use crate::sink::{DocMatches, MatchSink};
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
}

impl<W: Write> MatchWriter<W> {
    pub fn new(out: W, cfg: OutputConfig) -> Self {
        Self {
            pipe: Pipe::new(out),
            cfg,
            wrote: false,
            terms: 0,
            field: Vec::new(),
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
    fn term_start(&mut self, term: &str) -> Result<(), String> {
        self.wrote = false;
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
                }
                self.path(m.path, b"\n")?;
            }
            Format::Lines if self.cfg.context && self.wrote => self.pipe.raw(b"--\n")?,
            Format::Lines => {}
        }
        self.wrote = true;
        self.pipe.raw(m.printed)
    }
}

#[cfg(test)]
mod tests;

//! ripgrep-format match output, byte for byte rg 15.2.0's; `output/tests.rs`
//! carries the rg command behind every expectation.

use std::io::Write;

pub mod writer;

use crate::matching::LineHit;
use crate::sink::{DocMatches, MatchSink};
use writer::Pipe;

/// Prefix of every IO failure raised while printing, so [`crate::run`] keeps
/// a write failure at exit 1 instead of the argv exit 2.
pub const WRITE_FAILED: &str = "error: failed to write";

/// What each matching document prints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// Matched lines (rg default) and any context lines.
    Lines,
    /// rg `-o`: one line per match, matched bytes only.
    OnlyMatching,
    /// rg `-c`.
    CountLines,
    /// rg `--count-matches`.
    CountMatches,
    /// rg `-l`.
    FilesWithMatches,
}

#[derive(Clone, Copy, Debug)]
pub struct OutputConfig {
    pub format: Format,
    pub heading: bool,
    pub line_numbers: bool,
    /// `before > 0 || after > 0`: gates the `--` separator, which rg omits
    /// under `-C0`.
    pub context: bool,
    /// rg `-U`, which only changes `-o`: see [`MatchWriter::only_matching`].
    pub multiline: bool,
    pub color: bool,
    /// Print a `== term ==` banner: set when the query has several terms.
    pub banner: bool,
}

/// rg's default palette, raw ANSI. Every field opens with a reset, as rg
/// emits; separators and counts are uncoloured.
const PATH: &str = "\x1b[0m\x1b[35m";
const LINE_NO: &str = "\x1b[0m\x1b[32m";
const MATCHED: &str = "\x1b[0m\x1b[1m\x1b[31m";
const RESET: &str = "\x1b[0m";

/// Streams one query's matches in rg's formats.
///
/// Relies on the [`MatchSink`] contract that each document arrives exactly
/// once, ascending by path: every `doc` call is a new file, so no
/// current-file state is needed.
pub struct MatchWriter<W: Write> {
    pipe: Pipe<W>,
    cfg: OutputConfig,
    /// Anything written for this term yet: gates the blank line before a
    /// heading and the `--` before a new file in no-heading context mode.
    wrote: bool,
    /// Previous printed line number of the current document.
    last_line: Option<u64>,
    terms: usize,
}

impl<W: Write> MatchWriter<W> {
    pub fn new(out: W, cfg: OutputConfig) -> Self {
        Self {
            pipe: Pipe::new(out),
            cfg,
            wrote: false,
            last_line: None,
            terms: 0,
        }
    }

    pub fn finish(&mut self) -> Result<(), String> {
        self.pipe.finish()
    }

    fn raw(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.pipe.raw(bytes)
    }

    fn field(&mut self, bytes: &[u8], color: &str) -> Result<(), String> {
        if !self.cfg.color {
            return self.raw(bytes);
        }
        self.raw(color.as_bytes())?;
        self.raw(bytes)?;
        self.raw(RESET.as_bytes())
    }

    /// rg `-l` (no count) and `-c` / `--count-matches`.
    fn count_line(&mut self, path: &str, count: Option<u64>) -> Result<(), String> {
        self.field(path.as_bytes(), PATH)?;
        if let Some(n) = count {
            self.raw(b":")?;
            self.raw(n.to_string().as_bytes())?;
        }
        self.raw(b"\n")
    }

    fn heading(&mut self, path: &str) -> Result<(), String> {
        if self.wrote {
            self.raw(b"\n")?;
        }
        self.field(path.as_bytes(), PATH)?;
        self.raw(b"\n")
    }

    /// The `path:`/`N:` fields of one output line; `-` separators on a
    /// context line.
    fn prefix(&mut self, path: &str, line: u64, matched: bool) -> Result<(), String> {
        let sep: &[u8] = if matched { b":" } else { b"-" };
        if !self.cfg.heading {
            self.field(path.as_bytes(), PATH)?;
            self.raw(sep)?;
        }
        if self.cfg.line_numbers {
            self.field(line.to_string().as_bytes(), LINE_NO)?;
            self.raw(sep)?;
        }
        Ok(())
    }

    fn body(&mut self, raw: &[u8], hit: &LineHit) -> Result<(), String> {
        let line = &raw[hit.start..hit.end];
        if !self.cfg.color || hit.spans.is_empty() {
            return self.raw(line);
        }
        let mut at = 0usize;
        for &(off, len) in &hit.spans {
            // Clamped: no span may reach behind the last one or past the line.
            let off = (off as usize).max(at).min(line.len());
            let end = (off + len as usize).min(line.len());
            self.raw(&line[at..off])?;
            if end > off {
                self.field(&line[off..end], MATCHED)?;
            }
            at = end;
        }
        self.raw(&line[at..])
    }

    /// rg `-o`: one output line per span, the matched bytes alone. A line
    /// with no spans — context, or a spanless match — prints whole, except
    /// under `-U`, where a matched line the match only reaches through its
    /// terminator (and a zero-width match anywhere) prints nothing at all.
    fn only_matching(&mut self, path: &str, raw: &[u8], hit: &LineHit) -> Result<(), String> {
        let line = &raw[hit.start..hit.end];
        if hit.spans.is_empty() {
            if self.cfg.multiline && hit.matched {
                return Ok(());
            }
            self.prefix(path, hit.line, hit.matched)?;
            self.raw(line)?;
            return self.raw(b"\n");
        }
        for &(off, len) in &hit.spans {
            let off = (off as usize).min(line.len());
            let end = (off + len as usize).min(line.len());
            self.prefix(path, hit.line, true)?;
            self.field(&line[off..end], MATCHED)?;
            self.raw(b"\n")?;
        }
        Ok(())
    }
}

impl<W: Write> MatchSink for MatchWriter<W> {
    fn term_start(&mut self, term: &str) -> Result<(), String> {
        self.last_line = None;
        self.wrote = false;
        if self.cfg.banner {
            if self.terms > 0 {
                self.raw(b"\n")?;
            }
            self.raw(format!("== {term} ==\n").as_bytes())?;
        }
        self.terms += 1;
        Ok(())
    }

    fn doc(&mut self, m: DocMatches<'_>) -> Result<(), String> {
        match self.cfg.format {
            Format::FilesWithMatches => return self.count_line(m.path, None),
            Format::CountLines => return self.count_line(m.path, Some(m.outcome.count_lines())),
            Format::CountMatches => {
                return self.count_line(m.path, Some(m.outcome.count_matches()));
            }
            Format::Lines | Format::OnlyMatching => {}
        }
        if self.cfg.heading {
            self.heading(m.path)?;
        } else if self.cfg.context && self.wrote {
            // rg separates files too once context is on and headings are off.
            self.raw(b"--\n")?;
        }
        self.last_line = None;
        for hit in &m.outcome.lines {
            if self.cfg.context && self.last_line.is_some_and(|p| p + 1 != hit.line) {
                self.raw(b"--\n")?;
            }
            if self.cfg.format == Format::OnlyMatching {
                self.only_matching(m.path, m.raw, hit)?;
            } else {
                self.prefix(m.path, hit.line, hit.matched)?;
                self.body(m.raw, hit)?;
                self.raw(b"\n")?;
            }
            self.last_line = Some(hit.line);
        }
        self.wrote = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests;

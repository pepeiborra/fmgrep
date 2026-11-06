//! rg's `--json` stream: each document's events as grep-printer prints
//! them, then one summary per term over the whole index.

use std::io::Write;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::output::writer::Pipe;
use crate::sink::{DocMatches, MatchSink};
use crate::wire::QueryReport;

/// Streams one rg event stream per term, in argv order, to `out`.
pub struct JsonSink<W: Write> {
    pipe: Pipe<W>,
    /// `(searches, bytes_searched)` of a full scan of the index.
    corpus: (u64, u64),
    term: TermStats,
    started: Instant,
}

#[derive(Default)]
struct TermStats {
    bytes_printed: u64,
    matched_lines: u64,
    matches: u64,
    searches_with_match: u64,
}

impl<W: Write> JsonSink<W> {
    pub fn new(out: W) -> Self {
        Self {
            pipe: Pipe::new(out),
            corpus: (0, 0),
            term: TermStats::default(),
            started: Instant::now(),
        }
    }

    pub fn finish(&mut self) -> Result<(), String> {
        self.pipe.finish()
    }
}

impl<W: Write> MatchSink for JsonSink<W> {
    fn corpus(&mut self, docs: usize, bytes: u64) -> Result<(), String> {
        self.corpus = (docs as u64, bytes);
        Ok(())
    }

    fn term_start(&mut self, _: &str) -> Result<(), String> {
        self.term = TermStats::default();
        self.started = Instant::now();
        Ok(())
    }

    fn doc(&mut self, m: DocMatches<'_>) -> Result<(), String> {
        self.term.bytes_printed += m.stats.bytes_printed();
        self.term.matched_lines += m.stats.matched_lines();
        self.term.matches += m.stats.matches();
        self.term.searches_with_match += 1;
        self.pipe.raw(m.printed)
    }

    /// rg's summary; a `json!` map sorts its keys, as rg's does.
    fn term_end(&mut self, report: &QueryReport) -> Result<(), String> {
        let summary = json!({
            "data": {
                "elapsed_total": elapsed(self.started.elapsed()),
                "stats": {
                    "bytes_printed": self.term.bytes_printed,
                    "bytes_searched": self.corpus.1,
                    "elapsed": elapsed(Duration::from_micros(report.latency_us)),
                    "matched_lines": self.term.matched_lines,
                    "matches": self.term.matches,
                    "searches": self.corpus.0,
                    "searches_with_match": self.term.searches_with_match,
                },
            },
            "type": "summary",
        });
        self.pipe.raw(format!("{summary}\n").as_bytes())
    }
}

fn elapsed(d: Duration) -> Value {
    json!({
        "human": format!("{:.6}s", d.as_secs_f64()),
        "nanos": d.subsec_nanos(),
        "secs": d.as_secs(),
    })
}

#[cfg(test)]
mod tests;

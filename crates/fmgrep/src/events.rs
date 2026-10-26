//! rg's `--json` event stream, one JSON object per line, byte-exact against rg
//! 15.2.0. `events/tests.rs` carries the rg command behind every expectation;
//! `docs/protocol.md` lists the deviations.

use std::io::Write;
use std::time::{Duration, Instant};

mod schema;

use crate::matching::LineHit;
use crate::output::writer::Pipe;
use crate::sink::{DocMatches, MatchSink};
use crate::wire::QueryReport;
use schema::{
    Begin, Data, Elapsed, End, Event, Line, SortedElapsed, SortedStats, Stats, SubMatch, Summary,
    SummaryEvent,
};

pub struct JsonConfig {
    /// rg `-N` nulls `line_number` instead of dropping it.
    pub line_numbers: bool,
    /// rg `-U`, where one `match` event covers a whole BLOCK of consecutive
    /// matched lines rather than one line.
    pub multiline: bool,
}

/// Streams one rg event stream per term, in argv order, to `out`.
pub struct JsonSink<W: Write> {
    pipe: Pipe<W>,
    cfg: JsonConfig,
    /// `(searches, bytes_searched)`: a full scan of the index, which is what
    /// rg over the indexed file list would report.
    corpus: (u64, u64),
    term: TermStats,
    started: Instant,
    /// This term's previous path: the ascending contract.
    #[cfg(debug_assertions)]
    last: Option<String>,
}

#[derive(Default)]
struct TermStats {
    bytes_printed: u64,
    matched_lines: u64,
    matches: u64,
    searches_with_match: u64,
}

impl<W: Write> JsonSink<W> {
    pub fn new(out: W, cfg: JsonConfig) -> Self {
        Self {
            pipe: Pipe::new(out),
            cfg,
            corpus: (0, 0),
            term: TermStats::default(),
            started: Instant::now(),
            #[cfg(debug_assertions)]
            last: None,
        }
    }

    pub fn finish(&mut self) -> Result<(), String> {
        self.pipe.finish()
    }

    fn emit<T: serde::Serialize>(&mut self, event: &T) -> Result<(), String> {
        let mut line =
            serde_json::to_vec(event).map_err(|e| format!("error: failed to encode event: {e}"))?;
        line.push(b'\n');
        self.pipe.raw(&line)
    }
}

/// End of a line's content as rg reports it: the terminator belongs to it.
fn line_end(raw: &[u8], hit: &LineHit) -> usize {
    if raw.get(hit.end) == Some(&b'\n') {
        hit.end + 1
    } else {
        hit.end
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
        #[cfg(debug_assertions)]
        {
            self.last = None;
        }
        Ok(())
    }

    fn doc(&mut self, m: DocMatches<'_>) -> Result<(), String> {
        #[cfg(debug_assertions)]
        {
            debug_assert!(
                self.last.as_deref().is_none_or(|p| p < m.path),
                "paths must ascend"
            );
            self.last = Some(m.path.to_owned());
        }
        let at = self.pipe.written();
        self.emit(&Event::Begin(Begin {
            path: Data::Text(m.path),
        }))?;
        let hits = &m.outcome.lines;
        // Under `-U` the event unit is a BLOCK: the longest run of
        // consecutive matched lines, whose submatches are the whole matches
        // and may hold terminators. A context line is always its own event,
        // and on the line path so is every matched line.
        let (mut idx, mut span) = (0usize, 0usize);
        while idx < hits.len() {
            let head = &hits[idx];
            let mut tail = idx;
            if self.cfg.multiline && head.matched {
                while tail + 1 < hits.len()
                    && hits[tail + 1].matched
                    && hits[tail + 1].line == hits[tail].line + 1
                {
                    tail += 1;
                }
            }
            let (start, end) = (head.start, line_end(m.raw, &hits[tail]));
            let text = &m.raw[start..end];
            let submatches = if self.cfg.multiline {
                let mut subs = Vec::new();
                while let Some(&(s, e)) = m.outcome.spans.get(span).filter(|&&(s, _)| s < end) {
                    subs.push(SubMatch {
                        text: Data::new(&text[s - start..e - start]),
                        start: (s - start) as u64,
                        end: (e - start) as u64,
                    });
                    span += 1;
                }
                subs
            } else {
                head.spans
                    .iter()
                    .map(|&(off, len)| SubMatch {
                        text: Data::new(&text[off as usize..(off + len) as usize]),
                        start: u64::from(off),
                        end: u64::from(off + len),
                    })
                    .collect()
            };
            let line = Line {
                path: Data::Text(m.path),
                lines: Data::new(text),
                line_number: self.cfg.line_numbers.then_some(head.line),
                absolute_offset: start as u64,
                submatches,
            };
            let event = if head.matched {
                Event::Match(line)
            } else {
                Event::Context(line)
            };
            self.emit(&event)?;
            idx = tail + 1;
        }
        // rg's `bytes_printed` for a file excludes the `end` event itself.
        let bytes_printed = self.pipe.written() - at;
        self.emit(&Event::End(End {
            path: Data::Text(m.path),
            binary_offset: None,
            stats: Stats {
                elapsed: Elapsed::new(Duration::ZERO),
                searches: 1,
                searches_with_match: 1,
                bytes_searched: m.raw.len() as u64,
                bytes_printed,
                matched_lines: m.outcome.matched_lines,
                matches: m.outcome.matched_spans,
            },
        }))?;
        self.term.bytes_printed += bytes_printed;
        self.term.matched_lines += m.outcome.matched_lines;
        self.term.matches += m.outcome.matched_spans;
        self.term.searches_with_match += 1;
        Ok(())
    }

    fn term_end(&mut self, report: &QueryReport) -> Result<(), String> {
        let summary = SummaryEvent {
            data: Summary {
                elapsed_total: SortedElapsed::new(self.started.elapsed()),
                stats: SortedStats {
                    bytes_printed: self.term.bytes_printed,
                    bytes_searched: self.corpus.1,
                    elapsed: SortedElapsed::new(Duration::from_micros(report.latency_us)),
                    matched_lines: self.term.matched_lines,
                    matches: self.term.matches,
                    searches: self.corpus.0,
                    searches_with_match: self.term.searches_with_match,
                },
            },
            kind: "summary",
        };
        self.emit(&summary)
    }
}

#[cfg(test)]
mod tests;

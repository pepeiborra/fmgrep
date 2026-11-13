//! `per_file` frames: one frame per output event of a served query, so
//! that the client prints them through its own writer, and can put files
//! of its own between them in path order.
//!
//! A frame's payload is one JSON object on one line, then, for a matching
//! file, the bytes it prints:
//!
//! - `{"corpus":[docs,bytes]}`: the shape of a full scan of the index;
//! - `{"term":"<term>"}`: a term starts;
//! - `{"doc":"<root-relative path>","path":"<printed path>","stats":[matches,matched_lines,bytes_printed]}`
//!   then the printed bytes;
//! - `{"end":<QueryReport>}`: the term ends.

use std::io::Write;

use grep_printer::Stats;
use serde::{Deserialize, Serialize};

use crate::output::WRITE_FAILED;
use crate::sink::{DocMatches, MatchSink};
use crate::wire::QueryReport;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Event {
    Corpus(usize, u64),
    Term(String),
    End(QueryReport),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Doc {
    doc: String,
    path: String,
    stats: [u64; 3],
}

/// Writes each event of a query as one frame, `<len>\n<len bytes>`.
pub struct FileFrames<'a> {
    out: &'a mut dyn Write,
    buf: Vec<u8>,
}

impl<'a> FileFrames<'a> {
    pub fn new(out: &'a mut dyn Write) -> Self {
        Self {
            out,
            buf: Vec::new(),
        }
    }

    fn send(&mut self) -> Result<(), String> {
        let head = format!("{}\n", self.buf.len());
        let sent =
            (self.out.write_all(head.as_bytes())).and_then(|()| self.out.write_all(&self.buf));
        self.buf.clear();
        sent.map_err(|e| format!("{WRITE_FAILED} matches: {e}"))
    }

    fn event(&mut self, e: &impl Serialize) -> Result<(), String> {
        serde_json::to_writer(&mut self.buf, e)
            .map_err(|e| format!("error: failed to encode a frame: {e}"))?;
        self.buf.push(b'\n');
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), String> {
        self.out
            .flush()
            .map_err(|e| format!("{WRITE_FAILED} matches: {e}"))
    }
}

impl MatchSink for FileFrames<'_> {
    fn corpus(&mut self, docs: usize, bytes: u64) -> Result<(), String> {
        self.event(&Event::Corpus(docs, bytes))?;
        self.send()
    }

    fn term_start(&mut self, term: &str) -> Result<(), String> {
        self.event(&Event::Term(term.to_owned()))?;
        self.send()
    }

    fn doc(&mut self, m: DocMatches<'_>) -> Result<(), String> {
        let s = m.stats;
        self.event(&Doc {
            doc: m.rel.to_owned(),
            path: m.path.to_owned(),
            stats: [s.matches(), s.matched_lines(), s.bytes_printed()],
        })?;
        self.buf.extend_from_slice(m.printed);
        self.send()
    }

    fn term_end(&mut self, report: &QueryReport) -> Result<(), String> {
        self.event(&serde_json::json!({ "end": report }))?;
        self.send()
    }
}

/// One frame, decoded.
pub enum Frame<'a> {
    Corpus(usize, u64),
    Term(String),
    Doc {
        rel: String,
        path: String,
        stats: Stats,
        printed: &'a [u8],
    },
    End(QueryReport),
}

impl<'a> Frame<'a> {
    pub fn decode(frame: &'a [u8]) -> Result<Self, String> {
        let bad = |e: String| format!("error: the server sent a malformed frame ({e})");
        let end = memchr::memchr(b'\n', frame).ok_or_else(|| bad("no header line".into()))?;
        let (head, printed) = (&frame[..end], &frame[end + 1..]);
        if head.starts_with(b"{\"doc\"") {
            let d: Doc = serde_json::from_slice(head).map_err(|e| bad(e.to_string()))?;
            let mut stats = Stats::new();
            stats.add_matches(d.stats[0]);
            stats.add_matched_lines(d.stats[1]);
            stats.add_bytes_printed(d.stats[2]);
            return Ok(Frame::Doc {
                rel: d.doc,
                path: d.path,
                stats,
                printed,
            });
        }
        let e: Event = serde_json::from_slice(head).map_err(|e| bad(e.to_string()))?;
        Ok(match e {
            Event::Corpus(docs, bytes) => Frame::Corpus(docs, bytes),
            Event::Term(t) => Frame::Term(t),
            Event::End(r) => Frame::End(r),
        })
    }

    /// The event, to `sink`.
    pub fn replay(self, sink: &mut dyn MatchSink) -> Result<(), String> {
        match self {
            Frame::Corpus(docs, bytes) => sink.corpus(docs, bytes),
            Frame::Term(t) => sink.term_start(&t),
            Frame::Doc {
                rel,
                path,
                stats,
                printed,
            } => sink.doc(DocMatches {
                rel: &rel,
                path: &path,
                printed,
                stats: &stats,
            }),
            Frame::End(r) => sink.term_end(&r),
        }
    }
}

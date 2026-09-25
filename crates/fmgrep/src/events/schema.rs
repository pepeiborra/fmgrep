//! rg 15.2.0's `--json` schema; declaration order is the byte format. rg
//! serializes `begin`/`match`/`context`/`end` from structs (`type` first) but
//! `summary` through a sorted map: its keys, nested ones included, are
//! alphabetical and `data` precedes `type`, hence the two `Elapsed` shapes.

use std::time::Duration;

use serde::Serialize;

#[derive(Serialize)]
#[serde(tag = "type", content = "data", rename_all = "lowercase")]
pub enum Event<'a> {
    Begin(Begin<'a>),
    Match(Line<'a>),
    Context(Line<'a>),
    End(End<'a>),
}

#[derive(Serialize)]
pub struct Begin<'a> {
    pub path: Data<'a>,
}

#[derive(Serialize)]
pub struct Line<'a> {
    pub path: Data<'a>,
    /// Content including the line terminator, as rg emits it.
    pub lines: Data<'a>,
    /// `null` under `-N`.
    pub line_number: Option<u64>,
    pub absolute_offset: u64,
    /// Empty on a context line.
    pub submatches: Vec<SubMatch<'a>>,
}

#[derive(Serialize)]
pub struct SubMatch<'a> {
    #[serde(rename = "match")]
    pub text: Data<'a>,
    /// Byte offsets within `lines`.
    pub start: u64,
    pub end: u64,
}

#[derive(Serialize)]
pub struct End<'a> {
    pub path: Data<'a>,
    pub binary_offset: Option<u64>,
    pub stats: Stats,
}

#[derive(Serialize)]
pub struct Stats {
    pub elapsed: Elapsed,
    pub searches: u64,
    pub searches_with_match: u64,
    pub bytes_searched: u64,
    pub bytes_printed: u64,
    pub matched_lines: u64,
    pub matches: u64,
}

/// The one event rg emits through a sorted map.
#[derive(Serialize)]
pub struct SummaryEvent {
    pub data: Summary,
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Serialize)]
pub struct Summary {
    pub elapsed_total: SortedElapsed,
    pub stats: SortedStats,
}

#[derive(Serialize)]
pub struct SortedStats {
    pub bytes_printed: u64,
    pub bytes_searched: u64,
    pub elapsed: SortedElapsed,
    pub matched_lines: u64,
    pub matches: u64,
    pub searches: u64,
    pub searches_with_match: u64,
}

#[derive(Serialize)]
pub struct Elapsed {
    pub secs: u64,
    pub nanos: u32,
    pub human: String,
}

#[derive(Serialize)]
pub struct SortedElapsed {
    pub human: String,
    pub nanos: u32,
    pub secs: u64,
}

fn human(d: Duration) -> String {
    format!("{:.6}s", d.as_secs_f64())
}

impl Elapsed {
    pub fn new(d: Duration) -> Self {
        Self {
            secs: d.as_secs(),
            nanos: d.subsec_nanos(),
            human: human(d),
        }
    }
}

impl SortedElapsed {
    pub fn new(d: Duration) -> Self {
        Self {
            human: human(d),
            nanos: d.subsec_nanos(),
            secs: d.as_secs(),
        }
    }
}

/// rg's bytes union: UTF-8 verbatim, anything else standard base64.
#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Data<'a> {
    Text(&'a str),
    Bytes(String),
}

impl<'a> Data<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(s) => Self::Text(s),
            Err(_) => Self::Bytes(base64(bytes)),
        }
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// RFC 4648 standard alphabet, padded: what rg's serializer emits.
pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

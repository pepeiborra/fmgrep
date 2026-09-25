//! Match extraction with ripgrep's semantics. Each scan runs over one line's
//! content (terminator excluded, `\r` kept), so no match crosses a terminator,
//! `^ $ \A \z` anchor at line boundaries and a `\n` inside a class never
//! matches; under [`MatchMode::multiline`] a term that can match a terminator
//! scans the whole document instead ([`can_match_newline`]). A zero-width
//! match at an unterminated EOF matches its line but yields no span.

use memchr::memmem;

use crate::RegexError;

/// Leading bytes rg's BOM sniffing removes before searching.
const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";

/// A document as rg searches it: without its leading UTF-8 BOM, if any. Every
/// scan runs over this view, so line 1, printed text, offsets, columns and
/// `bytes_searched` follow rg's; the stored bytes keep the BOM.
pub fn strip_bom(raw: &[u8]) -> &[u8] {
    raw.strip_prefix(UTF8_BOM).unwrap_or(raw)
}

/// rg's wording for a pattern that can only match a line terminator.
const NEWLINE_BANNED: &str = "the literal \"\\n\" is not allowed in a regex";

/// How a term matches: rg's line scan, or `-U` over whole documents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchMode {
    /// rg `-U`: a match may cross line terminators, and a `\n` in the term
    /// is allowed.
    pub multiline: bool,
    /// rg `--multiline-dotall`: `.` matches `\n` too. Read only under
    /// `multiline`, as in rg.
    pub dotall: bool,
    /// rg `-i`: the term is compiled case-insensitively, as the FM-index
    /// pruned it.
    pub ignore_case: bool,
}

/// What the drive must produce per query; the CLI maps flags onto it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryOutput {
    /// Wire counts only: the harness path.
    Counts,
    /// Wire `total_matches` only; a literal is counted without a locate.
    Occurrences,
    /// Matched lines with spans and text, plus `before`/`after` context.
    Matches { before: usize, after: usize },
    /// Matched lines per file (rg `-c`).
    CountLines,
    /// Matches per file, rg's non-overlapping count (rg `--count-matches`).
    CountMatches,
    /// Files with at least one match (rg `-l`); the scan stops at the first.
    FilesWithMatches,
}

impl QueryOutput {
    fn keeps_lines(self) -> bool {
        matches!(self, Self::Matches { .. })
    }

    fn first_match_suffices(self) -> bool {
        matches!(self, Self::FilesWithMatches)
    }
}

/// One document's match extraction result.
pub struct DocOutcome {
    /// Lines holding at least one match. Under `-U` that counts every line a
    /// match touches, terminator included.
    pub matched_lines: u64,
    /// Non-overlapping matches, as rg's `--json` reports them in `matches`:
    /// one per span on the line path, one per whole match under `-U`.
    pub matched_spans: u64,
    /// Matched lines left with no span: the zero-width match at an
    /// unterminated EOF, which rg reports as a matched line carrying no
    /// submatch. Line path only — see [`DocOutcome::count_matches`].
    pub spanless_lines: u64,
    /// Whether the scan was rg's multiline one, which counts differently.
    pub multiline: bool,
    /// Matched and context lines, ascending; empty outside
    /// [`QueryOutput::Matches`]. A line with no spans may still be matched.
    pub lines: Vec<LineHit>,
    /// `-U` only: every match's document byte range, ascending. A match may
    /// cross terminators, so the per-line spans of `lines` cannot carry it;
    /// rg's `--json` reports these as one block's submatches. Filled under
    /// [`QueryOutput::Matches`] only.
    pub spans: Vec<(usize, usize)>,
}

impl DocOutcome {
    /// rg `--count-matches`. Its line-path summary printer counts a matched
    /// line even when no span survived, where its `--json` `matches` does
    /// not; the multiline one counts matches and nothing else.
    pub fn count_matches(&self) -> u64 {
        if self.multiline {
            self.matched_spans
        } else {
            self.matched_spans + self.spanless_lines
        }
    }

    /// rg `-c`: matched lines on the line path, matches under `-U`.
    pub fn count_lines(&self) -> u64 {
        if self.multiline {
            self.matched_spans
        } else {
            self.matched_lines
        }
    }
}

/// One line of output. The content is `raw[start..end]` of the document the
/// scan ran over: terminator excluded, `\r` kept, exact bytes.
pub struct LineHit {
    /// 1-based, like rg.
    pub line: u64,
    pub start: usize,
    pub end: usize,
    /// `(offset, len)` within the line, ascending and not merged. Empty on a
    /// context line and on a spanless matched line, so `matched` — not this
    /// — tells the two apart.
    pub spans: Vec<(u32, u32)>,
    pub matched: bool,
}

/// A line's 1-based number and its content range in the document.
#[derive(Clone, Copy)]
struct Line {
    no: u64,
    start: usize,
    end: usize,
}

/// Lines of a document: `\n` terminates, a trailing terminator does not open
/// an extra line, and an empty document has no lines.
struct Lines<'a> {
    raw: &'a [u8],
    pos: usize,
    no: u64,
}

impl<'a> Lines<'a> {
    fn new(raw: &'a [u8]) -> Self {
        Self { raw, pos: 0, no: 0 }
    }
}

impl Iterator for Lines<'_> {
    type Item = Line;

    fn next(&mut self) -> Option<Line> {
        if self.pos >= self.raw.len() {
            return None;
        }
        let start = self.pos;
        let end = memchr::memchr(b'\n', &self.raw[start..]).map_or(self.raw.len(), |i| start + i);
        self.pos = end + 1;
        self.no += 1;
        Some(Line {
            no: self.no,
            start,
            end,
        })
    }
}

/// Maps ascending byte offsets to their lines in one forward walk.
struct LineWalker<'a> {
    lines: Lines<'a>,
    cur: Line,
}

impl<'a> LineWalker<'a> {
    fn new(raw: &'a [u8]) -> Self {
        let mut lines = Lines::new(raw);
        let cur = lines.next().unwrap_or(Line {
            no: 0,
            start: 0,
            end: 0,
        });
        Self { lines, cur }
    }

    /// The line holding `pos`. `pos` must not decrease across calls.
    fn advance_to(&mut self, pos: usize) -> Line {
        while pos >= self.cur.end && self.cur.end < self.lines.raw.len() {
            match self.lines.next() {
                Some(l) => self.cur = l,
                None => break,
            }
        }
        self.cur
    }
}

/// Maps ascending byte offsets to their lines under `-U`, where a
/// terminator belongs to the line it ends and a position past the last line
/// belongs to none.
struct SpanWalker<'a> {
    lines: Lines<'a>,
    cur: Option<Line>,
}

impl<'a> SpanWalker<'a> {
    fn new(raw: &'a [u8]) -> Self {
        let mut lines = Lines::new(raw);
        let cur = lines.next();
        Self { lines, cur }
    }

    /// The line holding `pos`. `pos` must not decrease across calls.
    fn at(&mut self, pos: usize) -> Option<Line> {
        while self.cur.is_some_and(|l| pos > l.end) {
            match self.lines.next() {
                Some(l) => self.cur = Some(l),
                None => break,
            }
        }
        self.cur.filter(|l| pos <= l.end)
    }
}

/// Matcher for one query term: rg's line scan, or its multiline one.
#[derive(Debug)]
pub enum LineMatcher {
    /// An empty needle matches nothing, as the literal count contract has it.
    Literal(memmem::Finder<'static>),
    /// `anchor` is the pattern's required literal, reused as the candidate-
    /// line prefilter: it is contained in every match, and no match crosses a
    /// terminator, so a line without it cannot match.
    Regex {
        re: regex::bytes::Regex,
        anchor: Option<memmem::Finder<'static>>,
    },
    /// `-U` over a literal holding a terminator: one scan of the document.
    MultiLiteral(memmem::Finder<'static>),
    /// `-U` over a pattern that can match a terminator. The same required
    /// literal prunes the document, not a line: every match holds it, so
    /// a document without it has none.
    MultiRegex {
        re: regex::bytes::Regex,
        anchor: Option<memmem::Finder<'static>>,
    },
}

impl LineMatcher {
    pub fn literal(term: &[u8], mode: MatchMode) -> Result<Self, String> {
        let finder = memmem::Finder::new(term).into_owned();
        if !term.contains(&b'\n') {
            return Ok(Self::Literal(finder));
        }
        if !mode.multiline {
            return Err(format!(
                "invalid literal term {:?}: {NEWLINE_BANNED}",
                String::from_utf8_lossy(term)
            ));
        }
        Ok(Self::MultiLiteral(finder))
    }

    pub fn regex(src: &str, anchor: Option<&[u8]>, mode: MatchMode) -> Result<Self, RegexError> {
        let dotall = mode.multiline && mode.dotall;
        let hir = regex_syntax::ParserBuilder::new()
            .multi_line(true)
            .dot_matches_new_line(dotall)
            .case_insensitive(mode.ignore_case)
            .build()
            .parse(src)
            .map_err(|e| RegexError::new(src, e.to_string()))?;
        if !mode.multiline && matches_newline_only(&hir) {
            return Err(RegexError::new(src, NEWLINE_BANNED));
        }
        let re = regex::bytes::RegexBuilder::new(src)
            .multi_line(true)
            .dot_matches_new_line(dotall)
            .case_insensitive(mode.ignore_case)
            .build()
            .map_err(|e| RegexError::new(src, e.to_string()))?;
        let anchor = anchor.map(|a| memmem::Finder::new(a).into_owned());
        Ok(if mode.multiline && can_match_newline(&hir) {
            Self::MultiRegex { re, anchor }
        } else {
            Self::Regex { re, anchor }
        })
    }

    /// Matched lines of `raw`, ascending, with `output`'s context filled in.
    /// Counts are always produced; text and spans only under
    /// [`QueryOutput::Matches`].
    pub fn scan(&self, raw: &[u8], output: QueryOutput) -> DocOutcome {
        let mut acc = Accum::new(output);
        acc.multiline = matches!(self, Self::MultiLiteral(_) | Self::MultiRegex { .. });
        match self {
            Self::Literal(finder) => {
                let len = finder.needle().len();
                if len > 0 {
                    let mut walker = LineWalker::new(raw);
                    for pos in finder.find_iter(raw) {
                        let line = walker.advance_to(pos);
                        acc.push(line, (pos - line.start) as u32, len as u32);
                        if acc.done() {
                            break;
                        }
                    }
                }
            }
            Self::Regex { re, anchor: None } => {
                for line in Lines::new(raw) {
                    scan_line(re, raw, line, &mut acc);
                    if acc.done() {
                        break;
                    }
                }
            }
            Self::Regex {
                re,
                anchor: Some(finder),
            } => {
                let mut walker = LineWalker::new(raw);
                let mut last = 0u64;
                for pos in finder.find_iter(raw) {
                    let line = walker.advance_to(pos);
                    if line.no == last {
                        continue;
                    }
                    last = line.no;
                    scan_line(re, raw, line, &mut acc);
                    if acc.done() {
                        break;
                    }
                }
            }
            Self::MultiLiteral(finder) => {
                let len = finder.needle().len();
                scan_document(raw, finder.find_iter(raw).map(|p| (p, p + len)), &mut acc);
            }
            Self::MultiRegex { re, anchor } => {
                if anchor.as_ref().is_none_or(|f| f.find(raw).is_some()) {
                    scan_document(
                        raw,
                        re.find_iter(raw).map(|m| (m.start(), m.end())),
                        &mut acc,
                    );
                }
            }
        }
        acc.finish(raw, output)
    }
}

/// Folds rg's multiline matches, ascending and non-overlapping, into the
/// lines they touch: a match belongs to every line from the one holding its
/// first byte to the one holding its last, a terminator counting as part of
/// the line it ends. A line the match only reaches through its terminator is
/// matched with no span, which is how rg prints and colours it.
fn scan_document(raw: &[u8], matches: impl Iterator<Item = (usize, usize)>, acc: &mut Accum) {
    let mut walker = SpanWalker::new(raw);
    for (start, end) in matches {
        let Some(first) = walker.at(start) else {
            // Only reachable past the last line: a zero-width match after a
            // trailing terminator, which rg drops entirely.
            break;
        };
        if start == raw.len() {
            // rg keeps a zero-width match at an unterminated EOF as a
            // matched line carrying no submatch, and counts it nowhere.
            // Nothing can follow it.
            acc.open(first);
            break;
        }
        acc.count_match(start, end);
        let last = end.max(start + 1) - 1;
        let mut line = first;
        loop {
            let (lo, hi) = (start.max(line.start), end.min(line.end));
            if hi > lo {
                acc.record(line, (lo - line.start) as u32, (hi - lo) as u32);
            } else {
                acc.open(line);
            }
            if last <= line.end || line.end >= raw.len() {
                break;
            }
            match walker.at(line.end + 1) {
                Some(next) => line = next,
                None => break,
            }
        }
        if acc.done() {
            break;
        }
    }
}

fn scan_line(re: &regex::bytes::Regex, raw: &[u8], line: Line, acc: &mut Accum) {
    for m in re.find_iter(&raw[line.start..line.end]) {
        // rg's empty-match rule: a match starting at the document's
        // end — only reachable on an unterminated last line — reports the
        // line but no span. Nothing can follow it.
        if line.start + m.start() == raw.len() {
            acc.push_spanless(line);
            return;
        }
        acc.push(line, m.start() as u32, m.len() as u32);
        if acc.done() {
            return;
        }
    }
}

/// `true` when the pattern holds a literal `\n` or a class whose only member
/// is `\n` — rg's two refusal cases. Other `\n`-bearing classes are fine: a
/// line slice holds no terminator, so they simply never match one.
fn matches_newline_only(hir: &regex_syntax::hir::Hir) -> bool {
    use regex_syntax::hir::{Class, HirKind};
    match hir.kind() {
        HirKind::Literal(lit) => lit.0.contains(&b'\n'),
        HirKind::Class(Class::Unicode(c)) => {
            !c.ranges().is_empty()
                && c.ranges()
                    .iter()
                    .all(|r| r.start() == '\n' && r.end() == '\n')
        }
        HirKind::Class(Class::Bytes(c)) => {
            !c.ranges().is_empty()
                && c.ranges()
                    .iter()
                    .all(|r| r.start() == b'\n' && r.end() == b'\n')
        }
        HirKind::Capture(c) => matches_newline_only(&c.sub),
        HirKind::Repetition(r) => matches_newline_only(&r.sub),
        HirKind::Concat(v) | HirKind::Alternation(v) => v.iter().any(matches_newline_only),
        HirKind::Empty | HirKind::Look(_) => false,
    }
}

/// Whether rg would search whole documents for this pattern under `-U`,
/// i.e. whether a match can hold a line terminator.
///
/// This is grep-regex's `non_matching_bytes` test: a `\n` in any
/// literal or class counts, and so does every anchor except the word
/// boundaries — rg is deliberately conservative there, and `^ $ \A \z` all
/// take the multiline path. A pattern that fails this test keeps the line
/// scan under `-U`, exactly as rg keeps its line core, so `\A`/`\z` cannot
/// reach it and stay line anchors there.
fn can_match_newline(hir: &regex_syntax::hir::Hir) -> bool {
    use regex_syntax::hir::{Class, HirKind, Look};
    match hir.kind() {
        HirKind::Empty => false,
        HirKind::Literal(lit) => lit.0.contains(&b'\n'),
        HirKind::Class(Class::Unicode(c)) => c
            .ranges()
            .iter()
            .any(|r| r.start() <= '\n' && '\n' <= r.end()),
        HirKind::Class(Class::Bytes(c)) => c
            .ranges()
            .iter()
            .any(|r| r.start() <= b'\n' && b'\n' <= r.end()),
        HirKind::Look(look) => matches!(
            look,
            Look::Start | Look::End | Look::StartLF | Look::EndLF | Look::StartCRLF | Look::EndCRLF
        ),
        HirKind::Capture(c) => can_match_newline(&c.sub),
        HirKind::Repetition(r) => can_match_newline(&r.sub),
        HirKind::Concat(v) | HirKind::Alternation(v) => v.iter().any(can_match_newline),
    }
}

/// Folds ascending hits into per-line groups.
struct Accum {
    keep: bool,
    stop_early: bool,
    multiline: bool,
    matched_lines: u64,
    matched_spans: u64,
    spanless_lines: u64,
    cur: Option<(Line, Vec<(u32, u32)>)>,
    hits: Vec<(Line, Vec<(u32, u32)>)>,
    spans: Vec<(usize, usize)>,
}

impl Accum {
    fn new(output: QueryOutput) -> Self {
        Self {
            keep: output.keeps_lines(),
            stop_early: output.first_match_suffices(),
            multiline: false,
            matched_lines: 0,
            matched_spans: 0,
            spanless_lines: 0,
            cur: None,
            hits: Vec::new(),
            spans: Vec::new(),
        }
    }

    fn open(&mut self, line: Line) -> bool {
        let fresh = self.cur.as_ref().is_none_or(|(l, _)| l.no != line.no);
        if fresh {
            self.flush();
            self.cur = Some((line, Vec::new()));
            self.matched_lines += 1;
        }
        fresh
    }

    /// Open `line` and keep `off..off+len` on it without counting a match:
    /// under `-U` one match leaves a span on every line it touches.
    fn record(&mut self, line: Line, off: u32, len: u32) {
        self.open(line);
        if self.keep
            && let Some((_, spans)) = self.cur.as_mut()
        {
            spans.push((off, len));
        }
    }

    fn push(&mut self, line: Line, off: u32, len: u32) {
        self.record(line, off, len);
        self.matched_spans += 1;
    }

    /// One whole-document match, whatever lines it goes on to touch.
    fn count_match(&mut self, start: usize, end: usize) {
        self.matched_spans += 1;
        if self.keep {
            self.spans.push((start, end));
        }
    }

    /// A match rg reports on its line but not as a span. Already-pushed
    /// spans on the line keep it out of the spanless tally.
    fn push_spanless(&mut self, line: Line) {
        if self.open(line) {
            self.spanless_lines += 1;
        }
    }

    fn flush(&mut self) {
        if let Some(entry) = self.cur.take()
            && self.keep
        {
            self.hits.push(entry);
        }
    }

    fn done(&self) -> bool {
        self.stop_early && self.matched_lines > 0
    }

    fn finish(mut self, raw: &[u8], output: QueryOutput) -> DocOutcome {
        self.flush();
        let lines = match output {
            _ if self.matched_lines == 0 => Vec::new(),
            QueryOutput::Matches { before, after } if before > 0 || after > 0 => {
                with_context(raw, &self.hits, before as u64, after as u64)
            }
            QueryOutput::Matches { .. } => self
                .hits
                .into_iter()
                .map(|(l, spans)| LineHit {
                    line: l.no,
                    start: l.start,
                    end: l.end,
                    spans,
                    matched: true,
                })
                .collect(),
            _ => Vec::new(),
        };
        DocOutcome {
            matched_lines: self.matched_lines,
            matched_spans: self.matched_spans,
            spanless_lines: self.spanless_lines,
            multiline: self.multiline,
            lines,
            spans: self.spans,
        }
    }
}

/// Matched lines plus their context windows, ascending and deduplicated.
fn with_context(
    raw: &[u8],
    hits: &[(Line, Vec<(u32, u32)>)],
    before: u64,
    after: u64,
) -> Vec<LineHit> {
    let mut out = Vec::new();
    // `cover` is the first hit that can still reach the current line from
    // above; `exact` is the hit ON it, if any. Both advance monotonically.
    let (mut cover, mut exact) = (0usize, 0usize);
    for line in Lines::new(raw) {
        while cover < hits.len() && hits[cover].0.no + after < line.no {
            cover += 1;
        }
        while exact < hits.len() && hits[exact].0.no < line.no {
            exact += 1;
        }
        if !hits.get(cover).is_some_and(|h| h.0.no <= line.no + before) {
            continue;
        }
        let hit = hits.get(exact).filter(|h| h.0.no == line.no);
        out.push(LineHit {
            line: line.no,
            start: line.start,
            end: line.end,
            spans: hit.map(|h| h.1.clone()).unwrap_or_default(),
            matched: hit.is_some(),
        });
    }
    out
}

/// Whether `term` has a non-empty proper border, i.e. can overlap itself. A
/// term that cannot overlap itself has the same overlapping and
/// non-overlapping occurrence count, so rg's `--count-matches` can be served
/// from the literal probe with no fetch.
pub fn self_overlapping(term: &[u8]) -> bool {
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

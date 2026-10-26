//! Per-document leaf membership. Each answer equals the leaf's
//! [`LineMatcher`] finding a match in the BOM-stripped document. Line-path
//! patterns share one `RegexSet` pass, rewritten so no match crosses a line;
//! multiline-path patterns and CRLF anchors keep their own scan; literals test
//! containment.

use memchr::memmem;
use regex_syntax::hir::{
    Class, ClassBytes, ClassBytesRange, ClassUnicode, ClassUnicodeRange, Hir, HirKind, Look,
    Repetition,
};

use crate::matching::{LineMatcher, MatchMode, QueryOutput};

/// One leaf as verification sees it.
pub enum Leaf<'a> {
    /// The empty term, which matches nothing.
    Never,
    Literal(&'a [u8]),
    /// `matcher` is `src` compiled by [`LineMatcher::regex`].
    Pattern {
        src: &'a str,
        matcher: &'a LineMatcher,
    },
}

enum Test<'a> {
    Never,
    Literal(Box<memmem::Finder<'a>>),
    Set(usize, &'a LineMatcher),
    Own(&'a LineMatcher),
}

pub struct LeafSet<'a> {
    set: Option<regex::bytes::RegexSet>,
    tests: Vec<Test<'a>>,
}

impl<'a> LeafSet<'a> {
    pub fn new(leaves: &[Leaf<'a>], mode: MatchMode) -> Self {
        let mut members = Vec::new();
        let mut tests: Vec<Test<'a>> = leaves
            .iter()
            .map(|leaf| match *leaf {
                Leaf::Never => Test::Never,
                Leaf::Literal([]) => Test::Never,
                Leaf::Literal(lit) => Test::Literal(Box::new(memmem::Finder::new(lit))),
                Leaf::Pattern { src, matcher } => match line_pattern(src, matcher, mode) {
                    Some(p) => {
                        members.push(p);
                        Test::Set(members.len() - 1, matcher)
                    }
                    None => Test::Own(matcher),
                },
            })
            .collect();
        // A set past the regex size limit falls back to one scan per leaf.
        let set = (!members.is_empty())
            .then(|| regex::bytes::RegexSet::new(&members).ok())
            .flatten();
        if set.is_none() {
            for t in &mut tests {
                if let Test::Set(_, m) = *t {
                    *t = Test::Own(m);
                }
            }
        }
        Self { set, tests }
    }

    /// Fill every `None` of `known` with that leaf's membership in `doc`
    /// (BOM-stripped); entries already set are kept.
    pub fn fill(&self, doc: &[u8], known: &mut [Option<bool>]) {
        debug_assert_eq!(known.len(), self.tests.len());
        let wants_set = self
            .tests
            .iter()
            .zip(known.iter())
            .any(|(t, k)| k.is_none() && matches!(t, Test::Set(..)));
        // Every line of `doc` minus its final terminator is one haystack
        // line; an empty doc has no line at all.
        let hits = match &self.set {
            Some(set) if wants_set && !doc.is_empty() => {
                Some(set.matches(doc.strip_suffix(b"\n").unwrap_or(doc)))
            }
            _ => None,
        };
        for (t, k) in self.tests.iter().zip(known.iter_mut()) {
            if k.is_some() {
                continue;
            }
            *k = Some(match t {
                Test::Never => false,
                Test::Literal(f) => f.find(doc).is_some(),
                Test::Set(i, _) => hits.as_ref().is_some_and(|h| h.matched(*i)),
                Test::Own(m) => m.scan(doc, QueryOutput::FilesWithMatches).matched_lines > 0,
            });
        }
    }
}

/// `src` rewritten to match a multi-line haystack exactly where it matches
/// one of its lines, or `None` where only its own scan is exact.
fn line_pattern(src: &str, matcher: &LineMatcher, mode: MatchMode) -> Option<String> {
    if !matches!(
        matcher,
        LineMatcher::Regex { .. } | LineMatcher::LocalRegex(_)
    ) {
        return None;
    }
    let hir = regex_syntax::ParserBuilder::new()
        .multi_line(true)
        .dot_matches_new_line(mode.multiline && mode.dotall)
        .case_insensitive(mode.ignore_case)
        .build()
        .parse(src)
        .ok()?;
    Some(line_safe(&hir)?.to_string())
}

/// No `\n` in any class or literal, `\A` / `\z` as line anchors; `None` on
/// a CRLF anchor or a Unicode `\B` / half boundary (those decode across the
/// line start).
fn line_safe(hir: &Hir) -> Option<Hir> {
    Some(match hir.kind() {
        HirKind::Empty => Hir::empty(),
        HirKind::Literal(lit) if lit.0.contains(&b'\n') => Hir::fail(),
        HirKind::Literal(_) => hir.clone(),
        HirKind::Class(Class::Unicode(c)) => {
            let mut c = c.clone();
            c.difference(&ClassUnicode::new([ClassUnicodeRange::new('\n', '\n')]));
            Hir::class(Class::Unicode(c))
        }
        HirKind::Class(Class::Bytes(c)) => {
            let mut c = c.clone();
            c.difference(&ClassBytes::new([ClassBytesRange::new(b'\n', b'\n')]));
            Hir::class(Class::Bytes(c))
        }
        HirKind::Look(Look::Start) => Hir::look(Look::StartLF),
        HirKind::Look(Look::End) => Hir::look(Look::EndLF),
        HirKind::Look(
            Look::StartCRLF
            | Look::EndCRLF
            | Look::WordUnicodeNegate
            | Look::WordStartHalfUnicode
            | Look::WordEndHalfUnicode,
        ) => return None,
        HirKind::Look(_) => hir.clone(),
        HirKind::Capture(c) => line_safe(&c.sub)?,
        HirKind::Repetition(r) => Hir::repetition(Repetition {
            min: r.min,
            max: r.max,
            greedy: r.greedy,
            sub: Box::new(line_safe(&r.sub)?),
        }),
        HirKind::Concat(v) => Hir::concat(v.iter().map(line_safe).collect::<Option<_>>()?),
        HirKind::Alternation(v) => {
            Hir::alternation(v.iter().map(line_safe).collect::<Option<_>>()?)
        }
    })
}

#[cfg(test)]
mod tests;

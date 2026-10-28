//! The plan of an expression search, as `FMGREP_VERBOSE` prints it.

use std::fmt;
use std::ops::Range;

use super::demote::{FALLBACK_RATIO, Plan, Read, Why};
use super::{Hits, Leaf, Probe, Tri};
use crate::expr::Expr;
use crate::matching::QueryOutput;
use crate::verbose::{Us, plural};

pub(super) struct Explain<'a> {
    pub expr: &'a Expr,
    pub leaves: &'a [Leaf<'a>],
    pub plan: &'a Plan,
    /// Per leaf, its probe's time when probed.
    pub leaf_us: &'a [Option<u64>],
    pub hits: &'a [Hits],
    pub root: &'a Tri,
    pub stale: usize,
    pub output: QueryOutput,
    /// Documents the output mode fetches.
    pub reads: usize,
}

impl fmt::Display for Explain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let plan = self.plan;
        writeln!(f, "plan: {}", self.expr)?;
        let width = self.leaves.iter().map(|l| l.name.chars().count()).max();
        for (i, l) in self.leaves.iter().enumerate() {
            write!(f, "leaf {i} {:<w$}  ", l.name, w = width.unwrap_or(0))?;
            match &l.probe {
                Probe::Never => f.write_str("empty")?,
                Probe::Literal(lit) if lit == l.src.as_bytes() => f.write_str("literal")?,
                Probe::Literal(lit) => write!(f, "literal {:?}", String::from_utf8_lossy(lit))?,
                Probe::Regex => match l.prep.as_ref().and_then(crate::pattern::anchor) {
                    Some(a) => write!(f, "regex anchor {:?}", String::from_utf8_lossy(a))?,
                    None => f.write_str("regex, no anchor")?,
                },
            }
            let counts = &plan.counts[i];
            if plan.counted[i] {
                let total: u64 = counts.iter().map(|c| c.1).sum();
                let shards = counts.len();
                let runs = Runs(counts.iter().map(|c| c.0.clone()));
                write!(
                    f,
                    "  counted {total} in {shards} shard{} {runs}",
                    plural(shards)
                )?;
            } else {
                f.write_str("  not counted")?;
            }
            let reason = Reason {
                why: plan.why[i],
                demoted: plan.demoted[i],
                counted: plan.counted[i],
                regex_reference: match plan.why[i] {
                    Why::Ratio { reference, .. } => {
                        !matches!(self.leaves[reference].probe, Probe::Literal(_))
                    }
                    _ => false,
                },
            };
            write!(f, "\n  {reason}; ")?;
            if matches!(l.probe, Probe::Never) {
                writeln!(f, "matches nothing")?;
                continue;
            }
            let unsettled = &plan.unsettled[i];
            if plan.demoted[i] {
                writeln!(f, "verified on {}", Runs(unsettled.iter().cloned()))?;
                continue;
            }
            match &plan.within[i] {
                Some(w) => write!(f, "probed within {}", Runs(w.iter().cloned()))?,
                None => f.write_str("probed everywhere")?,
            }
            if !unsettled.is_empty() {
                write!(f, ", unsettled {}", Runs(unsettled.iter().cloned()))?;
            }
            write!(f, " in round {}", if plan.candidate[i] { 2 } else { 1 })?;
            if let Some(&Some(us)) = self.leaf_us.get(i) {
                write!(f, " ({})", Us(us as f64))?;
            }
            let (exact, candidates) = &self.hits[i];
            match l.probe {
                Probe::Regex => writeln!(
                    f,
                    ", {} candidate{}",
                    candidates.len(),
                    plural(candidates.len())
                )?,
                _ => writeln!(f, ", {} doc{}", exact.len(), plural(exact.len()))?,
            }
        }
        let mode = match self.output {
            QueryOutput::Counts => "counts",
            QueryOutput::FilesWithMatches => "-l",
            QueryOutput::CountLines => "-c",
            QueryOutput::CountMatches => "--count-matches",
            QueryOutput::Matches { .. } => "lines",
        };
        writeln!(
            f,
            "root: yes {}, maybe {} (stale {}); {mode} reads {}",
            self.root.yes.len(),
            self.root.maybe.len(),
            self.stale,
            self.reads
        )
    }
}

/// A leaf's placement and the numbers that decided it.
pub(super) struct Reason {
    pub why: Why,
    pub demoted: bool,
    pub counted: bool,
    pub regex_reference: bool,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let k = FALLBACK_RATIO;
        match self.why {
            Why::Located if self.counted => f.write_str("located: not demotable"),
            Why::Located => f.write_str("located"),
            Why::Reference => f.write_str("reference: rarest counted leaf of its AND"),
            Why::Ratio {
                reference,
                total,
                min,
            } => {
                let cause = match self.regex_reference {
                    true => "a regex reference",
                    false => "an AND under an OR",
                };
                match self.demoted {
                    true => write!(
                        f,
                        "demoted by the {k}x fallback ratio: {total} > {k} x {min} of leaf {reference} ({cause})"
                    ),
                    false => write!(
                        f,
                        "located: {total} <= {k} x {min} of leaf {reference}, within the {k}x fallback ratio ({cause})"
                    ),
                }
            }
            Why::Cost {
                reference, docs: 0, ..
            } => write!(f, "demoted: leaf {reference} has no document to read"),
            Why::Cost {
                reference,
                locate_us,
                docs,
                read_us,
                read,
            } => {
                let (verdict, op) = match self.demoted {
                    true => ("demoted", ">"),
                    false => ("located", "<="),
                };
                write!(
                    f,
                    "{verdict}: locate est {} {op} read {docs} doc{} of leaf {reference} x {} = {}",
                    Us(locate_us),
                    plural(docs),
                    Us(read_us),
                    Us(docs as f64 * read_us)
                )?;
                match read {
                    Read::Floor => f.write_str(" at the band floor"),
                    Read::Ceiling => f.write_str(" at the band ceiling"),
                    Read::Sampled(n) => {
                        write!(f, ", the read cost sampled over {n} doc{}", plural(n))
                    }
                    Read::Failed => f.write_str(" at the band ceiling, the read sample failed"),
                }
            }
        }
    }
}

/// Ascending ranges, adjacent ones merged, at most 4 shown.
pub(super) struct Runs<I>(pub I);

impl<I: Iterator<Item = Range<usize>> + Clone> fmt::Display for Runs<I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const SHOWN: usize = 4;
        let mut merged = self.0.clone().peekable();
        let mut runs = 0;
        while let Some(mut r) = merged.next() {
            while let Some(next) = merged.next_if(|n| n.start == r.end) {
                r.end = next.end;
            }
            if runs < SHOWN {
                let sep = if runs > 0 { " " } else { "" };
                write!(f, "{sep}[{}..{})", r.start, r.end)?;
            }
            runs += 1;
        }
        match runs {
            0 => f.write_str("[]"),
            n if n > SHOWN => write!(f, " +{} more", n - SHOWN),
            _ => Ok(()),
        }
    }
}

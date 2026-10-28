//! Which AND siblings are located and which are left to verification, from
//! count-only probes and the located rarest sibling's cost.

use std::ops::Range;

use super::{Leaf, Probe, demotable};
use crate::expr::Expr;
use crate::probe::{Answer, Query};
use crate::sharded::DocfmIndex;

/// Reference documents read to measure the read cost.
pub const SAMPLE_DOCS: usize = 16;

/// A regex reference, or an AND under an OR, demotes a sibling counted over
/// this many times the rarest.
pub const FALLBACK_RATIO: u64 = 8;

/// Microseconds per document read: outside the band its edge decides,
/// inside it the cost is sampled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReadBand {
    pub min_us: f64,
    pub max_us: f64,
}

impl ReadBand {
    pub const DEFAULT: ReadBand = ReadBand {
        min_us: 12.0,
        max_us: 80.0,
    };

    /// [`Self::DEFAULT`]'s shape from a warm read clamped to 2..200 us.
    pub fn around(warm_us: f64) -> ReadBand {
        let min_us = warm_us.clamp(2.0, 200.0);
        ReadBand {
            min_us,
            max_us: min_us * Self::DEFAULT.max_us / Self::DEFAULT.min_us,
        }
    }
}

pub fn read_band(band: ReadBand) -> ReadBand {
    #[cfg(test)]
    if let Some((min_us, max_us)) = crate::tests::BAND.get() {
        return ReadBand { min_us, max_us };
    }
    band
}

/// An AND node's direct leaf operands.
pub struct AndGroup {
    pub leaves: Vec<usize>,
    pub under_or: bool,
}

/// Per AND node, the leaf indices of its direct leaf operands.
pub fn and_groups(expr: &Expr) -> Vec<AndGroup> {
    fn walk(e: &Expr, under_or: bool, next: &mut usize, out: &mut Vec<AndGroup>) {
        let (Expr::And(v) | Expr::Or(v)) = e else {
            *next += 1;
            return;
        };
        let mut leaves = Vec::new();
        for child in v {
            if matches!(child, Expr::Term(_) | Expr::Regex(_)) {
                leaves.push(*next);
            }
            walk(child, under_or || matches!(e, Expr::Or(_)), next, out);
        }
        if matches!(e, Expr::And(_)) {
            out.push(AndGroup { leaves, under_or });
        }
    }
    let mut out = Vec::new();
    walk(expr, false, &mut 0, &mut out);
    out
}

/// The leaves to count: those of an AND with two countable and one
/// demotable direct leaves.
pub fn to_count(groups: &[AndGroup], countable: &[bool], demotable: &[bool]) -> Vec<bool> {
    let mut out = vec![false; countable.len()];
    #[cfg(test)]
    if crate::tests::NO_DEMOTION.get() {
        return out;
    }
    for g in groups.iter().map(|g| &g.leaves) {
        if g.iter().filter(|&&i| countable[i]).count() >= 2 && g.iter().any(|&i| demotable[i]) {
            g.iter()
                .filter(|&&i| countable[i])
                .for_each(|&i| out[i] = true);
        }
    }
    out
}

/// What placed a leaf; the verbose plan prints it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Why {
    Located,
    /// Its AND's first smallest counted leaf.
    Reference,
    /// Judged by [`FALLBACK_RATIO`]: `total` against the reference's `min`.
    Ratio {
        reference: usize,
        total: u64,
        min: u64,
    },
    /// Judged by [`decide`]: locating against reading `docs` at `read_us`.
    Cost {
        reference: usize,
        locate_us: f64,
        docs: usize,
        read_us: f64,
        read: Read,
    },
}

/// Where [`decide`]'s read cost came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Read {
    Floor,
    Ceiling,
    /// Sampled over this many documents.
    Sampled(usize),
    /// The sample failed; the ceiling stood in.
    Failed,
}

/// Which leaves are located, where, and what their exact counts may miss.
pub struct Plan {
    pub demoted: Vec<bool>,
    /// Leaves [`decide`] demotes or leaves to a second probe pass.
    pub candidate: Vec<bool>,
    /// Per located literal, the ranges it is probed on.
    pub within: Vec<Option<Vec<Range<usize>>>>,
    /// Per leaf, the ranges its exact counts may miss.
    pub unsettled: Vec<Vec<Range<usize>>>,
    /// Per leaf, its count pass answer.
    pub counts: Vec<Vec<(Range<usize>, u64)>>,
    pub groups: Vec<Group>,
    pub probe_us: u64,
    pub counted: Vec<bool>,
    pub why: Vec<Why>,
}

/// An AND whose candidates wait on its located reference.
pub struct Group {
    /// The first smallest counted leaf.
    pub reference: usize,
    pub candidates: Vec<usize>,
    /// Where every demotable counted leaf of the AND occurs.
    pub within: Vec<Range<usize>>,
}

/// The count pass; it probes nothing unless some AND can demote.
pub fn plan(
    index: &DocfmIndex,
    expr: &Expr,
    leaves: &[Leaf<'_>],
    within: Option<&[Range<usize>]>,
) -> Plan {
    let n = leaves.len();
    let terms: Vec<Option<&[u8]>> = leaves
        .iter()
        .map(|l| match &l.probe {
            Probe::Literal(lit) => Some(lit.as_slice()),
            Probe::Regex => l.prep.as_ref().and_then(crate::pattern::anchor),
            Probe::Never => None,
        })
        .collect();
    let demotable: Vec<bool> = leaves
        .iter()
        .map(|l| matches!(&l.probe, Probe::Literal(lit) if demotable(lit)))
        .collect();
    let literal: Vec<bool> = leaves
        .iter()
        .map(|l| matches!(l.probe, Probe::Literal(_)))
        .collect();
    let groups = and_groups(expr);
    let countable: Vec<bool> = terms.iter().map(Option::is_some).collect();
    let counted = to_count(&groups, &countable, &demotable);
    let queries: Vec<Query<'_>> = terms
        .iter()
        .zip(&counted)
        .filter_map(|(t, &c)| t.filter(|_| c).map(Query::Count))
        .collect();
    let mut counts = vec![Vec::new(); n];
    let mut probe_us = 0;
    if !queries.is_empty() {
        for (i, a) in (0..n)
            .filter(|&i| counted[i])
            .zip(index.probe_all(&queries, within))
        {
            probe_us += a.probe_us;
            counts[i] = a.counts;
        }
    }
    let mut plan = arrange(&groups, counts, &counted, &demotable, &literal);
    plan.probe_us = probe_us;
    plan
}

fn ranges(counts: &[(Range<usize>, u64)]) -> Vec<Range<usize>> {
    counts.iter().map(|(r, _)| r.clone()).collect()
}

/// The plan from the `counted` leaves' `counts`. Per AND the first smallest
/// is located; a regex smallest or an AND under an OR demotes now by
/// [`FALLBACK_RATIO`], otherwise every other demotable leaf is a candidate.
pub fn arrange(
    groups: &[AndGroup],
    counts: Vec<Vec<(Range<usize>, u64)>>,
    counted: &[bool],
    demotable: &[bool],
    literal: &[bool],
) -> Plan {
    let n = counted.len();
    let mut plan = Plan {
        demoted: vec![false; n],
        candidate: vec![false; n],
        within: vec![None; n],
        unsettled: vec![Vec::new(); n],
        counts: Vec::new(),
        groups: Vec::new(),
        probe_us: 0,
        counted: counted.to_vec(),
        why: vec![Why::Located; n],
    };
    for AndGroup {
        leaves: g,
        under_or,
    } in groups
    {
        let totals = g
            .iter()
            .filter(|&&i| counted[i])
            .map(|&i| (i, counts[i].iter().map(|(_, c)| c).sum::<u64>()));
        let Some((m, min)) = totals.clone().min_by_key(|&(_, t)| t) else {
            continue;
        };
        let ratio = (!literal[m] || *under_or).then_some(FALLBACK_RATIO);
        plan.why[m] = Why::Reference;
        let mut candidates = Vec::new();
        for (i, t) in totals.filter(|&(i, _)| i != m && demotable[i]) {
            match ratio {
                Some(k) => {
                    plan.demoted[i] = t > k.saturating_mul(min);
                    plan.why[i] = Why::Ratio {
                        reference: m,
                        total: t,
                        min,
                    };
                }
                None => candidates.push(i),
            }
        }
        // Under an OR, located literals are probed only where every demoted
        // leaf occurs.
        let spans = |i: usize| counted[i] && demotable[i] && (!under_or || plan.demoted[i]);
        let Some(within) = g
            .iter()
            .filter(|&&i| spans(i))
            .map(|&i| ranges(&counts[i]))
            .reduce(|a, b| intersect(&a, &b))
        else {
            continue;
        };
        for &i in g.iter().filter(|&&i| literal[i]) {
            if plan.demoted[i] {
                plan.unsettled[i] = ranges(&counts[i]);
            } else {
                plan.unsettled[i] = difference(&ranges(&counts[i]), &within);
                plan.within[i] = Some(within.clone());
            }
        }
        candidates.iter().for_each(|&i| plan.candidate[i] = true);
        if !candidates.is_empty() {
            plan.groups.push(Group {
                reference: m,
                candidates,
                within,
            });
        }
    }
    plan.counts = counts;
    plan
}

/// Demote a candidate when its locate estimate exceeds reading the
/// reference's documents in [`Group::within`]: the band floor keeps it, the
/// ceiling demotes it, in between one read sample of [`SAMPLE_DOCS`] decides
/// (a failed one counts as the ceiling). `read_us` times reads of the given
/// documents, per document.
pub fn decide(
    plan: &mut Plan,
    band: ReadBand,
    stage1: &[Option<&Answer>],
    read_us: &mut dyn FnMut(&[usize]) -> Result<f64, String>,
) {
    let mut sampled = None;
    for g in std::mem::take(&mut plan.groups) {
        let a = stage1[g.reference].expect("a reference is probed before its candidates");
        let docs: Vec<usize> = a
            .exact
            .iter()
            .filter(|&&(d, c)| c > 0 && contains(&g.within, d))
            .map(|&(d, _)| d)
            .collect();
        let occ_m = occurrences(&plan.counts[g.reference], &g.within);
        for &i in &g.candidates {
            let occ_i = occurrences(&plan.counts[i], &g.within);
            let cost = |read| demote_by_cost(a.probe_us, occ_m, docs.len(), occ_i, read);
            let (demote, us, read) = match (cost(band.min_us), cost(band.max_us)) {
                (false, _) => (false, band.min_us, Read::Floor),
                (true, true) => (true, band.max_us, Read::Ceiling),
                (true, false) => {
                    let (us, read) = *sampled.get_or_insert_with(|| {
                        let ids = spread(&docs, SAMPLE_DOCS);
                        match read_us(&ids) {
                            Ok(us) => (us, Read::Sampled(ids.len())),
                            Err(_) => (band.max_us, Read::Failed),
                        }
                    });
                    (cost(us), us, read)
                }
            };
            plan.why[i] = Why::Cost {
                reference: g.reference,
                locate_us: locate_est_us(a.probe_us, occ_m, occ_i),
                docs: docs.len(),
                read_us: us,
                read,
            };
            if demote {
                plan.demoted[i] = true;
                plan.within[i] = None;
                plan.unsettled[i] = ranges(&plan.counts[i]);
            }
        }
    }
}

/// Demote when locating costs more than reading `docs_m` documents at
/// `read_us` each, or there is none to read.
pub fn demote_by_cost(t_m_us: u64, occ_m: u64, docs_m: usize, occ_i: u64, read_us: f64) -> bool {
    docs_m == 0 || locate_est_us(t_m_us, occ_m, occ_i) > docs_m as f64 * read_us
}

/// Locating `occ_i` occurrences costs `t_m_us` per `occ_m`, linearly.
pub fn locate_est_us(t_m_us: u64, occ_m: u64, occ_i: u64) -> f64 {
    t_m_us.max(1) as f64 * occ_i as f64 / occ_m.max(1) as f64
}

/// The occurrences counted in ranges that meet `within`.
fn occurrences(counts: &[(Range<usize>, u64)], within: &[Range<usize>]) -> u64 {
    let meets = |r: &Range<usize>| {
        within
            .get(within.partition_point(|w| w.end <= r.start))
            .is_some_and(|w| w.start < r.end)
    };
    counts
        .iter()
        .filter(|(r, _)| meets(r))
        .map(|(_, c)| c)
        .sum()
}

/// At most `n` of ascending `docs`, evenly spaced.
pub fn spread(docs: &[usize], n: usize) -> Vec<usize> {
    match docs.len() <= n {
        true => docs.to_vec(),
        false => (0..n).map(|k| docs[k * docs.len() / n]).collect(),
    }
}

/// `a ∩ b`, both ascending disjoint.
pub fn intersect(a: &[Range<usize>], b: &[Range<usize>]) -> Vec<Range<usize>> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        let r = a[i].start.max(b[j].start)..a[i].end.min(b[j].end);
        if !r.is_empty() {
            out.push(r);
        }
        if a[i].end <= b[j].end {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// `a \ b`, both ascending disjoint.
pub fn difference(a: &[Range<usize>], b: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for r in a {
        let mut start = r.start;
        let first = b.partition_point(|c| c.end <= r.start);
        for cut in b[first..].iter().take_while(|c| c.start < r.end) {
            if cut.start > start {
                out.push(start..cut.start);
            }
            start = start.max(cut.end);
        }
        if start < r.end {
            out.push(start..r.end);
        }
    }
    out
}

/// Whether ascending disjoint `ranges` hold `doc`.
pub fn contains(ranges: &[Range<usize>], doc: usize) -> bool {
    let at = ranges.partition_point(|r| r.end <= doc);
    ranges.get(at).is_some_and(|r| r.start <= doc)
}

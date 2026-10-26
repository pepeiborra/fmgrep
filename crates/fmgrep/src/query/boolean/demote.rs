//! Which AND siblings are located and which are left to verification, from
//! count-only probes.

use std::ops::Range;

use super::{Leaf, Probe, demotable};
use crate::expr::Expr;
use crate::probe::Query;
use crate::sharded::DocfmIndex;

const RATIO_VAR: &str = "FMGREP_AND_LOCATE_RATIO";

/// K from `FMGREP_AND_LOCATE_RATIO`: unset is 8, 0 never demotes.
pub fn ratio() -> Result<u64, String> {
    #[cfg(test)]
    if let Some(k) = crate::tests::RATIO.get() {
        return Ok(k);
    }
    crate::env::count(RATIO_VAR, 8).map_err(|e| format!("error: {e}"))
}

/// Per AND node, the leaf indices of its direct leaf operands.
pub fn and_groups(expr: &Expr) -> Vec<Vec<usize>> {
    fn walk(e: &Expr, next: &mut usize, out: &mut Vec<Vec<usize>>) {
        let (Expr::And(v) | Expr::Or(v)) = e else {
            *next += 1;
            return;
        };
        let mut group = Vec::new();
        for child in v {
            if matches!(child, Expr::Term(_) | Expr::Regex(_)) {
                group.push(*next);
            }
            walk(child, next, out);
        }
        if matches!(e, Expr::And(_)) {
            out.push(group);
        }
    }
    let mut out = Vec::new();
    walk(expr, &mut 0, &mut out);
    out
}

/// The leaves to count: those of an AND with two countable and one
/// demotable direct leaves. None at `k == 0`.
pub fn to_count(
    groups: &[Vec<usize>],
    countable: &[bool],
    demotable: &[bool],
    k: u64,
) -> Vec<bool> {
    let mut out = vec![false; countable.len()];
    for g in groups.iter().filter(|_| k > 0) {
        if g.iter().filter(|&&i| countable[i]).count() >= 2 && g.iter().any(|&i| demotable[i]) {
            g.iter()
                .filter(|&&i| countable[i])
                .for_each(|&i| out[i] = true);
        }
    }
    out
}

/// Per group, every demotable counted leaf whose total exceeds `k` times the
/// group's smallest; the first smallest is never demoted.
pub fn demoted(
    groups: &[Vec<usize>],
    totals: &[Option<u64>],
    demotable: &[bool],
    k: u64,
) -> Vec<bool> {
    let mut out = vec![false; totals.len()];
    for g in groups.iter().filter(|_| k > 0) {
        let counted = g.iter().filter_map(|&i| totals[i].map(|t| (i, t)));
        let Some((min_leaf, min)) = counted.clone().min_by_key(|&(_, t)| t) else {
            continue;
        };
        for (i, t) in counted {
            out[i] = i != min_leaf && demotable[i] && t > k.saturating_mul(min);
        }
    }
    out
}

/// Which leaves are located, where, and what their exact counts may miss.
pub struct Plan {
    pub demoted: Vec<bool>,
    /// Per located literal, the ranges it is probed on.
    pub within: Vec<Option<Vec<Range<usize>>>>,
    /// Per leaf, the ranges its exact counts may miss.
    pub unsettled: Vec<Vec<Range<usize>>>,
    pub probe_us: u64,
}

/// The count pass; it probes nothing unless some AND can demote.
pub fn plan(index: &DocfmIndex, expr: &Expr, leaves: &[Leaf<'_>], ratio: u64) -> Plan {
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
    let groups = and_groups(expr);
    let countable: Vec<bool> = terms.iter().map(Option::is_some).collect();
    let counted = to_count(&groups, &countable, &demotable, ratio);
    let queries: Vec<Query<'_>> = terms
        .iter()
        .zip(&counted)
        .filter_map(|(t, &c)| t.filter(|_| c).map(Query::Count))
        .collect();
    let mut plan = Plan {
        demoted: vec![false; n],
        within: vec![None; n],
        unsettled: vec![Vec::new(); n],
        probe_us: 0,
    };
    if queries.is_empty() {
        return plan;
    }
    let mut counts = vec![Vec::new(); n];
    for (i, a) in (0..n)
        .filter(|&i| counted[i])
        .zip(index.probe_all(&queries))
    {
        plan.probe_us += a.probe_us;
        counts[i] = a.counts;
    }
    let totals: Vec<Option<u64>> = counts
        .iter()
        .zip(&counted)
        .map(|(c, &k)| k.then(|| c.iter().map(|(_, n)| n).sum()))
        .collect();
    plan.demoted = demoted(&groups, &totals, &demotable, ratio);
    let docs =
        |i: usize| -> Vec<Range<usize>> { counts[i].iter().map(|(r, _)| r.clone()).collect() };
    for g in &groups {
        let common = g.iter().filter(|&&i| plan.demoted[i]);
        let Some(within) = common.map(|&i| docs(i)).reduce(|a, b| intersect(&a, &b)) else {
            continue;
        };
        for &i in g {
            if plan.demoted[i] {
                plan.unsettled[i] = docs(i);
            } else if matches!(leaves[i].probe, Probe::Literal(_)) {
                plan.unsettled[i] = difference(&docs(i), &within);
                plan.within[i] = Some(within.clone());
            }
        }
    }
    plan
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

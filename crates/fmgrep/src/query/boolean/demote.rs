//! Which AND siblings are located and which are left to verification, from
//! count-only probes and the located rarest sibling's cost.

use std::ops::Range;

use super::super::read::{self, BYTE_NS};
use super::{Leaf, Probe, demotable};
use crate::expr::Expr;
use crate::pattern::PreparedPattern;
use crate::probe::{Answer, Query};
use crate::sharded::{DocfmIndex, Walk};

/// Reference documents read to measure the read cost.
pub const SAMPLE_DOCS: usize = 16;

/// Nanoseconds per byte of a large document a walked candidate's check
/// reads: 1 ns. A measurement supports the value: on a Mac, checking a
/// -i pattern over large documents cost 1.0 to 1.1 ns a byte at the
/// median, 0.4 ns at the tenth percentile and 1.7 ns at the ninetieth.
/// Half or twice it changes few plans. It is written as [`BYTE_NS`], a
/// plain read, times the default band's ceiling over its floor only
/// because that product is 1 ns; the band says nothing of the cost of the
/// fold matcher.
pub const LARGE_BYTE_NS: f64 = BYTE_NS * ReadBand::DEFAULT.max_us / ReadBand::DEFAULT.min_us;

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
    /// Judged by [`decide`]: locating against reading `docs` at `read_us`
    /// each, `read_all_us` in all. A walked candidate's `rows` are its
    /// busiest shard's and their µs each; `held` is the share of the
    /// documents that hold the candidates, which only a sample measures,
    /// and that locating reads too; `large` counts its large documents and
    /// their µs at [`LARGE_BYTE_NS`], which `read_us` does not price.
    Cost {
        reference: usize,
        locate_us: f64,
        rows: Option<(u64, f64)>,
        docs: usize,
        read_us: f64,
        read_all_us: f64,
        held: Option<f64>,
        large: Option<(usize, f64)>,
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
    /// Not sampled: reading the `docs` a sample takes may cost `us`, at the
    /// band ceiling, no less than demoting saves.
    Unsampled {
        docs: usize,
        us: f64,
    },
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

/// The count pass; it probes nothing unless some AND can demote. A counted
/// folded leaf is counted by its walk, which is returned so that locating
/// it later does not walk again.
pub fn plan<'i>(
    index: &'i DocfmIndex,
    expr: &Expr,
    leaves: &[Leaf<'_>],
    within: Option<&[Range<usize>]>,
) -> (Plan, Vec<Option<Walk<'i>>>) {
    let n = leaves.len();
    let terms: Vec<Option<Query<'_>>> = leaves
        .iter()
        .map(|l| match (&l.probe, &l.prep) {
            (Probe::Literal(lit), _) => Some(Query::Count(lit)),
            (Probe::Regex, Some(p)) => crate::pattern::anchor(p).map(Query::Count),
            _ => None,
        })
        .collect();
    let folded: Vec<Option<&PreparedPattern>> = leaves
        .iter()
        .map(|l| match (&l.probe, &l.prep) {
            (Probe::Fold, Some(p)) if p.fold().is_some() => Some(p),
            _ => None,
        })
        .collect();
    let demotable: Vec<bool> = leaves
        .iter()
        .map(|l| match &l.probe {
            Probe::Literal(lit) => demotable(lit),
            Probe::Fold => demotable(l.src.as_bytes()),
            Probe::Never | Probe::Regex => false,
        })
        .collect();
    let exact: Vec<bool> = leaves
        .iter()
        .map(|l| matches!(l.probe, Probe::Literal(_) | Probe::Fold))
        .collect();
    let narrowed: Vec<bool> = leaves
        .iter()
        .map(|l| matches!(l.probe, Probe::Literal(_)))
        .collect();
    let groups = and_groups(expr);
    let countable: Vec<bool> = (terms.iter().zip(&folded))
        .map(|(t, f)| t.is_some() || f.is_some())
        .collect();
    let counted = to_count(&groups, &countable, &demotable);
    let queries: Vec<Query<'_>> = terms
        .iter()
        .zip(&counted)
        .filter_map(|(t, &c)| t.filter(|_| c))
        .collect();
    let mut counts = vec![Vec::new(); n];
    let mut probe_us = 0;
    if !queries.is_empty() {
        for (i, a) in (0..n)
            .filter(|&i| counted[i] && terms[i].is_some())
            .zip(index.probe_all(&queries, within))
        {
            probe_us += a.probe_us;
            counts[i] = a.counts;
        }
    }
    let walks: Vec<Option<Walk<'i>>> = (folded.iter().zip(&counted))
        .map(|(f, &c)| f.filter(|_| c).and_then(|p| index.walk(p, within)))
        .collect();
    for (i, w) in walks.iter().enumerate() {
        if let Some(w) = w {
            probe_us += w.us();
            counts[i] = w.counts();
        }
    }
    let mut plan = arrange(&groups, counts, &counted, &demotable, &exact, &narrowed);
    plan.probe_us = probe_us;
    (plan, walks)
}

fn ranges(counts: &[(Range<usize>, u64)]) -> Vec<Range<usize>> {
    counts.iter().map(|(r, _)| r.clone()).collect()
}

/// The plan from the `counted` leaves' `counts`. Per AND the first smallest
/// is located; a smallest that is not `exact` (its located answer lists a
/// superset) or an AND under an OR demotes now by [`FALLBACK_RATIO`],
/// otherwise every other demotable leaf is a candidate. Only `narrowed`
/// leaves are probed within the AND's span.
pub fn arrange(
    groups: &[AndGroup],
    counts: Vec<Vec<(Range<usize>, u64)>>,
    counted: &[bool],
    demotable: &[bool],
    exact: &[bool],
    narrowed: &[bool],
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
        let ratio = (!exact[m] || *under_or).then_some(FALLBACK_RATIO);
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
        for &i in g {
            if plan.demoted[i] {
                plan.unsettled[i] = ranges(&counts[i]);
            } else if narrowed[i] {
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

/// What [`decide`] prices a walked candidate with: a folded literal the
/// count pass walked has only its rows left to locate.
pub struct Walked<'a> {
    /// Per leaf, whether the count pass walked it.
    pub leaves: &'a [bool],
    /// Per leaf, once a walked one is located, the longest one shard took.
    pub located_us: &'a [Option<f64>],
    /// µs per located row: the index's measured cost, or its default.
    pub row_us: f64,
    pub doc_lens: &'a [u64],
}

/// A read sample: µs per document, and the share of the documents that
/// hold every candidate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub us: f64,
    pub held: f64,
}

/// Demote a candidate when its locate estimate exceeds reading the
/// reference's documents in [`Group::within`]: the band floor keeps it, the
/// ceiling demotes it, in between one read sample of [`SAMPLE_DOCS`] decides
/// (a failed one counts as the ceiling). `read_us` reads the given
/// documents.
///
/// A literal candidate's locate is the reference's probe time scaled by
/// their occurrences. A walked candidate's is its busiest shard's rows (the
/// shards locate in parallel) at the reference's time per row of its own
/// busiest shard, clamped as a measured row cost is, and at most
/// `walked.row_us`: for a walked reference, the time of its slowest shard,
/// since a reference of few rows probes in mostly the fixed cost of
/// starting a probe, which scaling would multiply.
///
/// For a walked candidate only the documents that lack the candidates
/// count: a folded leaf decides no document by itself, so locating it still
/// reads the others. A sample measures that share, so the band floor no
/// longer decides alone. The band prices a document of the usual size; a
/// large one, whose plain read at [`BYTE_NS`] already costs more than the
/// band ceiling, costs its bytes at [`LARGE_BYTE_NS`], is not sampled, and
/// counts as lacking the candidates. Below the ceiling, a walked candidate
/// is sampled unless the sample may cost, at the ceiling, as much as
/// demoting can save: locating it, less reading the large documents.
pub fn decide(
    plan: &mut Plan,
    band: ReadBand,
    stage1: &[Option<&Answer>],
    walked: &Walked<'_>,
    read_us: &mut dyn FnMut(&[usize]) -> Result<Sample, String>,
) {
    let mut sampled = None;
    let large = |d: usize| walked.doc_lens[d] as f64 * BYTE_NS / 1000.0 > band.max_us;
    for g in std::mem::take(&mut plan.groups) {
        let a = stage1[g.reference].expect("a reference is probed before its candidates");
        let located = a.exact.iter().filter(|e| e.1 > 0).map(|e| e.0);
        let docs: Vec<usize> = (located.chain(a.candidates.iter().flatten().copied()))
            .filter(|&d| contains(&g.within, d))
            .collect();
        let occ_m = occurrences(&plan.counts[g.reference], &g.within);
        let reference_us = (walked.located_us[g.reference]).unwrap_or(a.probe_us.max(1) as f64);
        let per_row = reference_us / busiest(&plan.counts[g.reference]).max(1) as f64;
        let row_us = read::locate_us(Some(per_row)).min(walked.row_us);
        // For a walked candidate, the documents of the usual size, and the
        // large ones at their bytes.
        let (usual, big): (Vec<usize>, Vec<usize>) =
            match g.candidates.iter().any(|&i| walked.leaves[i]) {
                true => docs.iter().copied().partition(|&d| !large(d)),
                false => (docs.clone(), Vec::new()),
            };
        let big_us: f64 = (big.iter())
            .map(|&d| walked.doc_lens[d] as f64 * LARGE_BYTE_NS / 1000.0)
            .sum();
        let ids = spread(&usual, SAMPLE_DOCS);
        for &i in &g.candidates {
            let walk = walked.leaves[i];
            let occ_i = occurrences(&plan.counts[i], &g.within);
            let (locate_us, rows) = match walk {
                true => {
                    let rows = busiest(&plan.counts[i]);
                    (rows as f64 * row_us, Some((rows, row_us)))
                }
                false => (locate_est_us(a.probe_us, occ_m, occ_i), None),
            };
            // Reading at `us` a usual document, but for the share `held`
            // that locating reads too, and the large ones.
            let read_total = |us: f64, held: f64| match walk {
                true => usual.len() as f64 * us * (1.0 - held) + big_us,
                false => docs.len() as f64 * us,
            };
            let cost = |us, held| match walk {
                true => docs.is_empty() || locate_us > read_total(us, held),
                false => demote_by_cost(a.probe_us, occ_m, docs.len(), occ_i, us),
            };
            let ceiling = cost(band.max_us, 0.0);
            // What a sample may cost a walked candidate, when that is no
            // less than demoting can save.
            let unsampled = (walk && !ceiling && sampled.is_none())
                .then_some(ids.len() as f64 * band.max_us)
                .filter(|&us| locate_us - big_us <= us);
            let (demote, us, held, read) = match unsampled {
                Some(us) => {
                    let read = Read::Unsampled {
                        docs: ids.len(),
                        us,
                    };
                    (false, band.max_us, 0.0, read)
                }
                None => match (walk || cost(band.min_us, 0.0), ceiling) {
                    (false, _) => (false, band.min_us, 0.0, Read::Floor),
                    (true, true) => (true, band.max_us, 0.0, Read::Ceiling),
                    (true, false) => {
                        let (us, held, read, of) =
                            *sampled.get_or_insert_with(|| match read_us(&ids) {
                                Ok(Sample { us, held }) => {
                                    (us, held, Read::Sampled(ids.len()), g.reference)
                                }
                                Err(_) => (band.max_us, 0.0, Read::Failed, g.reference),
                            });
                        // Another group's documents say nothing of this one's.
                        let held = if of == g.reference { held } else { 0.0 };
                        (cost(us, held), us, held, read)
                    }
                },
            };
            plan.why[i] = Why::Cost {
                reference: g.reference,
                locate_us,
                rows,
                docs: docs.len(),
                read_us: us,
                read_all_us: read_total(us, 0.0),
                held: walk.then_some(held),
                large: walk.then_some((big.len(), big_us)),
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

/// The most occurrences one shard counts.
fn busiest(counts: &[(Range<usize>, u64)]) -> u64 {
    counts.iter().map(|c| c.1).max().unwrap_or(0)
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

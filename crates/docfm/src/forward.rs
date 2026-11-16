//! Forward locate: rows with their own mark first, then an occurrence at `p`
//! from a mark at `p + i` in a probed tail `path[i..]`, `i` LF steps back.

use std::cell::Cell;
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::build::word_start;
use crate::marks::Marks;
use crate::row::Row;
use crate::{Cursor, DOC_END, LocateStats, RING, View};

/// A tail is used while it holds at most `RATIO * count / d` marks, `d` the
/// text bytes per mark: fewer than `RATIO / 2` candidates per occurrence over
/// all depths.
const RATIO: u64 = 32;

/// Deepest tail used.
pub const MAX_DEPTH: usize = 15;

/// Rows one task walks back when a walk runs in parallel. A row costs 0.25
/// to 0.7 µs to walk back and a task about 45 µs to start on a parked
/// thread (Apple M5), so a task's start costs about 1% of its walk. A walk
/// of fewer than two tasks' rows runs on the caller's thread.
pub(crate) const GRAIN: usize = 8192;

/// Marks in `from..to`; decreasing ranks or more marks than rows mean a corrupt blob.
fn marks_in(marks: &Marks<'_>, from: Row, to: Row) -> usize {
    let inside = (marks.get_rank(to.get()).1)
        .checked_sub(marks.get_rank(from.get()).1)
        .expect("corrupt docfm index: mark ranks decrease");
    assert!(
        inside <= to - from,
        "corrupt docfm index: more marks than rows"
    );
    inside
}

impl Cursor<'_> {
    /// The deepest tail whose marks lie in the occurrence's document.
    fn depth_cap(&self) -> usize {
        let known = (self.len as usize).min(RING);
        (0..known)
            .find(|&j| self.path(j) == DOC_END)
            .unwrap_or(self.len.saturating_sub(1) as usize)
            .min(MAX_DEPTH)
    }

    /// Whether tail `i` is probed: `path[i]` can start a word, subword or line.
    pub(super) fn probed(&self, i: usize) -> bool {
        let (prev, b) = (self.path(i - 1), self.path(i));
        word_start(prev, b)
            || matches!(prev, b' ' | b'\t' | b'\r' | b'\n') && !b.is_ascii_whitespace()
    }

    /// The depth a locate uses, stopping before the first probed tail failing
    /// the [`RATIO`] gate over the rows without their own mark.
    pub(super) fn forward_depth(&self) -> usize {
        let header = self.view.header;
        let limit = self.depth_cap().min(header.depth_limit());
        let marks = header.marks(self.view.bytes);
        let own = marks_in(&marks, self.lo, self.hi);
        let count = self.count() - own as u64;
        if count == 0 {
            return 0;
        }
        let (num, den) = header.density();
        let budget = u128::from(RATIO * count) * u128::from(den);
        (1..=limit)
            .filter(|&i| self.probed(i))
            .take_while(|&i| {
                let (from, to) = self.tail(i);
                let inside = marks_in(&marks, from, to);
                inside as u128 * u128::from(num) <= budget
            })
            .last()
            .unwrap_or(0)
    }

    /// [`Cursor::locate_from`] counted, untimed.
    #[cfg(test)]
    pub(super) fn locate_at(&self, depth: usize, visit: &mut dyn FnMut(usize)) -> LocateStats {
        self.locate_from::<true>(depth, visit, None)
    }

    /// Visits the doc of every occurrence: from the rows' own marks, then
    /// from tails `1..=depth` (clamped to the cap), then by walking back.
    /// Counted under `STATS`, timed from `start` if given.
    pub(super) fn locate_from<const STATS: bool>(
        &self,
        depth: usize,
        visit: &mut dyn FnMut(usize),
        start: Option<Instant>,
    ) -> LocateStats {
        let mut stats = LocateStats::default();
        if self.root || self.lo == self.hi {
            return stats;
        }
        let left = self.resolve_marks::<STATS>(depth, &mut stats.probes, visit);
        let walk_start = if STATS {
            start.map(|_| Instant::now())
        } else {
            None
        };
        let (steps, longest) = walk_back::<STATS>(self.view, std::slice::from_ref(&left), visit);
        if STATS {
            stats.occurrences = self.count();
            stats.zero_step = left.own as u64;
            stats.forward = (left.hi - left.lo - left.own - left.count) as u64;
            stats.walk_steps = steps;
            stats.max_walk = longest as u32;
            if let (Some(start), Some(walk_start)) = (start, walk_start) {
                stats.forward_ns = (walk_start - start).as_nanos() as u64;
                stats.walk_ns = walk_start.elapsed().as_nanos() as u64;
            }
        }
        stats
    }

    /// Visits the doc of each row its own mark or a probed tail `1..=depth`
    /// (clamped to the cap) resolves; returns the rows left to walk back.
    /// Under `STATS` adds the tail marks probed to `probes`. Neither the
    /// root nor empty.
    fn resolve_marks<const STATS: bool>(
        &self,
        depth: usize,
        probes: &mut u64,
        visit: &mut dyn FnMut(usize),
    ) -> Left {
        let (lo, hi, view) = (self.lo, self.hi, self.view);
        let mut resolved = vec![0u64; (hi - lo).div_ceil(64)];
        let mut own = 0;
        for (row, rank) in view.header.marks(view.bytes).range(lo.get(), hi.get()) {
            let at = Row::new(row) - lo;
            resolved[at / 64] |= 1 << (at % 64);
            visit(view.sampled(rank));
            own += 1;
        }
        let tails =
            own < hi - lo && self.resolve_into::<STATS>(depth, &mut resolved, probes, visit);
        let ones = match tails {
            true => resolved.iter().map(|w| w.count_ones() as usize).sum(),
            false => own,
        };
        Left {
            lo,
            hi,
            filtered: tails || own > 0,
            resolved,
            own,
            count: hi - lo - ones,
        }
    }

    /// Visits the doc of each occurrence a probed tail `1..=depth` (clamped
    /// to the cap) resolves, once; returns those rows as a bitmap over
    /// `lo..hi`, `None` at depth 0.
    #[cfg(test)]
    pub(super) fn resolve(&self, depth: usize, visit: &mut dyn FnMut(usize)) -> Option<Vec<u64>> {
        let mut resolved = vec![0u64; (self.hi - self.lo).div_ceil(64)];
        self.resolve_into::<false>(depth, &mut resolved, &mut 0, visit)
            .then_some(resolved)
    }

    /// [`Cursor::resolve`] into `resolved`, skipping rows set there and,
    /// under `STATS`, adding the marks probed to `probes`; false at depth 0.
    fn resolve_into<const STATS: bool>(
        &self,
        depth: usize,
        resolved: &mut [u64],
        probes: &mut u64,
        visit: &mut dyn FnMut(usize),
    ) -> bool {
        let (lo, hi, view) = (self.lo, self.hi, self.view);
        let depth = depth.min(self.depth_cap());
        if depth == 0 {
            return false;
        }
        let codes: [u8; MAX_DEPTH] =
            std::array::from_fn(|j| view.header.code_of[usize::from(self.path(j))].unwrap_or(0));
        let marks = view.header.marks(view.bytes);
        for i in (1..=depth).filter(|&i| self.probed(i)) {
            let (from, to) = self.tail(i);
            for (row, rank) in marks.range(from.get(), to.get()) {
                if STATS {
                    *probes += 1;
                }
                let Some(at) = view.back(Row::new(row), &codes[..i]) else {
                    continue;
                };
                assert!(
                    (lo..hi).contains(&at),
                    "corrupt docfm index: a forward walk left the interval"
                );
                let (word, bit) = ((at - lo) / 64, 1u64 << ((at - lo) % 64));
                if resolved[word] & bit == 0 {
                    resolved[word] |= bit;
                    visit(view.sampled(rank));
                }
            }
        }
        true
    }

    /// The rows of `path[i..]`, `1 <= i < min(len, RING)`.
    fn tail(&self, i: usize) -> (Row, Row) {
        let (from, to) = self.tails[(self.len as usize).wrapping_sub(i) % RING];
        (Row::new(from as usize), Row::new(to as usize))
    }

    /// `path[j]`, `j < min(len, RING)`.
    fn path(&self, j: usize) -> u8 {
        self.front[(self.len as usize).wrapping_sub(1 + j) % RING]
    }
}

/// The rows of one interval that neither their own mark nor a tail
/// resolved: they walk back.
struct Left {
    lo: Row,
    hi: Row,
    /// Whether `resolved` has a bit set.
    filtered: bool,
    /// Bit `r - lo` set: row `r` is resolved.
    resolved: Vec<u64>,
    /// Rows resolved by their own mark.
    own: usize,
    /// Rows left.
    count: usize,
}

impl Left {
    /// The rows left in `from..to`, ascending.
    fn rows(&self, from: Row, to: Row) -> impl Iterator<Item = Row> + '_ {
        let (lo, resolved) = (self.lo, &self.resolved);
        Row::range(from, to).filter(move |&r| resolved[(r - lo) / 64] >> ((r - lo) % 64) & 1 == 0)
    }

    /// [`View::docs`] of the rows left, on the caller's thread.
    fn walk<const STATS: bool>(
        &self,
        view: View<'_>,
        visit: &mut dyn FnMut(usize),
    ) -> (u64, usize) {
        if self.count == 0 {
            (0, 0)
        } else if !self.filtered {
            view.docs::<STATS>(Row::range(self.lo, self.hi), visit)
        } else {
            view.docs::<STATS>(self.rows(self.lo, self.hi), visit)
        }
    }
}

/// [`View::docs`] of the rows `lefts` leave, all of `view`: on the caller's
/// thread below two [`GRAIN`]s of rows, else in tasks of [`GRAIN`] rows on
/// the current rayon pool, each task's docs visited in turn. Under `STATS`
/// the LF steps and the longest walk.
fn walk_back<const STATS: bool>(
    view: View<'_>,
    lefts: &[Left],
    visit: &mut dyn FnMut(usize),
) -> (u64, usize) {
    let join = |(s, m): (u64, usize), (t, n): (u64, usize)| (s + t, m.max(n));
    let rows: usize = lefts.iter().map(|l| l.count).sum();
    if rows < 2 * GRAIN {
        return (lefts.iter())
            .map(|l| l.walk::<STATS>(view, visit))
            .fold((0, 0), join);
    }
    let cuts = cuts(lefts);
    #[cfg(test)]
    TASKS.with(|t| t.set(t.get() + cuts.len() - 1));
    let start = Instant::now();
    let parts: Vec<(Vec<usize>, (u64, usize), Duration)> = cuts
        .par_windows(2)
        .map(|span| {
            let task = Instant::now();
            let mut docs = Vec::with_capacity(GRAIN);
            let rows = between(lefts, span[0], span[1]);
            let walked = view.docs::<STATS>(rows, &mut |d| docs.push(d));
            (docs, walked, task.elapsed())
        })
        .collect();
    let work: Duration = parts.iter().map(|p| p.2).sum();
    SAVED.with(|s| s.set(s.get() + work.saturating_sub(start.elapsed())));
    parts.into_iter().fold((0, 0), |sum, (docs, walked, _)| {
        docs.into_iter().for_each(&mut *visit);
        join(sum, walked)
    })
}

/// A place among the rows of `lefts`: row `.1` of `lefts[.0]`.
type Place = (usize, Row);

/// Where each task of a parallel walk of the rows `lefts` leave begins,
/// then where the last one ends: tasks of [`GRAIN`] rows, the last of the
/// rest. Counts the rows left a word of each bitmap at a time.
fn cuts(lefts: &[Left]) -> Vec<Place> {
    let mut cuts = vec![(0, lefts[0].lo)];
    // The rows the task begun last still takes.
    let mut need = GRAIN;
    for (i, l) in lefts.iter().enumerate() {
        for (w, &word) in l.resolved.iter().enumerate() {
            let at = l.lo + 64 * w;
            let mut left = !word & (u64::MAX >> (64 - (l.hi - at).min(64)));
            while left.count_ones() as usize >= need {
                for _ in 1..need {
                    left &= left - 1;
                }
                cuts.push((i, at + left.trailing_zeros() as usize + 1));
                left &= left - 1;
                need = GRAIN;
            }
            need -= left.count_ones() as usize;
        }
    }
    // A cut after the last row left would begin an empty task.
    if need == GRAIN {
        cuts.pop();
    }
    cuts.push((lefts.len() - 1, lefts[lefts.len() - 1].hi));
    cuts
}

/// The rows `lefts` leave from place `from` to place `to`, ascending.
fn between(lefts: &[Left], (i, from): Place, (j, to): Place) -> impl Iterator<Item = Row> + '_ {
    (lefts[i..=j].iter().enumerate()).flat_map(move |(k, l)| {
        let lo = if k == 0 { from } else { l.lo };
        let hi = if i + k == j { to } else { l.hi };
        l.rows(lo, hi)
    })
}

thread_local! {
    /// What the parallel walks begun on this thread saved: their tasks'
    /// time summed, less the time spent waiting for them.
    pub(super) static SAVED: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

#[cfg(test)]
thread_local! {
    /// Tasks the parallel walks begun on this thread split into.
    pub(crate) static TASKS: Cell<usize> = const { Cell::new(0) };
}

impl View<'_> {
    /// The row `codes.len()` LF steps before `row` if each step reads the
    /// next of `codes` from the back, else `None`.
    #[inline]
    fn back(&self, mut row: Row, codes: &[u8]) -> Option<Row> {
        for &want in codes.iter().rev() {
            if row == Row::new(self.header.sentinel_row) {
                return None;
            }
            let (code, rank) = self.header.access_rank(self.bytes, self.bwt_pos(row));
            if code != want {
                return None;
            }
            row = self.lf_to(want, rank);
        }
        Some(row)
    }
}

#[cfg(test)]
mod cuts_tests {
    use super::*;

    /// A [`Left`] over the rows from `lo`, one per flag, those flagged
    /// resolved.
    fn left(lo: usize, flags: impl Iterator<Item = bool>) -> Left {
        let flags: Vec<bool> = flags.collect();
        let mut resolved = vec![0u64; flags.len().div_ceil(64)];
        for at in (0..flags.len()).filter(|&at| flags[at]) {
            resolved[at / 64] |= 1 << (at % 64);
        }
        let count = flags.iter().filter(|&&f| !f).count();
        Left {
            lo: Row::new(lo),
            hi: Row::new(lo + flags.len()),
            filtered: count < flags.len(),
            resolved,
            own: 0,
            count,
        }
    }

    /// Resolved: about one row in four, rolled from `r`.
    fn rolled(r: usize) -> bool {
        (r.wrapping_mul(2_654_435_761) >> 7) & 3 == 0
    }

    /// Each task walks the next [`GRAIN`] rows left, the last task the
    /// rest: in order, each row once, none resolved, and no task empty.
    #[test]
    fn cuts_split_the_rows_left_into_grains() {
        let flat = |n: usize| std::iter::repeat_n(false, n);
        let cases: Vec<Vec<Left>> = vec![
            vec![left(0, flat(2 * GRAIN))],
            vec![left(5, flat(2 * GRAIN + 1))],
            // A cut at an interval's end, and resolved rows after the last
            // row left.
            vec![
                left(0, flat(GRAIN).chain([true; 3])),
                left(9000, flat(2 * GRAIN).chain([true; 70])),
            ],
            // Many small intervals, a quarter of their rows resolved.
            (0..300)
                .map(|k| left(k * 1000, (0..100 + k % 37).map(|r| rolled(k * 1000 + r))))
                .collect(),
        ];
        for lefts in &cases {
            let all: Vec<Row> = (lefts.iter()).flat_map(|l| l.rows(l.lo, l.hi)).collect();
            let rows: usize = lefts.iter().map(|l| l.count).sum();
            assert!(rows >= 2 * GRAIN && rows == all.len());
            let cuts = cuts(lefts);
            let tasks: Vec<Vec<Row>> = (cuts.windows(2))
                .map(|w| between(lefts, w[0], w[1]).collect())
                .collect();
            assert_eq!(tasks.len(), rows.div_ceil(GRAIN));
            let (last, full) = tasks.split_last().unwrap();
            assert!(full.iter().all(|t| t.len() == GRAIN));
            assert!((1..=GRAIN).contains(&last.len()));
            assert_eq!(tasks.concat(), all);
        }
    }
}

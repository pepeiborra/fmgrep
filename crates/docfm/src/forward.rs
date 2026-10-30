//! Forward locate: rows with their own mark first, then an occurrence at `p`
//! from a mark at `p + i` in a probed tail `path[i..]`, `i` LF steps back.

use crate::build::word_start;
use crate::{Cursor, DOC_END, RING, View};

/// A tail is used while it holds at most `RATIO * count / d` marks, `d` the
/// text bytes per mark: fewer than `RATIO / 2` candidates per occurrence over
/// all depths.
const RATIO: u64 = 32;

/// Deepest tail used.
pub const MAX_DEPTH: usize = 15;

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
        let own = marks.get_rank(self.hi).1 - marks.get_rank(self.lo).1;
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
                let inside = marks.get_rank(to).1 - marks.get_rank(from).1;
                inside as u128 * u128::from(num) <= budget
            })
            .last()
            .unwrap_or(0)
    }

    /// Visits the doc of every occurrence: from the rows' own marks, then
    /// from tails `1..=depth` (clamped to the cap), then by walking back.
    pub(super) fn locate_at(&self, depth: usize, visit: &mut dyn FnMut(usize)) {
        let (lo, hi, view) = (self.lo, self.hi, self.view);
        if self.root || lo == hi {
            return;
        }
        let mut resolved = vec![0u64; (hi - lo).div_ceil(64)];
        let mut own = 0;
        for (row, rank) in view.header.marks(view.bytes).range(lo, hi) {
            resolved[(row - lo) / 64] |= 1 << ((row - lo) % 64);
            visit(view.sampled(rank));
            own += 1;
        }
        if own == hi - lo {
            return;
        }
        if !self.resolve_into(depth, &mut resolved, visit) && own == 0 {
            return view.docs(lo..hi, visit);
        }
        view.docs(
            (lo..hi).filter(|&r| resolved[(r - lo) / 64] >> ((r - lo) % 64) & 1 == 0),
            visit,
        );
    }

    /// Visits the doc of each occurrence a probed tail `1..=depth` (clamped
    /// to the cap) resolves, once; returns those rows as a bitmap over
    /// `lo..hi`, `None` at depth 0.
    #[cfg(test)]
    pub(super) fn resolve(&self, depth: usize, visit: &mut dyn FnMut(usize)) -> Option<Vec<u64>> {
        let mut resolved = vec![0u64; (self.hi - self.lo).div_ceil(64)];
        self.resolve_into(depth, &mut resolved, visit)
            .then_some(resolved)
    }

    /// [`Cursor::resolve`] into `resolved`, skipping rows set there; false
    /// at depth 0.
    fn resolve_into(
        &self,
        depth: usize,
        resolved: &mut [u64],
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
            for (row, rank) in marks.range(from, to) {
                let Some(at) = view.back(row, &codes[..i]) else {
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
    fn tail(&self, i: usize) -> (usize, usize) {
        let (from, to) = self.tails[(self.len as usize).wrapping_sub(i) % RING];
        (from as usize, to as usize)
    }

    /// `path[j]`, `j < min(len, RING)`.
    fn path(&self, j: usize) -> u8 {
        self.front[(self.len as usize).wrapping_sub(1 + j) % RING]
    }
}

impl View<'_> {
    /// The row `codes.len()` LF steps before `row` if each step reads the
    /// next of `codes` from the back, else `None`.
    fn back(&self, mut row: usize, codes: &[u8]) -> Option<usize> {
        let bwt = self.bwt();
        for &want in codes.iter().rev() {
            if row == self.header.sentinel_row {
                return None;
            }
            let at = self.bwt_index(row);
            if bwt.access(at) != want {
                return None;
            }
            row =
                (self.header.c[usize::from(want)] as usize + bwt.rank(want, at)).min(self.header.n);
        }
        Some(row)
    }
}

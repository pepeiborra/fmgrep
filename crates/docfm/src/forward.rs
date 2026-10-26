//! Forward locate: an occurrence at `p` resolves from a mark at `p + i` in
//! the rows of the tail `path[i..]`, verified by `i` LF steps back.

use crate::{Cursor, DOC_END, RING, View};

/// A tail is used while it holds at most `RATIO * count / rate` marks:
/// fewer than `RATIO / 2` candidates per occurrence over all depths.
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

    /// The depth a locate uses: within the cap and `rate / 2 - 1`, stopping
    /// before the first tail whose marks fail the [`RATIO`] gate.
    pub(super) fn forward_depth(&self) -> usize {
        let rate = self.view.header.sample_rate as usize;
        let limit = self.depth_cap().min((rate / 2).saturating_sub(1));
        let marks = self.view.header.marks(self.view.bytes);
        let budget = RATIO * self.count();
        (1..=limit)
            .take_while(|&i| {
                let (from, to) = self.tail(i);
                let inside = marks.get_rank(to).1 - marks.get_rank(from).1;
                inside as u64 * rate as u64 <= budget
            })
            .last()
            .unwrap_or(0)
    }

    /// Visits the doc of every occurrence, resolving from tails `1..=depth`
    /// (clamped to the cap) first.
    pub(super) fn locate_at(&self, depth: usize, visit: &mut dyn FnMut(usize)) {
        let (lo, hi, view) = (self.lo, self.hi, self.view);
        if self.root || lo == hi {
            return;
        }
        match self.resolve(depth, visit) {
            None => view.docs(lo..hi, visit),
            Some(resolved) => view.docs(
                (lo..hi).filter(|&r| resolved[(r - lo) / 64] >> ((r - lo) % 64) & 1 == 0),
                visit,
            ),
        }
    }

    /// Visits the doc of each occurrence a tail `1..=depth` (clamped to the
    /// cap) resolves, once; returns those rows as a bitmap over `lo..hi`,
    /// `None` at depth 0.
    pub(super) fn resolve(&self, depth: usize, visit: &mut dyn FnMut(usize)) -> Option<Vec<u64>> {
        let (lo, hi, view) = (self.lo, self.hi, self.view);
        let depth = depth.min(self.depth_cap());
        if depth == 0 {
            return None;
        }
        let codes: [u8; MAX_DEPTH] =
            std::array::from_fn(|j| view.header.code_of[usize::from(self.path(j))].unwrap_or(0));
        let marks = view.header.marks(view.bytes);
        let mut resolved = vec![0u64; (hi - lo).div_ceil(64)];
        for i in 1..=depth {
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
        Some(resolved)
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

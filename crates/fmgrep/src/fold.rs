//! Case-folded backward search. A `-i` term compiles to a case-folded HIR and
//! a [`FoldPlan`] reads the per-position alternatives off it, so each step of
//! the branching walk over the FM-index accepts exactly what `regex -i`
//! accepts there, Unicode variants (`k` also folds to U+212A, `s` to U+017F)
//! included. Every alternative is a whole byte string, so no branch is a
//! partial UTF-8 sequence. Budgets widen as in [`crate::automaton`].

use crate::UNIFIED_SEPARATOR;
use regex_syntax::hir::{Class, Hir, HirKind};

use crate::cursor::{BackwardCursor, LocatingCursor};

/// Widest fold class one position may carry. Anything wider is not a
/// case-folded character and the whole plan is refused.
const MAX_STEP_ALTS: usize = 8;

/// Longest plan, in steps.
const MAX_STEPS: usize = 64;

/// Live intervals a walk holds before it gives up and widens.
const MAX_LIVE: usize = 256;

/// A case-folded term as a branching backward search: one step per pattern
/// position, in text order, each step the byte strings a match may carry
/// there. Adjacent single-alternative steps are merged at plan time.
pub struct FoldPlan {
    steps: Vec<Vec<Box<[u8]>>>,
}

impl FoldPlan {
    #[cfg(test)]
    fn steps(&self) -> &[Vec<Box<[u8]>>] {
        &self.steps
    }
}

/// Collect `hir`'s positions into `steps`, or refuse the shape.
fn collect(hir: &Hir, steps: &mut Vec<Vec<Box<[u8]>>>) -> Option<()> {
    match hir.kind() {
        HirKind::Literal(lit) => {
            if lit.0.is_empty() {
                return None;
            }
            steps.push(vec![lit.0.to_vec().into_boxed_slice()]);
        }
        HirKind::Class(Class::Unicode(class)) => {
            let mut alts: Vec<Box<[u8]>> = Vec::new();
            for range in class.ranges() {
                for ch in range.start()..=range.end() {
                    if alts.len() == MAX_STEP_ALTS {
                        return None;
                    }
                    let mut buf = [0u8; 4];
                    alts.push(ch.encode_utf8(&mut buf).as_bytes().into());
                }
            }
            if alts.is_empty() {
                return None;
            }
            steps.push(alts);
        }
        HirKind::Class(Class::Bytes(class)) => {
            let mut alts: Vec<Box<[u8]>> = Vec::new();
            for range in class.ranges() {
                for byte in range.start()..=range.end() {
                    if alts.len() == MAX_STEP_ALTS {
                        return None;
                    }
                    alts.push(vec![byte].into_boxed_slice());
                }
            }
            if alts.is_empty() {
                return None;
            }
            steps.push(alts);
        }
        HirKind::Capture(cap) => collect(&cap.sub, steps)?,
        HirKind::Concat(subs) => {
            for sub in subs {
                collect(sub, steps)?;
            }
        }
        HirKind::Empty | HirKind::Look(_) | HirKind::Repetition(_) | HirKind::Alternation(_) => {
            return None;
        }
    }
    Some(())
}

/// Plan `hir` (already case-folded) when it is a concatenation of literals
/// and small classes; `None` for any other shape, an over-wide class, an
/// over-long term, or any alternative holding [`UNIFIED_SEPARATOR`].
pub fn fold_plan(hir: &Hir) -> Option<FoldPlan> {
    let mut steps = Vec::new();
    collect(hir, &mut steps)?;
    if steps.is_empty() || steps.len() > MAX_STEPS {
        return None;
    }
    // A separator-bearing branch could match across a shard's document
    // boundary.
    if steps
        .iter()
        .flatten()
        .any(|alt| alt.is_empty() || alt.contains(&UNIFIED_SEPARATOR))
    {
        return None;
    }
    let mut merged: Vec<Vec<Box<[u8]>>> = Vec::with_capacity(steps.len());
    for step in steps {
        match merged.last_mut() {
            Some(last) if last.len() == 1 && step.len() == 1 => {
                let mut bytes = last[0].to_vec();
                bytes.extend_from_slice(&step[0]);
                last[0] = bytes.into_boxed_slice();
            }
            _ => merged.push(step),
        }
    }
    Some(FoldPlan { steps: merged })
}

/// The intervals of the folded matches that actually occur, or `None` when
/// the live-interval cap was hit.
fn fold_walk<C: BackwardCursor>(root: C, plan: &FoldPlan, cap: usize) -> Option<Vec<C>> {
    let mut live = vec![root];
    let mut next = Vec::new();
    for alts in plan.steps.iter().rev() {
        next.clear();
        for cursor in &live {
            for alt in alts {
                let ext = cursor.extend_front(alt);
                if ext.count() > 0 {
                    next.push(ext);
                }
            }
        }
        if next.is_empty() {
            return Some(Vec::new());
        }
        if next.len() > cap {
            return None;
        }
        std::mem::swap(&mut live, &mut next);
    }
    Some(live)
}

/// Boolean form of the walk, kept as [`fold_locate`]'s oracle: `Some(true)`
/// when some branch survives every step, `Some(false)` when none does, `None`
/// when the live-interval cap was hit.
#[cfg(test)]
fn fold_any_capped<C: BackwardCursor>(root: C, plan: &FoldPlan, cap: usize) -> Option<bool> {
    fold_walk(root, plan, cap).map(|live| !live.is_empty())
}

/// Located folded matches, or `None` when the live-interval cap
/// was hit. Each surviving interval is a whole match, so no term is searched
/// a second time.
pub fn fold_locate<C: LocatingCursor>(root: C, plan: &FoldPlan) -> Option<Vec<usize>> {
    fold_locate_capped(root, plan, MAX_LIVE)
}

fn fold_locate_capped<C: LocatingCursor>(
    root: C,
    plan: &FoldPlan,
    cap: usize,
) -> Option<Vec<usize>> {
    let live = fold_walk(root, plan, cap)?;
    let mut hits = Vec::new();
    for cursor in &live {
        cursor.locate(&mut |hit| hits.push(hit));
    }
    Some(hits)
}

#[cfg(test)]
mod tests;

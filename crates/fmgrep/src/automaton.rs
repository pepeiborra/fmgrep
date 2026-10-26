//! Regex candidate search as a branching backward walk: the pattern compiles
//! to a reversed, anchored byte DFA, stepped while an FM interval is extended
//! front to back, so a branch dies when its interval empties. Acceptance is
//! read at the node through [`Automaton::next_eoi_state`], since
//! regex-automata reports a match one byte after its last byte.

use std::sync::OnceLock;

use crate::UNIFIED_SEPARATOR;
use regex_automata::dfa::{Automaton, StartKind, dense};
use regex_automata::nfa::thompson;
use regex_automata::util::alphabet::{ByteClasses, Unit};
use regex_automata::util::primitives::StateID;
use regex_automata::util::syntax;
use regex_automata::{Anchored, MatchKind};
use regex_syntax::hir::{Class, ClassBytesRange, ClassUnicode, Hir, HirKind};

use crate::cursor::{BackwardCursor, LocatingCursor};
use crate::matching::MatchMode;
use crate::pattern::required_literal;

/// What a walk may spend before it gives up and widens.
struct Caps {
    /// Live intervals one frontier may hold.
    live: usize,
    /// Bytes one branch may walk backward.
    depth: usize,
    /// Node expansions per walk.
    nodes: usize,
    /// Distinct bytes one state may admit.
    class_bytes: usize,
}

const CAPS: Caps = Caps {
    live: 256,
    depth: 64,
    nodes: 20_000,
    class_bytes: 64,
};

const DFA_BYTES: usize = 8 << 20;

/// Longest required literal whose pattern plans its walk at prepare time.
pub const MAX_ANCHOR: usize = 3;

/// Shard-wide anchor occurrences at which a longer anchor's pattern
/// walks its [`DeferredPlan`] instead of locating the anchor.
pub const MIN_ANCHOR_COUNT: u64 = 256;

/// A regex as a branching backward search: a reversed, anchored byte DFA
/// stepped while the FM interval is extended front-to-back.
pub struct RegexPlan {
    dfa: dense::DFA<Vec<u32>>,
    start: StateID,
}

/// Whether some match of `hir` can hold a NUL byte. Such a pattern never
/// prunes: a shard's separator is NUL, so the indexed text is not the raw
/// document.
fn can_match_nul(hir: &Hir) -> bool {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => false,
        HirKind::Literal(lit) => lit.0.contains(&UNIFIED_SEPARATOR),
        HirKind::Class(Class::Unicode(class)) => class.ranges().iter().any(|r| r.start() == '\0'),
        HirKind::Class(Class::Bytes(class)) => class
            .ranges()
            .iter()
            .any(|r| r.start() == UNIFIED_SEPARATOR),
        HirKind::Capture(cap) => can_match_nul(&cap.sub),
        HirKind::Repetition(rep) => can_match_nul(&rep.sub),
        HirKind::Concat(subs) | HirKind::Alternation(subs) => subs.iter().any(can_match_nul),
    }
}

/// Whether the string walked into `sid` is a whole match.
fn accepts(dfa: &dense::DFA<Vec<u32>>, sid: StateID) -> bool {
    dfa.is_match_state(dfa.next_eoi_state(sid))
}

/// Plan `src`, whose parse under the query's flags is `hir`.
///
/// The DFA is built with the same syntax flags the authoritative matcher
/// compiles with; a plan built from other flags could prune away a real
/// match.
///
/// `None` — prune with the required literal instead — when the pattern
/// carries look-around (the walk sees no surrounding context) or can match
/// NUL, the DFA has no universal anchored start or exceeds its size limit,
/// the pattern matches the empty string, or its first step is wider than the
/// byte cap and so would widen on every document.
pub fn regex_plan(src: &str, hir: &Hir, mode: MatchMode) -> Option<RegexPlan> {
    if !hir.properties().look_set().is_empty() || can_match_nul(hir) {
        return None;
    }
    plan_capped(src, mode, DFA_BYTES)
}

fn dense_config(dfa_bytes: usize) -> dense::Config {
    dense::Config::new()
        .match_kind(MatchKind::All)
        .start_kind(StartKind::Anchored)
        .accelerate(false)
        .specialize_start_states(false)
        .dfa_size_limit(Some(dfa_bytes))
        .determinize_size_limit(Some(dfa_bytes))
}

fn plan_capped(src: &str, mode: MatchMode, dfa_bytes: usize) -> Option<RegexPlan> {
    let dotall = mode.multiline && mode.dotall;
    let dfa = dense::Builder::new()
        .configure(dense_config(dfa_bytes))
        .syntax(
            syntax::Config::new()
                .multi_line(true)
                .dot_matches_new_line(dotall)
                .case_insensitive(mode.ignore_case)
                .utf8(false),
        )
        .thompson(thompson::Config::new().reverse(true).utf8(false))
        .build(src)
        .ok()?;
    checked(dfa)
}

/// Plan an already parsed `hir`: its flags are in the tree.
fn plan_hir(hir: &Hir, dfa_bytes: usize) -> Option<RegexPlan> {
    let nfa = thompson::Compiler::new()
        .configure(
            thompson::Config::new()
                .reverse(true)
                .utf8(false)
                .which_captures(thompson::WhichCaptures::None),
        )
        .build_from_hir(hir)
        .ok()?;
    let dfa = dense::Builder::new()
        .configure(dense_config(dfa_bytes))
        .build_from_nfa(&nfa)
        .ok()?;
    checked(dfa)
}

/// `dfa` as a plan, unless it has no universal anchored start, accepts the
/// empty string, or its first step is over the byte cap.
fn checked(dfa: dense::DFA<Vec<u32>>) -> Option<RegexPlan> {
    let start = dfa.universal_start_state(Anchored::Yes)?;
    if accepts(&dfa, start) {
        return None;
    }
    live_bytes(
        &dfa,
        dfa.byte_classes(),
        start,
        CAPS.class_bytes,
        &mut Vec::new(),
    )?;
    Some(RegexPlan { dfa, start })
}

/// A plan compiled on first demand: only a shard where the pattern's anchor
/// is common walks it.
pub struct DeferredPlan {
    core: Hir,
    plan: OnceLock<Option<RegexPlan>>,
}

impl DeferredPlan {
    /// `None` when `hir` has no [`core()`] holding `anchor`.
    pub fn new(hir: &Hir, anchor: &[u8]) -> Option<Self> {
        Some(Self {
            core: core(hir, anchor, CAPS.class_bytes)?,
            plan: OnceLock::new(),
        })
    }

    /// The plan, compiled at most once.
    pub fn plan(&self) -> Option<&RegexPlan> {
        self.plan
            .get_or_init(|| plan_hir(&self.core, DFA_BYTES))
            .as_ref()
    }

    /// Whether a plan was compiled and usable.
    #[cfg(test)]
    pub fn built(&self) -> bool {
        self.plan.get().is_some_and(Option::is_some)
    }
}

/// What of `hir` a walk may follow to completion: the longest run of
/// plannable top-level concat items whose required literal holds `anchor`,
/// so the walk never yields more than the anchor would. `None` when there
/// is none or it is a plain literal.
fn core(hir: &Hir, anchor: &[u8], class_bytes: usize) -> Option<Hir> {
    let top = match hir.kind() {
        HirKind::Capture(cap) => &cap.sub,
        _ => hir,
    };
    let runs: Vec<Hir> = match top.kind() {
        HirKind::Concat(items) => items
            .split(|item| !plannable(item, class_bytes))
            .map(|run| Hir::concat(run.to_vec()))
            .collect(),
        _ if plannable(top, class_bytes) => vec![top.clone()],
        _ => Vec::new(),
    };
    runs.into_iter()
        .filter(|run| {
            let props = run.properties();
            !props.is_literal()
                && props.minimum_len().is_some_and(|n| n > 0)
                && required_literal(run)
                    .is_some_and(|lit| lit.windows(anchor.len()).any(|w| w == anchor))
        })
        .max_by_key(|run| run.properties().minimum_len())
}

fn plannable(hir: &Hir, class_bytes: usize) -> bool {
    hir.properties().look_set().is_empty() && !can_match_nul(hir) && !wide(hir, class_bytes)
}

/// Whether some class of `hir` ends on more than `class_bytes` distinct
/// bytes, where every walk reaching it gives up.
fn wide(hir: &Hir, class_bytes: usize) -> bool {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) | HirKind::Literal(_) => false,
        HirKind::Class(Class::Bytes(class)) => {
            let len = |r: &ClassBytesRange| usize::from(r.end() - r.start()) + 1;
            class.ranges().iter().map(len).sum::<usize>() > class_bytes
        }
        HirKind::Class(Class::Unicode(class)) => last_bytes(class) > class_bytes,
        HirKind::Capture(cap) => wide(&cap.sub, class_bytes),
        HirKind::Repetition(rep) => wide(&rep.sub, class_bytes),
        HirKind::Concat(subs) | HirKind::Alternation(subs) => {
            subs.iter().any(|h| wide(h, class_bytes))
        }
    }
}

/// Distinct last bytes of the UTF-8 encodings of `class`'s members.
fn last_bytes(class: &ClassUnicode) -> usize {
    let mut seen = [false; 256];
    let mut buf = [0u8; 4];
    for range in class.ranges() {
        let (lo, hi) = (u32::from(range.start()), u32::from(range.end()));
        for ascii in lo..=hi.min(0x7F) {
            seen[ascii as usize] = true;
        }
        let lo = lo.max(0x80);
        if hi >= lo + 63 {
            // 64 consecutive code points end on every continuation byte.
            seen[0x80..=0xBF].fill(true);
            continue;
        }
        for c in (lo..=hi).filter_map(char::from_u32) {
            let last = *c
                .encode_utf8(&mut buf)
                .as_bytes()
                .last()
                .expect("non-empty");
            seen[usize::from(last)] = true;
        }
    }
    seen.iter().filter(|&&s| s).count()
}

/// A frontier entry: the reverse DFA's state and the interval walked into it.
struct Node<C> {
    sid: StateID,
    cursor: C,
    depth: usize,
}

/// Bytes that keep `sid` alive, each with the state it steps to. `None` when
/// more than `cap` do: too many branches to put to the index at one node.
///
/// One probe per equivalence class, not per byte; the class then expands to
/// the members the index must actually be asked about.
fn live_bytes(
    dfa: &dense::DFA<Vec<u32>>,
    classes: &ByteClasses,
    sid: StateID,
    cap: usize,
    out: &mut Vec<(u8, StateID)>,
) -> Option<()> {
    out.clear();
    // `representatives` yields one byte per contiguous run of equal class
    // ids, so a non-contiguous class would be expanded twice.
    let mut seen = [false; 256];
    for rep in classes.representatives(..) {
        let Some(byte) = rep.as_u8() else { continue };
        let class = classes.get(byte) as usize;
        if std::mem::replace(&mut seen[class], true) {
            continue;
        }
        let to = dfa.next_state(sid, byte);
        if dfa.is_dead_state(to) || dfa.is_quit_state(to) {
            continue;
        }
        for unit in classes.elements(Unit::u8(class as u8)) {
            match unit.as_u8() {
                // Guard on the separator, whatever the DFA still admits.
                Some(UNIFIED_SEPARATOR) | None => continue,
                Some(member) => out.push((member, to)),
            }
        }
        if out.len() > cap {
            return None;
        }
    }
    Some(())
}

/// Breadth-first walk of `plan` over `root`, calling `accepted` on every
/// interval whose walked bytes form a whole match; `accepted` returns false
/// to stop the walk.
///
/// `Some(true)` when some branch was accepted, `Some(false)` when the
/// frontier emptied, `None` when a budget was exhausted.
fn walk<C, F>(root: C, plan: &RegexPlan, caps: &Caps, mut accepted: F) -> Option<bool>
where
    C: BackwardCursor,
    F: FnMut(&C) -> bool,
{
    let dfa = &plan.dfa;
    let classes = dfa.byte_classes();
    let mut live = vec![Node {
        sid: plan.start,
        cursor: root,
        depth: 0,
    }];
    let mut next: Vec<Node<C>> = Vec::new();
    let mut bytes: Vec<(u8, StateID)> = Vec::new();
    let mut run: Vec<u8> = Vec::new();
    let (mut found, mut nodes) = (false, 0usize);
    while !live.is_empty() {
        next.clear();
        for node in &live {
            nodes += 1;
            if nodes > caps.nodes {
                return None;
            }
            if accepts(dfa, node.sid) {
                found = true;
                if !accepted(&node.cursor) {
                    return Some(true);
                }
                continue;
            }
            if node.depth >= caps.depth {
                return None;
            }
            live_bytes(dfa, classes, node.sid, caps.class_bytes, &mut bytes)?;
            if bytes.len() != 1 {
                for &(byte, to) in &bytes {
                    let ext = node.cursor.extend_front(&[byte]);
                    if ext.count() > 0 {
                        next.push(Node {
                            sid: to,
                            cursor: ext,
                            depth: node.depth + 1,
                        });
                    }
                }
                continue;
            }
            // A run of states admitting one byte each is one extension, so
            // the FM-index runs its own multi-byte search.
            run.clear();
            let mut depth = node.depth;
            let mut sid;
            loop {
                let (byte, to) = bytes[0];
                run.push(byte);
                sid = to;
                depth += 1;
                if depth >= caps.depth || accepts(dfa, sid) {
                    break;
                }
                live_bytes(dfa, classes, sid, caps.class_bytes, &mut bytes)?;
                if bytes.len() != 1 {
                    break;
                }
            }
            run.reverse();
            let ext = node.cursor.extend_front(&run);
            if ext.count() > 0 {
                next.push(Node {
                    sid,
                    cursor: ext,
                    depth,
                });
            }
        }
        if next.len() > caps.live {
            return None;
        }
        std::mem::swap(&mut live, &mut next);
    }
    Some(found)
}

/// Boolean form of the walk, kept as [`automaton_locate`]'s oracle; see
/// [`walk`] for the tri-state contract.
#[cfg(test)]
fn any_capped<C: BackwardCursor>(root: C, plan: &RegexPlan, caps: &Caps) -> Option<bool> {
    walk(root, plan, caps, |_| false)
}

/// Located accepted matches, or `None` when a budget was exhausted, and the
/// caller then widens: keeps the document, the shard, or every document. An
/// empty list means the frontier emptied with no truncation.
///
/// Each hit locates a whole match, and every match contains one (the walk
/// stops at the shortest accepting suffix), so the hits' documents are a
/// superset of the matching documents.
pub fn automaton_locate<C: LocatingCursor>(root: C, plan: &RegexPlan) -> Option<Vec<usize>> {
    locate_capped(root, plan, &CAPS)
}

fn locate_capped<C: LocatingCursor>(root: C, plan: &RegexPlan, caps: &Caps) -> Option<Vec<usize>> {
    let mut hits = Vec::new();
    walk(root, plan, caps, |cursor| {
        cursor.locate(&mut |hit| hits.push(hit));
        true
    })?;
    Some(hits)
}

#[cfg(test)]
mod tests;

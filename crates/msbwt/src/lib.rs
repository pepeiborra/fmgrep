//! The multi-string BWT of NUL-terminated documents, built by induced
//! sorting without a suffix array: Holt's msbwt-is
//! (<https://github.com/holtjma/msbwt-is>) over a byte alphabet, in memory,
//! in safe Rust.
//!
//! Each document with its [`DOC_END`] is one cyclic string. Rows are the
//! rotations of every document, compared by their infinite repetitions, so a
//! row never reads into another document; the BWT has one symbol per text
//! byte, the one cyclically before each rotation.
//!
//! Going up, each level names its S* substrings and recurses on the string
//! of names, in which every document again starts with a terminator (the
//! name of its first substring), until every document is one symbol. Coming
//! down, induced sorting runs over (substring, offset) groups rather than
//! text positions: it orders the groups, and the level above orders the rows
//! within each group. From level 2 up, where names are nearly
//! all distinct, levels are sorted by positions instead, as SA-IS does.
//!
//! A build runs its parallel passes on the current rayon pool, and takes
//! texts shorter than `u32::MAX` bytes. Its peak heap has measured about
//! 4.3 times the text, or 5.9 times with [`bwt_sampled`].

#![forbid(unsafe_code)]

use std::cell::Cell;
use std::cmp::Ordering;
use std::hash::Hash;
use std::ops::Range;
use std::sync::OnceLock;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize};

use atomic::Atomic;
use bytemuck::NoUninit;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use typed_index_collections::{TiSlice, TiVec};

#[cfg(test)]
mod tests;

/// Ends every document, the last included.
pub const DOC_END: u8 = 0;

/// Why a build was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The text does not end in [`DOC_END`].
    #[error("the text does not end in a document terminator")]
    Unterminated,
    /// The text, of this many bytes, does not fit `u32` offsets.
    #[error("the text, of {0} bytes, does not fit u32 offsets")]
    TooLong(usize),
}

/// Levels from this one up sort by positions.
const SA_LEVEL: usize = 2;
/// How many items ahead the sorting passes prefetch.
const PREFETCH: usize = 16;
/// How many substrings ahead hashing prefetches.
const NAMING_AHEAD: usize = 16;
/// The fewest items a parallel pass gives each piece.
const PAR_MIN: usize = 1 << 16;

/// What decides a build's switch level, name widths and piece counts.
#[derive(Clone, Copy)]
enum Mode {
    /// The constants above, the input and the current rayon pool.
    Normal,
    /// Tests: small inputs then run every path.
    #[cfg(test)]
    Forced {
        /// The narrowest name width, in bytes.
        width: usize,
        /// How many pieces every parallel pass splits into, however small.
        pieces: Option<usize>,
        /// The level to sort by positions from.
        sa_level: usize,
    },
}

impl Mode {
    /// The level to sort by positions from.
    fn sa_level(self) -> usize {
        match self {
            Mode::Normal => SA_LEVEL,
            #[cfg(test)]
            Mode::Forced { sa_level, .. } => sa_level,
        }
    }

    /// Bytes per symbol of a level with `names` symbols: the narrowest type
    /// that holds them.
    fn width(self, names: usize) -> usize {
        let narrowest = match names {
            0..=0x100 => 1,
            0x101..=0x10000 => 2,
            _ => 4,
        };
        match self {
            Mode::Normal => narrowest,
            #[cfg(test)]
            Mode::Forced { width, .. } => narrowest.max(width),
        }
    }

    /// How many pieces to split `n` items into, one per thread of the
    /// current rayon pool at most, each at least `min` items and [`PAR_MIN`].
    fn pieces(self, n: usize, min: usize) -> usize {
        let threads = rayon::current_num_threads();
        let natural = (n / min.max(PAR_MIN).max(1)).clamp(1, threads);
        match self {
            Mode::Normal => natural,
            #[cfg(test)]
            Mode::Forced { pieces, .. } => pieces.map_or(natural, |k| k.clamp(1, n.max(1))),
        }
    }
}

/// The multi-string BWT of `text`: documents, each followed by one
/// [`DOC_END`], none containing it. An empty document is its terminator
/// alone, one row before itself.
pub fn bwt(text: Vec<u8>) -> Result<Vec<u8>, Error> {
    bwt_in(Mode::Normal, text)
}

/// [`bwt`], decided by `mode`.
fn bwt_in(mode: Mode, mut text: Vec<u8>) -> Result<Vec<u8>, Error> {
    if !checked(&text)? {
        return Ok(text);
    }
    // Each document starts with its terminator: the same cyclic strings.
    text.rotate_right(1);
    Ok(solve::<u8>(text, 256, 1, 0, Want::Nothing, mode).0)
}

/// [`bwt`], and the row of every sampled position of `text`: `sampled` is a
/// bitmap over them, position `p` bit `p % 64` of word `p / 64`. Each row
/// comes as `row << 32 | position`, in no particular order.
pub fn bwt_sampled(text: Vec<u8>, sampled: &[u64]) -> Result<(Vec<u8>, Vec<u64>), Error> {
    bwt_sampled_in(Mode::Normal, text, sampled)
}

/// [`bwt_sampled`], decided by `mode`.
fn bwt_sampled_in(
    mode: Mode,
    mut text: Vec<u8>,
    sampled: &[u64],
) -> Result<(Vec<u8>, Vec<u64>), Error> {
    if !checked(&text)? {
        return Ok((text, Vec::new()));
    }
    text.rotate_right(1);
    let samples = Samples::new(&text, sampled);
    let (bwt, _) = solve::<u8>(text, 256, 1, 0, Want::Sampled(&samples), mode);
    Ok((
        bwt,
        samples
            .rows
            .into_inner()
            .map_or_else(Vec::new, Sink::into_vec),
    ))
}

/// Whether `text` has anything to sort, refusing what no build takes.
fn checked(text: &[u8]) -> Result<bool, Error> {
    let n = text.len();
    if n as u64 >= u64::from(u32::MAX) {
        return Err(Error::TooLong(n));
    }
    if n > 0 && text[n - 1] != DOC_END {
        return Err(Error::Unterminated);
    }
    Ok(n > 0)
}

/// What a level reports besides its BWT.
#[derive(Clone, Copy)]
enum Want<'a> {
    Nothing,
    /// Every row's position: the level's rows as a suffix array.
    Every,
    /// The rows of the text's sampled positions; level 0 only.
    Sampled(&'a Samples<'a>),
}

/// What a level reported, as its [`Want`] asked.
enum Found {
    Nothing,
    Every(Vec<u32>),
    /// In the [`Samples`]' sink.
    Sampled,
}

/// The text positions whose rows are wanted, and how level 0's positions,
/// each document starting with its terminator, map back to the text's,
/// each document ending with it.
struct Samples<'a> {
    bits: &'a [u64],
    /// Level 0's document starts, a bit per position.
    firsts: Vec<u64>,
    docs: Docs,
    /// The rows found, made when level 0 comes down rather than held
    /// through the levels above.
    rows: OnceLock<Sink>,
}

impl<'a> Samples<'a> {
    fn new(seq: &[u8], bits: &'a [u64]) -> Samples<'a> {
        let docs = Docs::new(seq, 1);
        let firsts = docs.firsts();
        Samples {
            bits,
            firsts,
            docs,
            rows: OnceLock::new(),
        }
    }

    fn sink(&self) -> &Sink {
        self.rows
            .get_or_init(|| Sink::new(self.bits.iter().map(|w| w.count_ones() as usize).sum()))
    }

    /// Adds level 0's row `row` at position `q` to `batch` when sampled,
    /// moving the batch to the sink once full.
    fn note(&self, row: usize, q: usize, batch: &mut Vec<u64>) {
        if let Some(p) = self.sampled(q) {
            batch.push((row as u64) << 32 | u64::from(p));
            if batch.len() == BATCH {
                self.sink().put(batch);
            }
        }
    }

    /// The text position of level 0's position `q`, when it is sampled: a
    /// document's terminator is its first position here and its last there.
    fn sampled(&self, q: usize) -> Option<u32> {
        let first = self.firsts[q / 64] >> (q % 64) & 1 == 1;
        let p = self.docs.before(q, first);
        (self
            .bits
            .get(p / 64)
            .is_some_and(|w| w >> (p % 64) & 1 == 1))
        .then_some(p as u32)
    }
}

/// Rows a piece gathers before moving them to the [`Sink`].
const BATCH: usize = 1024;

/// The sampled rows, `row << 32 | position`, from several pieces at once
/// into one array the size of the samples, so they are never held twice.
struct Sink {
    slots: Vec<AtomicU64>,
    len: AtomicUsize,
}

impl Sink {
    fn new(capacity: usize) -> Sink {
        Sink {
            slots: (0..capacity).map(|_| AtomicU64::new(0)).collect(),
            len: AtomicUsize::new(0),
        }
    }

    /// Moves `batch` in, emptying it.
    fn put(&self, batch: &mut Vec<u64>) {
        let at = self.len.fetch_add(batch.len(), Relaxed);
        for (slot, &row) in self.slots[at..at + batch.len()].iter().zip(&*batch) {
            slot.store(row, Relaxed);
        }
        batch.clear();
    }

    fn into_vec(self) -> Vec<u64> {
        let len = self.len.into_inner();
        let mut rows: Vec<u64> = self.slots.into_iter().map(AtomicU64::into_inner).collect();
        rows.truncate(len);
        rows
    }
}

/// Where a level's S* substrings and one-symbol documents start, kept from
/// going up to place its rows coming down.
struct Place {
    /// The named substrings' starts, a bit per position: in text order, the
    /// level above's positions. A bitmap, an eighth of a byte per symbol, as
    /// it is held through every level above.
    named: Vec<u64>,
    /// One-symbol documents by terminator: (terminator, position), sorted.
    alone: Vec<(u32, u32)>,
}

/// The positions of `bits`' ones, in order.
fn ones(bits: &[u64]) -> Vec<u32> {
    let mut out = Vec::with_capacity(bits.iter().map(|w| w.count_ones() as usize).sum());
    for (w, &word) in bits.iter().enumerate() {
        let mut word = word;
        while word != 0 {
            out.push((w * 64) as u32 + word.trailing_zeros());
            word &= word - 1;
        }
    }
    out
}

/// A level's symbol: the text's bytes, or names.
trait Sym: Copy + Ord + Hash + Default + Send + Sync + NoUninit {
    fn idx(self) -> usize;
    fn of(i: usize) -> Self;
    /// Names, each below this type's bound, as this type.
    fn narrow(names: Vec<u32>) -> Vec<Self> {
        names.into_iter().map(|v| Self::of(v as usize)).collect()
    }
    /// Names the S* substrings of `seq`, which start at `star`'s ones.
    ///
    /// Sorting reads each substring once in order and then touches memory in
    /// passes; hashing probes at random, a miss each once the table outgrows
    /// the cache. A level of bytes has few names among many substrings: its
    /// table stays small, and records to sort would not, so it hashes
    /// ([`name_by_hashing`]). Wider levels sort.
    fn name(seq: &[Self], star: &[u64], num_sym: usize, num_term: usize) -> Named<Self> {
        name_by_sorting(seq, star, num_sym, num_term, sort_chunk::<Self>(seq.len()))
    }
}

impl Sym for u8 {
    fn idx(self) -> usize {
        self as usize
    }
    fn of(i: usize) -> Self {
        i as u8
    }
    fn name(seq: &[u8], star: &[u64], num_sym: usize, num_term: usize) -> Named<u8> {
        name_by_hashing(seq, star, num_sym, num_term, NAMING_AHEAD)
    }
}

impl Sym for u16 {
    fn idx(self) -> usize {
        self as usize
    }
    fn of(i: usize) -> Self {
        i as u16
    }
}

impl Sym for u32 {
    fn idx(self) -> usize {
        self as usize
    }
    fn of(i: usize) -> Self {
        i as u32
    }
    fn narrow(names: Vec<u32>) -> Vec<u32> {
        names
    }
}

/// A short substring of bytes and its final byte as one key, hashed and
/// compared without reading the level's sequence: up to 6 bytes, the final
/// byte after them, the length on top.
fn pack(sub: &[u8], fin: u8) -> Option<u64> {
    (sub.len() <= 6).then(|| {
        let mut key = (sub.len() as u64) << 56 | u64::from(fin) << (8 * sub.len());
        for (i, &b) in sub.iter().enumerate() {
            key |= u64::from(b) << (8 * i);
        }
        key
    })
}

/// [`pack`] of `seq[start..end]`: one load of eight bytes, masked, where
/// eight remain, so no loop to mispredict on the length.
fn pack_in(seq: &[u8], start: usize, end: usize, fin: u8) -> Option<u64> {
    let len = end - start;
    match seq.get(start..start + 8) {
        Some(word) if len <= 6 => {
            let word = u64::from_le_bytes(word.try_into().unwrap());
            let bits = 8 * len as u32;
            Some((len as u64) << 56 | u64::from(fin) << bits | word & ((1 << bits) - 1))
        }
        _ => pack(&seq[start..end], fin),
    }
}

/// The BWT of `seq`, whose symbols below `num_term` are terminators, one
/// starting each document.
fn solve<T: Sym>(
    seq: Vec<T>,
    num_sym: usize,
    num_term: usize,
    level: usize,
    want: Want<'_>,
    mode: Mode,
) -> (Vec<T>, Found) {
    let len = seq.len();
    if num_term >= num_sym {
        // Every document is one symbol: its only rotation, before itself.
        let found = match want {
            Want::Nothing => Found::Nothing,
            _ => {
                let mut sa: Vec<u32> = (0..len as u32).collect();
                sa.sort_by_key(|&p| seq[p as usize]);
                found_in(sa, want)
            }
        };
        let mut seq = seq;
        seq.sort_unstable();
        return (seq, found);
    }
    let place = (!matches!(want, Want::Nothing)).then(|| {
        let (named, alone) = stars_of(&Flags::new(&seq, num_term), len);
        let mut alone: Vec<(u32, u32)> = alone
            .into_iter()
            .map(|p| (seq[p as usize].idx() as u32, p))
            .collect();
        alone.sort_unstable();
        Place { named, alone }
    });
    let (up, next) = up(&seq, num_sym, num_term, mode);
    drop(seq);
    match mode.width(up.names) {
        4 => recurse::<T, u32>(&up, next, len, level, want, place, mode),
        2 => recurse::<T, u16>(&up, next, len, level, want, place, mode),
        _ => recurse::<T, u8>(&up, next, len, level, want, place, mode),
    }
}

/// A level's rows as `want` asks, from its suffix array `sa`.
fn found_in(sa: Vec<u32>, want: Want<'_>) -> Found {
    match want {
        Want::Nothing => Found::Nothing,
        Want::Every => Found::Every(sa),
        Want::Sampled(samples) => {
            let mut batch = Vec::with_capacity(BATCH);
            for (row, &q) in sa.iter().enumerate() {
                samples.note(row, q as usize, &mut batch);
            }
            samples.sink().put(&mut batch);
            Found::Sampled
        }
    }
}

/// Solves the level above `up`, its names `next`, by groups below
/// [`SA_LEVEL`] and by positions ([`solve_sa`]) from it, then this
/// level of `len` symbols from it.
fn recurse<T: Sym, U: Sym>(
    up: &Up<T>,
    next: Vec<u32>,
    len: usize,
    level: usize,
    want: Want<'_>,
    place: Option<Place>,
    mode: Mode,
) -> (Vec<T>, Found) {
    let next = U::narrow(next);
    // The level above's documents, read before solving it consumes `next`.
    let next_docs = place.as_ref().map(|_| Docs::new(&next, up.term_names));
    let (above, above_sa) = if level + 1 >= mode.sa_level() {
        let sa = solve_sa::<U>(&next, up.names, up.term_names, mode);
        let bwt = bwt_of(&next, up.term_names, &sa);
        (bwt, place.as_ref().map(|_| sa))
    } else {
        let want_above = match want {
            Want::Nothing => Want::Nothing,
            _ => Want::Every,
        };
        match solve::<U>(next, up.names, up.term_names, level + 1, want_above, mode) {
            (bwt, Found::Every(sa)) => (bwt, Some(sa)),
            (bwt, _) => (bwt, None),
        }
    };
    // Each row above as where, here, the substring before its rotation
    // starts: the one before it in the text, or at a document's start the
    // document's last.
    let (at, alone) = match (place, above_sa, next_docs) {
        (Some(Place { named, alone }), Some(mut sa), Some(docs)) => {
            let stars = ones(&named);
            drop(named);
            let firsts = docs.firsts();
            for j in &mut sa {
                let k = *j as usize;
                let first = firsts[k / 64] >> (k % 64) & 1 == 1;
                *j = stars[if first { docs.before(k, true) } else { k - 1 }];
            }
            (Some(sa), alone)
        }
        _ => (None, Vec::new()),
    };
    down(
        up,
        TiSlice::from_ref(&above),
        len,
        at.as_deref().map(TiSlice::from_ref),
        &alone,
        want,
        mode,
    )
}

/// The rows of `seq`, cyclic documents whose symbols below `num_term` are
/// terminators, one starting each: every rotation's start, in sorted order.
/// Induced sorting over positions, the way SA-IS does it, with each
/// document's predecessors wrapping from its terminator to its last symbol;
/// the S* order comes from the level above, solved the same way.
fn solve_sa<T: Sym>(seq: &[T], num_sym: usize, num_term: usize, mode: Mode) -> Vec<u32> {
    let len = seq.len();
    if num_term >= num_sym {
        // Every document is one symbol, so its only rotation: by symbol.
        let mut sa: Vec<u32> = (0..len as u32).collect();
        sa.sort_by_key(|&p| seq[p as usize]);
        return sa;
    }
    let (up, next) = up(seq, num_sym, num_term, mode);
    let above = match mode.width(up.names) {
        4 => sa_above::<u32>(next, &up, mode),
        2 => sa_above::<u16>(next, &up, mode),
        _ => sa_above::<u8>(next, &up, mode),
    };
    induce(seq, num_term, &up.counts.raw, &above)
}

fn sa_above<U: Sym>(next: Vec<u32>, up: &Up<impl Sym>, mode: Mode) -> Vec<u32> {
    let next = U::narrow(next);
    solve_sa::<U>(&next, up.names, up.term_names, mode)
}

/// A level's documents: where each starts (its terminator), for wrapping
/// a document's first position back to its last.
struct Docs {
    starts: Vec<u32>,
    len: usize,
}

impl Docs {
    fn new<T: Sym>(seq: &[T], num_term: usize) -> Docs {
        let starts = (0..seq.len() as u32)
            .filter(|&p| seq[p as usize].idx() < num_term)
            .collect();
        Docs {
            starts,
            len: seq.len(),
        }
    }

    /// The documents' starts, a bit per position.
    fn firsts(&self) -> Vec<u64> {
        let mut bits = vec![0u64; self.len.div_ceil(64)];
        for &s in &self.starts {
            bits[s as usize / 64] |= 1 << (s % 64);
        }
        bits
    }

    /// The position cyclically before `p`, which starts its document when
    /// `first`: the document's last.
    fn before(&self, p: usize, first: bool) -> usize {
        if !first {
            return p - 1;
        }
        let d = self.starts.partition_point(|&s| (s as usize) <= p);
        self.starts.get(d).map_or(self.len, |&s| s as usize) - 1
    }
}

/// The BWT of `seq` from its rows `sa`: each row's symbol cyclically before.
fn bwt_of<U: Sym>(seq: &[U], num_term: usize, sa: &[u32]) -> Vec<U> {
    let docs = Docs::new(seq, num_term);
    sa.iter()
        .map(|&p| {
            let p = p as usize;
            seq[docs.before(p, seq[p].idx() < num_term)]
        })
        .collect()
}

/// Every rotation of `seq` in sorted order, from its S* rotations' order:
/// `above` lists the S* substrings `up` named (those of two or more
/// symbols, in text order) by the order of the rotations they start.
fn induce<T: Sym>(seq: &[T], num_term: usize, counts: &[u32], above: &[u32]) -> Vec<u32> {
    let n = seq.len();
    let flags = Flags::new(seq, num_term);
    let (named, alone) = stars_of(&flags, n);
    let stars = ones(&named);
    drop(named);
    debug_assert_eq!(stars.len(), above.len());
    let docs = Docs::new(seq, num_term);

    let mut bucket = Vec::with_capacity(counts.len() + 1);
    let mut acc = 0u32;
    for &c in counts {
        bucket.push(acc);
        acc += c;
    }
    bucket.push(acc);
    const EMPTY: u32 = u32::MAX;
    let mut sa = vec![EMPTY; n];
    // One-symbol documents first in their terminator's bucket: their one
    // rotation repeats the terminator, below anything after it.
    let mut head: Vec<u32> = bucket[..counts.len()].to_vec();
    for &p in &alone {
        let c = seq[p as usize].idx();
        sa[head[c] as usize] = p;
        head[c] += 1;
    }
    // Sorted S* rotations at their buckets' ends.
    let mut tail: Vec<u32> = bucket[1..].to_vec();
    for &j in above.iter().rev() {
        let p = stars[j as usize];
        let c = seq[p as usize].idx();
        tail[c] -= 1;
        sa[tail[c] as usize] = p;
    }
    // Each rotation induces the one before it: a scattered read of its
    // symbol and flags, of the bucket's cursor, and a scattered write. The
    // scan reads `sa` in order, so the entries ahead are prefetched in
    // stages, each reading what the one before brought in, as libsais does.
    let before = |p: usize| docs.before(p, flags.starts(p));
    let distance = PREFETCH;
    let warm = |sa: &[u32], cursors: &[u32], at: Option<usize>, stage: usize| {
        let Some(&p) = at.and_then(|k| sa.get(k)) else {
            return;
        };
        if p == EMPTY {
            return;
        }
        let p = p as usize;
        match stage {
            2 => {
                prefetch(seq, p.wrapping_sub(1));
                flags.warm(p);
            }
            1 => prefetch(cursors, seq[before(p)].idx()),
            _ => {
                let slot = cursors[seq[before(p)].idx()] as usize;
                prefetch(sa, slot);
            }
        }
    };
    // L-types, left to right, each from the rotation after it.
    for i in 0..n {
        warm(&sa, &head, Some(i + 2 * distance), 2);
        warm(&sa, &head, Some(i + distance), 1);
        warm(&sa, &head, Some(i + distance / 2), 0);
        let p = sa[i];
        if p == EMPTY {
            continue;
        }
        let q = before(p as usize);
        if q != p as usize && !flags.s(q) {
            let c = seq[q].idx();
            sa[head[c] as usize] = q as u32;
            head[c] += 1;
        }
    }
    // S-types, right to left, overwriting the S* placed above. The tail
    // cursors point one past their next slot.
    let mut tail: Vec<u32> = bucket[1..].to_vec();
    for i in (0..n).rev() {
        warm(&sa, &tail, i.checked_sub(2 * distance), 2);
        warm(&sa, &tail, i.checked_sub(distance), 1);
        warm(&sa, &tail, i.checked_sub(distance / 2), 0);
        let p = sa[i];
        if p == EMPTY {
            continue;
        }
        let q = before(p as usize);
        if q != p as usize && flags.s(q) {
            let c = seq[q].idx();
            tail[c] -= 1;
            sa[tail[c] as usize] = q as u32;
        }
    }
    sa
}

/// Where the S* substrings `up` names start (those of two or more symbols),
/// a bit per position, and the documents of one symbol, from a level's
/// `flags` over `n` symbols: S* positions are terminators and S-types after
/// an L-type.
fn stars_of(flags: &Flags, n: usize) -> (Vec<u64>, Vec<u32>) {
    let star = |p: usize| flags.starts(p) || (p > 0 && flags.s(p) && !flags.s(p - 1));
    let mut named = vec![0u64; n.div_ceil(64)];
    let mut alone = Vec::new();
    let mut prev = 0;
    for x in (1..n).filter(|&x| star(x)).chain([n]) {
        if x - prev == 1 {
            alone.push(prev as u32);
        } else {
            named[prev / 64] |= 1 << (prev % 64);
        }
        prev = x;
    }
    (named, alone)
}

/// Two bits per position, side by side so one read answers both for a
/// rotation and the one before it: S-type, and starts a document.
struct Flags(Vec<u64>);

impl Flags {
    /// By `up`'s rule: a document's last symbol meets the next terminator,
    /// below every name, so it is L-type as before its own.
    fn new<T: Sym>(seq: &[T], num_term: usize) -> Flags {
        let n = seq.len();
        let mut bits = vec![0u64; n.div_ceil(32)];
        let mut s_here = false;
        for x in (0..n).rev() {
            if x + 1 < n {
                s_here = seq[x] < seq[x + 1] || (seq[x] == seq[x + 1] && s_here);
            }
            let start = seq[x].idx() < num_term;
            bits[x / 32] |= (u64::from(s_here) | u64::from(start) << 1) << (2 * (x % 32));
        }
        Flags(bits)
    }

    fn s(&self, p: usize) -> bool {
        self.0[p / 32] >> (2 * (p % 32)) & 1 == 1
    }

    fn starts(&self, p: usize) -> bool {
        self.0[p / 32] >> (2 * (p % 32) + 1) & 1 == 1
    }

    /// Prefetches the flags of `p` and the position before it.
    fn warm(&self, p: usize) {
        prefetch(&self.0, p.wrapping_sub(1) / 32);
    }
}

/// Defines the descent's index kinds, each a `u32` as the arrays hold it:
/// indexes only their own arrays, and move by plain counts. No conversion
/// from `usize`: it would truncate, and only [`TiSlice`]'s enumerating
/// methods would want it.
macro_rules! keys {
    ($($(#[$doc:meta])* $key:ident;)+) => {$(
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        struct $key(u32);

        impl From<$key> for usize {
            #[inline(always)]
            fn from(key: $key) -> usize {
                key.0 as usize
            }
        }

        impl std::ops::Add<usize> for $key {
            type Output = $key;
            #[inline(always)]
            fn add(self, n: usize) -> $key {
                $key(self.0 + n as u32)
            }
        }

        impl std::ops::Sub<usize> for $key {
            type Output = $key;
            #[inline(always)]
            fn sub(self, n: usize) -> $key {
                $key(self.0 - n as u32)
            }
        }

        impl std::ops::AddAssign<usize> for $key {
            #[inline(always)]
            fn add_assign(&mut self, n: usize) {
                self.0 += n as u32;
            }
        }
    )+};
}

keys! {
    /// A name of the level above: its count, offsets, LF row, last symbol.
    Name;
    /// A place in `alphabet`, and that place's group.
    Offset;
    /// A group of offsets, whose rows are filled together.
    Group;
    /// A row of this level.
    Row;
    /// A row of the level above.
    RowAbove;
    /// A symbol of this level: its count, loopers and queues.
    Symbol;
}

/// What a level keeps from going up for coming down.
struct Up<T> {
    num_sym: usize,
    num_term: usize,
    /// The distinct S* substrings in name order, each followed by its final
    /// symbol (the next S* symbol, or its own terminator at a document end).
    alphabet: TiVec<Offset, T>,
    /// Where each name starts in `alphabet`, then its length.
    offsets: TiVec<Name, Offset>,
    /// Per terminator, the documents that are that terminator alone.
    loopers: TiVec<Symbol, u32>,
    /// Per symbol, its occurrences.
    counts: TiVec<Symbol, u32>,
    names: usize,
    /// Names starting with a terminator, all below the others.
    term_names: usize,
}

/// An S* substring and its final symbol.
type Sub<'a, T> = (&'a [T], T);

/// Lexicographic on the substring then its final symbol, where one that is
/// a prefix of the other is the larger.
fn cmp_sub<T: Sym>((a, fa): Sub<'_, T>, (b, fb): Sub<'_, T>) -> Ordering {
    let m = a.len().min(b.len());
    match a[..m].cmp(&b[..m]) {
        Ordering::Equal => {}
        o => return o,
    }
    match a.len().cmp(&b.len()) {
        Ordering::Less => match fa.cmp(&b[m]) {
            Ordering::Equal => Ordering::Greater,
            o => o,
        },
        Ordering::Greater => match a[m].cmp(&fb) {
            Ordering::Equal => Ordering::Less,
            o => o,
        },
        Ordering::Equal => fa.cmp(&fb),
    }
}

/// Names `seq`'s S* substrings; returns the level and the string of names.
fn up<T: Sym>(seq: &[T], num_sym: usize, num_term: usize, mode: Mode) -> (Up<T>, Vec<u32>) {
    let (star, counts) = types(seq, num_sym, num_term, mode);
    let named = T::name(seq, &star, num_sym, num_term);
    let up = Up {
        num_sym,
        num_term,
        names: named.offsets.len() - 1,
        alphabet: named.alphabet.into(),
        offsets: named.offsets.into_iter().map(Offset).collect(),
        loopers: named.loopers.into(),
        counts: counts.into(),
        term_names: named.term_names,
    };
    (up, named.next)
}

/// Where `seq`'s S* substrings start, a bit per position, and the
/// occurrences of each of its `num_sym` symbols.
fn types<T: Sym>(seq: &[T], num_sym: usize, num_term: usize, mode: Mode) -> (Vec<u64>, Vec<u32>) {
    let n = seq.len();
    let mut star = vec![0u64; n.div_ceil(64)];
    let mut counts = vec![0u32; num_sym];
    // Pieces of whole words, so each writes its own part of the bitmap.
    let size = n.div_ceil(mode.pieces(n, 2 * num_sym)).next_multiple_of(64);
    if size >= n {
        scan_types(seq, 0..n, false, num_term, &mut star, &mut counts);
    } else {
        // Each piece needs the type just past it, which only a run of equal
        // symbols hides: walk it, from the last piece, which knows its own.
        let bounds: Vec<usize> = (0..n).step_by(size).chain([n]).collect();
        let mut s_at = vec![false; bounds.len()];
        for j in (1..bounds.len() - 1).rev() {
            let next = bounds[j + 1];
            let mut x = bounds[j];
            while x + 1 < next && seq[x] == seq[x + 1] {
                x += 1;
            }
            s_at[j] = if x + 1 < n {
                let s_after = x + 1 < next || s_at[j + 1];
                seq[x] < seq[x + 1] || (seq[x] == seq[x + 1] && s_after)
            } else {
                false
            };
        }
        let mut more: Vec<Vec<u32>> = vec![Vec::new(); bounds.len() - 2];
        let tallies = std::iter::once(&mut counts).chain(&mut more);
        star.par_chunks_mut(size / 64)
            .zip(tallies.collect::<Vec<_>>())
            .enumerate()
            .for_each(|(j, (words, tally))| {
                tally.resize(num_sym, 0);
                let range = bounds[j]..bounds[j + 1];
                scan_types(seq, range, s_at[j + 1], num_term, words, tally);
            });
        add_into(&mut counts, &more);
    }
    star[0] |= 1;
    (star, counts)
}

/// The most records [`name_by_sorting`] sorts at a time, for a level of `n`
/// symbols: as many bytes as the level holds.
fn sort_chunk<T>(n: usize) -> usize {
    let bytes = n * size_of::<T>();
    (bytes / size_of::<Rec>()).max(1 << 16)
}

/// A level's names: [`Up`]'s fields of them, and the string of names.
struct Named<T> {
    alphabet: Vec<T>,
    offsets: Vec<u32>,
    loopers: Vec<u32>,
    term_names: usize,
    next: Vec<u32>,
}

/// Calls `f(start, end, fin)` for each S* substring of `seq` in order: `end`
/// is the next S* position or the end of `seq`, `fin` the final symbol.
fn each_sub<T: Sym>(seq: &[T], star: &[u64], num_term: usize, mut f: impl FnMut(usize, usize, T)) {
    let n = seq.len();
    let term = |t: T| t.idx() < num_term;
    let mut prev_term = seq[0];
    each_span(star, n, |start, end| {
        if term(seq[start]) {
            prev_term = seq[start];
        }
        let fin = if end < n && !term(seq[end]) {
            seq[end]
        } else {
            prev_term
        };
        f(start, end, fin);
    });
}

/// Calls `f(start, end)` for each S* position and the next, or `n`.
fn each_span(star: &[u64], n: usize, mut f: impl FnMut(usize, usize)) {
    let mut prev = 0usize;
    for (w, &word) in star.iter().enumerate() {
        let mut bits = word;
        while bits != 0 {
            let x = w * 64 + bits.trailing_zeros() as usize;
            bits &= bits - 1;
            if x > 0 {
                f(prev, x);
                prev = x;
            }
        }
    }
    f(prev, n);
}

/// Open addressing from packed keys to ids, probed linearly, at most half
/// full: where a key's probe starts is known from the key alone, so its
/// line can be fetched well before it is probed.
struct Table {
    slots: Vec<(u64, u32)>,
    /// 64 less the log of the slots.
    shift: u32,
    len: usize,
}

impl Table {
    const EMPTY: u32 = u32::MAX;

    fn with_log(log: u32) -> Self {
        Table {
            slots: vec![(0, Self::EMPTY); 1 << log],
            shift: 64 - log,
            len: 0,
        }
    }

    /// Where `key`'s probe starts: the top bits of a multiplicative hash.
    fn place(&self, key: u64) -> usize {
        (key.wrapping_mul(0xd6e8_feb8_6659_fd93) >> self.shift) as usize
    }

    fn warm(&self, key: u64) {
        prefetch(&self.slots, self.place(key));
    }

    /// `key`'s id, or the empty slot it would take.
    fn find(&self, key: u64) -> Result<u32, usize> {
        let mask = self.slots.len() - 1;
        let mut i = self.place(key);
        loop {
            match self.slots[i] {
                (_, Self::EMPTY) => return Err(i),
                (k, id) if k == key => return Ok(id),
                _ => i = (i + 1) & mask,
            }
        }
    }

    /// `key`'s id, or a `fresh` one now its own.
    fn get_or_insert(&mut self, key: u64, fresh: impl FnOnce() -> u32) -> u32 {
        let i = match self.find(key) {
            Ok(id) => return id,
            Err(i) => i,
        };
        let id = fresh();
        self.slots[i] = (key, id);
        self.len += 1;
        if 2 * self.len > self.slots.len() {
            let old = std::mem::replace(self, Self::with_log(65 - self.shift));
            self.len = old.len;
            for (key, id) in old.slots.into_iter().filter(|s| s.1 != Self::EMPTY) {
                if let Err(i) = self.find(key) {
                    self.slots[i] = (key, id);
                }
            }
        }
        id
    }
}

/// Names a level of bytes by hashing: ids as substrings come, then ranked.
/// Short substrings go to a [`Table`], probed `ahead` substrings after
/// their slot is prefetched; longer ones to a `FxHashMap`.
fn name_by_hashing(
    seq: &[u8],
    star: &[u64],
    num_sym: usize,
    num_term: usize,
    ahead: usize,
) -> Named<u8> {
    let term = |t: u8| t.idx() < num_term;
    let mut map: FxHashMap<Sub<'_, u8>, u32> = FxHashMap::default();
    let mut table = Table::with_log(10);
    // Short substrings whose slot is on its way: key, index in `next`.
    let mut pending = std::collections::VecDeque::with_capacity(ahead + 1);
    let mut subs: Vec<Sub<'_, u8>> = Vec::new();
    fn fresh<'a, T>(subs: &mut Vec<Sub<'a, T>>, sub: Sub<'a, T>) -> u32 {
        subs.push(sub);
        (subs.len() - 1) as u32
    }
    let stars = star.iter().map(|w| w.count_ones() as usize).sum();
    let mut next: Vec<u32> = Vec::with_capacity(stars);
    let mut loopers = vec![0u32; num_term];
    each_sub(seq, star, num_term, |start, end, fin| {
        if end - start == 1 {
            loopers[seq[start].idx()] += 1;
            return;
        }
        let sub = (&seq[start..end], fin);
        match pack_in(seq, start, end, fin) {
            Some(key) => {
                table.warm(key);
                pending.push_back((key, next.len(), sub));
                next.push(0);
                if pending.len() > ahead {
                    let (key, i, sub) = pending.pop_front().unwrap();
                    next[i] = table.get_or_insert(key, || fresh(&mut subs, sub));
                }
            }
            None => next.push(*map.entry(sub).or_insert_with(|| fresh(&mut subs, sub))),
        }
    });
    for (key, i, sub) in pending {
        next[i] = table.get_or_insert(key, || fresh(&mut subs, sub));
    }
    drop((map, table));

    // Keys decide most comparisons without reading the sequence.
    let keys = Keys::new(num_sym);
    let mut order: Vec<(u64, u32)> = subs
        .iter()
        .enumerate()
        .map(|(id, &(s, fin))| (keys.of(s, fin), id as u32))
        .collect();
    order.sort_unstable_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| cmp_sub(subs[a.1 as usize], subs[b.1 as usize]))
    });
    let mut rank = vec![0u32; subs.len()];
    let mut alphabet = Vec::with_capacity(subs.iter().map(|s| s.0.len() + 1).sum());
    let mut offsets = Vec::with_capacity(subs.len() + 1);
    let mut term_names = 0;
    for (r, &(_, id)) in order.iter().enumerate() {
        let (s, fin) = subs[id as usize];
        rank[id as usize] = r as u32;
        offsets.push(alphabet.len() as u32);
        alphabet.extend_from_slice(s);
        alphabet.push(fin);
        term_names += usize::from(term(s[0]));
    }
    offsets.push(alphabet.len() as u32);
    for v in &mut next {
        *v = rank[*v as usize];
    }
    Named {
        alphabet,
        offsets,
        loopers,
        term_names,
        next,
    }
}

/// A substring to sort: its [`Keys`] key and its index in the string of
/// names. Packed: there are as many as substrings.
#[derive(Clone, Copy)]
#[repr(C, packed(4))]
struct Rec {
    key: u64,
    id: u32,
}

/// Packs symbols and a final one (a substring, or what follows its first
/// symbol) into a `u64` that sorts as [`cmp_sub`] does: `slots` symbols of
/// `width` bits each, then a bit set when the symbols end there. Symbols
/// that end sooner fill the slots after them with `top`, above every
/// symbol, as [`cmp_sub`] puts a prefix after what it prefixes. Equal keys
/// with the bit set are equal symbols.
struct Keys {
    width: u32,
    slots: usize,
    top: u64,
}

impl Keys {
    fn new(num_sym: usize) -> Keys {
        let width = u64::BITS - (num_sym as u64).leading_zeros();
        Keys {
            width,
            slots: (63 / width) as usize,
            top: num_sym as u64,
        }
    }

    /// The key of `rest` then `fin`.
    fn of<T: Sym>(&self, rest: &[T], fin: T) -> u64 {
        self.at(rest, 0, rest.len(), fin)
    }

    /// The key of `seq[from..end]` then `fin`. Reads past `end` within
    /// `seq` and selects, rather than branch on the length.
    fn at<T: Sym>(&self, seq: &[T], from: usize, end: usize, fin: T) -> u64 {
        let len = end - from;
        let mut key = 0;
        for j in 0..self.slots {
            let s = seq.get(from + j).map_or(0, |s| s.idx() as u64);
            let v = if j < len {
                s
            } else if j == len {
                fin.idx() as u64
            } else {
                self.top
            };
            key = key << self.width | v;
        }
        key << 1 | u64::from(len < self.slots)
    }

    /// Appends a whole key's symbols, the final one last.
    fn unpack<T: Sym>(&self, key: u64, out: &mut Vec<T>) {
        let mask = (1 << self.width) - 1;
        for j in (0..self.slots).rev() {
            let v = key >> (1 + self.width as usize * j) & mask;
            if v == self.top {
                break;
            }
            out.push(T::of(v as usize));
        }
    }
}

/// The substrings by their index in the string of names, read back from
/// where they start (which the string holds until they are named).
struct Subs<'a, T> {
    seq: &'a [T],
    star: &'a [u64],
    num_term: usize,
    /// Substrings that end a document, by index, with their final symbol:
    /// the document's terminator, which their end does not show.
    ends: Vec<(u32, T)>,
}

impl<'a, T: Sym> Subs<'a, T> {
    fn get(&self, next: &[u32], id: u32) -> Sub<'a, T> {
        let start = next[id as usize] as usize;
        let end = self.end(start);
        (&self.seq[start..end], self.fin(id, end))
    }

    /// The S* position after `start`, or the sequence's end.
    fn end(&self, start: usize) -> usize {
        let mut w = (start + 1) / 64;
        let mut bits = self
            .star
            .get(w)
            .map_or(0, |&b| b & (!0 << ((start + 1) % 64)));
        loop {
            if bits != 0 {
                return w * 64 + bits.trailing_zeros() as usize;
            }
            w += 1;
            match self.star.get(w) {
                Some(&b) => bits = b,
                None => return self.seq.len(),
            }
        }
    }

    /// The final symbol of substring `id`, which ends at `end`.
    fn fin(&self, id: u32, end: usize) -> T {
        match self.seq.get(end) {
            Some(&s) if s.idx() >= self.num_term => s,
            _ => {
                let at = self.ends.binary_search_by_key(&id, |e| e.0);
                self.ends[at.expect("a document's last substring")].1
            }
        }
    }
}

/// Names by sorting: substrings go to buckets by first symbol, which are
/// sorted on their keys, then tied runs on keys further in; names follow in
/// that order. No table to probe at random: the scatter's stores and the
/// sorts' passes are what touch memory.
fn name_by_sorting<T: Sym>(
    seq: &[T],
    star: &[u64],
    num_sym: usize,
    num_term: usize,
    chunk: usize,
) -> Named<T> {
    let n = seq.len();
    let term = |t: T| t.idx() < num_term;
    let stars = star.iter().map(|w| w.count_ones() as usize).sum();
    // Until named, each substring's start.
    let mut next: Vec<u32> = Vec::with_capacity(stars);
    let mut loopers = vec![0u32; num_term];
    let mut bucket = vec![0u32; num_sym];
    let mut ends = Vec::new();
    each_sub(seq, star, num_term, |start, end, fin| {
        if end - start == 1 {
            loopers[seq[start].idx()] += 1;
            return;
        }
        if end == n || term(seq[end]) {
            ends.push((next.len() as u32, fin));
        }
        bucket[seq[start].idx()] += 1;
        next.push(start as u32);
    });

    // Runs of buckets, as few as keep runs within `chunk` records, of about
    // equal size: each ends at the bucket edge nearest its share.
    let share = next.len().div_ceil(next.len().div_ceil(chunk).max(1));
    let mut runs = Vec::new();
    let (mut first, mut size) = (0, 0);
    for (s, &b) in bucket.iter().enumerate() {
        if size > 0 && size + b as usize / 2 > share {
            runs.push((first, s, size));
            (first, size) = (s, 0);
        }
        size += b as usize;
    }
    runs.push((first, num_sym, size));
    let most = runs.iter().map(|r| r.2).max().unwrap_or(0);
    let mut recs = vec![Rec { key: 0, id: 0 }; most];
    let mut at = vec![0u32; num_sym];

    let mut namer = Namer {
        subs: Subs {
            seq,
            star,
            num_term,
            ends,
        },
        keys: Keys::new(num_sym),
        next,
        alphabet: Vec::new(),
        offsets: Vec::new(),
        term_names: 0,
        stack: Vec::new(),
    };
    for (first, last, size) in runs {
        let mut acc = 0;
        for s in first..last {
            at[s] = acc;
            acc += bucket[s];
        }
        // Each record lands in its bucket at random: for the substrings
        // ahead, the bucket's cursor is prefetched, then the slot it holds.
        let (subs, keys) = (&namer.subs, &namer.keys);
        let put = |(start, end, id): (usize, usize, u32), recs: &mut [Rec], at: &mut [u32]| {
            let s = seq[start].idx();
            recs[at[s] as usize] = Rec {
                key: keys.at(seq, start + 1, end, subs.fin(id, end)),
                id,
            };
            at[s] += 1;
        };
        const AHEAD: usize = 8;
        let mut ring = [(0, 0, 0); 2 * AHEAD];
        let (mut id, mut k) = (0u32, 0usize);
        each_span(star, n, |start, end| {
            if end - start == 1 {
                return;
            }
            let s = seq[start].idx();
            if (first..last).contains(&s) {
                prefetch(&at, s);
                if k >= AHEAD {
                    let near = seq[ring[(k - AHEAD) % (2 * AHEAD)].0].idx();
                    prefetch(&recs, at[near] as usize);
                }
                if k >= 2 * AHEAD {
                    put(ring[k % (2 * AHEAD)], &mut recs, &mut at);
                }
                ring[k % (2 * AHEAD)] = (start, end, id);
                k += 1;
            }
            id += 1;
        });
        for j in k.saturating_sub(2 * AHEAD)..k {
            put(ring[j % (2 * AHEAD)], &mut recs, &mut at);
        }
        let (mut a, mut warmed) = (0, 0);
        for (s, &count) in (first..last).zip(&bucket[first..last]) {
            let b = a + count as usize;
            namer.name_bucket(s, &mut recs[..size], a..b, &mut warmed);
            a = b;
        }
        debug_assert_eq!(a, size);
    }
    drop((recs, at, bucket));
    let Namer {
        next,
        mut alphabet,
        mut offsets,
        term_names,
        ..
    } = namer;
    offsets.push(alphabet.len() as u32);
    // Grown without knowing the names: the slack would live as long as the
    // level.
    alphabet.shrink_to_fit();
    offsets.shrink_to_fit();
    Named {
        alphabet,
        offsets,
        loopers,
        term_names,
        next,
    }
}

/// Hands out names in sorted order, each over its substrings' starts in
/// `next`.
struct Namer<'a, T> {
    subs: Subs<'a, T>,
    keys: Keys,
    next: Vec<u32>,
    alphabet: Vec<T>,
    offsets: Vec<u32>,
    term_names: usize,
    /// Tied runs still to name, the next on top: their bounds, and the depth
    /// to key them at, 0 once they are equal.
    stack: Vec<(usize, usize, usize)>,
}

impl<T: Sym> Namer<'_, T> {
    /// Names the substrings of the first symbol `first`, `recs[bucket]`,
    /// keyed at depth 0. Prefetches run on through the buckets after, up to
    /// `warmed`: a bucket holds the same records sorted or not.
    fn name_bucket(
        &mut self,
        first: usize,
        recs: &mut [Rec],
        bucket: Range<usize>,
        warmed: &mut usize,
    ) {
        recs[bucket.clone()].sort_unstable_by_key(|r| r.key);
        let mut y = bucket.start;
        while y < bucket.end {
            let key = recs[y].key;
            let z = y + recs[y..bucket.end]
                .iter()
                .take_while(|r| r.key == key)
                .count();
            while *warmed < z {
                self.warm(recs, *warmed, 0);
                *warmed += 1;
            }
            if key & 1 != 0 {
                // The key holds the whole substring.
                self.offsets.push(self.alphabet.len() as u32);
                self.alphabet.push(T::of(first));
                self.keys.unpack(key, &mut self.alphabet);
                self.term_names += usize::from(first < self.subs.num_term);
                self.assign(&recs[y..z]);
            } else {
                self.name_ties(&mut recs[y..z]);
            }
            y = z;
        }
    }

    /// Names substrings that tie on keys short of their ends: keyed again
    /// past what the keys held, sorted, and so on.
    fn name_ties(&mut self, recs: &mut [Rec]) {
        let slots = self.keys.slots;
        self.stack.push((0, recs.len(), 1));
        while let Some((a, b, depth)) = self.stack.pop() {
            let run = &mut recs[a..b];
            if depth == 0 || run.len() == 1 {
                let (s, fin) = self.subs.get(&self.next, run[0].id);
                self.offsets.push(self.alphabet.len() as u32);
                self.alphabet.extend_from_slice(s);
                self.alphabet.push(fin);
                self.term_names += usize::from(s[0].idx() < self.subs.num_term);
                self.assign(run);
                continue;
            }
            for k in 0..run.len() {
                self.warm(run, k, depth);
                let start = self.next[run[k].id as usize] as usize;
                let (s, fin) = self.subs.get(&self.next, run[k].id);
                let from = start + 1 + depth * slots;
                run[k].key = self.keys.at(self.subs.seq, from, start + s.len(), fin);
            }
            run.sort_unstable_by_key(|r| r.key);
            let mut z = run.len();
            while z > 0 {
                let key = run[z - 1].key;
                let y = z - run[..z].iter().rev().take_while(|r| r.key == key).count();
                let deeper = if key & 1 != 0 { 0 } else { depth + 1 };
                self.stack.push((a + y, a + z, deeper));
                z = y;
            }
        }
    }

    /// Prefetches for the records ahead of `recs[k]`: the slot in `next` of
    /// one, then for one nearer whose key at `depth` is still to read, the
    /// sequence there and the S* bits that end it.
    fn warm(&self, recs: &[Rec], k: usize, depth: usize) {
        const AHEAD: usize = 8;
        if let Some(r) = recs.get(k + 2 * AHEAD) {
            prefetch(&self.next, r.id as usize);
        }
        if let Some(r) = recs.get(k + AHEAD).filter(|r| depth > 0 || r.key & 1 == 0) {
            let start = self.next[r.id as usize] as usize;
            let read = start + 1 + depth * self.keys.slots;
            prefetch(self.subs.seq, read);
            prefetch(self.subs.star, start / 64);
        }
    }

    /// Gives `recs` the newest name.
    fn assign(&mut self, recs: &[Rec]) {
        let name = (self.offsets.len() - 1) as u32;
        for r in recs {
            self.next[r.id as usize] = name;
        }
    }
}

/// Marks the S* positions of `seq[range]` in `star`, its words from the
/// range's start (a multiple of 64, its own position only when not 0), and
/// counts its symbols; `s_end` is the type just past the range.
fn scan_types<T: Sym>(
    seq: &[T],
    range: Range<usize>,
    s_end: bool,
    num_term: usize,
    star: &mut [u64],
    counts: &mut [u32],
) {
    let (lo, hi) = (range.start, range.end);
    let term = |t: T| t.idx() < num_term;
    let mut mark = |p: usize| star[(p - lo) / 64] |= 1 << (p % 64);
    // The last symbol ends a document: L, before its own terminator.
    let mut s_here = hi < seq.len() && (seq[hi - 1] < seq[hi] || (seq[hi - 1] == seq[hi] && s_end));
    counts[seq[hi - 1].idx()] += 1;
    for x in (lo..hi - 1).rev() {
        let s_next = s_here;
        s_here = seq[x] < seq[x + 1] || (seq[x] == seq[x + 1] && s_next);
        if term(seq[x + 1]) || (!s_here && s_next) {
            mark(x + 1);
        }
        counts[seq[x].idx()] += 1;
    }
    if lo > 0 {
        let s_before = seq[lo - 1] < seq[lo] || (seq[lo - 1] == seq[lo] && s_here);
        if term(seq[lo]) || (!s_before && s_here) {
            mark(lo);
        }
    }
}

/// Occurrences of each symbol, all below `bins`.
fn tally<U: Sym>(seq: &[U], bins: usize) -> Vec<u32> {
    let mut counts = vec![0u32; bins];
    for &c in seq {
        counts[c.idx()] += 1;
    }
    counts
}

/// [`tally`], in pieces at once.
fn tally_all<U: Sym>(seq: &[U], bins: usize, mode: Mode) -> Vec<u32> {
    let k = mode.pieces(seq.len(), 2 * bins);
    if k == 1 {
        return tally(seq, bins);
    }
    let mut parts: Vec<Vec<u32>> = seq
        .par_chunks(seq.len().div_ceil(k))
        .map(|part| tally(part, bins))
        .collect();
    let rest = parts.split_off(1);
    let mut sum = parts.pop().unwrap();
    add_into(&mut sum, &rest);
    sum
}

/// Into `sum`, every one of `parts`, as long or shorter.
fn add_into(sum: &mut [u32], parts: &[Vec<u32>]) {
    for part in parts {
        for (s, &p) in sum.iter_mut().zip(part) {
            *s += p;
        }
    }
}

/// A queue entry: every occurrence of a name, at an offset into `alphabet`.
type Entry = (Flagged, Offset);

/// The bit of a [`Flagged`] that starts a group.
const GROUP: u32 = 1 << 31;

/// A [`Name`] and whether its entry starts a group, in one word. Nothing
/// else reads the flag, so a stale one cannot pass for a name.
#[derive(Clone, Copy)]
struct Flagged(u32);

impl Flagged {
    #[inline(always)]
    fn new(name: Name, starts_group: bool) -> Flagged {
        Flagged(name.0 | (GROUP * u32::from(starts_group)))
    }

    #[inline(always)]
    fn name(self) -> Name {
        Name(self.0 & !GROUP)
    }

    #[inline(always)]
    fn starts_group(self) -> bool {
        self.0 & GROUP != 0
    }
}

/// A hint to bring `slice[i]`'s cache line in; nothing past the end.
/// `safe_arch` holds the intrinsic's `unsafe`.
#[inline(always)]
fn prefetch<T>(slice: &[T], i: usize) {
    if let Some(x) = slice.get(i) {
        #[cfg(target_arch = "x86_64")]
        safe_arch::prefetch_t0(x);
        #[cfg(not(target_arch = "x86_64"))]
        let _ = x;
    }
}

/// [`prefetch`] of a keyed array, by its own kind of key.
#[inline(always)]
fn prefetch_at<K: Into<usize>, V>(slice: &TiSlice<K, V>, key: K) {
    prefetch(&slice.raw, key.into());
}

/// A symbol of the level above as what it is there, a name.
#[inline(always)]
fn name_of<U: Sym>(symbol: U) -> Name {
    Name(symbol.idx() as u32)
}

/// Per-symbol queues in one array, each sized from the alphabet up front.
struct Queues {
    entries: Vec<Entry>,
    start: TiVec<Symbol, u32>,
    end: TiVec<Symbol, u32>,
    /// The source group that last pushed to each queue.
    last: TiVec<Symbol, u32>,
}

impl Queues {
    fn new(sizes: &TiSlice<Symbol, u32>) -> Queues {
        let mut start = TiVec::with_capacity(sizes.len());
        let mut acc = 0;
        for &s in sizes {
            start.push(acc);
            acc += s;
        }
        Queues {
            // Built from plain zeros, which `vec!` leaves to the allocator,
            // in the same buffer: pushes first touch each page.
            entries: vec![(0u32, 0u32); acc as usize]
                .into_iter()
                .map(|(name, ind)| (Flagged(name), Offset(ind)))
                .collect(),
            end: start.clone(),
            start,
            last: vec![u32::MAX; sizes.len()].into(),
        }
    }

    /// Appends to queue `q` from source group `src`; a new source starts a
    /// group.
    fn push(&mut self, q: Symbol, src: u32, (name, ind): (Name, Offset)) {
        // Branchless: whether the source changed is data, not control.
        let starts_group = self.last[q] != src;
        self.last[q] = src;
        self.entries[self.end[q] as usize] = (Flagged::new(name, starts_group), ind);
        self.end[q] += 1;
    }

    /// Prefetches queue `q`'s tail bookkeeping, for a push soon.
    fn warm_tail(&self, q: Symbol) {
        prefetch_at(&self.end, q);
        prefetch_at(&self.last, q);
    }

    /// Prefetches the slot queue `q`'s next push writes; its tail warmed.
    fn warm_slot(&self, q: Symbol) {
        prefetch(&self.entries, self.end[q] as usize);
    }
}

/// The level's BWT, of `len` symbols, from the BWT of its names `above`.
/// `at`, when given, is where each row above's preceding substring starts here,
/// and `alone` this level's one-symbol documents (see [`Place`]): with them the
/// rows' positions are found as `want` asks.
fn down<T: Sym, U: Sym>(
    up: &Up<T>,
    above: &TiSlice<RowAbove, U>,
    len: usize,
    at: Option<&TiSlice<RowAbove, u32>>,
    alone: &[(u32, u32)],
    want: Want<'_>,
    mode: Mode,
) -> (Vec<T>, Found) {
    let Up {
        num_sym,
        num_term,
        alphabet,
        offsets,
        loopers,
        counts,
        names,
        ..
    } = up;
    let (num_sym, num_term) = (*num_sym, *num_term);
    let sym = |i: Offset| Symbol(alphabet[i].idx() as u32);
    // Occurrences per name.
    let per_name: TiVec<Name, u32> = tally_all(&above.raw, *names, mode).into();

    // Each alphabet offset enters exactly one queue once: an L-type the L
    // queue of its symbol (and the LS queue when an S-type precedes it), an
    // S-type the S queue; each name's final symbol seeds the S* queue.
    let sizes = |names: Range<usize>| {
        let [mut l_size, mut ls_size, mut s_size, mut ss_size] =
            [(); 4].map(|_| TiVec::<Symbol, u32>::from(vec![0u32; num_sym]));
        for name in names {
            let name = Name(name as u32);
            let (a, b) = (offsets[name], offsets[name + 1]);
            ss_size[sym(b - 1)] += 1;
            let mut s_type = true;
            for i in (a.0..b.0 - 1).rev().map(Offset) {
                s_type =
                    alphabet[i] < alphabet[i + 1] || (alphabet[i] == alphabet[i + 1] && s_type);
                if s_type {
                    s_size[sym(i)] += 1;
                } else {
                    l_size[sym(i)] += 1;
                    ls_size[sym(i)] += u32::from(alphabet[i - 1] < alphabet[i]);
                }
            }
        }
        [l_size, ls_size, s_size, ss_size]
    };
    let k = mode.pieces(alphabet.len(), 2 * 4 * num_sym);
    let [l_size, ls_size, s_size, ss_size] = if k == 1 {
        sizes(0..*names)
    } else {
        // Pieces of names of about as many offsets each.
        let cuts: Vec<usize> = (0..=k)
            .map(|j| {
                offsets.raw[..*names].partition_point(|&o| usize::from(o) < alphabet.len() * j / k)
            })
            .collect();
        let mut parts: Vec<[TiVec<Symbol, u32>; 4]> = (0..k)
            .into_par_iter()
            .map(|j| sizes(cuts[j]..cuts[j + 1]))
            .collect();
        let rest = parts.split_off(1);
        let mut sum = parts.pop().unwrap();
        for part in rest {
            for (s, p) in sum.iter_mut().zip(part) {
                add_into(&mut s.raw, &[p.raw]);
            }
        }
        sum
    };
    let mut ss = Queues::new(&ss_size);
    for name in (0..*names as u32).map(Name) {
        let last = offsets[name + 1] - 1;
        ss.push(sym(last), 0, (name, last));
    }
    let mut l = Queues::new(&l_size);
    let mut ls = Queues::new(&ls_size);
    let mut s = Queues::new(&s_size);
    drop((l_size, ls_size, s_size, ss_size));

    // The first row of each group, then the next row to fill.
    let mut starts: TiVec<Group, Row> = TiVec::new();
    // Cells, so the prefetch below can read it while the passes write it.
    let group_cells: TiVec<Offset, Cell<Group>> =
        (0..alphabet.len()).map(|_| Cell::new(Group(0))).collect();
    let group_of = group_cells.as_slice();
    let mut src = 0u32;

    // Each entry reads its name's count, its symbol before and its group
    // slot, then pushes to the queue of that symbol: all scattered. Entries
    // are read in order, so those ahead are prefetched in stages: the
    // entry's own lines, then its target queue's tail, then the slot.
    let distance = PREFETCH;
    let warm = |(flagged, ind): Entry| {
        prefetch_at(&per_name, flagged.name());
        prefetch_at(alphabet, Offset(ind.0.wrapping_sub(1)));
        prefetch_at(group_of, ind);
    };
    let before = |(_, ind): Entry| ind.0.checked_sub(1).map(|i| sym(Offset(i)));

    // L-types, left to right.
    let mut bucket_start = 0usize;
    for (x, &count) in counts.iter().enumerate() {
        let x = Symbol(x as u32);
        let mut row = bucket_start;
        bucket_start += count as usize;
        let mut y = l.start[x] as usize;
        while y < l.end[x] as usize {
            let end = l.end[x] as usize;
            if let Some(&e) = l.entries[..end].get(y + 2 * distance) {
                warm(e);
            }
            if let Some(&e) = l.entries[..end].get(y + distance) {
                match before(e) {
                    Some(t) if t < x => ls.warm_tail(x),
                    Some(t) => l.warm_tail(t),
                    None => {}
                }
            }
            if let Some(&e) = l.entries[..end].get(y + distance / 2) {
                match before(e) {
                    Some(t) if t < x => ls.warm_slot(x),
                    Some(t) => l.warm_slot(t),
                    None => {}
                }
            }
            let (flagged, ind) = l.entries[y];
            y += 1;
            if flagged.starts_group() {
                starts.push(Row(row as u32));
                src += 1;
            }
            let name = flagged.name();
            row += per_name[name] as usize;
            group_of[ind].set(Group((starts.len() - 1) as u32));
            let t = sym(ind - 1);
            if t < x {
                ls.push(x, src, (name, ind));
            } else {
                l.push(t, src, (name, ind - 1));
            }
        }
        if ss.end[x] > ss.start[x] {
            src += 1;
            for y in ss.start[x]..ss.end[x] {
                let (flagged, ind) = ss.entries[y as usize];
                let t = sym(ind - 1);
                l.push(t, src, (flagged.name(), ind - 1));
            }
        }
    }
    drop((l, ss));

    // S-types, right to left. A group's first row is known when it closes.
    let mut out: TiVec<Row, T> = vec![T::default(); len].into();
    // Every row's position, or a batch of the sampled rows for the sink.
    let mut every: Option<TiVec<Row, u32>> =
        matches!(want, Want::Every).then(|| vec![0u32; len].into());
    let mut batch: Vec<u64> = Vec::new();
    for i in (0..num_sym as u32).rev().map(Symbol) {
        let mut row = bucket_start;
        bucket_start -= counts[i] as usize;
        let mut y = s.start[i] as usize;
        let mut open = false;
        while y < s.end[i] as usize {
            let end = s.end[i] as usize;
            if let Some(&e) = s.entries[..end].get(y + 2 * distance) {
                warm(e);
            }
            // A substring's start pushes nothing; its symbol before is still
            // a valid line to touch.
            let entry = |k: usize| s.entries[..end].get(k).and_then(|&e| before(e));
            if let Some(t) = entry(y + distance) {
                s.warm_tail(t);
            }
            if let Some(t) = entry(y + distance / 2) {
                s.warm_slot(t);
            }
            let (flagged, ind) = s.entries[y];
            y += 1;
            if flagged.starts_group() {
                if open {
                    starts.push(Row(row as u32));
                }
                open = true;
                src += 1;
            }
            let name = flagged.name();
            row -= per_name[name] as usize;
            group_of[ind].set(Group(starts.len() as u32));
            if offsets[name] != ind {
                let t = sym(ind - 1);
                s.push(t, src, (name, ind - 1));
            }
        }
        if open {
            starts.push(Row(row as u32));
        }
        // LS entries right to left: a flag ends a group.
        let mut fresh = true;
        let first = ls.start[i] as usize;
        for y in (ls.start[i]..ls.end[i]).rev() {
            // Read right to left: the entries ahead are below.
            let behind = |d: usize| {
                (y as usize)
                    .checked_sub(d)
                    .filter(|&k| k >= first)
                    .map(|k| ls.entries[k])
            };
            if let Some(e) = behind(2 * distance) {
                warm(e);
            }
            if let Some(t) = behind(distance).and_then(before) {
                s.warm_tail(t);
            }
            if let Some(t) = behind(distance / 2).and_then(before) {
                s.warm_slot(t);
            }
            let (flagged, ind) = ls.entries[y as usize];
            if fresh {
                src += 1;
            }
            let t = sym(ind - 1);
            s.push(t, src, (flagged.name(), ind - 1));
            fresh = flagged.starts_group();
        }
        if usize::from(i) < num_term {
            let mine = alone.partition_point(|&(t, _)| t < i.0);
            for k in 0..loopers[i] as usize {
                row -= 1;
                out[Row(row as u32)] = T::of(usize::from(i));
                if let Some(&(_, q)) = alone.get(mine + k).filter(|_| at.is_some()) {
                    match want {
                        Want::Every => every.as_mut().unwrap()[Row(row as u32)] = q,
                        Want::Sampled(samples) => samples.note(row, q as usize, &mut batch),
                        Want::Nothing => {}
                    }
                }
            }
        }
    }
    drop((s, ls));

    // Plain again for the fill, which shares it across threads.
    let group_of: TiVec<Offset, Group> = group_cells.into_iter().map(Cell::into_inner).collect();

    // Rows of each name in the level above, as its LF walks them.
    let mut lf: TiVec<Name, RowAbove> = TiVec::with_capacity(*names);
    let mut acc = 0u32;
    for &c in &per_name {
        lf.push(RowAbove(acc));
        acc += c;
    }
    drop(per_name);
    // Each name's last symbol before its final one: what precedes the start
    // of the substring after it, one load instead of two dependent ones.
    let last_of: TiVec<Name, T> = (0..*names as u32)
        .map(|name| alphabet[offsets[Name(name) + 1] - 2])
        .collect();

    // Pieces write apart rows of one `out` at once: relaxed atomic stores,
    // plain stores in the machine code, and safe whatever the pieces do.
    let out: TiVec<Row, Atomic<T>> = out.into_iter().map(Atomic::new).collect();
    let every: Option<TiVec<Row, AtomicU32>> =
        every.map(|rows| rows.into_iter().map(AtomicU32::new).collect());
    let fill = Fill {
        alphabet,
        offsets,
        group_of: &group_of,
        last_of: &last_of,
        above,
        distance,
        rows: &out,
        at,
        track: match (want, &every) {
            (Want::Every, Some(rows)) => Track::Every(rows),
            (Want::Sampled(samples), _) if at.is_some() => Track::Sampled(samples),
            _ => Track::Nothing,
        },
    };
    if let Want::Sampled(samples) = want {
        samples.sink().put(&mut batch);
    }
    // The level's BWT, and its positions as asked.
    let done = |out: TiVec<Row, Atomic<T>>, every: Option<TiVec<Row, AtomicU32>>| {
        let found = match (want, every) {
            (Want::Every, Some(rows)) => {
                Found::Every(rows.into_iter().map(AtomicU32::into_inner).collect())
            }
            (Want::Sampled(_), _) => Found::Sampled,
            _ => Found::Nothing,
        };
        (out.into_iter().map(Atomic::into_inner).collect(), found)
    };
    // Pieces of `above` fill rows apart, each from its own cursors: a copy
    // of every name's LF row and every group's next row, advanced past the
    // pieces before it. The level's size caps those copies.
    let per_piece = 4 * (*names + starts.len()).max(1);
    let budget = len * size_of::<T>();
    let k = mode.pieces(above.len(), above.len() / (1 + budget / per_piece));
    if k == 1 {
        fill.run(
            RowAbove(0)..RowAbove(above.len() as u32),
            &mut starts,
            &mut lf,
        );
        return done(out, every);
    }
    let size = above.len().div_ceil(k);
    let bounds: Vec<RowAbove> = (0..above.len())
        .step_by(size)
        .chain([above.len()])
        .map(|y| RowAbove(y as u32))
        .collect();
    let k = bounds.len() - 1;
    // Per piece after the first, its names' LF rows: the counts of the
    // pieces before it, summed over pieces per name.
    let mut lfs: Vec<Vec<u32>> = above[..bounds[k - 1]]
        .raw
        .par_chunks(size)
        .map(|part| tally(part, *names))
        .collect();
    let span = names.div_ceil(k);
    let mut cols: Vec<Vec<&mut [u32]>> = (0..names.div_ceil(span)).map(|_| Vec::new()).collect();
    for counts in &mut lfs {
        for (col, part) in cols.iter_mut().zip(counts.chunks_mut(span)) {
            col.push(part);
        }
    }
    cols.into_par_iter().enumerate().for_each(|(c, mut col)| {
        for i in 0..col[0].len() {
            let mut acc = lf[Name((c * span + i) as u32)].0;
            for part in &mut col {
                acc += part[i];
                part[i] = acc;
            }
        }
    });
    // Summed, in the same buffers.
    let mut lfs: Vec<TiVec<Name, RowAbove>> = lfs
        .into_par_iter()
        .map(|rows| rows.into_iter().map(RowAbove).collect())
        .collect();
    // Its groups' next rows: each name's rows before it, in every group of
    // the name's offsets.
    let mut nexts: Vec<TiVec<Group, Row>> = lfs
        .par_iter()
        .map(|piece_lf| {
            let mut next = starts.clone();
            for name in (0..*names as u32).map(Name) {
                let before = piece_lf[name].0 - lf[name].0;
                if before > 0 {
                    let (a, b) = (offsets[name], offsets[name + 1]);
                    for &g in &group_of[a..b - 1] {
                        next[g] += before as usize;
                    }
                }
            }
            next
        })
        .collect();
    let first = (
        bounds[0]..bounds[1],
        starts.as_mut_slice(),
        lf.as_mut_slice(),
    );
    let rest = nexts.iter_mut().zip(&mut lfs).enumerate();
    let jobs: Vec<_> = std::iter::once(first)
        .chain(rest.map(|(j, (next, piece_lf))| {
            (
                bounds[j + 1]..bounds[j + 2],
                next.as_mut_slice(),
                piece_lf.as_mut_slice(),
            )
        }))
        .collect();
    // The pieces' rows are apart, each piece's cursors starting where the
    // pieces before it left every name and group.
    jobs.into_par_iter()
        .for_each(|(range, starts, lf)| fill.run(range, starts, lf));
    done(out, every)
}

/// What filling the level's rows from the level above reads.
struct Fill<'a, T: Sym, U> {
    alphabet: &'a TiSlice<Offset, T>,
    offsets: &'a TiSlice<Name, Offset>,
    /// Each alphabet offset's group.
    group_of: &'a TiSlice<Offset, Group>,
    /// Each name's last symbol before its final one.
    last_of: &'a TiSlice<Name, T>,
    above: &'a TiSlice<RowAbove, U>,
    distance: usize,
    /// The level's rows, written by several pieces at once.
    rows: &'a TiSlice<Row, Atomic<T>>,
    /// Where each row above's preceding substring starts here, when the
    /// rows' positions are wanted.
    at: Option<&'a TiSlice<RowAbove, u32>>,
    track: Track<'a>,
}

/// Where a fill puts its rows' positions.
enum Track<'a> {
    Nothing,
    Every(&'a TiSlice<Row, AtomicU32>),
    Sampled(&'a Samples<'a>),
}

impl Track<'_> {
    /// Rows `o..` hold the `positions`, each `offset` on: every one stored,
    /// or the sampled ones batched for the sink.
    fn note(&self, o: Row, positions: &TiSlice<RowAbove, u32>, offset: u32, batch: &mut Vec<u64>) {
        match self {
            Track::Nothing => {}
            Track::Every(rows) => {
                for (row, &s) in rows[o..o + positions.len()].iter().zip(positions) {
                    row.store(s + offset, Relaxed);
                }
            }
            Track::Sampled(samples) => {
                for (z, &s) in positions.iter().enumerate() {
                    samples.note(usize::from(o) + z, (s + offset) as usize, batch);
                }
            }
        }
    }
}

impl<T: Sym, U: Sym> Fill<'_, T, U> {
    /// Fills the rows of `above[range]`'s names, from each group's next row
    /// `starts` and each name's next LF row `lf`, advancing both, and their
    /// positions as tracked.
    fn run(
        &self,
        range: Range<RowAbove>,
        starts: &mut TiSlice<Group, Row>,
        lf: &mut TiSlice<Name, RowAbove>,
    ) {
        let mut found = Vec::new();
        let Fill {
            alphabet,
            offsets,
            group_of,
            last_of,
            distance,
            rows: out,
            ..
        } = *self;
        let (all, above) = (self.above, &self.above[..range.end]);
        // The level above orders each group's rows: one pass over its BWT, a
        // run of one name at a time. Each row is a chain of dependent misses
        // (the name's offsets, its groups, their next rows, the store), so
        // the names ahead are prefetched in stages, each stage reading what
        // the one before brought in, as libsais does.
        let ahead = |y: RowAbove| above.get(y).map(|&c| name_of(c));
        let mut y = range.start;
        while usize::from(y) < above.len() {
            if let Some(name) = ahead(y + 4 * distance) {
                prefetch_at(offsets, name);
                prefetch_at(lf, name);
            }
            if let Some(name) = ahead(y + 2 * distance) {
                let (a, b) = (offsets[name], offsets[name + 1]);
                prefetch_at(group_of, a);
                prefetch_at(group_of, b - 2);
                prefetch_at(all, lf[name]);
            }
            if let Some(name) = ahead(y + distance) {
                let (a, b) = (offsets[name], offsets[name + 1]);
                for &g in &group_of[a..b - 1] {
                    prefetch_at(starts, g);
                }
                if let Some(&pred) = all.get(lf[name]) {
                    prefetch_at(last_of, name_of(pred));
                }
            }
            let name = name_of(above[y]);
            let run_start = y;
            y += 1;
            while usize::from(y) < above.len() && name_of(above[y]) == name {
                y += 1;
            }
            let run = (y.0 - run_start.0) as usize;
            let (a, b) = (offsets[name], offsets[name + 1]);
            for i in (a.0 + 1..b.0 - 1).rev().map(Offset) {
                let g = group_of[i];
                let o = starts[g];
                if run == 1 {
                    out[o].store(alphabet[i - 1], Relaxed);
                } else {
                    for row in &out[o..o + run] {
                        row.store(alphabet[i - 1], Relaxed);
                    }
                }
                if let Some(at) = self.at {
                    let offset = i.0 - a.0;
                    self.track.note(o, &at[run_start..y], offset, &mut found);
                }
                starts[g] += run;
            }
            // The substring's start: before it, the end of the name before.
            let g = group_of[a];
            let o = starts[g];
            let from = lf[name];
            for (row, &pred) in out[o..o + run].iter().zip(&all[from..from + run]) {
                row.store(last_of[name_of(pred)], Relaxed);
            }
            if let Some(at) = self.at {
                self.track.note(o, &at[run_start..y], 0, &mut found);
            }
            lf[name] += run;
            starts[g] += run;
        }
        if let Track::Sampled(samples) = self.track {
            samples.sink().put(&mut found);
        }
    }
}

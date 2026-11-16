//! The parallel walk back: the same docs as walking each row alone, tasks
//! only from two grains of rows, and a corrupt sample as loud as before.

use super::*;
use crate::forward::{GRAIN, TASKS};

/// Tasks the parallel walks begun on this thread split into, so far.
fn tasks() -> usize {
    TASKS.with(std::cell::Cell::get)
}

/// The sorted docs of `cursor`'s rows, each walked back alone.
fn rowwise(cursor: &Cursor<'_>) -> Vec<usize> {
    let mut docs: Vec<usize> = match cursor.root {
        true => Vec::new(),
        false => (Row::range(cursor.lo, cursor.hi))
            .map(|r| cursor.view.doc(r))
            .collect(),
    };
    docs.sort_unstable();
    docs
}

/// The rows `cursor`'s locate walks back: neither its own mark nor a tail
/// resolves them.
fn walked(cursor: &Cursor<'_>) -> usize {
    let s = cursor.locate_stats(&mut |_| ());
    (s.occurrences - s.zero_step - s.forward) as usize
}

/// The tasks a walk of `rows` rows splits into.
fn split(rows: usize) -> usize {
    match rows >= 2 * GRAIN {
        true => rows.div_ceil(GRAIN),
        false => 0,
    }
}

/// Sorted docs of `cursor`'s locate, and the tasks it began.
fn located_by(cursor: &Cursor<'_>) -> (Vec<usize>, usize) {
    let before = tasks();
    let mut got = Vec::new();
    cursor.locate(&mut |d| got.push(d));
    got.sort_unstable();
    (got, tasks() - before)
}

/// About 150 KB of documents over four letters with a few spaces and
/// newlines: a letter has some 30,000 rows, most of them unmarked under the
/// wide samplings, so they walk back.
fn wide_text(fate: &mut Fate) -> Vec<u8> {
    let mut text = Vec::new();
    while text.len() < 150_000 {
        for _ in 0..fate.roll(dice::uni_u16(1..=3000)) {
            text.push(match fate.roll(dice::uni_u8(0..40)) {
                0 => b' ',
                1 => b'\n',
                k => b"abcd"[usize::from(k % 4)],
            });
        }
        text.push(DOC_END);
    }
    text
}

/// Over rolled texts and every sampling, in pools of one and three
/// threads: a locate gives the docs of walking each row alone, and splits
/// into tasks exactly when its rows left to walk reach two grains.
#[test]
fn parallel_walks_equal_rowwise() {
    Dicetest::repeatedly().passes(3).run(|mut fate| {
        let text = wide_text(&mut fate);
        let start = fate.roll(dice::uni_usize(0..text.len() - 3));
        let rolled = text[start..start + 3].to_vec();
        let mut splits = 0;
        for sampling in samplings() {
            let index = build_with(text.clone(), sampling).unwrap();
            for pattern in [&b"a"[..], b"ab", b"cd", b"b", &rolled] {
                let cursor = index.cursor().extend_front(pattern);
                let rows = walked(&cursor);
                for threads in [1, 3] {
                    let (got, began) = in_pool(threads, || located_by(&cursor));
                    assert_eq!(got, rowwise(&cursor), "{sampling:?} {threads} threads");
                    assert_eq!(began, split(rows), "{sampling:?} {pattern:?} {rows} rows");
                    splits += began;
                }
            }
        }
        assert!(splits > 0, "no walk split");
    });
}

/// Sorted docs of [`Cursor::locate_all`] over `cursors`, and the tasks it began.
fn all_of(cursors: &[Cursor<'_>]) -> (Vec<usize>, usize) {
    let before = tasks();
    let mut got = Vec::new();
    Cursor::locate_all(cursors, &mut |d| got.push(d));
    got.sort_unstable();
    (got, tasks() - before)
}

/// `cursor`'s rows cut into pieces of random sizes, a few of one row.
fn pieces<'a>(fate: &mut Fate, cursor: &Cursor<'a>) -> Vec<Cursor<'a>> {
    let (mut out, mut at) = (Vec::new(), 0);
    while at < cursor.count() {
        let len = match fate.roll(dice::uni_u8(0..4)) {
            0 => 1,
            _ => fate.roll(dice::uni_u64(1..=5000)),
        };
        out.push(cursor.rows(at..at + len));
        at += len;
    }
    out
}

/// Over rolled texts and every sampling, in pools of one and three
/// threads: one locate of many intervals (a large one cut into pieces of
/// random sizes, another one, the root, an empty one) gives the docs of
/// walking each row alone, and splits into tasks exactly when the rows
/// left to walk, summed, reach two grains.
#[test]
fn many_intervals_walk_back_together() {
    Dicetest::repeatedly().passes(3).run(|mut fate| {
        let text = wide_text(&mut fate);
        let mut splits = 0;
        for sampling in samplings() {
            let index = build_with(text.clone(), sampling).unwrap();
            let (a, b) = (
                index.cursor().extend_front(b"a"),
                index.cursor().extend_front(b"b"),
            );
            let want = {
                let mut w = [rowwise(&a), rowwise(&b)].concat();
                w.sort_unstable();
                w
            };
            for threads in [1, 3] {
                let mut cut = pieces(&mut fate, &a);
                cut.extend([b, index.cursor(), index.cursor().extend_front(b"zz")]);
                let rows: usize = cut.iter().map(walked).sum();
                let (got, began) = in_pool(threads, || all_of(&cut));
                assert_eq!(
                    got,
                    want,
                    "{sampling:?} {threads} threads, {} pieces",
                    cut.len()
                );
                assert_eq!(began, split(rows), "{sampling:?} {rows} rows in pieces");
                splits += began;
            }
        }
        assert!(splits > 0, "no walk of many intervals split");
    });
}

/// Documents of 8 to 56 bytes of `abcdefgh` runs: a mark falls on every
/// `a` (each 8 bytes, no word start), none on a `b`, so each row of `b`
/// walks back one LF step to its document's `a`.
fn periodic(rows: usize) -> Vec<u8> {
    let mut text = Vec::new();
    let (mut doc, mut left) = (0, rows);
    while left > 0 {
        let runs = (1 + doc % 7).min(left);
        text.extend(b"abcdefgh".repeat(runs));
        text.push(DOC_END);
        (doc, left) = (doc + 1, left - runs);
    }
    text
}

#[test]
fn below_two_grains_a_walk_begins_no_task() {
    let index = Built::build(periodic(2 * GRAIN)).unwrap();
    let b = index.cursor().extend_front(b"b");
    assert_eq!((b.count(), walked(&b)), (2 * GRAIN as u64, 2 * GRAIN));
    let below = b.rows(0..2 * GRAIN as u64 - 1);
    assert_eq!(located_by(&below), (rowwise(&below), 0));
    assert_eq!(located_by(&b), (rowwise(&b), 2));
    // The steps of a split walk add up.
    let s = b.locate_stats(&mut |_| ());
    assert_eq!((s.walk_steps, s.max_walk), (2 * GRAIN as u64, 1));
}

/// What parallel walks saved: nothing below two grains, nothing on a pool
/// of one thread (its tasks run one after another), and a scope inside
/// another keeps its own.
#[test]
fn saved_by_tasks_counts_only_overlapping_tasks() {
    let index = Built::build(periodic(4 * GRAIN)).unwrap();
    let b = index.cursor().extend_front(b"b");
    let below = b.rows(0..2 * GRAIN as u64 - 1);
    let zero = std::time::Duration::ZERO;
    assert_eq!(saved_by_tasks(|| below.locate(&mut |_| ())).1, zero);
    let one = in_pool(1, || saved_by_tasks(|| (located_by(&b).1, ())));
    assert_eq!((one.0.0, one.1), (4, zero));
    // Scopes nest: each returns what was saved inside it, the outer one
    // gets back what it had.
    let ms = std::time::Duration::from_millis;
    let add = |d| crate::forward::SAVED.with(|s| s.set(s.get() + d));
    add(ms(5));
    let (inner, outer) = saved_by_tasks(|| {
        add(ms(2));
        saved_by_tasks(|| add(ms(3))).1
    });
    assert_eq!((inner, outer), (ms(3), ms(2)));
    assert_eq!(saved_by_tasks(|| ()).1, zero);
    assert_eq!(crate::forward::SAVED.with(std::cell::Cell::take), ms(5));
}

#[test]
fn below_two_grains_many_intervals_begin_no_task() {
    let index = Built::build(periodic(2 * GRAIN)).unwrap();
    let b = index.cursor().extend_front(b"b");
    let end = 2 * GRAIN as u64;
    let cut = |end: u64| -> Vec<Cursor<'_>> {
        (0..end)
            .step_by(100)
            .map(|at| b.rows(at..(at + 100).min(end)))
            .collect()
    };
    // Each walks on its own, as a locate of each would.
    assert_eq!(all_of(&cut(end - 1)), (rowwise(&b.rows(0..end - 1)), 0));
    // Together, two grains: two tasks.
    assert_eq!(all_of(&cut(end)), (rowwise(&b), 2));
}

/// Every sample but the sentinel's names a doc past the last: a locate
/// that reads one must panic as corrupt, from a task as from the caller.
#[test]
fn a_corrupt_sample_panics_from_a_task() {
    let blob = encode(&Built::build(periodic(3 * GRAIN)).unwrap());
    let h = Header::parse(&blob).unwrap();
    let sentinel = h.marks(&blob).get_rank(h.sentinel_row).1;
    let mut bad = blob.clone();
    let ids = (0..h.num_samples).map(|k| if k == sentinel { 0 } else { u64::MAX });
    h.ids.write(&mut bad[h.samples_at..], ids);
    let index = FmIndex::view(bad.as_slice()).unwrap();
    let b = index.cursor().extend_front(b"b");
    for (cursor, splits) in [(b.rows(0..100), false), (b, true)] {
        let (run, began) = in_pool(3, || {
            let before = tasks();
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cursor.locate(&mut |_| {});
            }));
            (run.map_err(|p| panic_message(&*p)), tasks() - before)
        });
        let msg = run.expect_err("a corrupt sample must panic");
        assert!(msg.contains("corrupt docfm index: doc"), "{msg}");
        assert_eq!(began > 0, splits, "{} rows", cursor.count());
    }
}

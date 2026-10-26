use super::*;

use std::sync::Condvar;
use std::sync::mpsc;
use std::time::Duration;

use crate::sharded::build::{build_plan, window_from};

/// Plan `docs` at `cap` and build it on a pool of `pool` threads holding
/// `window` shards, over an already-final cut source.
fn run<T: Send>(
    docs: &[Document],
    cap: u64,
    (pool, window): (usize, usize),
    build: &(dyn Fn(&Cut) -> Result<T, String> + Sync),
    emit: &mut dyn FnMut(usize, T) -> Result<(), String>,
) -> Result<(), String> {
    let lens = doc_lens(docs);
    let paths: Vec<String> = docs.iter().map(|d| d.path.clone()).collect();
    let plan = plan_shards(&lens, cap);
    assert!(plan.len() > 2, "the fixture must plan more than two shards");
    let cuts = Cuts::closed(None, plan, &lens, &paths).unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(pool)
        .build()
        .unwrap();
    build_plan(&pool, &cuts, window, build, emit)
}

/// Six two-byte documents: at cap 1 each is its own shard, so a slot index
/// is its range start.
fn many() -> Vec<Document> {
    docs_of(&[b"aa", b"bb", b"cc", b"dd", b"ee", b"ff"])
}

/// Releases the read of the highest slot first, so the builds start in
/// descending slot order.
struct Reverse {
    done: Mutex<(usize, Vec<usize>)>,
    ready: Condvar,
    slots: usize,
}

impl Reverse {
    fn wait(&self, slot: usize) {
        let mut done = self.done.lock().unwrap();
        while done.0 < self.slots - 1 - slot {
            done = self.ready.wait(done).unwrap();
        }
        done.0 += 1;
        done.1.push(slot);
        self.ready.notify_all();
    }
}

#[test]
fn emit_sees_slots_ascending_whatever_the_build_order() {
    let docs = many();
    let slots = plan_shards(&doc_lens(&docs), 1).len();
    let reverse = Reverse {
        done: Mutex::new((0, Vec::new())),
        ready: Condvar::new(),
        slots,
    };
    let mut emitted = Vec::new();
    run(
        &docs,
        1,
        (slots, slots),
        &|cut| {
            reverse.wait(cut.docs.start);
            Ok(())
        },
        &mut |slot, ()| {
            emitted.push(slot);
            Ok(())
        },
    )
    .unwrap();
    let read_order = reverse.done.into_inner().unwrap().1;
    assert_eq!(read_order, (0..slots).rev().collect::<Vec<_>>());
    assert_eq!(emitted, (0..slots).collect::<Vec<_>>());
}

#[test]
fn a_panicking_build_fails_loudly_instead_of_hanging() {
    let docs = many();
    let msg = panic_message(|| {
        let _ = run(
            &docs,
            1,
            (4, 4),
            &|cut| {
                if cut.docs.start == 2 {
                    refuse("shard refuses to build".to_string());
                }
                Ok(())
            },
            &mut |_slot, ()| Ok(()),
        );
    });
    assert!(msg.contains("shard refuses to build"), "got: {msg:?}");
    assert!(msg.contains("docfm build"), "got: {msg:?}");
}

#[test]
fn an_erroring_emit_stops_the_build() {
    let docs = many();
    let mut emitted = Vec::new();
    let err = run(&docs, 1, (4, 4), &|_cut| Ok(()), &mut |slot, ()| {
        if slot == 1 {
            return Err("emit refused".to_string());
        }
        emitted.push(slot);
        Ok(())
    })
    .expect_err("an erroring emit must fail the build");
    assert_eq!(err, "emit refused");
    // Slot 0 went through, nothing above the failure did.
    assert_eq!(emitted, vec![0]);
}

/// Every shard sees the whole pool, however few shards the window holds.
#[test]
fn every_shard_builds_in_the_whole_pool() {
    let docs = many();
    let slots = plan_shards(&doc_lens(&docs), 1).len();
    let seen = Mutex::new(Vec::new());
    run(
        &docs,
        1,
        (3, 2),
        &|_cut| {
            seen.lock().unwrap().push(rayon::current_num_threads());
            Ok(())
        },
        &mut |_slot, ()| Ok(()),
    )
    .unwrap();
    assert_eq!(seen.into_inner().unwrap(), vec![3; slots]);
}

/// A built shard keeps its window place until emitted: with slot 1 built
/// and slot 0 still building, a window of 2 claims no slot 2.
#[test]
fn a_finished_shard_holds_its_place_until_emitted() {
    let docs = many();
    let (built_tx, built_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let (built_rx, started_rx) = (Mutex::new(built_rx), Mutex::new(started_rx));
    let early = Mutex::new(None);
    run(
        &docs,
        1,
        (2, 2),
        &|cut| {
            match cut.docs.start {
                0 => {
                    built_rx.lock().unwrap().recv().unwrap();
                    let started = started_rx.lock().unwrap();
                    *early.lock().unwrap() = started.recv_timeout(Duration::from_millis(200)).ok();
                }
                1 => built_tx.send(()).unwrap(),
                slot => started_tx.send(slot).unwrap(),
            }
            Ok(())
        },
        &mut |_slot, ()| Ok(()),
    )
    .unwrap();
    assert_eq!(early.into_inner().unwrap(), None);
}

/// A thread that waits inside one build, for work it split off, never
/// starts a second build there.
#[test]
fn no_build_starts_inside_another() {
    thread_local! {
        static INSIDE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    let texts: Vec<Vec<u8>> = (0..40).map(|i| format!("{i:02}").into_bytes()).collect();
    let docs = docs_of(&texts.iter().map(Vec::as_slice).collect::<Vec<_>>());
    let nested = AtomicUsize::new(0);
    run(
        &docs,
        1,
        (4, 4),
        &|_cut| {
            if INSIDE.replace(true) {
                nested.fetch_add(1, Ordering::SeqCst);
            }
            (0..64).into_par_iter().for_each(|i| {
                let until = std::time::Instant::now() + Duration::from_micros(20 + i % 7);
                while std::time::Instant::now() < until {}
            });
            INSIDE.set(false);
            Ok(())
        },
        &mut |_slot, ()| Ok(()),
    )
    .unwrap();
    assert_eq!(nested.into_inner(), 0);
}

/// `(pool, budget, transient) -> shards held at once`.
#[test]
fn window_table() {
    let gb = 1u64 << 30;
    let cases = [
        // The pool under the budget.
        ((10, 8 * gb, gb), 8),
        ((4, 8 * gb, gb), 4),
        ((10, 8 * gb, 3 * gb), 2),
        ((10, 8 * gb, 8 * (100 << 20)), 10),
        // The floor is one shard, however tight the budget.
        ((10, 1, 100 * gb), 1),
        ((10, 0, 0), 1),
        ((0, 8 * gb, gb), 1),
    ];
    for ((pool, budget, transient), want) in cases {
        assert_eq!(
            window_from(pool, budget, transient),
            want,
            "pool={pool} budget={budget} transient={transient}"
        );
    }
}

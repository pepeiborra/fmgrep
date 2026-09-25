use super::*;

use std::sync::Condvar;

use crate::sharded::build::{
    build_plan, parse_knob, shard_threads, shard_threads_from, threads_from,
};

/// Plan `docs` at `cap` and build it with `threads` in flight, over an
/// already-final cut source.
fn run<T: Send>(
    docs: &[Document],
    cap: u64,
    threads: usize,
    build: &(dyn Fn(&Cut) -> Result<T, String> + Sync),
    emit: &(dyn Fn(usize, T) -> Result<(), String> + Sync),
) -> Result<(), String> {
    let lens = doc_lens(docs);
    let paths: Vec<String> = docs.iter().map(|d| d.path.clone()).collect();
    let plan = plan_shards(&lens, cap);
    assert!(plan.len() > 2, "the fixture must plan more than two shards");
    let cuts = Cuts::closed(None, plan, &lens, &paths).unwrap();
    build_plan(&cuts, threads, build, emit)
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
    let emitted = Mutex::new(Vec::new());
    run(
        &docs,
        1,
        slots,
        &|cut| {
            reverse.wait(cut.docs.start);
            Ok(())
        },
        &|slot, ()| {
            emitted.lock().unwrap().push(slot);
            Ok(())
        },
    )
    .unwrap();
    let read_order = reverse.done.into_inner().unwrap().1;
    assert_eq!(read_order, (0..slots).rev().collect::<Vec<_>>());
    assert_eq!(
        emitted.into_inner().unwrap(),
        (0..slots).collect::<Vec<_>>()
    );
}

#[test]
fn a_panicking_worker_fails_loudly_instead_of_hanging() {
    let docs = many();
    let msg = panic_message(|| {
        let _ = run(
            &docs,
            1,
            4,
            &|cut| {
                if cut.docs.start == 2 {
                    refuse("shard refuses to build".to_string());
                }
                Ok(())
            },
            &|_slot, ()| Ok(()),
        );
    });
    assert!(msg.contains("shard refuses to build"), "got: {msg:?}");
    assert!(msg.contains("docfm build"), "got: {msg:?}");
}

#[test]
fn an_erroring_emit_stops_every_worker() {
    let docs = many();
    let emitted = Mutex::new(Vec::new());
    let err = run(&docs, 1, 4, &|_cut| Ok(()), &|slot, ()| {
        if slot == 1 {
            return Err("emit refused".to_string());
        }
        emitted.lock().unwrap().push(slot);
        Ok(())
    })
    .expect_err("an erroring emit must fail the build");
    assert_eq!(err, "emit refused");
    // Slot 0 went through, nothing above the failure did.
    assert_eq!(emitted.into_inner().unwrap(), vec![0]);
}

/// The rule is plumbing, not arithmetic: a shard builds inside a pool that
/// reports `shard_threads` to `rayon::current_num_threads()`.
#[test]
fn a_shard_builds_inside_a_pool_of_that_size() {
    let docs = many();
    let slots = plan_shards(&doc_lens(&docs), 1).len();
    let machine = std::thread::available_parallelism().map_or(1, |n| n.get());
    let seen = Mutex::new(Vec::new());
    run(
        &docs,
        1,
        2,
        &|_cut| {
            seen.lock().unwrap().push(rayon::current_num_threads());
            Ok(())
        },
        &|_slot, ()| Ok(()),
    )
    .unwrap();
    assert_eq!(
        seen.into_inner().unwrap(),
        vec![shard_threads(2, machine); slots]
    );
}

/// `(shards, machine, knob, budget, transient) -> shards in flight`.
#[test]
fn build_threads_table() {
    let gb = 1u64 << 30;
    let cases = [
        // The knob is obeyed exactly, over and under machine parallelism.
        ((8, 4, Some(7), 8 * gb, gb), 7),
        ((8, 4, Some(1), 0, gb), 1),
        // No knob: machine parallelism under the footprint budget.
        ((8, 10, None, 8 * gb, gb), 8),
        ((8, 4, None, 8 * gb, gb), 4),
        ((8, 10, None, 8 * gb, 3 * gb), 2),
        // The floor is one shard in flight, however tight the budget.
        ((8, 10, None, 1, 100 * gb), 1),
        ((8, 10, None, 0, 0), 1),
        // Never more shards in flight than there are shards.
        ((1, 10, None, 8 * gb, gb), 1),
        ((3, 10, Some(9), 8 * gb, gb), 3),
        ((0, 10, None, 8 * gb, gb), 1),
        // A live stream bounds the flight by this machine, so an outsized
        // knob is clamped instead of spawning one worker per request.
        ((10, 10, Some(1000), 8 * gb, gb), 10),
        ((10, 10, None, 8 * gb, 3 * gb), 2),
    ];
    for ((shards, machine, knob, budget, transient), want) in cases {
        assert_eq!(
            threads_from(shards, machine, knob, budget, transient),
            want,
            "shards={shards} machine={machine} knob={knob:?} budget={budget} transient={transient}"
        );
    }
}

/// `(shards in flight, machine, knob) -> threads inside one shard`:
/// integer division, floor 1, never oversubscribing.
#[test]
fn shard_threads_table() {
    let cases = [
        ((1, 10, None), 10),
        ((2, 10, None), 5),
        ((3, 10, None), 3),
        ((4, 10, None), 2),
        ((8, 10, None), 1),
        ((16, 10, None), 1),
        ((0, 10, None), 10),
        // The knob is obeyed exactly, over and under machine parallelism.
        ((8, 10, Some(4)), 4),
        ((1, 10, Some(16)), 16),
        // Junk and zero are no knob, so the rule decides.
        ((2, 10, parse_knob(Some("0"))), 5),
        ((2, 10, parse_knob(Some("nonsense"))), 5),
    ];
    for ((in_flight, machine, knob), want) in cases {
        assert_eq!(
            shard_threads_from(in_flight, machine, knob),
            want,
            "in_flight={in_flight} machine={machine} knob={knob:?}"
        );
    }
}

/// Junk and zero are no knob, so the budget — not machine parallelism —
/// decides: a machine of 4 under a 1-shard budget runs 1 shard in flight.
#[test]
fn unusable_knobs_take_the_budget_path() {
    for raw in [Some("nonsense"), Some("0"), Some("-1"), Some(""), None] {
        assert_eq!(parse_knob(raw), None, "{raw:?}");
    }
    assert_eq!(parse_knob(Some(" 3 ")), Some(3));
    let gb = 1u64 << 30;
    assert_eq!(threads_from(8, 4, parse_knob(Some("nonsense")), gb, gb), 1);
}

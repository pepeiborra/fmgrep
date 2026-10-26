//! Shard construction on one rayon pool: each shard is a task, and docfm
//! splits its phases into units any idle thread takes. At most a window of
//! shards is held from claim to emit, each with its construction transient.
//! A thread waiting inside a shard build never starts another. Blobs are
//! emitted in slot order, so `blobs.shard` depends on neither the pool nor
//! the window.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, mpsc};

use crate::docs::{Cut, Cuts};
use crate::env::physical_memory;

const BYTES_VAR: &str = "FMGREP_BUILD_BYTES";
/// Build budget when physical memory is unknown.
const FALLBACK_BUILD_BYTES: u64 = 8 << 30;
/// Peak construction bytes per shard text byte: the window's only per-shard
/// input. Too low overcommits memory, too high serializes the build.
const BUILD_BYTES_PER_TEXT_BYTE: u64 = 8;

/// An unbuildable corpus is a loud panic: there is no fallback.
pub fn refuse(what: String) -> ! {
    panic!("docfm build: {what}");
}

/// Shards held at once: the budget over one shard's transient, at most the
/// pool size, at least 1.
pub fn window_from(pool: usize, budget: u64, transient: u64) -> usize {
    usize::try_from(budget / transient.max(1))
        .unwrap_or(usize::MAX)
        .min(pool)
        .max(1)
}

/// Build every shard the stream cuts into `emit`, ascending, on a pool sized
/// by rayon's defaults (`RAYON_NUM_THREADS`, else machine parallelism).
/// `window` is the shards held at once; `None` derives it from the build
/// budget (`FMGREP_BUILD_BYTES`, else half of physical memory) and one
/// shard's transient at `cap`.
pub fn build_shards<T: Send>(
    cuts: &Cuts,
    cap: u64,
    window: Option<usize>,
    build: &(dyn Fn(&Cut) -> Result<T, String> + Sync),
    emit: &mut dyn FnMut(usize, T) -> Result<(), String>,
) -> Result<(), String> {
    if cuts.get(0).is_none() {
        // A stream that died cut nothing: its error is the cause.
        if let Some(e) = cuts.error() {
            return Err(e);
        }
        refuse(format!(
            "corpus holds no indexable bytes ({} doc(s), 0 non-empty)",
            cuts.num_docs()
        ));
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .build()
        .unwrap_or_else(|e| refuse(format!("cannot create a build thread pool: {e}")));
    let window = match window {
        Some(n) => n,
        None => window_from(
            pool.current_num_threads(),
            crate::env::bytes(
                BYTES_VAR,
                physical_memory().map_or(FALLBACK_BUILD_BYTES, |m| m / 2),
            )?,
            BUILD_BYTES_PER_TEXT_BYTE.saturating_mul(cap),
        ),
    };
    build_plan(&pool, cuts, window, build, emit)
}

thread_local! {
    static BUILDING: Cell<bool> = const { Cell::new(false) };
}

enum Event<T> {
    Built(usize, std::thread::Result<Result<T, String>>),
    /// A thread waiting inside one build picked up another: starting it
    /// there would stall the first build until the second one ends.
    Busy(usize, Cut),
    Skipped,
    /// The claims are over, after this many slots.
    End(usize),
}

/// Slots emitted so far, and whether the build stopped.
struct Emitted {
    state: Mutex<(usize, bool)>,
    moved: Condvar,
}

impl Emitted {
    fn lock(&self) -> MutexGuard<'_, (usize, bool)> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn update(&self, f: impl FnOnce(&mut (usize, bool))) {
        f(&mut self.lock());
        self.moved.notify_all();
    }
}

/// Stops the claims if `emit` panics, so the scope can end.
struct StopOnUnwind<'a>(&'a Emitted, &'a Cuts);

impl Drop for StopOnUnwind<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.update(|state| state.1 = true);
            self.1.abort();
        }
    }
}

/// Build every slot `cuts` closes on `pool`, holding at most `window` slots
/// from claim until emitted. A claiming thread starts each build as the
/// stream closes its cut; the caller's thread runs `emit` on every slot
/// exactly once, ascending. The lowest-slot error is returned; a panicking
/// build panics here.
pub fn build_plan<T: Send>(
    pool: &rayon::ThreadPool,
    cuts: &Cuts,
    window: usize,
    build: &(dyn Fn(&Cut) -> Result<T, String> + Sync),
    emit: &mut dyn FnMut(usize, T) -> Result<(), String>,
) -> Result<(), String> {
    let window = window.max(1);
    let stop = AtomicBool::new(false);
    let emitted = Emitted {
        state: Mutex::new((0, false)),
        moved: Condvar::new(),
    };
    let (tx, rx) = mpsc::channel();
    let mut failure: Option<(usize, String)> = None;
    let mut panic = None;
    pool.in_place_scope(|scope| {
        let start = |slot: usize, cut: Cut| {
            let (tx, stop) = (tx.clone(), &stop);
            scope.spawn(move |_| {
                let event = if stop.load(Ordering::Relaxed) {
                    Event::Skipped
                } else if BUILDING.get() {
                    Event::Busy(slot, cut)
                } else {
                    BUILDING.set(true);
                    let built = catch_unwind(AssertUnwindSafe(|| build(&cut)));
                    BUILDING.set(false);
                    Event::Built(slot, built)
                };
                let _ = tx.send(event);
            });
        };
        // A plain thread: it waits on the window, so on the pool it could starve the builds.
        std::thread::scope(|threads| {
            threads.spawn(|| {
                let mut slot = 0;
                loop {
                    let mut state = emitted.lock();
                    while !state.1 && slot - state.0 >= window {
                        state = emitted.moved.wait(state).unwrap_or_else(|e| e.into_inner());
                    }
                    if state.1 {
                        break;
                    }
                    drop(state);
                    let Some(cut) = cuts.get(slot) else { break };
                    start(slot, cut);
                    slot += 1;
                }
                let _ = tx.send(Event::End(slot));
            });
            let _unwind = StopOnUnwind(&emitted, cuts);
            let mut ready = BTreeMap::new();
            let (mut received, mut next) = (0, 0);
            let mut claimed = None;
            while claimed != Some(received) {
                let stopped = failure.is_some() || panic.is_some();
                match rx.recv().expect("the caller holds a sender") {
                    Event::End(n) => claimed = Some(n),
                    Event::Busy(slot, cut) if !stopped => start(slot, cut),
                    Event::Busy(..) | Event::Skipped => received += 1,
                    Event::Built(slot, built) => {
                        received += 1;
                        match built {
                            Ok(Ok(shard)) => {
                                ready.insert(slot, shard);
                            }
                            Ok(Err(e)) => fail(&mut failure, slot, e),
                            Err(payload) => {
                                panic.get_or_insert(payload);
                            }
                        }
                    }
                }
                while failure.is_none() && panic.is_none() {
                    let Some(shard) = ready.remove(&next) else {
                        break;
                    };
                    if let Err(e) = emit(next, shard) {
                        fail(&mut failure, next, e);
                    }
                    next += 1;
                    emitted.update(|state| state.0 = next);
                }
                if !stopped && (failure.is_some() || panic.is_some()) {
                    stop.store(true, Ordering::Relaxed);
                    emitted.update(|state| state.1 = true);
                    cuts.abort();
                }
            }
        });
    });
    if let Some(payload) = panic {
        resume_unwind(payload);
    }
    match failure {
        Some((_, e)) => Err(e),
        None => Ok(()),
    }
}

fn fail(failure: &mut Option<(usize, String)>, slot: usize, e: String) {
    if failure.as_ref().is_none_or(|(at, _)| slot < *at) {
        *failure = Some((slot, e));
    }
}

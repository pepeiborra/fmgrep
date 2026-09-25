//! Shard construction: up to K shards in flight (`FMGREP_BUILD_THREADS`), each
//! built by T threads (`FMGREP_BUILD_SHARD_THREADS`). K is a memory budget,
//! not a core count, since every shard in flight holds its own construction
//! transient. A gate emits blobs in slot order, so `blobs.shard` depends on
//! neither K nor T.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};

use crate::bytes::env_bytes;
use crate::docs::{Cut, Cuts, Shape};

const THREADS_VAR: &str = "FMGREP_BUILD_THREADS";
const SHARD_THREADS_VAR: &str = "FMGREP_BUILD_SHARD_THREADS";
const BYTES_VAR: &str = "FMGREP_BUILD_BYTES";
/// Construction footprint the build may take when `FMGREP_BUILD_BYTES` is
/// unset. A constant, not a reading of this machine's RAM.
const DEFAULT_BUILD_BYTES: u64 = 8 << 30;
/// Peak construction bytes per shard text byte: the shard-concurrency
/// budget's only per-shard input. Too low overcommits memory, too high
/// serializes the build.
const BUILD_BYTES_PER_TEXT_BYTE: u64 = 8;

/// An unbuildable corpus is a loud panic: there is no fallback.
pub fn refuse(what: String) -> ! {
    panic!("docfm build: {what}");
}

/// [`threads_from`] over the environment, one shard's transient at `cap`,
/// and this machine. `shards` is the shard count or an upper bound on it.
fn build_threads(shards: usize, cap: u64) -> Result<usize, String> {
    let machine = machine_threads();
    let knob = parse_knob(std::env::var(THREADS_VAR).ok().as_deref());
    let budget = env_bytes(BYTES_VAR, DEFAULT_BUILD_BYTES)?;
    let transient = BUILD_BYTES_PER_TEXT_BYTE.saturating_mul(cap);
    Ok(threads_from(shards, machine, knob, budget, transient))
}

/// Threads this machine can run at once, 1 when the count is unavailable.
pub fn machine_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// Threads inside one shard build, from `FMGREP_BUILD_SHARD_THREADS`.
pub fn shard_threads(in_flight: usize, machine: usize) -> usize {
    let knob = parse_knob(std::env::var(SHARD_THREADS_VAR).ok().as_deref());
    shard_threads_from(in_flight, machine, knob)
}

/// The knob is obeyed exactly; without one the machine is divided over the
/// shards in flight, so K x T never oversubscribes it. At least 1.
pub fn shard_threads_from(in_flight: usize, machine: usize, knob: Option<usize>) -> usize {
    knob.unwrap_or(machine / in_flight.max(1)).max(1)
}

/// A knob that is not a positive count is no knob: junk and zero fall back to
/// the budget rather than to machine parallelism.
pub fn parse_knob(raw: Option<&str>) -> Option<usize> {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
}

/// Shards in flight: the knob exactly, else the budget divided by one
/// shard's transient, capped by machine parallelism. Clamped to `1..=shards`.
pub fn threads_from(
    shards: usize,
    machine: usize,
    knob: Option<usize>,
    budget: u64,
    transient: u64,
) -> usize {
    let threads = match knob {
        Some(n) => n,
        None => {
            let fits = (budget / transient.max(1)).max(1);
            machine.min(usize::try_from(fits).unwrap_or(usize::MAX))
        }
    };
    threads.clamp(1, shards.max(1))
}

/// Build every shard the stream cuts, ascending by slot into `emit` and
/// never concurrently, so the bytes do not depend on `threads` (the shards in
/// flight; `None` derives it from the environment).
///
/// Blocks on the first cut: `Shape::One` must stay at one worker, or a
/// one-shard corpus would build in a 1-thread rayon pool.
pub fn build_shards<T: Send>(
    cuts: &Cuts,
    cap: u64,
    threads: Option<usize>,
    build: &(dyn Fn(&Cut) -> Result<T, String> + Sync),
    emit: &(dyn Fn(usize, T) -> Result<(), String> + Sync),
) -> Result<(), String> {
    let Some(shape) = cuts.shape() else {
        // A stream that died cut nothing: its error is the cause.
        if let Some(e) = cuts.error() {
            return Err(e);
        }
        refuse(format!(
            "corpus holds no indexable bytes ({} doc(s), 0 non-empty)",
            cuts.num_docs()
        ));
    };
    let threads = match threads {
        Some(n) => n,
        None => build_threads(
            match shape {
                Shape::One => 1,
                // The slot count is still unknown, so this machine is the
                // ceiling: a knob may not ask for more workers than it.
                Shape::Many => machine_threads(),
            },
            cap,
        )?,
    };
    build_plan(cuts, threads, build, emit)
}

/// Build every slot `cuts` closes, with at most `threads` in flight.
///
/// The slot count is unknown up front: a worker blocks on the next cut, so
/// construction overlaps the document stream. `build` runs on every worker,
/// inside its rayon pool; `emit` sees every slot exactly once, ascending,
/// never concurrently.
pub fn build_plan<T: Send>(
    cuts: &Cuts,
    threads: usize,
    build: &(dyn Fn(&Cut) -> Result<T, String> + Sync),
    emit: &(dyn Fn(usize, T) -> Result<(), String> + Sync),
) -> Result<(), String> {
    let threads = threads.max(1);
    let per_shard = shard_threads(threads, machine_threads());
    let gate = Gate::new();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| scope.spawn(|| work(cuts, per_shard, &next, &gate, build, emit)))
            .collect();
        // Join eagerly: a worker panic is a build refusal and must reach the
        // caller as a panic, not as the gate's abort error.
        for worker in workers {
            if let Err(payload) = worker.join() {
                std::panic::resume_unwind(payload);
            }
        }
    });
    match gate.failure() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn work<T: Send>(
    cuts: &Cuts,
    per_shard: usize,
    next: &AtomicUsize,
    gate: &Gate,
    build: &(dyn Fn(&Cut) -> Result<T, String> + Sync),
    emit: &(dyn Fn(usize, T) -> Result<(), String> + Sync),
) {
    let abort = AbortOnDrop(gate, cuts);
    // Built on the first claim, not up front: a worker spawned against an
    // unknown shard count may never claim a slot.
    let mut pool = None;
    loop {
        let slot = next.fetch_add(1, Ordering::Relaxed);
        if gate.aborted() {
            break;
        }
        let Some(cut) = cuts.get(slot) else { break };
        let pool = pool.get_or_insert_with(|| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(per_shard)
                .build()
                .unwrap_or_else(|e| refuse(format!("cannot create a build thread pool: {e}")))
        });
        let built = pool.install(|| build(&cut));
        let done = built.and_then(|shard| gate.emit_in_order(slot, || emit(slot, shard)));
        match done {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                gate.fail(slot, e);
                // Wake the workers blocked on a cut, and stop the stream.
                cuts.abort();
                break;
            }
        }
    }
    abort.disarm();
}

/// Admits slot `s` to `emit` only once every lower slot has been emitted.
///
/// The mutex guards only the turn counter: `emit` runs unlocked, since only
/// the worker whose slot equals `next` may emit and `next` advances after.
/// No worker can strand the others: a failure records itself and aborts, and
/// [`AbortOnDrop`] aborts for a worker that unwinds, so every waiter wakes.
struct Gate {
    state: Mutex<GateState>,
    ready: Condvar,
    /// Set before the mutex is taken to notify, so a waiter that checks it
    /// under the mutex cannot miss the wake-up.
    aborted: AtomicBool,
}

struct GateState {
    next: usize,
    /// Lowest-slot failure, the one reported.
    failure: Option<(usize, String)>,
}

impl Gate {
    fn new() -> Self {
        Self {
            state: Mutex::new(GateState {
                next: 0,
                failure: None,
            }),
            ready: Condvar::new(),
            aborted: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn aborted(&self) -> bool {
        self.aborted.load(Ordering::Acquire)
    }

    /// `Ok(false)` means another worker aborted and this slot must not be
    /// emitted.
    fn emit_in_order(
        &self,
        slot: usize,
        emit: impl FnOnce() -> Result<(), String>,
    ) -> Result<bool, String> {
        let mut state = self.lock();
        while !self.aborted() && state.next != slot {
            state = self.ready.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        if self.aborted() {
            return Ok(false);
        }
        drop(state);
        // An error or unwind leaves `next` at `slot`: nobody else emits.
        emit()?;
        self.lock().next += 1;
        self.ready.notify_all();
        Ok(true)
    }

    fn fail(&self, slot: usize, e: String) {
        let mut state = self.lock();
        if state.failure.as_ref().is_none_or(|(at, _)| slot < *at) {
            state.failure = Some((slot, e));
        }
        drop(state);
        self.abort();
    }

    fn abort(&self) {
        self.aborted.store(true, Ordering::Release);
        // Taking the mutex orders the store before any waiter's next check.
        drop(self.lock());
        self.ready.notify_all();
    }

    fn failure(&self) -> Option<String> {
        self.lock().failure.take().map(|(_, e)| e)
    }
}

/// Aborts the gate and the cut feed unless the worker left its loop cleanly:
/// a waiter blocked on a cut that will never come must wake, and the stream
/// behind it must stop.
struct AbortOnDrop<'a>(&'a Gate, &'a Cuts);

impl AbortOnDrop<'_> {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for AbortOnDrop<'_> {
    fn drop(&mut self) {
        self.0.abort();
        self.1.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A worker claiming its next slot checks the gate while another worker
    /// is still emitting: the check must not wait out the emit.
    #[test]
    fn a_slow_emit_does_not_block_the_next_claim() {
        let gate = Gate::new();
        std::thread::scope(|scope| {
            let emitted = gate.emit_in_order(0, || {
                let (tx, rx) = mpsc::channel();
                let gate = &gate;
                scope.spawn(move || {
                    let _ = tx.send(gate.aborted());
                });
                match rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(aborted) => {
                        assert!(!aborted);
                        Ok(())
                    }
                    Err(_) => Err("the claim waited out the emit".to_string()),
                }
            });
            assert_eq!(emitted, Ok(true));
            let next = scope.spawn(|| gate.emit_in_order(1, || Ok(())));
            assert_eq!(next.join().unwrap(), Ok(true));
        });
    }
}

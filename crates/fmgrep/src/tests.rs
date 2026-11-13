//! Tests over the real command line, and the helpers every test module shares.

mod drive;
mod expr;
pub(crate) mod filter;
mod index;
mod json;
mod matches;
mod output;
mod serve;
#[cfg(unix)]
mod served;
pub(crate) mod support;
#[cfg(unix)]
mod watched;
#[cfg(unix)]
pub(crate) mod watchman;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::cli::{Outcome, dispatch};
use crate::index::INDEX_DIR;
use crate::manifest::{BLOBS_SHARD, DOCS_SHARD, read_manifest};
use crate::matching::QueryOutput;
use crate::pattern::{PreparedPattern, count_overlapping};
use crate::query::run_query;
use crate::wire::OutputReport;
use support::*;

thread_local! {
    /// Searches on this thread count nothing and demote nothing.
    pub static NO_DEMOTION: Cell<bool> = const { Cell::new(false) };
    /// The AND cost rule's read band `(min_us, max_us)` on this thread.
    pub static BAND: Cell<Option<(f64, f64)>> = const { Cell::new(None) };
    /// `FMGREP_SHARD_BYTES` for indexes built on this thread, over the env.
    pub static SHARD_BYTES: Cell<Option<u64>> = const { Cell::new(None) };
    /// Loads on this thread of at least this many docs ask watchman; none
    /// when `None`.
    pub static ASK_FROM: Cell<Option<usize>> = const { Cell::new(None) };
    /// The watchman socket found on this thread; none when `None`, so tests
    /// reach only a daemon they started.
    pub static WATCHMAN: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    /// The watchman client run on this thread when no socket is given: a
    /// path where none is, unless a test puts a stand-in there.
    pub static WATCHMAN_PROGRAM: std::cell::RefCell<PathBuf> =
        std::cell::RefCell::new(PathBuf::from("/nonexistent/watchman"));
    /// The mountinfo file searches on this thread read; none when `None`,
    /// so no mount check is made.
    pub static MOUNTS: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    /// The cache directory of searches on this thread; none when `None`.
    pub static CACHE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    /// The warnings given by this thread's searches, every one.
    pub static NOTES: std::cell::RefCell<Arc<Mutex<Vec<String>>>> = std::cell::RefCell::default();
    /// The cause a watch started on this thread fails with.
    pub static WATCH_FAILS: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// This thread's watchman hooks, for a thread a load spawns.
pub struct Hooks {
    ask_from: Option<usize>,
    watchman: Option<PathBuf>,
    program: PathBuf,
    mounts: Option<PathBuf>,
    cache: Option<PathBuf>,
    notes: Arc<Mutex<Vec<String>>>,
}

impl Hooks {
    pub fn here() -> Self {
        Self {
            ask_from: ASK_FROM.get(),
            watchman: WATCHMAN.with_borrow(Clone::clone),
            program: WATCHMAN_PROGRAM.with_borrow(Clone::clone),
            mounts: MOUNTS.with_borrow(Clone::clone),
            cache: CACHE.with_borrow(Clone::clone),
            notes: NOTES.with_borrow(Arc::clone),
        }
    }

    pub fn apply(self) {
        ASK_FROM.set(self.ask_from);
        WATCHMAN.set(self.watchman);
        WATCHMAN_PROGRAM.set(self.program);
        MOUNTS.set(self.mounts);
        CACHE.set(self.cache);
        NOTES.set(self.notes);
    }
}

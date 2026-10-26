//! Tests over the real command line, and the helpers every test module shares.

mod drive;
mod expr;
pub(crate) mod filter;
mod index;
mod json;
mod matches;
mod output;
mod serve;
pub(crate) mod support;

use std::cell::Cell;
use std::path::{Path, PathBuf};

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
    /// `FMGREP_SAMPLE_RATE` for indexes built on this thread, over the env.
    pub static SAMPLE_RATE: Cell<Option<u32>> = const { Cell::new(None) };
    /// `FMGREP_JOURNAL` for indexes and loads on this thread, over the env.
    pub static JOURNAL: Cell<Option<&'static str>> = const { Cell::new(Some("off")) };
}

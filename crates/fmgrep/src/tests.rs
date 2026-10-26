//! Tests over the real command line, and the helpers every test module shares.

mod drive;
mod expr;
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
    /// `FMGREP_AND_LOCATE_RATIO` for searches on this thread, over the env.
    pub static RATIO: Cell<Option<u64>> = const { Cell::new(None) };
    /// `FMGREP_SHARD_BYTES` for indexes built on this thread, over the env.
    pub static SHARD_BYTES: Cell<Option<u64>> = const { Cell::new(None) };
}

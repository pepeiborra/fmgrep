//! fmgrep is indexed grep: it prints what ripgrep prints, answered from a
//! persistent, sharded FM-index built once per tree.
//!
//! ```sh
//! cargo install fmgrep
//! fmgrep index                        # writes ./.fmgrep
//! fmgrep HscEnv                       # as rg HscEnv
//! fmgrep -l '/Dyn[A-Z]\w+/'           # a /../ term is a regex
//! fmgrep --expr 'HscEnv AND (DynFlags OR /Dyn[A-Z]\w+/)'
//! ```
//!
//! How a search runs:
//!
//! 1. `index` walks the tree, cuts it into shards and builds one docfm blob
//!    per shard into `.fmgrep/blobs.shard`.
//! 2. A search maps `blobs.shard` and, unless the index was built with
//!    `--store-docs`, stats every file against its stamp.
//! 3. Each term probes every shard: a literal by backward search, `-i` by a
//!    case-folding walk, a pattern by its required literal or a reversed-DFA
//!    walk. The result is exact documents or candidates.
//! 4. Candidates and changed files are read from the tree (or `docs.shard`),
//!    verified by the regex crate and printed as rg prints them.
//!
//! Reference, as paths in the repository:
//!
//! - `docs/index-format.md`: the on-disk layout, the walk and staleness.
//! - `docs/protocol.md`: `serve`, `--server` and the `--json` shapes.
//! - `docs/tuning.md`: the `FMGREP_*` environment variables.
//! - `docs/design/`: the decisions and the measurements behind them.
//! - `fmgrep --help`: the command line.
//!
//! The library items serve the binary and its benchmark harness; they are
//! not a stable API.

mod automaton;
mod binary;
mod blobs;
mod budget;
mod bytes;
mod cli;
mod cursor;
mod docs;
mod events;
mod expr;
mod fetch;
mod fold;
mod index;
mod manifest;
mod matching;
mod output;
mod pattern;
mod plan;
mod probe;
mod query;
mod registry;
mod sharded;
mod sink;
mod source;
mod term;
#[cfg(test)]
mod tests;
mod unified;
mod verbose;
mod verify;
mod walk;
mod wire;

pub use cli::{Cli, Outcome, dispatch, run};
pub use registry::cli_spec;
pub use term::{RegexError, regex_pattern};
pub use unified::{UNIFIED_SEPARATOR, UnifiedCorpus};
pub use verbose::set_verbose;
pub use walk::{Corpus, DocMeta, Document, Stamp, discover_index_docs, discover_index_meta};
pub use wire::{IndexSummary, LeafReport, OutputReport, QueryReport};

use binary::is_binary_doc;
use walk::{for_each_index_doc, read_stamped};

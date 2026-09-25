//! The benchmark harness library: corpora and query sets, the in-process
//! index traits, the ripgrep row, reports and JSONL traces.

pub mod corpora;
mod index;
mod report;
pub mod rg;
mod staging;
pub mod terms;
mod trace;

pub use fmgrep::{
    Corpus, DocMeta, Document, RegexError, Stamp, UNIFIED_SEPARATOR, UnifiedCorpus,
    discover_index_docs, discover_index_meta, regex_pattern, set_verbose,
};
pub use index::{UnifiedIndex, UnifiedRegexSearch};
pub use report::{
    BenchmarkReport, LoadSpan, MemoryStats, QueryMetrics, QueryMode, run_unified_benchmark,
};
pub use staging::StagedDocs;
pub use trace::{EngineBinary, TraceOptions, write_jsonl_trace_with_options};

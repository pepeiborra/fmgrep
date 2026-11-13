//! The non-match output: `--help`, the index summary, `status`, the
//! `--json` reports.

use std::io::Write;
use std::path::Path;

use crate::manifest::Manifest;
use crate::output::WRITE_FAILED;
use crate::wire::{IndexSummary, OutputReport};

/// A closed stdout ends the report silently, as it does for matches.
pub fn line(out: &mut dyn Write, text: &str) -> Result<(), String> {
    match writeln!(out, "{text}") {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(format!("{WRITE_FAILED} report: {e}")),
    }
}

fn encoded<T: serde::Serialize>(what: &str, value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("error: failed to encode {what}: {e}"))
}

pub fn index_summary(out: &mut dyn Write, json: bool, s: &IndexSummary) -> Result<(), String> {
    if json {
        return line(out, &encoded("summary", s)?);
    }
    line(
        out,
        &format!(
            "indexed {num} file{plural} ({bytes} bytes) -> {path} in {ms} ms",
            num = s.num_files,
            plural = if s.num_files == 1 { "" } else { "s" },
            bytes = s.num_bytes,
            path = s.path,
            ms = s.build_time_us / 1000,
        ),
    )
}

/// `status`: under `--json` the manifest as one JSON line, else one
/// `name: value` line per scalar, by the manifest's names, without the
/// tables.
pub fn status(out: &mut dyn Write, json: bool, dir: &Path, m: &Manifest) -> Result<(), String> {
    if json {
        return line(out, &encoded("manifest", m)?);
    }
    let mut lines = vec![
        format!("index: {}", dir.display()),
        format!("format: {}", m.format),
        format!("store_docs: {}", m.store_docs),
        format!("num_docs: {}", m.num_docs),
        format!("num_bytes: {}", m.num_bytes),
        format!("bom_bytes: {}", m.bom_bytes),
        format!("index_size_bytes: {}", m.index_size_bytes),
        format!("build_time_us: {}", m.build_time_us),
        format!("index_started_ns: {}", m.index_started_ns),
        format!("shards: {}", m.blobs.len()),
    ];
    lines.extend(m.corpus_root.as_ref().map(|r| format!("corpus_root: {r}")));
    lines.iter().try_for_each(|l| line(out, l))
}

/// The wire report; the human query path prints matches instead.
pub fn query_report(out: &mut dyn Write, report: &OutputReport) -> Result<(), String> {
    line(out, &encoded("report", report)?)
}

/// Human `--approx-counts`: the report's FM numbers, one line per term,
/// then each expression leaf's total indented.
pub fn counts_summary(out: &mut dyn Write, report: &OutputReport) -> Result<(), String> {
    for q in &report.queries {
        let (n, docs) = (q.total_matches, q.docs_matched);
        line(
            out,
            &format!(
                "{term}: {n} {} in {docs} file{fplural}",
                matches(n),
                term = q.term,
                fplural = if docs == 1 { "" } else { "s" },
            ),
        )?;
        for leaf in &q.leaves {
            let n = leaf.total_matches;
            line(out, &format!("  {}: {n} {}", leaf.term, matches(n)))?;
        }
    }
    Ok(())
}

fn matches(n: u64) -> &'static str {
    if n == 1 { "match" } else { "matches" }
}

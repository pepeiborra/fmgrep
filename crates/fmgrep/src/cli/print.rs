//! The non-match output: `--help`, the index summary, the `--json` reports.

use std::io::Write;

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
            "indexed {num} file{plural} ({bytes} bytes) -> {path} ({name}) in {ms} ms",
            num = s.num_files,
            plural = if s.num_files == 1 { "" } else { "s" },
            bytes = s.num_bytes,
            path = s.path,
            name = s.index_name,
            ms = s.build_time_us / 1000,
        ),
    )
}

/// The wire report; the human query path prints matches instead.
pub fn query_report(out: &mut dyn Write, report: &OutputReport) -> Result<(), String> {
    line(out, &encoded("report", report)?)
}

/// Human `--approx-counts` / `--occurrences`: the report's FM numbers, one
/// line per term, then each expression leaf's total indented.
pub fn counts_summary(out: &mut dyn Write, report: &OutputReport) -> Result<(), String> {
    for q in &report.queries {
        let n = q.total_matches;
        let text = match q.docs_matched {
            Some(docs) => format!(
                "{term}: {n} {} in {docs} file{fplural}",
                matches(n),
                term = q.term,
                fplural = if docs == 1 { "" } else { "s" },
            ),
            None => format!("{}: {n} {}", q.term, occurrences(n)),
        };
        line(out, &text)?;
        for leaf in &q.leaves {
            let n = leaf.total_matches;
            let unit = if q.docs_matched.is_some() {
                matches(n)
            } else {
                occurrences(n)
            };
            line(out, &format!("  {}: {n} {unit}", leaf.term))?;
        }
    }
    Ok(())
}

fn matches(n: u64) -> &'static str {
    if n == 1 { "match" } else { "matches" }
}

fn occurrences(n: u64) -> &'static str {
    if n == 1 { "occurrence" } else { "occurrences" }
}

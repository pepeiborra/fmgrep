//! The non-match output: `--help`, the index summary, `status`, the
//! `--json` reports.

use std::io::Write;

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

/// `status` of an index: under `--json` the `json` report, which holds
/// the manifest, as one line, else `first`, then one `name: value` line
/// per scalar, by the manifest's names, without the tables.
pub fn status(
    out: &mut dyn Write,
    json: Option<impl serde::Serialize>,
    first: &str,
    m: &Manifest,
) -> Result<(), String> {
    if let Some(report) = json {
        // Every path is printed, and the encoding checks the path text
        // once. The paths are the only part of a manifest whose encoding
        // can fail: a damaged one is refused in the manifest's words.
        let json = serde_json::to_string(&report).map_err(|e| e.to_string())?;
        return line(out, &json);
    }
    let mut lines = vec![
        first.to_owned(),
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
    lines.extend(
        m.commit
            .as_ref()
            .map(|c| format!("commit: {} {}", c.vcs, c.id)),
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Commit, FileTable, MANIFEST_FORMAT, damaged};

    fn manifest(files: FileTable) -> Manifest {
        Manifest {
            format: MANIFEST_FORMAT,
            store_docs: true,
            num_docs: files.len(),
            num_bytes: 0,
            bom_bytes: 0,
            index_size_bytes: 0,
            build_time_us: 0,
            index_started_ns: 0,
            doc_lens: std::iter::repeat_n(0, files.len()).collect(),
            files,
            blobs: vec![],
            blob_lens: vec![],
            corpus_root: None,
            commit: None,
        }
    }

    fn status_json(m: &Manifest) -> Result<String, String> {
        let mut out = Vec::new();
        status(&mut out, Some(m), "idx", m)?;
        Ok(String::from_utf8(out).unwrap())
    }

    /// `status --json` prints the manifest's JSON; a damaged path is
    /// refused in the manifest's words, as the whole check gives them, not
    /// as a failed encoding.
    #[test]
    fn status_json_refuses_a_damaged_path_in_the_manifests_words() {
        let m = manifest(["a.c", "é/b.c"].into_iter().collect());
        let want = serde_json::to_string(&m).unwrap() + "\n";
        assert_eq!(status_json(&m), Ok(want));
        let not_utf8 = manifest(damaged(&[3, 5], b"a.cb\xFF"));
        assert_eq!(
            status_json(&not_utf8),
            Err("error: idx/manifest.bin holds a path that is not UTF-8; re-run `index`".into())
        );
        let cut = manifest(damaged(&[1, 3], "éb".as_bytes()));
        assert_eq!(
            status_json(&cut),
            Err("error: idx/manifest.bin cuts path 0 inside a character; re-run `index`".into())
        );
    }

    /// The served commit is the manifest's last line, and its last key in
    /// `--json`.
    #[test]
    fn status_prints_the_served_commit_last() {
        let m = Manifest {
            format: MANIFEST_FORMAT,
            store_docs: true,
            num_docs: 1,
            num_bytes: 3,
            bom_bytes: 0,
            index_size_bytes: 9,
            build_time_us: 7,
            index_started_ns: 5,
            files: ["a.c"].into_iter().collect(),
            blobs: vec!["shard-00000000-00000001.bin".into()],
            doc_lens: vec![3].into(),
            blob_lens: vec![11],
            corpus_root: None,
            commit: Some(Commit {
                vcs: "git".into(),
                id: "1a2b".into(),
            }),
        };
        let printed = |json: bool| {
            let mut out = Vec::new();
            status(&mut out, json.then_some(&m), "index: /idx", &m).unwrap();
            String::from_utf8(out).unwrap()
        };
        let text = printed(false);
        assert!(text.ends_with("shards: 1\ncommit: git 1a2b\n"), "{text}");
        let json = printed(true);
        let tail = r#""blob_lens":[11],"commit":{"vcs":"git","id":"1a2b"}}"#;
        assert!(json.ends_with(&format!("{tail}\n")), "{json}");
    }
}

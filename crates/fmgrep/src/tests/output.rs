//! The writer over the real drive: what a search prints.

use super::*;

use crate::output::{Format, MatchWriter, OutputConfig};

fn cfg(format: Format) -> OutputConfig {
    OutputConfig {
        format,
        heading: false,
        line_numbers: true,
        context: false,
        multiline: false,
        color: false,
        banner: false,
    }
}

/// Print one query through the drive.
fn written(index: &Path, terms: &[&str], output: QueryOutput, cfg: OutputConfig) -> String {
    let manifest = read_manifest(index).unwrap();
    let terms: Vec<String> = terms.iter().map(|t| t.to_string()).collect();
    let mut buf: Vec<u8> = Vec::new();
    let mut writer = MatchWriter::new(&mut buf, cfg);
    run_query(&request(index, &manifest, &terms, output), &mut writer).unwrap();
    writer.finish().unwrap();
    String::from_utf8(buf).unwrap()
}

const MATCHES: QueryOutput = QueryOutput::Matches {
    before: 0,
    after: 0,
    line_numbers: true,
};

#[test]
fn matches_mode_prints_every_matched_line() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        assert_eq!(
            written(&index, &["printk"], MATCHES, cfg(Format::Lines)),
            "a.c:1:printk one\na.c:3:printk two printk\nb.c:2:tail printk\n",
            "store_docs={store_docs}"
        );
        assert_eq!(
            written(&index, &["printk"], MATCHES, cfg(Format::OnlyMatching)),
            "a.c:1:printk\na.c:3:printk\na.c:3:printk\nb.c:2:printk\n",
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn streamed_output_is_ascending_by_path() {
    // The drive merges probe-exact and fetched documents; the printed order
    // is the manifest's, whatever the fetch order was.
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("corpus");
    std::fs::create_dir_all(folder.join("deep/er")).unwrap();
    std::fs::write(folder.join("deep/er/x.c"), b"printk\n").unwrap();
    std::fs::write(folder.join("top.c"), b"printk\n").unwrap();
    std::fs::write(folder.join("mid.c"), b"printk\n").unwrap();
    for store_docs in LAYOUTS {
        let index = tmp.path().join(format!("idx-{store_docs}"));
        index_it(&folder, &index, store_docs);
        assert_eq!(
            written(&index, &["printk"], MATCHES, cfg(Format::Lines)),
            "deep/er/x.c:1:printk\nmid.c:1:printk\ntop.c:1:printk\n",
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn probe_only_documents_still_print_counts() {
    // The index answers `printk` exactly from the probe: `-l` and
    // `--count-matches` print without fetching a single document.
    let tmp = tempfile::tempdir().unwrap();
    let index = staged(tmp.path(), true);
    std::fs::remove_file(index.join(DOCS_SHARD)).unwrap();
    assert_eq!(
        written(
            &index,
            &["printk"],
            QueryOutput::FilesWithMatches,
            cfg(Format::FilesWithMatches)
        ),
        "a.c\nb.c\n"
    );
    assert_eq!(
        written(
            &index,
            &["printk"],
            QueryOutput::CountMatches,
            cfg(Format::CountMatches)
        ),
        "a.c:3\nb.c:1\n"
    );
}

#[test]
fn count_lines_prints_rgs_line_counts() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        assert_eq!(
            written(
                &index,
                &["printk"],
                QueryOutput::CountLines,
                cfg(Format::CountLines)
            ),
            "a.c:2\nb.c:1\n",
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn the_wire_report_carries_counts_only() {
    // The human path writes as it streams; the report it returns keeps the
    // counting numbers and nothing per file.
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let manifest = read_manifest(&index).unwrap();
        let terms = vec!["printk".to_string()];
        let req = request(&index, &manifest, &terms, MATCHES);
        let mut buf: Vec<u8> = Vec::new();
        let mut writer = MatchWriter::new(&mut buf, cfg(Format::Lines));
        let out = run_query(&req, &mut writer).unwrap();
        writer.finish().unwrap();
        assert!(!buf.is_empty(), "store_docs={store_docs}");
        assert_eq!(
            (out.queries[0].total_matches, out.queries[0].docs_matched),
            (4, 2),
            "store_docs={store_docs}"
        );
    }
}

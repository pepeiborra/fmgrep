//! The writer over the real drive: what a search prints.

use super::*;

use crate::matching::Print;
use crate::output::{Format, MatchWriter, OutputConfig};

fn cfg(format: Format) -> OutputConfig {
    OutputConfig {
        format,
        heading: false,
        context: false,
        color: false,
        banner: false,
        filename: true,
        stats: false,
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

/// rg `--no-heading -n`.
const MATCHES: QueryOutput = QueryOutput::Matches(Print {
    before: 0,
    after: 0,
    line_numbers: true,
    only_matching: false,
    color: false,
    path: true,
    json: false,
    stats: false,
});

const ONLY: QueryOutput = QueryOutput::Matches(Print {
    only_matching: true,
    ..match MATCHES {
        QueryOutput::Matches(p) => p,
        _ => unreachable!(),
    }
});

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
            written(&index, &["printk"], ONLY, cfg(Format::Lines)),
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
            cfg(Format::Count)
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
                cfg(Format::Count)
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

#[test]
fn stats_follow_the_lines() {
    // rg --no-heading --stats printk, without its two lines of times.
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let got = printed(&index, &["--stats", "--no-heading"], &["printk"]);
        let timeless: String = (got.lines())
            .filter(|l| !l.ends_with(" seconds spent searching") && !l.ends_with(" seconds total"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            timeless,
            "a.c:printk one\na.c:printk two printk\nb.c:tail printk\n\n\
             4 matches\n3 matched lines\n2 files contained matches\n2 files searched\n\
             53 bytes printed\n53 bytes searched\n",
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn stats_count_the_files_a_glob_keeps() {
    // As rg -g counts them, but for d.c's BOM: the index does not record
    // which file holds one, so a narrowed search counts it (rg: 18 bytes).
    let files: [(&str, &[u8]); 3] = [
        ("a.c", b"printk one\n"),
        ("b.c", b"tail printk\n"),
        ("d.c", b"\xef\xbb\xbfprintk\n"),
    ];
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let folder = stage_corpus(tmp.path(), &files);
        let index = tmp.path().join("idx");
        index_it(&folder, &index, store_docs);
        for (flags, terms, want) in [
            (&["--stats"][..], &["printk"][..], (3, 30)),
            (&["--stats", "-g", "*.c"], &["printk"], (3, 30)),
            (&["--stats", "-g", "b.c"], &["printk"], (1, 12)),
            (&["--stats", "-g", "!b.c"], &["printk"], (2, 21)),
            (
                &["--stats", "-g", "b.c", "--expr", "printk AND tail"],
                &[],
                (1, 12),
            ),
        ] {
            let got = printed(&index, flags, terms);
            let number = |what: &str| -> u64 {
                let line = got.lines().find(|l| l.ends_with(what)).unwrap();
                line.split(' ').next().unwrap().parse().unwrap()
            };
            assert_eq!(
                (number(" files searched"), number(" bytes searched")),
                want,
                "{flags:?} store_docs={store_docs}"
            );
        }
    }
}

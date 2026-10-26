//! The event stream over the real drive: what `--json` prints.

use super::*;

/// `(event type, path, line_number)` of every event but the summary.
fn events(stream: &str) -> Vec<(String, String, Option<u64>)> {
    stream
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|v| v["type"] != "summary")
        .map(|v| {
            (
                v["type"].as_str().unwrap().to_owned(),
                v["data"]["path"]["text"].as_str().unwrap().to_owned(),
                v["data"]["line_number"].as_u64(),
            )
        })
        .collect()
}

#[test]
fn json_stream_is_ascending_over_both_layouts() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let got = printed(&index, &["--json", "--matches"], &["printk"]);
        assert_eq!(
            events(&got),
            vec![
                ("begin".into(), "a.c".into(), None),
                ("match".into(), "a.c".into(), Some(1)),
                ("match".into(), "a.c".into(), Some(3)),
                ("end".into(), "a.c".into(), None),
                ("begin".into(), "b.c".into(), None),
                ("match".into(), "b.c".into(), Some(2)),
                ("end".into(), "b.c".into(), None),
            ],
            "store_docs={store_docs}"
        );
        // rg ignores -o in its JSON printer; so do we.
        assert_eq!(
            timeless_lines(&got),
            timeless_lines(&printed(&index, &["--json", "-o"], &["printk"])),
            "store_docs={store_docs}"
        );
    }
}

/// rg strips a leading UTF-8 BOM in its searcher, so the whole stream — line
/// text, submatch columns, per file and corpus `bytes_searched` — is the
/// stripped one. rg --json '^module' bom.c plain.c: 29 + 17 = 46 bytes.
#[test]
fn a_leading_bom_is_stripped_from_the_whole_stream() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let folder = stage_corpus(
            tmp.path(),
            &[
                ("bom.c", b"\xef\xbb\xbfmodule Foo where\nsecond line\n"),
                ("plain.c", b"module Bar where\n"),
            ],
        );
        let index = tmp.path().join(format!("idx-{store_docs}"));
        let summary = index_it(&folder, &index, store_docs);
        // The corpus keeps its BOM on disk; only the searcher drops it.
        assert_eq!(summary.num_bytes, 49, "store_docs={store_docs}");
        let got = printed(&index, &["--json", "--matches", "--regex"], &["^module"]);
        let events = timeless_lines(&got);
        assert_eq!(
            events[1]["data"]["lines"]["text"], "module Foo where\n",
            "store_docs={store_docs}"
        );
        assert_eq!(
            events[1]["data"]["submatches"][0]["start"], 0,
            "store_docs={store_docs}"
        );
        assert_eq!(
            events[1]["data"]["submatches"][0]["end"], 6,
            "store_docs={store_docs}"
        );
        assert_eq!(
            events[2]["data"]["stats"]["bytes_searched"], 29,
            "store_docs={store_docs}"
        );
        assert_eq!(
            events[5]["data"]["stats"]["bytes_searched"], 17,
            "store_docs={store_docs}"
        );
        let last = events.last().unwrap();
        assert_eq!(last["type"], "summary", "store_docs={store_docs}");
        assert_eq!(
            last["data"]["stats"]["bytes_searched"], 46,
            "store_docs={store_docs}"
        );
        assert_eq!(
            last["data"]["stats"]["matched_lines"], 2,
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn json_matches_mode_prints_no_wire_report() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let got = printed(&index, &["--json", "--matches"], &["printk"]);
        assert!(
            !got.contains("index_time_us"),
            "store_docs={store_docs}: {got}"
        );
        let last: serde_json::Value =
            serde_json::from_str(got.lines().next_back().unwrap()).unwrap();
        assert_eq!(last["type"], "summary", "store_docs={store_docs}");
        assert_eq!(
            last["data"]["stats"]["matches"], 4,
            "store_docs={store_docs}"
        );
        assert_eq!(
            last["data"]["stats"]["matched_lines"], 3,
            "store_docs={store_docs}"
        );
        assert_eq!(
            last["data"]["stats"]["searches"], 2,
            "store_docs={store_docs}"
        );
        assert_eq!(
            last["data"]["stats"]["searches_with_match"], 2,
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn json_counts_mode_prints_the_report() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let all = [
            &["--json"][..],
            &["--json", "--approx-counts"][..],
            &["--json", "-c"][..],
        ];
        for flags in all {
            let got = printed(&index, flags, &["printk"]);
            let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
            assert_eq!(
                v["queries"][0]["total_matches"], 4,
                "store_docs={store_docs} {flags:?}"
            );
            assert!(
                v["queries"][0].get("files").is_none(),
                "store_docs={store_docs} {flags:?}"
            );
        }
    }
}

/// Without `--json` the counts-only mode has no matches to print, so it
/// prints its numbers: one line per term, in argv order.
#[test]
fn human_approx_counts_prints_a_line_per_term() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        assert_eq!(
            printed(&index, &["--approx-counts"], &["printk", "aa"]),
            "printk: 4 matches in 2 files\naa: 3 matches in 1 file\n",
            "store_docs={store_docs}"
        );
    }
}

#[test]
fn json_stream_of_a_missing_term_is_a_lone_summary() {
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let index = staged(tmp.path(), store_docs);
        let got = printed(&index, &["--json", "--matches"], &["nowhere"]);
        assert_eq!(got.lines().count(), 1, "store_docs={store_docs}: {got}");
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        assert_eq!(v["type"], "summary", "store_docs={store_docs}");
        assert_eq!(
            v["data"]["stats"]["searches_with_match"], 0,
            "store_docs={store_docs}"
        );
        assert_eq!(
            v["data"]["stats"]["bytes_searched"], 53,
            "store_docs={store_docs}"
        );
    }
}

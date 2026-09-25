use serde::{Deserialize, Serialize};

/// `index --json` stdout summary.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexSummary {
    pub index_name: String,
    /// The index keeps a copy of the documents (`index --store-docs`).
    pub store_docs: bool,
    pub path: String,
    pub num_files: usize,
    pub num_bytes: u64,
    pub index_size_bytes: u64,
    pub build_time_us: u64,
}

/// One per-term result line, mirroring the harness's per-term metrics.
///
/// `total_matches` / `docs_matched` are rg's numbers in every mode that runs
/// the matching pass; in counts mode they are the FM-index's document-wide
/// numbers, and `counts_exact` says whether the two coincide.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryReport {
    pub term: String,
    pub latency_us: u64,
    pub total_matches: u64,
    /// Absent under `--occurrences`, which places no match in a doc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_matched: Option<usize>,
    /// False when the counts above can differ from rg's: in counts mode any
    /// pattern, and a literal that is empty, self-overlapping or holds `\n`;
    /// under `-l`, which proves membership only. Absent on the wire when
    /// true, so rg-exact reports keep their shape.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub counts_exact: bool,
    /// An expression's per-leaf totals within its matching documents, in
    /// the counting modes; absent otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leaves: Vec<LeafReport>,
}

/// One expression leaf's `total_matches`, as `term` would count it.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeafReport {
    pub term: String,
    pub total_matches: u64,
}

fn yes() -> bool {
    true
}

fn is_yes(v: &bool) -> bool {
    *v
}

/// `--json` stdout report, mirroring the harness's benchmark report.
/// `index_time_us` is the query-spawn blob-load wall, not the manifest's
/// canonical `build_time_us`.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputReport {
    pub index_name: String,
    pub index_time_us: u64,
    pub index_size_bytes: u64,
    pub num_files_indexed: usize,
    pub queries: Vec<QueryReport>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_round_trips_through_wire_schema() {
        let out = OutputReport {
            index_name: "fmgrep".into(),
            index_time_us: 12,
            index_size_bytes: 345,
            num_files_indexed: 1,
            queries: vec![QueryReport {
                term: "printk".into(),
                latency_us: 3,
                total_matches: 2,
                docs_matched: Some(1),
                counts_exact: true,
                leaves: Vec::new(),
            }],
        };
        let json = serde_json::to_string(&out).unwrap();
        // Counts mode stays byte-identical: no `files` key at all.
        assert!(
            !json.contains("\"files\""),
            "counts JSON must not grow: {json}"
        );
        let back: OutputReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, out);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        for key in [
            "index_name",
            "index_time_us",
            "index_size_bytes",
            "num_files_indexed",
            "queries",
        ] {
            assert!(v.get(key).is_some(), "missing top-level key {key}");
        }
        for key in ["term", "latency_us", "total_matches", "docs_matched"] {
            assert!(
                v["queries"][0].get(key).is_some(),
                "missing query key {key}"
            );
        }
        // Exact counts stay off the wire; only an inexact term carries the key.
        assert!(v["queries"][0].get("counts_exact").is_none(), "{json}");
        let mut inexact = out;
        inexact.queries[0].counts_exact = false;
        let json = serde_json::to_string(&inexact).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["queries"][0]["counts_exact"], false);
        assert_eq!(
            serde_json::from_str::<OutputReport>(&json).unwrap(),
            inexact
        );
    }

    #[test]
    fn occurrences_report_omits_docs_matched() {
        let mut q = QueryReport {
            term: "printk".into(),
            latency_us: 3,
            total_matches: 2,
            docs_matched: None,
            counts_exact: true,
            leaves: Vec::new(),
        };
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(
            json,
            r#"{"term":"printk","latency_us":3,"total_matches":2}"#
        );
        assert_eq!(serde_json::from_str::<QueryReport>(&json).unwrap(), q);
        q.docs_matched = Some(1);
        assert_eq!(
            serde_json::to_string(&q).unwrap(),
            r#"{"term":"printk","latency_us":3,"total_matches":2,"docs_matched":1}"#
        );
    }

    #[test]
    fn leaves_stay_off_the_default_wire() {
        let mut q = QueryReport {
            term: "a AND b".into(),
            latency_us: 3,
            total_matches: 5,
            docs_matched: Some(2),
            counts_exact: true,
            leaves: Vec::new(),
        };
        let bare = r#"{"term":"a AND b","latency_us":3,"total_matches":5,"docs_matched":2}"#;
        assert_eq!(serde_json::to_string(&q).unwrap(), bare);
        assert_eq!(serde_json::from_str::<QueryReport>(bare).unwrap(), q);
        q.leaves = vec![
            LeafReport {
                term: "a".into(),
                total_matches: 3,
            },
            LeafReport {
                term: "b".into(),
                total_matches: 2,
            },
        ];
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(
            json,
            r#"{"term":"a AND b","latency_us":3,"total_matches":5,"docs_matched":2,"leaves":[{"term":"a","total_matches":3},{"term":"b","total_matches":2}]}"#
        );
        assert_eq!(serde_json::from_str::<QueryReport>(&json).unwrap(), q);
    }

    #[test]
    fn index_summary_round_trips_through_wire_schema() {
        let s = IndexSummary {
            index_name: "fmgrep".into(),
            store_docs: true,
            path: "/tmp/idx".into(),
            num_files: 3,
            num_bytes: 42,
            index_size_bytes: 99,
            build_time_us: 7,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<IndexSummary>(&json).unwrap(), s);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        for key in [
            "index_name",
            "store_docs",
            "path",
            "num_files",
            "num_bytes",
            "index_size_bytes",
            "build_time_us",
        ] {
            assert!(v.get(key).is_some(), "missing key {key}");
        }
    }
}

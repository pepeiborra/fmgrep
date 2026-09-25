//! `--occurrences` over both layouts: counts mode's totals, no docs.

use super::*;

fn run(index: &Path, flags: &[&str], terms: &[&str]) -> Result<OutputReport, String> {
    let mut words = argv(&["--index", &index.display().to_string()]);
    words.extend(argv(flags));
    words.push("--".into());
    words.extend(argv(terms));
    match dispatch(&cli_spec(), &words)? {
        Outcome::Query(r) => Ok(r),
        _ => panic!("expected a query report"),
    }
}

/// `(term, total_matches, counts_exact)` per query; `docs_matched` is
/// present exactly when `docs`.
fn totals(r: &OutputReport, docs: bool) -> Vec<(String, u64, bool)> {
    r.queries
        .iter()
        .map(|q| {
            assert_eq!(q.docs_matched.is_some(), docs, "{q:?}");
            (q.term.clone(), q.total_matches, q.counts_exact)
        })
        .collect()
}

#[test]
fn occurrences_totals_equal_counts_mode() {
    let terms = [
        "printk",
        "aa",
        "/print[k]/",
        "/pr[i1]ntk/",
        "/a+/",
        "PrintK",
        "",
        "a\0b",
        "missing",
    ];
    for store_docs in LAYOUTS {
        let tmp = tempfile::tempdir().unwrap();
        let folder = stage_corpus(
            tmp.path(),
            &[
                ("a.c", b"printk one\nPrintK two printk\n"),
                ("b.c", b"aaaa\ntail printk"),
                ("c.c", b""),
            ],
        );
        let index = tmp.path().join("idx");
        index_it(&folder, &index, store_docs);
        for flags in [&[][..], &["-i"]] {
            let counts = run(&index, &[flags, &["--approx-counts"]].concat(), &terms).unwrap();
            let occurrences = run(&index, &[flags, &["--occurrences"]].concat(), &terms).unwrap();
            assert_eq!(
                totals(&occurrences, false),
                totals(&counts, true),
                "store_docs={store_docs} {flags:?}"
            );
        }
    }
}

#[test]
fn occurrences_on_a_files_source_count_stale_docs_current_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = stage_corpus(
        tmp.path(),
        &[
            ("a.c", b"printk hello printk"),
            ("b.c", b"nothing here"),
            ("c.c", b"printk"),
        ],
    );
    let index = tmp.path().join("idx");
    index_it(&folder, &index, false);
    std::fs::write(folder.join("b.c"), b"nothing here, changed").unwrap();
    let got = run(&index, &["--occurrences"], &["printk", "missing"]).unwrap();
    assert_eq!(
        totals(&got, false),
        [("printk".into(), 3, true), ("missing".into(), 0, true)]
    );
    std::fs::write(folder.join("a.c"), b"printk changed, printk printk").unwrap();
    std::fs::remove_file(folder.join("c.c")).unwrap();
    let terms = ["printk", "/pr[i1]ntk/", "/printk\\x20/", "hello"];
    let got = run(&index, &["--occurrences"], &terms).unwrap();
    let want = run(&index, &["--approx-counts"], &terms).unwrap();
    assert_eq!(totals(&got, false), totals(&want, true));
    assert_eq!(
        got.queries
            .iter()
            .map(|q| q.total_matches)
            .collect::<Vec<_>>(),
        [3, 3, 2, 0]
    );
}

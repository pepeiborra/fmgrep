use super::*;
use crate::cli::params::Mode;

/// The client's part counts as the server does: a lines search its
/// matched lines, with `--stats` and under `-o` rg's matches; so the
/// report of a merged search adds like numbers.
#[test]
fn the_clients_part_counts_as_the_server_does() {
    let tmp = tempfile::tempdir().unwrap();
    let texts = [("a.c", "printk one printk\nnone\n"), ("b.c", "printk b\n")];
    for (rel, text) in texts {
        std::fs::write(tmp.path().join(rel), text).unwrap();
    }
    let local = Local {
        files: texts.iter().map(|(rel, _)| (*rel).to_owned()).collect(),
        bytes: texts.iter().map(|(_, t)| t.len() as u64).sum(),
        lens: texts.iter().map(|(_, t)| t.len() as u64).collect(),
    };
    for (mode, stats, want) in [
        (Mode::Lines, false, 2),
        (Mode::Lines, true, 3),
        (Mode::Only, false, 3),
    ] {
        let p = QueryParams {
            terms: vec!["printk".to_owned()],
            mode,
            stats,
            ..QueryParams::default()
        };
        let part = part(&p, tmp.path(), &local, &mut Vec::new()).unwrap();
        let q = &part.report.queries[0];
        assert_eq!(
            (q.total_matches, q.docs_matched),
            (want, 2),
            "{mode:?} {stats}"
        );
    }
}

/// Under `--stats`, the client's corpus is its files that the globs
/// keep, as the server's is; without, all of them.
#[test]
fn the_clients_stats_corpus_is_what_the_filter_keeps() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("d")).unwrap();
    let texts = [("a.c", "printk a\n"), ("d/b.h", "printk bb\n")];
    for (rel, text) in texts {
        std::fs::write(tmp.path().join(rel), text).unwrap();
    }
    let local = Local {
        files: texts.iter().map(|(rel, _)| (*rel).to_owned()).collect(),
        bytes: 19,
        lens: vec![9, 10],
    };
    for (globs, stats, want) in [
        (&[][..], true, (2, 19)),
        (&["*.h"], true, (1, 10)),
        (&["!d/"], true, (1, 9)),
        (&["*.h"], false, (2, 19)),
    ] {
        let p = QueryParams {
            terms: vec!["printk".to_owned()],
            mode: Mode::Lines,
            stats,
            globs: globs.iter().map(|g| (*g).to_owned()).collect(),
            ..QueryParams::default()
        };
        let part = part(&p, tmp.path(), &local, &mut Vec::new()).unwrap();
        assert_eq!(part.corpus, want, "{globs:?} {stats}");
    }
}

//! CLI parsing is hand-rolled: the lockfile carries no CLI parser and the
//! flag set is tiny.

use bench::terms::source;

use crate::corpus::selector::{CorpusBase, CorpusSelector};

pub const DEFAULT_TRACE: &str = "trace.jsonl";

pub fn parse_arg(args: &[String], names: &[&str]) -> Option<String> {
    let mut iter = args.iter().peekable();
    while let Some(a) = iter.next() {
        for n in names {
            if a == n {
                return iter.next().cloned();
            }
            if let Some(v) = a.strip_prefix(&format!("{n}=")) {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn count_flag_occurrences(args: &[String], name: &str) -> usize {
    let prefix = format!("{name}=");
    args.iter()
        .skip(1)
        .filter(|a| a.as_str() == name || a.starts_with(prefix.as_str()))
        .count()
}

/// Raw `--corpus` value, rejecting a repeated flag (first-wins would silently
/// ignore all but one selection) and a bare `--corpus` whose value is missing
/// or would swallow the next flag. Never a silent default.
pub fn corpus_raw_arg(args: &[String]) -> Result<Option<String>, String> {
    let occurrences = count_flag_occurrences(args, "--corpus");
    if occurrences > 1 {
        return Err(format!(
            "--corpus given {occurrences} times: repeated --corpus is not \
             accepted (first-wins would silently ignore all but one selection)"
        ));
    }
    let raw = parse_arg(args, &["--corpus"]);
    if occurrences > 0 {
        match raw.as_deref() {
            None => {
                return Err("--corpus requires a value: expected <base>[-N] with base \
                     one of \"linux\", \"chromium\", \"ghc\", \"all\" (got no value)"
                    .to_string());
            }
            Some(v) if v.starts_with('-') => {
                return Err(format!(
                    "--corpus requires a value: got {v:?} (a flag-looking \
                     token — expected <base>[-N] with base one of \"linux\", \
                     \"chromium\", \"ghc\", \"all\"; a bare `--corpus` followed by \
                     another flag would swallow that flag, so this is rejected)"
                ));
            }
            _ => {}
        }
    }
    Ok(raw)
}

/// `--results FILE`: rejects a missing or flag-looking value and a parent
/// that is not a directory, so a bad path fails before the run.
pub fn results_arg(args: &[String]) -> Result<Option<String>, String> {
    if count_flag_occurrences(args, "--results") == 0 {
        return Ok(None);
    }
    let path = match parse_arg(args, &["--results"]) {
        Some(v) if !v.is_empty() && !v.starts_with('-') => v,
        Some(v) => return Err(format!("--results requires a file path, not {v:?}")),
        None => return Err("--results requires a file path".to_string()),
    };
    let parent = std::path::Path::new(&path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    if !parent.is_dir() {
        return Err(format!(
            "--results {path}: {} is not a directory",
            parent.display()
        ));
    }
    Ok(Some(path))
}

pub fn has_flag(args: &[String], name: &str) -> bool {
    args.iter()
        .any(|a| a == name || a == &format!("{name}=true") || a == &format!("{name}=1"))
}

pub fn warn_on_unknown_args(args: &[String]) {
    // Known flags (with or without `=value`); everything else starting with
    // `-` is reported but ignored so scripts keep working across phases.
    const KNOWN: &[&str] = &[
        "--terms",
        "--trace",
        "--verify",
        "--corpus",
        "--rows",
        "--dry-run",
        "--allow-stale",
        "--results",
        "--print-source",
        "--verbose",
        "-v",
        "--help",
        "-h",
    ];
    for a in args.iter().skip(1) {
        if !a.starts_with('-') {
            continue;
        }
        let key = a.split('=').next().unwrap_or(a);
        // A `--flag value` value starting with `-` would false-positive here —
        // acceptably rare for bench terms.
        if !KNOWN.contains(&key) {
            eprintln!("warning: unknown flag {a:?}; ignoring");
        }
    }
}

/// `<repo_url> <commit>` of the corpus a bare `--corpus` base is pinned to;
/// a merged or replicated selection has no single source.
pub fn source_line(raw: Option<&str>) -> Result<String, String> {
    let sel = CorpusSelector::resolve(raw)?;
    if sel.base == CorpusBase::All || sel.replicas != 1 {
        return Err(format!(
            "--print-source needs a single corpus (linux, chromium or ghc), not {:?}",
            sel.corpus_id()
        ));
    }
    let id = sel.corpus_id();
    let src = source(&id).ok_or_else(|| format!("corpus {id} has no query file"))?;
    Ok(format!("{} {}", src.repo_url, src.commit))
}

pub fn print_usage() {
    println!(
        "\
bench: fmgrep against ripgrep on one corpus, one table

Usage: bench [--corpus <base>[-N]] [--rows a,b] [--verify] [--results FILE] ...

Rows run in order over one staged copy of the corpus: rg scans per query;
fmgrep indexes, starts one server (`ready`) and times one `fmgrep --server`
process per query. Every query of the corpus's set runs: `/.../` is a regex,
anything else a literal. scripts/bench.sh clones the pinned corpus and runs
this with --verify.

Flags:
  --corpus <base>[-N]  linux, chromium, ghc (default) or all (every corpus
                       merged); -N replicates it N times (counts exact,
                       timings not comparable with N = 1)
  --rows a,b           rg, fmgrep, fmgrep-store-docs (default rg,fmgrep)
  --terms t1,/p1/,...  replace the corpus's query set
  --verify             fail unless every row agrees on every count and on the
                       indexed file count (within 0.1%)
  --results FILE       write the run as markdown, verify outcome included
  --print-source       print the corpus's pinned `<repo_url> <commit>`, exit
  --trace PATH         JSONL trace (default {DEFAULT_TRACE})
  --dry-run            print the header, queries and disk forecast, then exit
  --allow-stale        run binaries older than their sources (default: refuse)
  --verbose, -v        list every file the corpus walk skips
  --help, -h           print this help

The run refuses to start when the disk forecast does not fit.

Env:
  FMGREP_CORPUS_GHC, FMGREP_CORPUS_LINUX, FMGREP_CORPUS_CHROMIUM
                       corpus checkout; a set path must exist (default
                       target/bench/<id>)
  FMGREP_BENCH_BIN     fmgrep binary (default: next to bench, then PATH)
  FMGREP_BENCH_RG_BIN  rg binary (default: rg on PATH)
  FMGREP_BENCH_CHILD_TIMEOUT_SECS  per-child timeout (default 300)
  FMGREP_BENCH_RSS_OFF=1, FMGREP_BENCH_RSS_INTERVAL_MS  memory sampling
  FMGREP_VERBOSE=1     same as --verbose"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- --corpus CLI-shape rejection: duplicates and missing values are
    // loud usage errors, never silent first-wins or a silent default ----

    fn cli_args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn corpus_repeated_flag_fails_loudly() {
        // A repeated --corpus must not silently first-win.
        for argv in [
            vec!["bench", "--corpus", "linux", "--corpus", "chromium"],
            vec!["bench", "--corpus=linux", "--corpus=chromium"],
            vec!["bench", "--corpus", "linux", "--corpus=all"],
        ] {
            let err = corpus_raw_arg(&cli_args(&argv)).unwrap_err();
            assert!(err.contains("--corpus"), "argv {argv:?}: {err}");
            assert!(
                err.contains("times") || err.contains("repeated"),
                "argv {argv:?}: {err}"
            );
        }
    }

    #[test]
    fn corpus_bare_trailing_flag_fails_loudly() {
        // A trailing bare --corpus must not silently become the default.
        let err = corpus_raw_arg(&cli_args(&["bench", "--corpus"])).unwrap_err();
        assert!(err.contains("--corpus"), "err: {err}");
        assert!(err.contains("requires a value"), "err: {err}");
    }

    #[test]
    fn corpus_flag_swallowing_next_flag_fails_loudly() {
        // `--corpus --verify` must not swallow --verify as its value.
        let err = corpus_raw_arg(&cli_args(&["bench", "--corpus", "--verify"])).unwrap_err();
        assert!(err.contains("--corpus"), "err: {err}");
        assert!(err.contains("requires a value"), "err: {err}");
    }

    #[test]
    fn print_source_rejects_merged_and_replicated_selections() {
        for raw in ["all", "ghc-2", "all-3"] {
            let err = source_line(Some(raw)).unwrap_err();
            assert!(err.contains("--print-source"), "{raw}: {err}");
        }
        assert_eq!(
            source_line(None).unwrap(),
            "https://github.com/ghc/ghc.git 4cf7628aa66f56000c29aef93924b6b8515db8f7"
        );
        assert!(
            source_line(Some("linux"))
                .unwrap()
                .contains("torvalds/linux")
        );
    }

    #[test]
    fn corpus_wellformed_shapes_pass_through() {
        // The loud-reject above must not break the well-formed shapes.
        assert_eq!(corpus_raw_arg(&cli_args(&["bench"])).unwrap(), None);
        assert_eq!(
            corpus_raw_arg(&cli_args(&["bench", "--corpus", "linux"])).unwrap(),
            Some("linux".to_string())
        );
        assert_eq!(
            corpus_raw_arg(&cli_args(&["bench", "--corpus=all"])).unwrap(),
            Some("all".to_string())
        );
        assert_eq!(
            corpus_raw_arg(&cli_args(&["bench", "--corpus", "linux-5"])).unwrap(),
            Some("linux-5".to_string())
        );
    }

    #[test]
    fn results_rejects_missing_or_flag_value_and_missing_dir() {
        assert_eq!(results_arg(&cli_args(&["bench"])).unwrap(), None);
        assert!(results_arg(&cli_args(&["bench", "--results"])).is_err());
        assert!(results_arg(&cli_args(&["bench", "--results", "--verify"])).is_err());
        assert!(results_arg(&cli_args(&["bench", "--results", "/nonexistent/out.md"])).is_err());
        assert_eq!(
            results_arg(&cli_args(&["bench", "--results", "out.md"])).unwrap(),
            Some("out.md".to_string())
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.md").display().to_string();
        assert_eq!(
            results_arg(&cli_args(&["bench", &format!("--results={path}")])).unwrap(),
            Some(path)
        );
    }
}

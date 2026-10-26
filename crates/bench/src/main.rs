//! Benchmarks fmgrep against ripgrep, and tgrep when installed, over one corpus.

mod report;
mod rows;
mod tgrep;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use fmgrep::regex_pattern;

const USAGE: &str = "\
usage: bench [--corpus ghc|linux|chromium] [--rows ROWS] [--fixture] [--trace FILE]

Stages the corpus, runs every row over its queries, checks that every row
agrees with the first on every count, and prints the results as markdown.

  --corpus ID    ghc (default), linux or chromium; the checkout is
                 $FMGREP_CORPUS_<ID>, by default target/bench/<ID>
  --rows ROWS    comma-separated rg, fmgrep, fmgrep-store-docs, tgrep;
                 default rg,fmgrep, and tgrep when it is on PATH
  --fixture      search one file of the query set's literals instead
  --trace FILE   write one JSON line per query and per row

Exits 1 when a row fails or the rows disagree, 2 on a usage or corpus error.";

const CORPORA: [&str; 3] = ["ghc", "linux", "chromium"];
const ROWS: [&str; 4] = ["rg", "fmgrep", "fmgrep-store-docs", "tgrep"];

pub struct Query {
    pub term: String,
    pub latency: Duration,
    pub matches: u64,
    pub files: usize,
}

#[derive(Default)]
pub struct Row {
    pub name: &'static str,
    pub files: usize,
    pub queries: Vec<Query>,
    pub index_time: Option<Duration>,
    pub index_bytes: Option<u64>,
    pub ready: Option<Duration>,
    pub index_peak_rss: Option<u64>,
    pub footprint: Option<u64>,
    pub load1: Option<(f64, f64)>,
}

impl Query {
    fn new(term: &str, latency: Duration, (matches, files): (u64, usize)) -> Self {
        Query {
            term: term.to_string(),
            latency,
            matches,
            files,
        }
    }
}

impl Row {
    fn new(name: &'static str, files: usize) -> Self {
        Row {
            name,
            files,
            ..Row::default()
        }
    }
}

pub struct QuerySet {
    pub repo_url: String,
    pub commit: String,
    pub queries: Vec<String>,
}

fn query_set(corpus: &str) -> QuerySet {
    let json = match corpus {
        "ghc" => include_str!("../queries/ghc.json"),
        "linux" => include_str!("../queries/linux.json"),
        _ => include_str!("../queries/chromium.json"),
    };
    let v: serde_json::Value = serde_json::from_str(json).expect("query file is JSON");
    let text = |v: &serde_json::Value| v.as_str().expect("a string").to_string();
    let queries = v["queries"].as_array().expect("a query list");
    QuerySet {
        repo_url: text(&v["repo_url"]),
        commit: text(&v["commit"]),
        queries: queries.iter().map(text).collect(),
    }
}

/// An error line and the exit code it ends the run with.
type Fail = (u8, String);

fn usage(e: impl std::fmt::Display) -> Fail {
    (2, format!("error: {e}"))
}

fn failed(e: impl std::fmt::Display) -> Fail {
    (1, format!("error: {e}"))
}

fn select_rows(
    spec: Option<&str>,
    tgrep: &Result<tgrep::Tgrep, String>,
) -> Result<Vec<&'static str>, String> {
    let default = if tgrep.is_ok() {
        "rg,fmgrep,tgrep"
    } else {
        "rg,fmgrep"
    };
    let spec = spec.unwrap_or(default);
    let wanted: Vec<&str> = spec.split(',').map(str::trim).collect();
    if let Some(bad) = wanted.iter().find(|w| !ROWS.contains(w)) {
        return Err(format!("--rows: {bad:?} is not one of {}", ROWS.join(", ")));
    }
    if let (true, Err(note)) = (wanted.contains(&"tgrep"), tgrep) {
        return Err(format!("--rows names tgrep, but {note}"));
    }
    Ok(ROWS.into_iter().filter(|r| wanted.contains(r)).collect())
}

/// `fmgrep` next to this executable, else on `PATH`.
fn fmgrep_bin() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = exe.parent().map(Path::to_path_buf).into_iter();
    let mut bins = dirs
        .chain(std::env::split_paths(&path))
        .map(|d| d.join("fmgrep"));
    bins.find(|p| p.is_file())
}

fn corpus_root(id: &str) -> Result<PathBuf, String> {
    let var = format!("FMGREP_CORPUS_{}", id.to_uppercase());
    let root = match std::env::var_os(&var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/bench")).join(id),
    };
    if !root.exists() {
        return Err(format!(
            "no {id} corpus at {}: run scripts/bench.sh --corpus {id} [--repo-path DIR] \
             to clone the pinned commit, or set {var}=DIR",
            root.display()
        ));
    }
    Ok(root)
}

/// Every row's counts must equal the first row's.
fn verify(rows: &[Row]) -> Vec<String> {
    let Some((base, others)) = rows.split_first() else {
        return Vec::new();
    };
    let mut diffs = Vec::new();
    for row in others {
        if row.files != base.files {
            let (bn, bf, rn, rf) = (base.name, base.files, row.name, row.files);
            diffs.push(format!("files: {bn} {bf}, {rn} {rf}"));
        }
        for (b, q) in base.queries.iter().zip(&row.queries) {
            if (b.matches, b.files) != (q.matches, q.files) {
                let (bn, qn) = (base.name, row.name);
                diffs.push(format!(
                    "{:?}: {bn} {} matches in {} files, {qn} {} in {}",
                    b.term, b.matches, b.files, q.matches, q.files
                ));
            }
        }
    }
    diffs
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, e)) => {
            eprintln!("{e}");
            ExitCode::from(code)
        }
    }
}

fn run() -> Result<(), Fail> {
    let (mut corpus, mut rows, mut fixture, mut trace) = ("ghc", None, false, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(usage(format!("{arg} needs a value")));
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--corpus" => {
                let id = value()?;
                let known = CORPORA.into_iter().find(|c| *c == id);
                corpus = known.ok_or(usage(format!("no corpus {id:?}\n{USAGE}")))?;
            }
            "--rows" => rows = Some(value()?),
            "--trace" => trace = Some(PathBuf::from(value()?)),
            "--fixture" => fixture = true,
            _ => return Err(usage(format!("unknown argument {arg:?}\n{USAGE}"))),
        }
    }
    let tgrep = tgrep::detect();
    let rows = select_rows(rows.as_deref(), &tgrep).map_err(usage)?;
    let fmgrep = fmgrep_bin();
    if fmgrep.is_none() && rows.iter().any(|r| r.starts_with("fmgrep")) {
        return Err(usage("no fmgrep next to bench or on PATH"));
    }
    let set = query_set(corpus);
    let fixture_dir = fixture.then(tempfile::tempdir).transpose();
    let fixture_dir = fixture_dir.map_err(failed)?;
    let root = match &fixture_dir {
        Some(dir) => {
            let literals = set.queries.iter().filter(|q| regex_pattern(q).is_none());
            let text: String = literals.map(|q| format!("{q}\n")).collect();
            std::fs::write(dir.path().join(format!("{corpus}.txt")), text).map_err(failed)?;
            dir.path().to_path_buf()
        }
        None => corpus_root(corpus).map_err(usage)?,
    };
    let docs = fmgrep::discover_index_meta(&root);
    let docs = docs.map_err(|e| usage(e.trim_start_matches("error: ")))?;
    let start = Instant::now();
    let staged = stage(&root, &docs).map_err(failed)?;
    let secs = start.elapsed().as_secs_f64();
    eprintln!("staged {} files in {secs:.1} s", docs.len());

    let (n, staged, terms) = (docs.len(), staged.path(), &set.queries);
    let mut results = Vec::new();
    for &name in &rows {
        eprintln!("running {name}");
        let before = rows::load1();
        let row = match (name, &fmgrep, &tgrep) {
            ("rg", _, _) => rows::rg(staged, n, terms),
            ("tgrep", _, Ok(t)) => tgrep::row(&t.bin, staged, n, terms),
            (_, Some(bin), _) => rows::fmgrep(bin, staged, name == "fmgrep-store-docs", terms),
            _ => unreachable!("rows are checked above"),
        };
        let mut row = row.map_err(|e| failed(format!("row {name}: {e}")))?;
        row.load1 = before.zip(rows::load1());
        results.push(row);
    }

    let diffs = verify(&results);
    let root_str = root.to_str().filter(|_| !fixture);
    let head = root_str.and_then(|r| report::run("git", &["-C", r, "rev-parse", "HEAD"]));
    let run = report::Run {
        corpus,
        fixture,
        revision: head,
        files: n,
        bytes: docs.iter().map(|d| d.len).sum(),
        stamps: report::stamps(fmgrep.as_deref(), &tgrep),
        set,
    };
    print!("{}", report::markdown(&run, &results, &diffs));
    if let Some(path) = &trace {
        let written = std::fs::write(path, report::trace(&run, &results));
        written.map_err(|e| failed(format!("{}: {e}", path.display())))?;
    }
    if !diffs.is_empty() {
        return Err((1, format!("verify: FAILED\n{}", diffs.join("\n"))));
    }
    eprintln!("verify: OK");
    Ok(())
}

/// Copies every document to `{id:08}.c` in a fresh directory; tgrep skips `.bin`.
fn stage(root: &Path, docs: &[fmgrep::DocMeta]) -> Result<tempfile::TempDir, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("cannot create the staging: {e}"))?;
    for (id, doc) in docs.iter().enumerate() {
        let dst = dir.path().join(format!("{id:08}.c"));
        std::fs::copy(root.join(&doc.path), &dst)
            .map_err(|e| format!("cannot stage {}: {e}", doc.path))?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regexes(set: &QuerySet) -> impl Iterator<Item = &str> {
        set.queries.iter().filter_map(|q| regex_pattern(q))
    }

    #[test]
    fn query_files_pin_a_commit_and_list_literals_first() {
        for (id, literals, regexes) in [("ghc", 30, 5), ("linux", 92, 10), ("chromium", 30, 5)] {
            let QuerySet {
                repo_url,
                commit,
                queries,
            } = query_set(id);
            assert!(repo_url.starts_with("https://") && repo_url.ends_with(".git"));
            assert!(commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()));
            let kinds: Vec<bool> = queries.iter().map(|q| regex_pattern(q).is_some()).collect();
            assert_eq!(
                kinds,
                [vec![false; literals], vec![true; regexes]].concat(),
                "{id}"
            );
            let mut sorted = queries.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), queries.len(), "{id}: duplicate query");
            assert!(queries.iter().all(|q| !q.trim().is_empty()), "{id}");
        }
        let ghc = query_set("ghc").commit;
        assert_eq!(ghc, "4cf7628aa66f56000c29aef93924b6b8515db8f7");
    }

    fn can_match_newline(hir: &regex_syntax::hir::Hir) -> bool {
        use regex_syntax::hir::{Class, HirKind};
        match hir.kind() {
            HirKind::Empty | HirKind::Look(_) => false,
            HirKind::Literal(lit) => lit.0.contains(&b'\n'),
            HirKind::Class(Class::Unicode(c)) => c
                .ranges()
                .iter()
                .any(|r| r.start() <= '\n' && '\n' <= r.end()),
            HirKind::Class(Class::Bytes(c)) => c
                .ranges()
                .iter()
                .any(|r| r.start() <= b'\n' && b'\n' <= r.end()),
            HirKind::Capture(c) => can_match_newline(&c.sub),
            HirKind::Repetition(r) => can_match_newline(&r.sub),
            HirKind::Concat(v) | HirKind::Alternation(v) => v.iter().any(can_match_newline),
        }
    }

    /// Rows search whole documents, so a pattern spanning `\n` would count
    /// differently from a line-oriented rg.
    #[test]
    fn no_query_can_match_across_a_newline() {
        let parse = |p: &str| regex_syntax::parse(p).unwrap();
        assert!(can_match_newline(&parse(r"a\s+b")) && can_match_newline(&parse(r"a[^;]*b")));
        assert!(!can_match_newline(&parse(r"a[^;\n]*b")) && !can_match_newline(&parse(r"^a.b$")));
        for id in CORPORA {
            let set = query_set(id);
            for p in regexes(&set) {
                assert!(
                    !can_match_newline(&parse(p)),
                    "{id}: {p:?} can match a newline"
                );
            }
            assert!(set.queries.iter().all(|q| !q.contains('\n')), "{id}");
        }
    }

    #[test]
    fn every_regex_set_compiles_and_prefilters() {
        use regex_syntax::hir::literal::Extractor;
        for id in CORPORA {
            let set = query_set(id);
            for p in regexes(&set) {
                regex::bytes::Regex::new(p).unwrap_or_else(|e| panic!("{id} {p:?}: {e}"));
                let seq = Extractor::new().extract(&regex_syntax::parse(p).unwrap());
                assert!(
                    matches!(seq.min_literal_len(), Some(n) if n > 0),
                    "{id} {p:?}"
                );
            }
        }
    }

    #[test]
    fn verify_names_the_term_and_row_of_a_mismatch() {
        let row = |name, files, counts: &[(u64, usize)]| {
            let mut row = Row::new(name, files);
            for (i, &c) in counts.iter().enumerate() {
                row.queries
                    .push(Query::new(&format!("t{i}"), Duration::ZERO, c));
            }
            row
        };
        let (rg, same) = (
            row("rg", 5, &[(3, 2), (0, 0)]),
            row("fmgrep", 5, &[(3, 2), (0, 0)]),
        );
        assert!(verify(&[rg, same]).is_empty());
        let (rg, other) = (
            row("rg", 5, &[(3, 2), (0, 0)]),
            row("fmgrep", 4, &[(3, 2), (1, 1)]),
        );
        let diffs = verify(&[rg, other]);
        let term = "\"t1\": rg 0 matches in 0 files, fmgrep 1 in 1";
        assert_eq!(diffs, ["files: rg 5, fmgrep 4", term]);
    }
}

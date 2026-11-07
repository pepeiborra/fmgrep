//! Benchmarks fmgrep against ripgrep, and tgrep and elgrep when installed,
//! over one corpus.

mod elgrep;
mod report;
mod rows;
mod tgrep;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use fmgrep::regex_pattern;
use strum::{Display, EnumIter, EnumString, IntoEnumIterator, IntoStaticStr};

const USAGE: &str = "\
usage: bench [--corpus ghc|linux|chromium] [--rows ROWS] [--fixture] [--trace FILE]

Stages the corpus, runs every row over its queries, checks that every row
prints the same lines as the first, and prints the results as markdown.

  --corpus ID    ghc (default), linux or chromium; the checkout is
                 $FMGREP_CORPUS_<ID>, by default target/bench/<ID>
  --rows ROWS    comma-separated rg, fmgrep, fmgrep-store-docs,
                 fmgrep-direct (no server: a process per query), tgrep,
                 elgrep; default rg,fmgrep, tgrep when it is on PATH, and
                 elgrep when it is on PATH on Linux, where its index answers
  --fixture      search one file of the query set's literals instead
  --trace FILE   write one JSON line per query and per row

Exits 1 when a row fails or the rows disagree, 2 on a usage or corpus error.";

const CORPORA: [&str; 3] = ["ghc", "linux", "chromium"];

/// A row, named in kebab-case by `--rows`, the results and the trace; rows
/// run and print in this order.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Display, EnumIter, EnumString, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum RowKind {
    #[default]
    Rg,
    Fmgrep,
    FmgrepStoreDocs,
    FmgrepDirect,
    Tgrep,
    Elgrep,
}

pub struct Query {
    pub term: String,
    pub latency: Duration,
    pub hits: rows::Hits,
}

#[derive(Default)]
pub struct Row {
    pub name: RowKind,
    pub files: usize,
    pub queries: Vec<Query>,
    pub index_time: Option<Duration>,
    pub index_bytes: Option<u64>,
    pub ready: Option<Duration>,
    pub index_peak_rss: Option<u64>,
    pub footprint: Option<u64>,
    pub query_rss: Option<u64>,
    pub load1: Option<(f64, f64)>,
    /// Printed under the summary as `- row: note`.
    pub notes: Vec<String>,
}

impl Query {
    fn new(term: &str, latency: Duration, hits: rows::Hits) -> Self {
        Query {
            term: term.to_string(),
            latency,
            hits,
        }
    }
}

impl Row {
    fn new(name: RowKind, files: usize) -> Self {
        Row {
            name,
            files,
            ..Row::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Literal,
    Regex,
    Intersection,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Literal, Kind::Regex, Kind::Intersection];

    pub fn of(query: &str) -> Kind {
        match (leaves(query).len(), regex_pattern(query)) {
            (2.., _) => Kind::Intersection,
            (_, Some(_)) => Kind::Regex,
            _ => Kind::Literal,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Literal => "literal",
            Kind::Regex => "regex",
            Kind::Intersection => "intersection",
        }
    }
}

/// The sides of `A AND B [AND ..]`; the query itself otherwise.
pub fn leaves(query: &str) -> Vec<&str> {
    query.split(" AND ").collect()
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
    elgrep: &Result<elgrep::Elgrep, String>,
) -> Result<Vec<RowKind>, String> {
    let by_default = |r: &RowKind| match r {
        RowKind::Rg | RowKind::Fmgrep => true,
        RowKind::FmgrepStoreDocs | RowKind::FmgrepDirect => false,
        RowKind::Tgrep => tgrep.is_ok(),
        RowKind::Elgrep => elgrep.is_ok() && cfg!(target_os = "linux"),
    };
    let parse = |w: &str| {
        let names: Vec<&str> = RowKind::iter().map(<&str>::from).collect();
        let bad = format!("--rows: {w:?} is not one of {}", names.join(", "));
        w.parse::<RowKind>().map_err(|_| bad)
    };
    let wanted: Vec<RowKind> = match spec {
        Some(spec) => spec
            .split(',')
            .map(str::trim)
            .map(parse)
            .collect::<Result<_, _>>()?,
        None => RowKind::iter().filter(by_default).collect(),
    };
    if let (true, Err(note)) = (wanted.contains(&RowKind::Tgrep), tgrep) {
        return Err(format!("--rows names tgrep, but {note}"));
    }
    if let (true, Err(note)) = (wanted.contains(&RowKind::Elgrep), elgrep) {
        return Err(format!("--rows names elgrep, but {note}"));
    }
    Ok(RowKind::iter().filter(|r| wanted.contains(r)).collect())
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

/// Every row must answer the first row's queries with the same lines.
fn verify(rows: &[Row]) -> Vec<String> {
    let Some((base, others)) = rows.split_first() else {
        return Vec::new();
    };
    let terms = |r: &Row| r.queries.iter().map(|q| q.term.clone()).collect::<Vec<_>>();
    let mut diffs = Vec::new();
    for row in others {
        if row.files != base.files {
            let (bn, bf, rn, rf) = (base.name, base.files, row.name, row.files);
            diffs.push(format!("files: {bn} {bf}, {rn} {rf}"));
        }
        if terms(row) != terms(base) {
            diffs.push(format!("queries: {} and {} differ", base.name, row.name));
            continue;
        }
        for (b, q) in base.queries.iter().zip(&row.queries) {
            let (bn, qn, bh, qh) = (base.name, row.name, &b.hits, &q.hits);
            if (bh.lines, bh.files) != (qh.lines, qh.files) {
                diffs.push(format!(
                    "{:?}: {bn} {} lines in {} files, {qn} {} in {}",
                    b.term, bh.lines, bh.files, qh.lines, qh.files
                ));
            } else if bh.digest != qh.digest {
                diffs.push(format!("{:?}: {bn} and {qn} print different lines", b.term));
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
    let (tgrep, elgrep) = (tgrep::detect(), elgrep::detect());
    let eg_dropped = rows.is_none() && elgrep.is_ok() && !cfg!(target_os = "linux");
    let rows = select_rows(rows.as_deref(), &tgrep, &elgrep).map_err(usage)?;
    let fmgrep = fmgrep_bin();
    let fmgrep_row = |r: &RowKind| {
        matches!(
            r,
            RowKind::Fmgrep | RowKind::FmgrepStoreDocs | RowKind::FmgrepDirect
        )
    };
    if fmgrep.is_none() && rows.iter().any(fmgrep_row) {
        return Err(usage("no fmgrep next to bench or on PATH"));
    }
    let set = query_set(corpus);
    let fixture_dir = fixture.then(tempfile::tempdir).transpose();
    let fixture_dir = fixture_dir.map_err(failed)?;
    let root = match &fixture_dir {
        Some(dir) => {
            let leaves = set.queries.iter().flat_map(|q| leaves(q));
            let literals = leaves.filter(|l| regex_pattern(l).is_none());
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
        let row = match (name, &fmgrep, &tgrep, &elgrep) {
            (RowKind::Rg, ..) => rows::rg(staged, n, terms),
            (RowKind::Tgrep, _, Ok(t), _) => tgrep::row(&t.bin, staged, n, terms),
            (RowKind::Elgrep, _, _, Ok(e)) => elgrep::row(e, staged, n, terms),
            (RowKind::Fmgrep | RowKind::FmgrepStoreDocs | RowKind::FmgrepDirect, Some(bin), ..) => {
                rows::fmgrep(bin, staged, name, terms)
            }
            _ => unreachable!("rows are checked above"),
        };
        let mut row = row.map_err(|e| failed(format!("row {name}: {e}")))?;
        row.load1 = before.zip(rows::load1());
        results.push(row);
    }

    let diffs = verify(&results);
    let root_str = root.to_str().filter(|_| !fixture);
    let head = root_str.and_then(|r| report::run("git", &["-C", r, "rev-parse", "HEAD"]));
    let mut run = report::Run {
        corpus,
        fixture,
        revision: head,
        files: n,
        bytes: docs.iter().map(|d| d.len).sum(),
        stamps: report::stamps(fmgrep.as_deref(), &tgrep, &elgrep),
        set,
    };
    if eg_dropped {
        let note = "elgrep: not run by default, its index answers only on Linux";
        run.stamps.notes.push(note.to_owned());
    }
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
        set.queries
            .iter()
            .flat_map(|q| leaves(q))
            .filter_map(regex_pattern)
    }

    #[test]
    fn query_files_pin_a_commit_and_list_literals_first() {
        let counts = [
            ("ghc", 30, 5, 5),
            ("linux", 92, 13, 8),
            ("chromium", 30, 8, 8),
        ];
        for (id, literals, regexes, intersections) in counts {
            let QuerySet {
                repo_url,
                commit,
                queries,
            } = query_set(id);
            assert!(repo_url.starts_with("https://") && repo_url.ends_with(".git"));
            assert!(commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()));
            let kinds: Vec<Kind> = queries.iter().map(|q| Kind::of(q)).collect();
            let want = [
                vec![Kind::Literal; literals],
                vec![Kind::Regex; regexes],
                vec![Kind::Intersection; intersections],
            ];
            assert_eq!(kinds, want.concat(), "{id}");
            let mut sorted = queries.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), queries.len(), "{id}: duplicate query");
            assert!(queries.iter().all(|q| !q.trim().is_empty()), "{id}");
        }
        let ghc = query_set("ghc").commit;
        assert_eq!(ghc, "4cf7628aa66f56000c29aef93924b6b8515db8f7");
    }

    #[test]
    fn kind_and_leaves_follow_the_spelling() {
        assert_eq!((Kind::of("a"), leaves("a")), (Kind::Literal, vec!["a"]));
        assert_eq!((Kind::of("/a/"), leaves("/a/")), (Kind::Regex, vec!["/a/"]));
        for (q, want) in [
            ("a AND b", &["a", "b"][..]),
            ("/-- \\|/ AND x", &["/-- \\|/", "x"]),
            ("a AND b AND c", &["a", "b", "c"]),
        ] {
            assert_eq!((Kind::of(q), &leaves(q)[..]), (Kind::Intersection, want));
        }
        let names = Kind::ALL.map(Kind::name);
        assert_eq!(names, ["literal", "regex", "intersection"]);
    }

    /// The rg, tgrep and elgrep rows' last stage reads literal leaves as regexes.
    #[test]
    fn intersection_literals_need_no_escaping() {
        for id in CORPORA {
            let set = query_set(id);
            let ands = set
                .queries
                .iter()
                .filter(|q| Kind::of(q) == Kind::Intersection);
            for q in ands {
                assert_eq!(leaves(q).len(), 2, "{id} {q:?}");
                for l in leaves(q).into_iter().filter(|l| regex_pattern(l).is_none()) {
                    let plain = !l.contains(char::is_whitespace) && regex::escape(l) == l;
                    assert!(plain, "{id} {q:?}");
                }
            }
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
    fn select_rows_defaults_follow_detection() {
        let bin = PathBuf::from("x");
        let version = String::new();
        let t = Ok(tgrep::Tgrep {
            bin: bin.clone(),
            version: version.clone(),
        });
        fn absent<T>() -> Result<T, String> {
            Err("absent".to_string())
        }
        let daemon = bin.clone();
        let e = Ok(elgrep::Elgrep {
            bin,
            daemon,
            version,
        });
        let elgrep = if cfg!(target_os = "linux") {
            vec![RowKind::Elgrep]
        } else {
            vec![]
        };
        assert_eq!(
            select_rows(None, &absent(), &absent()).unwrap(),
            [RowKind::Rg, RowKind::Fmgrep]
        );
        let all = select_rows(None, &t, &e).unwrap();
        let found = [RowKind::Rg, RowKind::Fmgrep, RowKind::Tgrep];
        assert_eq!(all, [&found[..], &elgrep].concat());
        let picked = select_rows(Some("elgrep,rg"), &absent(), &e).unwrap();
        assert_eq!(picked, [RowKind::Rg, RowKind::Elgrep]);
        let err = select_rows(Some("rg,elgrep"), &t, &absent()).unwrap_err();
        assert_eq!(err, "--rows names elgrep, but absent");
        let err = select_rows(Some("rg,x"), &t, &e).unwrap_err();
        let names = "rg, fmgrep, fmgrep-store-docs, fmgrep-direct, tgrep, elgrep";
        assert_eq!(err, format!("--rows: \"x\" is not one of {names}"));
    }

    #[test]
    fn verify_names_the_term_and_row_of_a_mismatch() {
        let row = |name, files, hits: &[(u64, usize, u64)]| {
            let mut row = Row::new(name, files);
            for (i, &(lines, files, digest)) in hits.iter().enumerate() {
                let hits = rows::Hits {
                    lines,
                    files,
                    digest,
                };
                row.queries
                    .push(Query::new(&format!("t{i}"), Duration::ZERO, hits));
            }
            row
        };
        let rg = || row(RowKind::Rg, 5, &[(3, 2, 7), (0, 0, 0), (2, 1, 8)]);
        assert!(verify(&[rg(), rg()]).is_empty());
        let mut partial = rg();
        partial.name = RowKind::Tgrep;
        partial.queries.remove(1);
        assert_eq!(verify(&[rg(), partial]), ["queries: rg and tgrep differ"]);
        let other = row(RowKind::Fmgrep, 4, &[(3, 2, 7), (1, 1, 1), (2, 1, 9)]);
        let diffs = verify(&[rg(), other]);
        let t1 = "\"t1\": rg 0 lines in 0 files, fmgrep 1 in 1";
        let t2 = "\"t2\": rg and fmgrep print different lines";
        assert_eq!(diffs, ["files: rg 5, fmgrep 4", t1, t2]);
    }
}

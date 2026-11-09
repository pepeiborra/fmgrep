//! Benchmarks fmgrep against ripgrep, and tgrep and elgrep when installed,
//! over one corpus.

mod corpus;
mod elgrep;
#[cfg(test)]
mod fake_bin;
mod report;
mod rows;
mod tgrep;
mod verify;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use corpus::{CORPORA, corpus_root, query_set};
use fmgrep::regex_pattern;
use strum::{Display, EnumIter, EnumString, IntoEnumIterator, IntoStaticStr};

const USAGE: &str = "\
usage: bench [--corpus ghc|linux|chromium|aosp] [--rows ROWS] [--fixture] [--trace FILE]

Runs every row over the checkout in place, checks that the rows print the
same lines for every file they both searched, lists the files only some
rows searched, and prints the results as markdown.

  --corpus ID    ghc (default), linux, chromium or aosp; the checkout is
                 $FMGREP_CORPUS_<ID>, by default target/bench/<ID>
  --rows ROWS    comma-separated rg, fmgrep, fmgrep-store-docs,
                 fmgrep-direct (no server: a process per query), tgrep,
                 elgrep; default rg,fmgrep, tgrep when it is on PATH, and
                 elgrep when it is on PATH on Linux, where its index answers
  --fixture      search one file of the query set's literals instead
  --trace FILE   write one JSON line per query and per row

Exits 1 when a row fails or the rows disagree, 2 on a usage or corpus error.";

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
    pub hits: verify::Hits,
}

#[derive(Default)]
pub struct Row {
    pub name: RowKind,
    /// The non-empty files it searched, sorted.
    pub searched: Vec<u32>,
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
    fn new(term: &str, latency: Duration, hits: verify::Hits) -> Self {
        Query {
            term: term.to_string(),
            latency,
            hits,
        }
    }
}

impl Row {
    fn new(name: RowKind) -> Self {
        Row {
            name,
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
    let root =
        std::fs::canonicalize(&root).map_err(|e| usage(format!("{}: {e}", root.display())))?;
    let docs = fmgrep::discover_index_meta(&root);
    let docs = docs.map_err(|e| usage(e.trim_start_matches("error: ")))?;
    let start = Instant::now();
    let paths = verify::Paths::new(&root, &docs).map_err(failed)?;
    let secs = start.elapsed().as_secs_f64();
    eprintln!("listed {} files in {secs:.1} s", paths.len());

    let terms = &set.queries;
    let mut results = Vec::new();
    for &name in &rows {
        eprintln!("running {name}");
        let before = rows::load1();
        let row = match (name, &fmgrep, &tgrep, &elgrep) {
            (RowKind::Rg, ..) => rows::rg(&paths, &root, terms),
            (RowKind::Tgrep, _, Ok(t), _) => tgrep::row(&t.bin, &paths, &root, terms),
            (RowKind::Elgrep, _, _, Ok(e)) => elgrep::row(e, &paths, &root, terms),
            (RowKind::Fmgrep | RowKind::FmgrepStoreDocs | RowKind::FmgrepDirect, Some(bin), ..) => {
                let tree = (&paths, root.as_path(), docs.as_slice());
                rows::fmgrep(bin, tree, name, terms)
            }
            _ => unreachable!("rows are checked above"),
        };
        let mut row = row.map_err(|e| failed(format!("row {name}: {e}")))?;
        row.load1 = before.zip(rows::load1());
        results.push(row);
    }

    let (diffs, unsearched) = verify::verify(&paths, &root, &results);
    let mut run = report::Run {
        corpus,
        fixture,
        revision: (!fixture).then(|| corpus::revision(&root)).flatten(),
        files: docs.len(),
        bytes: docs.iter().map(|d| d.len).sum(),
        stamps: report::stamps(fmgrep.as_deref(), &tgrep, &elgrep),
        set,
    };
    if eg_dropped {
        let note = "elgrep: not run by default, its index answers only on Linux";
        run.stamps.notes.push(note.to_owned());
    }
    let found = report::Found {
        diffs: &diffs,
        unsearched: &unsearched,
        paths: &paths,
    };
    print!("{}", report::markdown(&run, &results, &found));
    if let Some(path) = &trace {
        let written = std::fs::write(path, report::trace(&run, &results, &found));
        written.map_err(|e| failed(format!("{}: {e}", path.display())))?;
    }
    if !diffs.is_empty() {
        return Err((1, format!("verify: FAILED\n{}", diffs.join("\n"))));
    }
    eprintln!("verify: OK");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

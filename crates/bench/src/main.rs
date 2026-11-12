//! Benchmarks fmgrep against ripgrep, and tgrep and elgrep when installed,
//! over one corpus.

mod corpus;
mod deviations;
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
             [--server [LABEL=]HOST:PORT]...

Runs every row over the checkout in place, checks each row's lines from
every file against rg's, lists the files only some rows searched, and
prints the results as markdown; tgrep and elgrep differences are notes.

  --corpus ID    ghc (default), linux, chromium or aosp; the checkout is
                 $FMGREP_CORPUS_<ID>, by default target/bench/<ID>
  --rows ROWS    comma-separated rg, fmgrep, fmgrep-store-docs,
                 fmgrep-direct (no server: a process per query), tgrep,
                 elgrep; default rg,fmgrep, tgrep when it is on PATH, and
                 elgrep when it is on PATH on Linux, where its index answers
  --fixture      search one file of the query set's literals instead
  --trace FILE   write one JSON line per query and per row
  --server [LABEL=]HOST:PORT
                 also query the fmgrep serve already running there, as
                 row fmgrep@LABEL (default fmgrep@HOST:PORT); repeatable

Exits 1 when a row fails or an fmgrep row differs from rg, 2 on a usage or
corpus error.";

/// A row's kind, named in kebab-case by `--rows`; rows run and print in
/// this order, `--server` rows in place of `fmgrep-server`.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Display,
    EnumIter,
    EnumString,
    IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum RowKind {
    #[default]
    Rg,
    Fmgrep,
    FmgrepStoreDocs,
    FmgrepDirect,
    FmgrepServer,
    Tgrep,
    Elgrep,
}

/// A row's name in the results and the trace: its kind's, or `fmgrep@LABEL`
/// for a `--server` row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct RowName {
    pub kind: RowKind,
    server: Option<&'static str>,
}

impl RowName {
    pub fn server(label: &str) -> Self {
        let name = format!("fmgrep@{label}").leak();
        RowName {
            kind: RowKind::FmgrepServer,
            server: Some(name),
        }
    }

    pub fn as_str(self) -> &'static str {
        self.server.unwrap_or(self.kind.into())
    }
}

impl From<RowKind> for RowName {
    fn from(kind: RowKind) -> Self {
        RowName { kind, server: None }
    }
}

impl std::fmt::Display for RowName {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `--server` row: the fmgrep serve it queries.
#[derive(Debug)]
pub struct Server {
    pub name: RowName,
    pub addr: String,
}

/// Rows whose differences from the reference are noted, not failures.
const THIRD_PARTY: [RowKind; 2] = [RowKind::Tgrep, RowKind::Elgrep];

pub struct Query {
    pub term: String,
    pub latency: Duration,
    pub hits: verify::Hits,
}

#[derive(Default)]
pub struct Row {
    pub name: RowName,
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
    fn new(name: impl Into<RowName>) -> Self {
        Row {
            name: name.into(),
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

/// The terms a row runs, as `(term, warm_up)`: the set's first term once as
/// a warm-up, its output checked but neither timed nor kept, so that no row
/// times the first query a client of it sends; then every term.
pub fn with_warm_up(terms: &[String]) -> impl Iterator<Item = (&String, bool)> {
    let first = terms.first().map(|t| (t, true));
    first.into_iter().chain(terms.iter().map(|t| (t, false)))
}

/// An error line and the exit code it ends the run with.
type Fail = (u8, String);

fn usage(e: impl std::fmt::Display) -> Fail {
    (2, format!("error: {e}"))
}

fn failed(e: impl std::fmt::Display) -> Fail {
    (1, format!("error: {e}"))
}

/// The `--server [LABEL=]HOST:PORT` flags, in order.
fn servers(specs: &[String]) -> Result<Vec<Server>, String> {
    let mut servers: Vec<Server> = Vec::new();
    for spec in specs {
        let bad = |why: &str| format!("--server {spec:?}: {why}");
        let (label, addr) = match spec.split_once('=') {
            Some((label, addr)) => (Some(label), addr),
            None => (None, spec.as_str()),
        };
        let port = addr.rsplit_once(':').and_then(|(host, port)| {
            let port = port.parse::<u16>().ok().filter(|&p| p > 0);
            port.filter(|_| !host.is_empty())
        });
        if port.is_none() {
            return Err(bad("not [LABEL=]HOST:PORT"));
        }
        let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
        if label.is_some_and(|l| l.is_empty() || !l.chars().all(ok)) {
            return Err(bad("a label is letters, digits, '.', '_' or '-'"));
        }
        let name = RowName::server(label.unwrap_or(addr));
        if servers.iter().any(|s| s.name == name) {
            return Err(format!("--server: two rows named {name}"));
        }
        let addr = addr.to_string();
        servers.push(Server { name, addr });
    }
    Ok(servers)
}

fn select_rows(
    spec: Option<&str>,
    servers: &[Server],
    tgrep: &Result<tgrep::Tgrep, String>,
    elgrep: &Result<elgrep::Elgrep, String>,
) -> Result<Vec<RowName>, String> {
    let by_default = |r: &RowKind| match r {
        RowKind::Rg | RowKind::Fmgrep => true,
        RowKind::FmgrepStoreDocs | RowKind::FmgrepDirect | RowKind::FmgrepServer => false,
        RowKind::Tgrep => tgrep.is_ok(),
        RowKind::Elgrep => elgrep.is_ok() && cfg!(target_os = "linux"),
    };
    let named = || RowKind::iter().filter(|&r| r != RowKind::FmgrepServer);
    let parse = |w: &str| {
        let names: Vec<&str> = named().map(<&str>::from).collect();
        let bad = format!("--rows: {w:?} is not one of {}", names.join(", "));
        let kind = w.parse::<RowKind>().ok();
        kind.filter(|&k| k != RowKind::FmgrepServer).ok_or(bad)
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
    let rows = RowKind::iter().flat_map(|r| match r {
        RowKind::FmgrepServer => servers.iter().map(|s| s.name).collect(),
        r if wanted.contains(&r) => vec![r.into()],
        _ => vec![],
    });
    Ok(rows.collect())
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
    let mut specs = Vec::new();
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
            "--server" => specs.push(value()?),
            "--fixture" => fixture = true,
            _ => return Err(usage(format!("unknown argument {arg:?}\n{USAGE}"))),
        }
    }
    let servers = servers(&specs).map_err(usage)?;
    if fixture && !servers.is_empty() {
        return Err(usage("--server needs the corpus, not --fixture"));
    }
    let (tgrep, elgrep) = (tgrep::detect(), elgrep::detect());
    let eg_dropped = rows.is_none() && elgrep.is_ok() && !cfg!(target_os = "linux");
    let rows = select_rows(rows.as_deref(), &servers, &tgrep, &elgrep).map_err(usage)?;
    let fmgrep = fmgrep_bin();
    let fmgrep_row =
        |r: &RowName| !matches!(r.kind, RowKind::Rg | RowKind::Tgrep | RowKind::Elgrep);
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
    for (spec, s) in specs.iter().zip(&servers) {
        let bin = fmgrep.as_deref().expect("server rows need fmgrep");
        let checked = rows::check_server(bin, &s.addr, &root, docs.len());
        checked.map_err(|e| usage(format!("--server {spec}: {e}")))?;
    }
    let start = Instant::now();
    let paths = verify::Paths::new(&root, &docs).map_err(failed)?;
    let secs = start.elapsed().as_secs_f64();
    eprintln!("listed {} files in {secs:.1} s", paths.len());

    let terms = &set.queries;
    let mut results = Vec::new();
    for &name in &rows {
        eprintln!("running {name}");
        let before = rows::load1();
        let row = match (name.kind, &fmgrep, &tgrep, &elgrep) {
            (RowKind::Rg, ..) => rows::rg(&paths, &root, terms),
            (RowKind::Tgrep, _, Ok(t), _) => tgrep::row(&t.bin, &paths, &root, terms),
            (RowKind::Elgrep, _, _, Ok(e)) => elgrep::row(e, &paths, &root, terms),
            (RowKind::FmgrepServer, Some(bin), ..) => {
                let server = servers.iter().find(|s| s.name == name);
                let server = server.expect("server rows come from --server");
                rows::server(bin, &paths, &docs, server, terms)
            }
            (
                kind @ (RowKind::Fmgrep | RowKind::FmgrepStoreDocs | RowKind::FmgrepDirect),
                Some(bin),
                ..,
            ) => {
                let tree = (&paths, root.as_path(), docs.as_slice());
                rows::fmgrep(bin, tree, kind, terms)
            }
            _ => unreachable!("rows are checked above"),
        };
        let mut row = row.map_err(|e| failed(format!("row {name}: {e}")))?;
        row.load1 = before.zip(rows::load1());
        results.push(row);
    }

    let verdict = verify::verify(&paths, &root, &results);
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
        diffs: &verdict.fatal,
        unsearched: &verdict.unsearched,
        deviations: &verdict.deviations,
        paths: &paths,
    };
    for (name, note) in deviations::notes(run.set.queries.len(), &found) {
        let row = results.iter_mut().find(|r| r.name == name);
        row.expect("deviations name run rows").notes.push(note);
    }
    print!("{}", report::markdown(&run, &results, &found));
    if let Some(path) = &trace {
        let written = std::fs::write(path, report::trace(&run, &results, &found));
        written.map_err(|e| failed(format!("{}: {e}", path.display())))?;
    }
    if !verdict.fatal.is_empty() {
        return Err((1, format!("verify: FAILED\n{}", verdict.fatal.join("\n"))));
    }
    match deviations::count(&verdict.deviations) {
        0 => eprintln!("verify: OK"),
        k => eprintln!("verify: OK ({})", deviations::differences(k)),
    }
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
    fn the_first_term_runs_once_more_first_as_the_warm_up() {
        let terms = ["a", "b AND c"].map(String::from);
        let runs: Vec<_> = with_warm_up(&terms).map(|(t, w)| (t.as_str(), w)).collect();
        assert_eq!(runs, [("a", true), ("a", false), ("b AND c", false)]);
        assert_eq!(with_warm_up(&[]).count(), 0);
    }

    fn kinds(rows: Result<Vec<RowName>, String>) -> Vec<RowKind> {
        rows.unwrap().iter().map(|r| r.kind).collect()
    }

    #[test]
    fn select_rows_defaults_follow_detection() {
        let bin = PathBuf::from("x");
        let version = String::new();
        let t = Ok(tgrep::Tgrep {
            bin: bin.clone(),
            version: version.clone(),
        });
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
            kinds(select_rows(None, &[], &absent(), &absent())),
            [RowKind::Rg, RowKind::Fmgrep]
        );
        let all = kinds(select_rows(None, &[], &t, &e));
        let found = [RowKind::Rg, RowKind::Fmgrep, RowKind::Tgrep];
        assert_eq!(all, [&found[..], &elgrep].concat());
        let picked = kinds(select_rows(Some("elgrep,rg"), &[], &absent(), &e));
        assert_eq!(picked, [RowKind::Rg, RowKind::Elgrep]);
        let err = select_rows(Some("rg,elgrep"), &[], &t, &absent()).unwrap_err();
        assert_eq!(err, "--rows names elgrep, but absent");
        let err = select_rows(Some("rg,x"), &[], &t, &e).unwrap_err();
        assert_eq!(err, format!("--rows: \"x\" is not one of {NAMES}"));
    }

    const NAMES: &str = "rg, fmgrep, fmgrep-store-docs, fmgrep-direct, tgrep, elgrep";

    fn absent<T>() -> Result<T, String> {
        Err("absent".to_string())
    }

    fn specs(specs: &[&str]) -> Result<Vec<Server>, String> {
        servers(&specs.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn server_flags_name_rows_and_refuse_bad_addresses_labels_and_collisions() {
        let got = specs(&[
            "a=127.0.0.1:7001",
            "127.0.0.1:7001",
            "localhost:1",
            "[::1]:1",
        ]);
        let got: Vec<_> = got
            .unwrap()
            .into_iter()
            .map(|s| (s.name.as_str(), s.addr))
            .collect();
        let want = [
            ("fmgrep@a", "127.0.0.1:7001"),
            ("fmgrep@127.0.0.1:7001", "127.0.0.1:7001"),
            ("fmgrep@localhost:1", "localhost:1"),
            ("fmgrep@[::1]:1", "[::1]:1"),
        ];
        assert_eq!(got, want.map(|(n, a)| (n, a.to_string())));
        assert_eq!(RowName::server("a").to_string(), "fmgrep@a");
        for bad in ["127.0.0.1", "h:", "h:0", "h:99999", "h:x", ":7100", "a=h"] {
            let want = format!("--server {bad:?}: not [LABEL=]HOST:PORT");
            assert_eq!(specs(&[bad]).unwrap_err(), want);
        }
        for bad in ["=h:1", "a b=h:1", "a|b=h:1", "a@b=h:1"] {
            let want = format!("--server {bad:?}: a label is letters, digits, '.', '_' or '-'");
            assert_eq!(specs(&[bad]).unwrap_err(), want);
        }
        let two = "--server: two rows named fmgrep@a";
        assert_eq!(specs(&["a=h:1", "a=h:2"]).unwrap_err(), two);
        assert_eq!(specs(&["a=h:1", "b=h:1", "A.b_-9=h:1"]).unwrap().len(), 3);
        let same = specs(&["h:1", "h:1"]).unwrap_err();
        assert_eq!(same, "--server: two rows named fmgrep@h:1");
    }

    #[test]
    fn server_rows_run_after_the_fmgrep_rows_in_flag_order() {
        let servers = specs(&["b=h:1", "a=h:2"]).unwrap();
        let t = Ok(tgrep::Tgrep {
            bin: PathBuf::from("x"),
            version: String::new(),
        });
        let rows = select_rows(Some("tgrep,fmgrep-direct,rg"), &servers, &t, &absent());
        let names: Vec<_> = rows.unwrap().iter().map(|r| r.as_str()).collect();
        assert_eq!(
            names,
            ["rg", "fmgrep-direct", "fmgrep@b", "fmgrep@a", "tgrep"]
        );
        let rows = select_rows(None, &servers, &absent(), &absent()).unwrap();
        let names: Vec<_> = rows.iter().map(|r| r.as_str()).collect();
        assert_eq!(names, ["rg", "fmgrep", "fmgrep@b", "fmgrep@a"]);
        let err = select_rows(Some("rg,fmgrep-server"), &servers, &t, &absent());
        let want = format!("--rows: \"fmgrep-server\" is not one of {NAMES}");
        assert_eq!(err.unwrap_err(), want);
    }
}

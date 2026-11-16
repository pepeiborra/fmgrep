use std::ffi::OsString;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::Parser;

mod args;
mod checkout;
mod frames;
mod locate;
mod merge;
mod params;
mod print;
mod serve;
mod status;

use crate::events::JsonSink;
use crate::filter::SearchPath;
use crate::index::{INDEX_DIR, IndexRequest, run_index};
use crate::manifest::{BLOBS_SHARD, MANIFEST_FORMAT, Manifest, read_manifest};
use crate::output::{MatchWriter, WRITE_FAILED};
use crate::query::run_query;
use crate::remote::Conn;
use crate::sink::{Discard, MatchSink};
use crate::wire::{IndexSummary, OutputReport};
use args::{Cli, Command, Search, ServeArgs, StatusArgs};
use locate::{Located, locate};
use params::{Mode, QueryParams};
use serve::{ServeRequest, run_serve};

/// What a successful dispatch produced.
pub enum Outcome {
    Index(#[cfg_attr(not(test), allow(dead_code))] IndexSummary),
    Query(OutputReport),
    /// `serve` ended, help or the version printed, `status` printed, or
    /// `--shutdown` answered.
    Done,
}

fn serve(a: &ServeArgs, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<Outcome, String> {
    let (found, manifest) = open_index(a.path.as_deref())?;
    let req = ServeRequest {
        index_dir: &found.index_dir,
        manifest: &manifest,
        corpus_root: root_of(&found, &manifest),
        listen: (!a.stdio).then_some(a.listen),
        watch: !a.no_watch,
    };
    run_serve(&req, input, out)?;
    Ok(Outcome::Done)
}

/// `status`: what a search started in PATH (else the cwd) uses, the index
/// or server that [`choose`] and [`reach`] pick, with `--server` as a
/// search's, and of an index what its check of what changed just did.
/// Fails where that search fails before it searches, in its words.
fn status(a: &StatusArgs, env: Env, out: &mut dyn Write) -> Result<Outcome, String> {
    let start = a.path.as_deref().unwrap_or(Path::new("."));
    let here = match &a.path {
        Some(path) => format!("in {}", path.display()),
        None => "here".to_owned(),
    };
    let note = match choose(None, a.server.as_deref(), env, start)? {
        Choice::Index { note, .. } => note,
        Choice::Server(server) => match reach(&server, start)? {
            Reached::Conn(mut conn) => {
                let reply = serve::status_on(&mut conn)?;
                let first = format!("a search {here} uses {server}");
                serve::report(out, a.json, &first, &reply)?;
                return Ok(Outcome::Done);
            }
            Reached::Index { note, .. } => Some(note),
        },
    };
    let (found, manifest) = open_index(Some(start))?;
    let facts = status::check(&found.index_dir, &manifest, root_of(&found, &manifest))?;
    let dir = std::fs::canonicalize(&found.index_dir).unwrap_or(found.index_dir);
    let note = note.unwrap_or_default();
    let first = format!(
        "a search {here} uses the local index {}{note}",
        dir.display()
    );
    let json = a.json.then(|| status::Json {
        manifest: &manifest,
        uses: &dir,
        note: Some(note.as_str()).filter(|n| !n.is_empty()),
        facts: &facts,
    });
    print::status(out, json, &first, &manifest)?;
    if !a.json {
        status::print(out, &facts)?;
    }
    Ok(Outcome::Done)
}

/// The index found from `start` (else the cwd) and its manifest. A
/// files-source dotdir reads the tree it sits in, not the manifest's.
fn open_index(start: Option<&Path>) -> Result<(Located, Manifest), String> {
    let span = tracing::info_span!("locate").entered();
    let found = locate(start.unwrap_or(Path::new(".")))?;
    if !found.index_dir.join(BLOBS_SHARD).is_file() {
        return Err(format!(
            "error: {} has no {BLOBS_SHARD}",
            found.index_dir.display()
        ));
    }
    drop(span);
    let manifest = tracing::info_span!("manifest").in_scope(|| read_manifest(&found.index_dir))?;
    Ok((found, manifest))
}

/// `typed`, resolved from the cwd, as `p`'s PATHs into the indexed `tree`.
fn rebase(p: &mut QueryParams, typed: &[String], tree: Option<&Path>) -> Result<(), String> {
    rebase_into(p, typed, tree, "indexed tree")
}

/// [`rebase`] into `tree`, which errors call `what`.
fn rebase_into(
    p: &mut QueryParams,
    typed: &[String],
    tree: Option<&Path>,
    what: &str,
) -> Result<(), String> {
    if typed.is_empty() {
        return Ok(());
    }
    let resolve = |p: &Path| {
        std::fs::canonicalize(p).map_err(|e| format!("error: cannot resolve {}: {e}", p.display()))
    };
    let tree = tree.ok_or("error: a PATH needs the indexed tree; this index records none")?;
    let root = resolve(tree)?;
    p.paths = typed
        .iter()
        .map(|t| {
            let path = Path::new(t);
            // A symlink names its own path, not its target's.
            let abs = match (path.parent(), path.file_name()) {
                (Some(dir), Some(name)) if !dir.as_os_str().is_empty() => resolve(dir)?.join(name),
                (_, Some(name)) => resolve(Path::new("."))?.join(name),
                _ => resolve(path)?,
            };
            let outside = || format!("error: {t} is outside the {what} at {}", root.display());
            let rel = abs.strip_prefix(&root).map_err(|_| outside())?;
            let parts: Option<Vec<&str>> =
                rel.components().map(|c| c.as_os_str().to_str()).collect();
            Ok(SearchPath {
                path: parts
                    .map(|p| p.join("/"))
                    .ok_or_else(|| format!("error: {t} is not UTF-8"))?,
                name: t.clone(),
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(())
}

/// The tree a load of `found` reads the files from, when it overrides the
/// manifest's: none for stored documents.
fn root_of<'a>(found: &'a Located, manifest: &Manifest) -> Option<&'a Path> {
    found.root.as_deref().filter(|_| !manifest.store_docs)
}

/// The tree a manifest's paths are relative to.
fn tree_of(found: &Located, manifest: &Manifest) -> Option<PathBuf> {
    found
        .root
        .clone()
        .or_else(|| manifest.corpus_root.as_ref().map(PathBuf::from))
}

/// What a search started in `start` reads, before it reads anything.
enum Choice {
    /// The index found, and what a search warns of the server it passed
    /// over, if any.
    Index {
        found: Located,
        note: Option<String>,
    },
    Server(Server),
}

/// `--server`; else `--index`; else the index found by walking up from
/// `start`, which wins over the server that `env` (`FMGREP_SERVER`)
/// gives; else that server. `env` is called only when neither flag is
/// given, and a value that is not UTF-8 fails only a choice that needs it.
fn choose(
    index: Option<&Path>,
    server: Option<&str>,
    env: Env,
    start: &Path,
) -> Result<Choice, String> {
    if let Some(addr) = server {
        let addr = addr.to_owned();
        return Ok(Choice::Server(Server { addr, env: false }));
    }
    if let Some(index) = index {
        let found = locate(index)?;
        return Ok(Choice::Index { found, note: None });
    }
    Ok(match (locate(start), env()) {
        (Ok(found), Ok(Some(addr))) => {
            let note = format!(", not {}", Server { addr, env: true });
            Choice::Index {
                found,
                note: Some(note),
            }
        }
        (Ok(found), _) => Choice::Index { found, note: None },
        (Err(_), Ok(Some(addr))) => Choice::Server(Server { addr, env: true }),
        (Err(_), Err(e)) | (Err(e), Ok(None)) => return Err(e),
    })
}

/// A connection to a server, or the index a search falls back to.
enum Reached {
    Conn(Conn),
    Index { found: Located, note: String },
}

/// Connect to `server`. When `--server` gave it and it cannot be reached,
/// an index found by walking up from `start` answers instead; with
/// `FMGREP_SERVER`, such an index has already won.
fn reach(server: &Server, start: &Path) -> Result<Reached, String> {
    let e = match Conn::connect(&server.addr) {
        Ok(conn) => return Ok(Reached::Conn(conn)),
        Err(e) => e,
    };
    if !server.env
        && let Ok(found) = locate(start)
    {
        let note = format!(": cannot reach {server} ({e})");
        return Ok(Reached::Index { found, note });
    }
    Err(format!("error: cannot reach {server}: {e}"))
}

/// A search: what [`choose`] and [`reach`] pick from the cwd.
fn search(
    s: &Search,
    env: Env,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    tty: bool,
    printing: bool,
) -> Result<Outcome, String> {
    if s.shutdown {
        let addr = s
            .server
            .as_deref()
            .ok_or("error: --shutdown needs --server ADDR; FMGREP_SERVER does not stop a server")?;
        return serve::shutdown(addr);
    }
    let mut p = s.params(input, printing, tty)?;
    let typed = s.paths()?;
    p.cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cwd = Path::new(".");
    let choice = match choose(s.index.as_deref(), s.server.as_deref(), env, cwd) {
        Ok(choice) => choice,
        Err(e) if s.index.is_none() && env().is_err() => return Err(e),
        // No index: `local` fails to find it again, inside its spans.
        Err(_) => return local(s.index.as_deref(), p, &typed, s.json, out),
    };
    let (found, note) = match choice {
        Choice::Index { found, note } => (found, note),
        Choice::Server(server) => match reach(&server, cwd)? {
            Reached::Conn(conn) => return client(&server, conn, p, &typed, s.json, out),
            Reached::Index { found, note } => (found, Some(note)),
        },
    };
    if let Some(note) = note {
        warn(&format!(
            "Using local index {}{note}",
            found.index_dir.display()
        ));
    }
    // `local` finds the index again, inside its spans.
    local(s.index.as_deref(), p, &typed, s.json, out)
}

/// A search of the index at or above `index`, else above the cwd.
fn local(
    index: Option<&Path>,
    mut p: QueryParams,
    typed: &[String],
    json: bool,
    out: &mut dyn Write,
) -> Result<Outcome, String> {
    crate::verbose::record_spans();
    let _span = tracing::info_span!("search").entered();
    let (found, manifest) = open_index(index)?;
    let tree = tree_of(&found, &manifest);
    rebase(&mut p, typed, tree.as_deref())?;
    p.scope(tree.as_deref());
    let req = p.request(&found.index_dir, &manifest, root_of(&found, &manifest))?;
    let report = render(&p, json, out, |sink| run_query(&req, sink));
    drop(req);
    // Off the exit path: dropping a large tree's paths costs milliseconds.
    let _ = std::thread::Builder::new().spawn(move || drop(manifest));
    report.map(Outcome::Query)
}

/// `line` on stderr, as a warning.
fn warn(line: &str) {
    eprintln!("warning: {line}");
}

/// A server's address, and whether `FMGREP_SERVER` gave it.
struct Server {
    addr: String,
    env: bool,
}

impl std::fmt::Display for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.env {
            true => write!(f, "the server at {} (FMGREP_SERVER)", self.addr),
            false => write!(f, "the server at {}", self.addr),
        }
    }
}

/// A search through `server`, on `conn`; PATHs resolve against the served
/// tree.
fn client(
    server: &Server,
    mut conn: Conn,
    mut p: QueryParams,
    typed: &[String],
    json: bool,
    out: &mut dyn Write,
) -> Result<Outcome, String> {
    let status = serve::status_on(&mut conn)?;
    same_format(server, status.format)?;
    let cwd = std::fs::canonicalize(".").unwrap_or_else(|_| PathBuf::from(&p.cwd));
    let mut ours = None;
    match checkout::plan(server, &status, &cwd) {
        // The checkout holds the served commit: PATHs and the cwd are
        // resolved here, against its top, and its changes searched here.
        checkout::Plan::Checkout { top, commit } => {
            rebase_into(&mut p, typed, Some(&top), "checkout")?;
            p.scope(Some(&top));
            match mine(&top, &commit) {
                Ok(changes) => ours = Some((top, changes)),
                Err(why) => checkout::degraded(&why),
            }
            p.commit = Some(commit);
        }
        plan => {
            let pinned = match plan {
                checkout::Plan::Degraded(why) => {
                    checkout::degraded(&why);
                    checkout::top(&cwd).zip(status.commit.as_ref())
                }
                _ => None,
            };
            match pinned {
                // A checkout that lacks the served commit, or where git
                // cannot run, is still where the developer is: PATHs and
                // the cwd resolve against its top, and the commit pins the
                // query, which the server then scopes by its PATHs alone.
                // A server that serves no commit gives no sign that the
                // checkout is of its tree.
                Some((top, commit)) => {
                    rebase_into(&mut p, typed, Some(&top), "checkout")?;
                    p.scope(Some(&top));
                    p.commit = Some(commit.id.clone());
                }
                None => {
                    let root = Path::new(&status.root);
                    rebase(&mut p, typed, served_tree(root, status.store_docs))?;
                }
            }
        }
    }
    p.stream = true;
    p.format = Some(MANIFEST_FORMAT);
    let answered = match ours {
        None => render(&p, json, out, |sink| {
            serve::remote(&mut conn, &p, sink).and_then(|report| searched(report, &p))
        }),
        Some((top, (skip, local))) => merged(&mut conn, p, &top, skip, &local, json, out),
    };
    answered
        .map(Outcome::Query)
        .map_err(|e| moved(server, conn.code(), e))
}

/// `report`, unless the filter of `p` kept the search from every file:
/// then the error of a local search that searched none, before anything
/// is printed.
fn searched(report: OutputReport, p: &QueryParams) -> Result<OutputReport, String> {
    if report.none_searched
        && let Some(filter) = p.filter()?
    {
        return Err(crate::query::none_searched(&filter));
    }
    Ok(report)
}

/// The skip list of the checkout at `top` against the served commit
/// `commit`, and the files of it this client searches; else why the
/// checkout cannot be compared.
fn mine(top: &Path, commit: &str) -> Result<(Vec<String>, checkout::Local), String> {
    let skip = checkout::changes(top, commit)?;
    let local = checkout::resolve(top, &skip, commit)?;
    Ok((skip, local))
}

/// `p` answered by the client for its files `local` under `top`, and by
/// the server for every other file, the skip list `skip` left out; the
/// files printed in path order.
fn merged(
    conn: &mut Conn,
    mut p: QueryParams,
    top: &Path,
    skip: Vec<String>,
    local: &checkout::Local,
    json: bool,
    out: &mut dyn Write,
) -> Result<OutputReport, String> {
    let part = merge::part(&p, top, local)?;
    p.skip = skip;
    render(&p, json, out, |sink| {
        let mut both = merge::Merge::new(sink, &part);
        let server = serve::remote(conn, &p, &mut both)?;
        searched(merge::report(server, &part.report), &p)
    })
}

/// Refuse a server whose index format is not this fmgrep's, or that
/// reports none.
fn same_format(server: &Server, format: Option<u32>) -> Result<(), String> {
    match format {
        Some(MANIFEST_FORMAT) => Ok(()),
        Some(other) => Err(format!(
            "error: {server} serves an index of format {other}; this fmgrep reads format \
             {MANIFEST_FORMAT} (use one fmgrep version on both sides)"
        )),
        None => Err(format!(
            "error: {server} reports no index format; it is older than this fmgrep (use one \
             fmgrep version on both sides)"
        )),
    }
}

/// A query error `e` of code `code`, or the server's index change it means.
fn moved(server: &Server, code: Option<i64>, e: String) -> String {
    match code {
        Some(serve::OTHER_INDEX) => {
            format!("error: {server} changed its index during this search; run the search again")
        }
        _ => e,
    }
}

/// The tree a server's PATHs are relative to.
fn served_tree(root: &Path, store_docs: bool) -> Option<&Path> {
    match store_docs {
        true => root.parent().filter(|_| root.ends_with(INDEX_DIR)),
        false => Some(root),
    }
}

/// Parse and run `args` (argv without the binary name), writing output to
/// `out`. `serve --stdio` and `--expr-json -` read `input`; `tty` resolves
/// `--color auto`. `FMGREP_SERVER` is not read: a developer's value never
/// reaches the tests.
#[cfg(test)]
pub fn dispatch_to(
    args: impl IntoIterator<Item = impl Into<OsString> + Clone>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    tty: bool,
) -> Result<Outcome, String> {
    dispatch_in(args, input, out, tty, true, || Ok(None))
}

/// [`dispatch_to`] with empty input, discarded output, and a search with no
/// mode flag driving counts.
#[cfg(test)]
pub fn dispatch(
    args: impl IntoIterator<Item = impl Into<OsString> + Clone>,
) -> Result<Outcome, String> {
    let (mut input, mut out) = (std::io::empty(), std::io::sink());
    dispatch_in(args, &mut input, &mut out, false, false, || Ok(None))
}

/// Reads `FMGREP_SERVER`, when a search needs it.
type Env = fn() -> Result<Option<String>, String>;

/// `FMGREP_SERVER`: unset when empty.
fn server_env() -> Result<Option<String>, String> {
    match std::env::var_os("FMGREP_SERVER") {
        Some(v) if v.is_empty() => Ok(None),
        Some(v) => (v.into_string())
            .map(Some)
            .map_err(|v| format!("error: FMGREP_SERVER={v:?} is not UTF-8")),
        None => Ok(None),
    }
}

/// [`dispatch_to`], with `env` reading `FMGREP_SERVER`.
fn dispatch_in(
    args: impl IntoIterator<Item = impl Into<OsString> + Clone>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    tty: bool,
    printing: bool,
    env: Env,
) -> Result<Outcome, String> {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) if !e.use_stderr() => {
            print::line(out, e.to_string().trim_end())?;
            return Ok(Outcome::Done);
        }
        Err(e) => return Err(e.to_string().trim_end().to_owned()),
    };
    let verbose = matches!(&cli.command, Some(Command::Index(a)) if a.verbose);
    crate::verbose::set_verbose(verbose).map_err(|e| format!("error: {e}"))?;
    match &cli.command {
        Some(Command::Index(a)) => {
            let summary = run_index(&IndexRequest {
                folder: &a.path,
                output: a.output.as_deref(),
                store_docs: a.store_docs,
                require_commit: a.require_served_commit,
            })?;
            print::index_summary(out, a.json, &summary)?;
            Ok(Outcome::Index(summary))
        }
        Some(Command::Serve(a)) => serve(a, input, out),
        Some(Command::Status(a)) => status(a, env, out),
        None => search(&cli.search, env, input, out, tty, printing),
    }
}

/// `run`'s matches printed as `p` asks: rg's human format, a counts summary,
/// or under `--json` rg's event stream or the wire report.
fn render(
    p: &QueryParams,
    json: bool,
    out: &mut dyn Write,
    run: impl FnOnce(&mut dyn MatchSink) -> Result<OutputReport, String>,
) -> Result<OutputReport, String> {
    Ok(match p.mode {
        Mode::JsonMatches => {
            let mut sink = JsonSink::new(out);
            let report = run(&mut sink)?;
            sink.finish()?;
            report
        }
        _ if json => {
            let report = run(&mut Discard)?;
            print::query_report(out, &report)?;
            report
        }
        Mode::Counts => {
            let report = run(&mut Discard)?;
            print::counts_summary(out, &report)?;
            report
        }
        _ => {
            let mut writer = MatchWriter::new(out, p.output_config());
            let report = run(&mut writer)?;
            writer.finish()?;
            report
        }
    })
}

/// Run the process argv and print the result.
///
/// Exit codes, as rg's: 0 success, 1 a search that matched nothing or an IO
/// failure writing output, 2 bad argv / unreadable input / build refusal. A
/// closed pipe is not a failure.
pub fn run() {
    crate::verbose::start_clock();
    let tty = std::io::stdout().is_terminal();
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let mut input = std::io::stdin().lock();
    let args = std::env::args_os().skip(1);
    let dispatched = dispatch_in(args, &mut input, &mut out, tty, true, server_env);
    let mut result = dispatched.map(|o| match o {
        Outcome::Query(r) => r
            .queries
            .iter()
            .any(|q| q.total_matches > 0 || q.docs_matched > 0),
        _ => true,
    });
    // Flush on every path: a failed run must still end on a whole line.
    // A closed pipe left the writer silent; nothing else may be.
    if let Err(e) = tracing::info_span!("flush").in_scope(|| out.flush())
        && e.kind() != std::io::ErrorKind::BrokenPipe
        && result.is_ok()
    {
        result = Err(format!("{WRITE_FAILED} output: {e}"));
    }
    crate::verbose::print_spans();
    match result {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(m) => {
            eprintln!("{m}");
            std::process::exit(if m.starts_with(WRITE_FAILED) { 1 } else { 2 });
        }
    }
}

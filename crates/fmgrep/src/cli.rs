use std::ffi::OsString;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::Parser;

mod args;
mod frames;
mod locate;
mod params;
mod print;
mod serve;

use crate::events::JsonSink;
use crate::filter::SearchPath;
use crate::index::{INDEX_DIR, IndexRequest, run_index};
use crate::manifest::{BLOBS_SHARD, MANIFEST_FORMAT, Manifest, read_manifest};
use crate::output::{MatchWriter, WRITE_FAILED};
use crate::query::run_query;
use crate::remote::Conn;
use crate::sink::{Discard, MatchSink};
use crate::wire::{IndexSummary, OutputReport};
use args::{Cli, Command, Search, ServeArgs};
use locate::{Located, locate};
use params::{Mode, QueryParams};
use serve::{ServeRequest, run_client, run_serve};

/// What a successful dispatch produced.
pub enum Outcome {
    Index(#[cfg_attr(not(test), allow(dead_code))] IndexSummary),
    Query(OutputReport),
    /// `serve` ended, help or the version printed, or `--status|--shutdown`
    /// answered.
    Done,
}

fn serve(a: &ServeArgs, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<Outcome, String> {
    let (found, manifest) = open_index(a.path.as_deref())?;
    let root = found.root.filter(|_| !manifest.store_docs);
    let req = ServeRequest {
        index_dir: &found.index_dir,
        manifest: &manifest,
        corpus_root: root.as_deref(),
        listen: (!a.stdio).then_some(a.listen),
        watch: !a.no_watch,
    };
    run_serve(&req, input, out)?;
    Ok(Outcome::Done)
}

/// The index found from `start` (else the cwd) and its manifest. A
/// files-source dotdir reads the tree it sits in, not the manifest's.
fn open_index(start: Option<&Path>) -> Result<(Located, Manifest), String> {
    let found = locate(start.unwrap_or(Path::new(".")))?;
    if !found.index_dir.join(BLOBS_SHARD).is_file() {
        return Err(format!(
            "error: {} has no {BLOBS_SHARD}",
            found.index_dir.display()
        ));
    }
    let manifest = read_manifest(&found.index_dir)?;
    Ok((found, manifest))
}

/// `typed`, resolved from the cwd, as `p`'s PATHs into the indexed `tree`.
fn rebase(p: &mut QueryParams, typed: &[String], tree: Option<&Path>) -> Result<(), String> {
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
            let outside = || {
                format!(
                    "error: {t} is outside the indexed tree at {}",
                    root.display()
                )
            };
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

/// The tree a manifest's paths are relative to.
fn tree_of(found: &Located, manifest: &Manifest) -> Option<PathBuf> {
    found
        .root
        .clone()
        .or_else(|| manifest.corpus_root.as_ref().map(PathBuf::from))
}

/// A search: through `--server`; else of `--index`; else of the index found
/// by walking up from the cwd, or through the server that `env`
/// (`FMGREP_SERVER`) gives when none is found.
fn search(
    s: &Search,
    env: Option<&str>,
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
        return serve::manage(addr, true, out);
    }
    if s.status {
        let addr = (s.server.as_deref())
            .or(env)
            .ok_or("error: --status needs --server ADDR or FMGREP_SERVER")?;
        return serve::manage(addr, false, out);
    }
    let mut p = s.params(input, printing, tty)?;
    let typed = s.paths()?;
    p.cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(addr) = &s.server {
        return client(Server { addr, env: false }, p, &typed, s.json, out);
    }
    if let (None, Some(addr)) = (&s.index, env) {
        match locate(Path::new(".")) {
            Ok(found) => warn(&format!(
                "Using local index {}, not the server at {addr} (FMGREP_SERVER)",
                found.index_dir.display()
            )),
            Err(_) => return client(Server { addr, env: true }, p, &typed, s.json, out),
        }
    }
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
    let (found, manifest) = open_index(index)?;
    let tree = tree_of(&found, &manifest);
    rebase(&mut p, typed, tree.as_deref())?;
    p.scope(tree.as_deref());
    let root = found.root.filter(|_| !manifest.store_docs);
    let req = p.request(&found.index_dir, &manifest, root.as_deref())?;
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
#[derive(Clone, Copy)]
struct Server<'a> {
    addr: &'a str,
    env: bool,
}

impl std::fmt::Display for Server<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.env {
            true => write!(f, "the server at {} (FMGREP_SERVER)", self.addr),
            false => write!(f, "the server at {}", self.addr),
        }
    }
}

/// A search through `server`: one connection; PATHs resolve against the
/// served tree. When `--server` gives a server that cannot be reached, an
/// index found by walking up answers, with a warning.
fn client(
    server: Server<'_>,
    mut p: QueryParams,
    typed: &[String],
    json: bool,
    out: &mut dyn Write,
) -> Result<Outcome, String> {
    let mut conn = match Conn::connect(server.addr) {
        Ok(conn) => conn,
        Err(e) => {
            // With `FMGREP_SERVER`, such an index has already won.
            if !server.env
                && let Ok(found) = locate(Path::new("."))
            {
                warn(&format!(
                    "Using local index {}: cannot reach {server} ({e})",
                    found.index_dir.display()
                ));
                return local(None, p, typed, json, out);
            }
            return Err(format!("error: cannot reach {server}: {e}"));
        }
    };
    let status = serve::status_on(&mut conn)?;
    same_format(server, status.format)?;
    if !typed.is_empty() {
        let root = Path::new(&status.root);
        rebase(&mut p, typed, served_tree(root, status.store_docs))?;
    }
    p.stream = true;
    p.per_file = true;
    p.format = Some(MANIFEST_FORMAT);
    let answered = run_client(&mut conn, &p, json, out);
    answered.map_err(|e| moved(server, conn.code(), e))
}

/// Refuse a server whose index format is not this fmgrep's, or that
/// reports none.
fn same_format(server: Server<'_>, format: Option<u32>) -> Result<(), String> {
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
fn moved(server: Server<'_>, code: Option<i64>, e: String) -> String {
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
    dispatch_in(args, input, out, tty, true, None)
}

/// [`dispatch_to`] with empty input, discarded output, and a search with no
/// mode flag driving counts.
#[cfg(test)]
pub fn dispatch(
    args: impl IntoIterator<Item = impl Into<OsString> + Clone>,
) -> Result<Outcome, String> {
    let (mut input, mut out) = (std::io::empty(), std::io::sink());
    dispatch_in(args, &mut input, &mut out, false, false, None)
}

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

/// [`dispatch_to`], with `env` the value of `FMGREP_SERVER`.
fn dispatch_in(
    args: impl IntoIterator<Item = impl Into<OsString> + Clone>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    tty: bool,
    printing: bool,
    env: Option<&str>,
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
            })?;
            print::index_summary(out, a.json, &summary)?;
            Ok(Outcome::Index(summary))
        }
        Some(Command::Serve(a)) => serve(a, input, out),
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
    let tty = std::io::stdout().is_terminal();
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let mut input = std::io::stdin().lock();
    let args = std::env::args_os().skip(1);
    let dispatched = server_env()
        .and_then(|env| dispatch_in(args, &mut input, &mut out, tty, true, env.as_deref()));
    let mut result = dispatched.map(|o| match o {
        Outcome::Query(r) => r
            .queries
            .iter()
            .any(|q| q.total_matches > 0 || q.docs_matched > 0),
        _ => true,
    });
    // Flush on every path: a failed run must still end on a whole line.
    // A closed pipe left the writer silent; nothing else may be.
    if let Err(e) = out.flush()
        && e.kind() != std::io::ErrorKind::BrokenPipe
        && result.is_ok()
    {
        result = Err(format!("{WRITE_FAILED} output: {e}"));
    }
    match result {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(m) => {
            eprintln!("{m}");
            std::process::exit(if m.starts_with(WRITE_FAILED) { 1 } else { 2 });
        }
    }
}

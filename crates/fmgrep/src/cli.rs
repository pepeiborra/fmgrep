use std::ffi::OsString;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::Parser;

mod args;
mod baseline;
mod frames;
mod locate;
mod merge;
mod params;
mod print;
mod serve;
mod status;

use crate::checkout;
use crate::diagnostic::{Check, Diagnostic};
use crate::events::JsonSink;
use crate::fetch::Ask;
use crate::filter::SearchPath;
use crate::index::{INDEX_DIR, IndexRequest, run_index};
use crate::manifest::{BLOBS_SHARD, MANIFEST_FORMAT, Manifest, read_manifest};
use crate::output::{MatchWriter, WRITE_FAILED};
use crate::query::{answer as answer_local, load};
use crate::remote::{Conn, Server};
use crate::sink::{Discard, MatchSink};
use crate::source::Unsaid;
use crate::wire::{IndexSummary, OutputReport, ServeStatus};
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
        Choice::Server(server) => match reach(&server, Some(start))? {
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
/// an index found by walking up from `start`, if given, answers instead;
/// with `FMGREP_SERVER`, such an index has already won.
fn reach(server: &Server, start: Option<&Path>) -> Result<Reached, String> {
    let e = match Conn::connect(&server.addr) {
        Ok(conn) => return Ok(Reached::Conn(conn)),
        Err(e) => e,
    };
    if !server.env
        && let Some(Ok(found)) = start.map(locate)
    {
        let note = format!(": cannot reach {server} ({e})");
        return Ok(Reached::Index { found, note });
    }
    Err(format!("error: cannot reach {server}: {e}"))
}

/// A search: what [`choose`] and [`reach`] pick from the cwd. A PATTERN that
/// is not a regex says how to search for it as it is. As in rg, a PATH that
/// does not exist is an error that leaves the others searched, and the
/// search exits 2 whatever it found.
fn search(
    s: &Search,
    env: Env,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    tty: bool,
    printing: bool,
) -> Result<Outcome, String> {
    let patterns = !s.fixed_strings && s.expr.is_none() && s.expr_json.is_none();
    let (typed, mut errors) = existing_paths(&s.paths());
    let found = find(
        s,
        env,
        input,
        out,
        tty,
        printing,
        &typed,
        !errors.is_empty(),
    );
    // A PATH that exists, with no file the index or the added files hold:
    // rg would search it, an index cannot.
    if let Ok(Outcome::Query(r)) = &found {
        errors.extend(r.unindexed.iter().filter_map(|t| unindexed_error(t)));
    }
    // A pattern /x/ matches its slashes too, as in rg; v0.1.0 read it as x.
    // Each such pattern that found nothing says so, in a search of several.
    if patterns && let Ok(Outcome::Query(r)) = &found {
        for t in (s.typed_terms().iter())
            .filter(|t| t.len() > 1 && t.starts_with('/') && t.ends_with('/'))
            .filter(|t| found_nothing(r, t))
        {
            say(Diagnostic::Slashes { pattern: t.clone() });
        }
    }
    let found = found.map_err(|e| match e {
        // A NUL is no matter of syntax: -F would not help.
        e if patterns
            && e.contains("invalid regex pattern")
            && !e.contains("impossible to match") =>
        {
            format!("{e}\nhint: to search for the text as it is, pass -F")
        }
        e => e,
    });
    match found {
        Ok(done) if errors.is_empty() => Ok(done),
        Ok(_) => Err(errors.join("\n")),
        Err(e) => {
            errors.extend(Some(e).filter(|e| !e.is_empty()));
            Err(errors.join("\n"))
        }
    }
}

/// The error for the PATH `t`, which exists and holds no file the index or the
/// added files hold; none for a directory in which rg would read nothing
/// either (empty, or holding only hidden, ignored or binary files), where the
/// search simply found nothing.
fn unindexed_error(t: &str) -> Option<String> {
    let path = std::path::Path::new(t);
    if path.is_dir() && !crate::walk::rg_reads_in(path) {
        return None;
    }
    Some(match respelled(t) {
        Some(real) => format!(
            "error: {t}: spelled in another case than on disk ({real}); the index holds names as \
             they are"
        ),
        None => format!(
            "error: {t}: not in the index (hidden, ignored, binary, a symlink, or new to a \
             --store-docs index); search it with rg"
        ),
    })
}

/// Where the PATH `t` is on disk, when `t` differs from it only in case, as on
/// a case-insensitive file system: its last components, as spelled there.
fn respelled(t: &str) -> Option<String> {
    use std::path::Component::Normal;
    let typed = std::path::Path::new(t);
    // A symlink is another name, not another case.
    if std::fs::symlink_metadata(typed).ok()?.is_symlink() {
        return None;
    }
    let real = std::fs::canonicalize(typed).ok()?;
    let mut spelled = Vec::new();
    let mut other = false;
    for (a, b) in typed.components().rev().zip(real.components().rev()) {
        let (Normal(a), Normal(b)) = (a, b) else {
            break;
        };
        if !a.eq_ignore_ascii_case(b) {
            return None;
        }
        other |= a != b;
        spelled.push(b.to_string_lossy());
    }
    spelled.reverse();
    other.then(|| spelled.join("/"))
}

/// The PATHs `told` that exist, and the errors, one for each that does
/// not: rg's words, with `error:` before them.
fn existing_paths(told: &[String]) -> (Vec<String>, Vec<String>) {
    let mut existing = Vec::new();
    let mut errors = Vec::new();
    for t in told {
        match std::fs::metadata(t) {
            Ok(_) => existing.push(t.clone()),
            Err(e) => errors.push(format!("error: {t}: {e}")),
        }
    }
    (existing, errors)
}

/// Whether the pattern `t` found nothing: its own section if it has one,
/// else its own total in a counting search of an expression, else the whole
/// search.
fn found_nothing(r: &OutputReport, t: &str) -> bool {
    if let Some(q) = r.queries.iter().find(|q| q.term == t) {
        return q.total_matches == 0 && q.docs_matched == 0;
    }
    if let Some(leaf) = (r.queries.iter())
        .flat_map(|q| &q.leaves)
        .find(|l| l.term == crate::expr::Expr::Regex(t.to_owned()).to_string())
    {
        return leaf.total_matches == 0;
    }
    (r.queries.iter()).all(|q| q.total_matches == 0 && q.docs_matched == 0)
}

/// [`search`] without the hint, of the PATHs `typed` that exist; `missing`
/// when some PATH does not.
#[allow(clippy::too_many_arguments)]
fn find(
    s: &Search,
    env: Env,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    tty: bool,
    printing: bool,
    typed: &[String],
    missing: bool,
) -> Result<Outcome, String> {
    if s.shutdown {
        let addr = s
            .server
            .as_deref()
            .ok_or("error: --shutdown needs --server ADDR; FMGREP_SERVER does not stop a server")?;
        return serve::shutdown(addr);
    }
    let mut p = s.params(input, printing, tty)?;
    // Every PATH is missing: no search is left to run, and the caller says why.
    if typed.is_empty() && missing {
        return Err(String::new());
    }
    p.cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ignore = s.ignore_local_changes;
    let cwd = Path::new(".");
    let choice = match choose(s.index.as_deref(), s.server.as_deref(), env, cwd) {
        Ok(choice) => choice,
        Err(e) if s.index.is_none() && env().is_err() => return Err(e),
        Err(_) if ignore => return Err(NEEDS_SERVER.to_owned()),
        // No index: `local` fails to find it again, inside its spans.
        Err(_) => return local(s.index.as_deref(), p, typed, s.json, out),
    };
    // With `ignore`, a server that cannot be reached is an error.
    let fallback = (!ignore).then_some(cwd);
    let (found, note) = match choice {
        Choice::Index { found, note } => (found, note),
        Choice::Server(server) => match reach(&server, fallback)? {
            Reached::Conn(conn) => return client(&server, conn, ignore, p, typed, s.json, out),
            Reached::Index { found, note } => (found, Some(note)),
        },
    };
    if ignore {
        // A note here says that the index won over `FMGREP_SERVER`.
        return Err(match (note, env()) {
            (Some(_), Ok(Some(addr))) => local_index_answers(&found, &addr),
            _ => NEEDS_SERVER.to_owned(),
        });
    }
    if let Some(note) = note {
        say(Diagnostic::LocalIndex {
            dir: found.index_dir.clone(),
            note,
        });
    }
    // `local` finds the index again, inside its spans.
    local(s.index.as_deref(), p, typed, s.json, out)
}

/// Why a search with `--ignore-local-changes` fails that no server answers.
const NEEDS_SERVER: &str = "error: --ignore-local-changes needs --server ADDR or FMGREP_SERVER";

/// Why a search with `--ignore-local-changes` fails when the local index
/// `found` wins over the server at `addr`: the flag asks for that server's
/// answer alone, and a local index searches the checkout as it is.
fn local_index_answers(found: &Located, addr: &str) -> String {
    format!(
        "error: the local index {} wins over the server at {addr} (FMGREP_SERVER), and \
         --ignore-local-changes needs the server's answer; pass --server {addr} to search it",
        found.index_dir.display()
    )
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
    let report = render(&p, json, out, |sink| {
        let mut left_out = Vec::new();
        let loaded = load(
            req.index_dir,
            req.manifest,
            req.corpus_root,
            Ask::Load,
            None,
            &mut left_out,
        );
        let loaded = loaded.map(|(index, mut loaded)| {
            loaded.note_stale(&req.manifest.files, &mut left_out);
            (index, loaded)
        });
        print_diagnostics(&std::mem::take(&mut left_out));
        let (index, mut loaded) = loaded?;
        let report = answer_local(&index, &mut loaded, &req, sink, &mut left_out);
        print_diagnostics(&left_out);
        tracing::info_span!("drop").in_scope(|| drop((index, loaded)));
        report
    });
    drop(req);
    // Off the exit path: dropping a large tree's paths costs milliseconds.
    let _ = std::thread::Builder::new().spawn(move || drop(manifest));
    report.map(Outcome::Query)
}

/// What a load or a search left out, on stderr: the one place that prints
/// entries, in the order they were found. A search through a server keeps its
/// entries in a `Try` until the server accepts the pin, and drops them with it
/// if the server refuses.
fn print_diagnostics(left_out: &[Diagnostic]) {
    #[cfg(test)]
    crate::tests::note_entries(left_out);
    for d in left_out {
        eprintln!("{d}");
    }
}

/// One entry, printed at once.
fn say(d: Diagnostic) {
    print_diagnostics(&[d]);
}

/// What a search through a server has to say, kept as values until the
/// server has taken its query: the first frame, or the result, prints them.
/// A query that the server refuses spoke of a commit it no longer serves, so
/// its `Try` is dropped, and with it what it holds.
#[derive(Default)]
struct Try {
    entries: Vec<Diagnostic>,
    /// How many of `entries` are printed.
    shown: usize,
    /// The note that keeps a warning quiet next time, kept once it is printed.
    said: Option<Unsaid>,
    /// Whether the server took the query.
    printed: bool,
}

impl Try {
    /// Print what is not printed yet, then keep the note of what was said.
    fn print(&mut self) {
        print_diagnostics(&self.entries[self.shown..]);
        self.shown = self.entries.len();
        self.printed = true;
        if let Some(said) = self.said.take() {
            said.keep();
        }
    }

    /// Whether a record or a pin that could not be saved is among the
    /// entries, printed or not: it is said once.
    fn cannot_save(&self) -> bool {
        let is = |d: &Diagnostic| matches!(d, Diagnostic::Check(Check::CannotSave { .. }));
        self.entries.iter().any(is)
    }
}

/// A search through `server`, on `conn`. With `ignore`
/// (`--ignore-local-changes`), the server's answer alone, and no look at
/// the checkout's changes.
///
/// From a git checkout, the served commit of each status is pinned for
/// the next search. A search that finds a pin of a commit asks for no
/// status. It searches as with a status of that commit: a search from a
/// checkout reads only the format and the commit of a status, and a
/// server of another commit or format refuses its pinned query before it
/// searches. Its lines on stderr are kept in a [`Try`] until the server
/// answers. When the server refuses, or the search fails before an
/// answer, the client asks for the status. If the status has the pinned
/// commit, the lines print and the search ends as it would have; else the
/// `Try` is dropped, and the search runs again as one without a pin.
fn client(
    server: &Server,
    mut conn: Conn,
    ignore: bool,
    p: QueryParams,
    typed: &[String],
    json: bool,
    out: &mut dyn Write,
) -> Result<Outcome, String> {
    use crate::source::Pinned;
    let cwd = std::fs::canonicalize(".").unwrap_or_else(|_| PathBuf::from(&p.cwd));
    let top = checkout::top(&cwd);
    let had =
        (top.as_deref()).map(|top| crate::source::pinned(top, &server.addr, checkout::full_id));
    let mut search = |conn: &mut Conn, status: &ServeStatus, p: QueryParams, attempt: &mut Try| {
        answer(
            server, conn, status, &cwd, ignore, p, typed, json, out, attempt,
        )
    };
    let mut attempt = Try::default();
    let status = match &had {
        Some(Pinned::Commit(id)) => {
            if crate::verbose::verbose() {
                let pin =
                    (top.as_deref()).and_then(|top| crate::source::pin_path(top, &server.addr));
                attempt.entries.push(Diagnostic::SentPinned {
                    commit: id.clone(),
                    pin,
                });
            }
            let tried = search(&mut conn, &ServeStatus::pinned(id), p.clone(), &mut attempt);
            if attempt.printed || tried.is_ok() {
                attempt.print();
                return tried;
            }
            let status = serve::status_on(&mut conn)
                .and_then(|status| same_format(server, status.format).map(|()| status));
            match status {
                Ok(status) if checkout::pin_of(&status) == Some(id) => {
                    attempt
                        .entries
                        .extend(asked(server, "the search failed before a result came"));
                    attempt.print();
                    return tried;
                }
                Ok(status) => {
                    // The refusal: what the try held spoke of a commit the
                    // server no longer serves.
                    drop(attempt);
                    attempt = Try::default();
                    let short = checkout::short(id);
                    let why = format!("it no longer serves {short}, the commit that the pin keeps");
                    if let Some(d) = asked(server, &why) {
                        say(d);
                    }
                    status
                }
                Err(e) => return Err(e),
            }
        }
        other => {
            match other {
                Some(Pinned::Damaged(why)) => say(Diagnostic::PinDamaged {
                    why: why.clone(),
                    server: server.clone(),
                }),
                // Outside a checkout no pin applies: no note.
                None => {}
                Some(_) => {
                    if let Some(d) = asked(server, "no pin of an earlier search") {
                        say(d);
                    }
                }
            }
            let status = serve::status_on(&mut conn)?;
            same_format(server, status.format)?;
            status
        }
    };
    let answered = search(&mut conn, &status, p, &mut attempt);
    attempt.print();
    // After the search, so that a record that cannot be saved says so first.
    let now = checkout::pin_of(&status);
    let kept = match &had {
        Some(Pinned::Commit(id)) => now == Some(id.as_str()),
        Some(Pinned::Missing) => now.is_none(),
        _ => false,
    };
    if let Some(top) = top.filter(|_| !kept)
        && let Err(check) = crate::source::pin(&top, &server.addr, now)
        && !attempt.cannot_save()
    {
        say(Diagnostic::Check(check));
    }
    answered
}

/// Under `FMGREP_VERBOSE`, that a search asked `server` for its status,
/// and `why`.
fn asked(server: &Server, why: &str) -> Option<Diagnostic> {
    crate::verbose::verbose().then(|| Diagnostic::Asked {
        server: server.clone(),
        why: why.to_owned(),
    })
}

/// A search through `server` of `status`, on `conn`, from the canonical
/// `cwd`; PATHs resolve against the served tree, unless the cwd is in a
/// checkout and the server serves a commit. What it has to say goes to
/// `attempt`.
#[allow(clippy::too_many_arguments)]
fn answer(
    server: &Server,
    conn: &mut Conn,
    status: &ServeStatus,
    cwd: &Path,
    ignore: bool,
    mut p: QueryParams,
    typed: &[String],
    json: bool,
    out: &mut dyn Write,
    attempt: &mut Try,
) -> Result<Outcome, String> {
    let mut ours = None;
    let plan = match ignore {
        true => checkout::Plan::Ignored,
        false => checkout::plan(server, status, cwd),
    };
    let degraded = |why: String| Diagnostic::Degraded { why };
    match plan {
        // The checkout holds the served commit: PATHs and the cwd are
        // resolved here, against its top, and its changes searched here.
        checkout::Plan::Checkout { top, commit } => {
            rebase_into(&mut p, typed, Some(&top), "checkout")?;
            p.scope(Some(&top));
            match baseline::skip_list(&top, &commit, &p, &mut attempt.entries) {
                Ok((skip, local, unsaid)) => {
                    attempt.said = unsaid;
                    ours = Some((top, (skip, local)));
                }
                Err(why) => attempt.entries.push(degraded(why)),
            }
            p.commit = Some(commit);
        }
        plan => {
            let pinned = match plan {
                checkout::Plan::Degraded(why) => {
                    attempt.entries.push(degraded(why));
                    checkout::top(cwd).zip(status.commit.as_ref())
                }
                checkout::Plan::Ignored => checkout::top(cwd).zip(status.commit.as_ref()),
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
        // The client's own files go between the server's: it needs each
        // file of the server's in a frame of its own.
        Some((top, (skip, local))) if !local.files.is_empty() => {
            merged(conn, p, &top, skip, &local, json, out, attempt)
        }
        // The server's files only, those on the skip list (deleted
        // files) left out: its output is the search's.
        ours => {
            // Counts have no output to copy.
            p.rendered = p.mode != Mode::Counts;
            if let Some((_, (skip, _))) = ours {
                p.skip = skip;
            }
            serve::rendered(conn, &p, json, out, attempt)
        }
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

/// `p` answered by the client for its files `local` under `top`, and by
/// the server for every other file, the skip list `skip` left out; the
/// files printed in path order. What the client's part leaves out goes to
/// `attempt`.
#[allow(clippy::too_many_arguments)]
fn merged(
    conn: &mut Conn,
    mut p: QueryParams,
    top: &Path,
    skip: Vec<String>,
    local: &checkout::Local,
    json: bool,
    out: &mut dyn Write,
    attempt: &mut Try,
) -> Result<OutputReport, String> {
    let part = merge::part(&p, top, local, &mut attempt.entries)?;
    p.skip = skip;
    render(&p, json, out, |sink| {
        let mut both = merge::Merge::new(sink, &part);
        let server = serve::remote(conn, &p, &mut both, attempt)?;
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
        Err(e) => return Err(refusal(&e).unwrap_or_else(|| e.to_string().trim_end().to_owned())),
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

/// The error of an rg flag that no index can honour, which clap met as an
/// unknown argument of a search: what the index lacks, and rg to use instead.
/// `fmgrep index --hidden` is clap's error: rg builds no index.
fn refusal(e: &clap::Error) -> Option<String> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    if e.kind() != ErrorKind::UnknownArgument {
        return None;
    }
    // The usage line is `fmgrep [OPTIONS] ...` for a search, `fmgrep index ...`
    // for a subcommand.
    let usage = e.get(ContextKind::Usage).map(ToString::to_string);
    let word = usage.as_deref().and_then(|u| u.split_whitespace().nth(2));
    if word.is_some_and(|w| !w.starts_with(['[', '<'])) {
        return None;
    }
    match e.get(ContextKind::InvalidArg)? {
        ContextValue::String(flag) => args::unindexable(flag),
        _ => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Of, unsaid};

    /// A try keeps its note of what it said only once it prints, and prints
    /// each entry once; a try that is dropped keeps nothing, so the next
    /// search says the reason again.
    #[test]
    fn a_try_keeps_what_it_said_only_when_it_prints() {
        let tmp = tempfile::tempdir().unwrap();
        crate::tests::CACHE.set(Some(tmp.path().join("cache")));
        let top = Path::new("/src/linux");
        let of = Of::commit(&"a".repeat(40));
        let attempt = || Try {
            entries: vec![Diagnostic::AnswersAgain],
            said: unsaid(top, &of, "no daemon"),
            ..Try::default()
        };
        drop(attempt());
        assert!(unsaid(top, &of, "no daemon").is_some(), "dropped: not said");
        let mut printed = attempt();
        assert!(!printed.printed && printed.shown == 0);
        printed.print();
        assert!(printed.printed && printed.shown == 1 && printed.said.is_none());
        assert!(unsaid(top, &of, "no daemon").is_none(), "printed: said");
        printed.entries.push(Diagnostic::AnswersAgain);
        printed.print();
        assert_eq!(printed.shown, 2, "each entry once");
    }

    /// A record or pin that cannot be saved is said once: by the try that
    /// holds it, not again by the pin.
    #[test]
    fn a_try_knows_it_holds_a_failed_save() {
        let mut attempt = Try::default();
        assert!(!attempt.cannot_save());
        attempt.entries.push(Diagnostic::Check(Check::CannotSave {
            what: "the record".into(),
        }));
        attempt.print();
        assert!(attempt.cannot_save(), "also once printed");
    }
}

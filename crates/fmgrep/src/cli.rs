use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

mod args;
mod locate;
mod print;
mod serve;

use crate::events::JsonSink;
use crate::index::{IndexRequest, run_index};
use crate::manifest::{Manifest, NAME, read_manifest};
use crate::output::{MatchWriter, WRITE_FAILED};
use crate::query::{QueryRequest, run_query};
use crate::sink::{Discard, MatchSink};
use crate::wire::{IndexSummary, OutputReport};
use args::{ClientAction, ClientArgs, Command, Parsed, ServeArgs, parse_args};
use locate::locate;
use serve::{ServeRequest, run_client, run_serve};

/// The binary's whole CLI text.
pub struct Cli {
    /// One-line usage, quoted verbatim inside argv errors.
    pub usage: &'static str,
    /// `--help` body, printed verbatim.
    pub help: &'static str,
}

/// What a successful dispatch produced.
pub enum Outcome {
    Help,
    Version,
    Index(IndexSummary),
    Query(OutputReport),
    /// `serve` ended, or `--server` with `--status|--shutdown` answered.
    Done,
}

fn serve(a: &ServeArgs, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<Outcome, String> {
    let (index_dir, manifest, root) = open_index(a.path.as_deref())?;
    let req = ServeRequest {
        index_dir: &index_dir,
        manifest: &manifest,
        corpus_root: root.as_deref(),
        listen: a.listen,
        watch: !a.no_watch,
    };
    run_serve(&req, input, out)?;
    Ok(Outcome::Done)
}

/// The index found from `start` (else the cwd), its manifest, and the
/// corpus root overriding the manifest's: a files-source dotdir reads the
/// tree it sits in.
fn open_index(start: Option<&Path>) -> Result<(PathBuf, Manifest, Option<PathBuf>), String> {
    let found = locate(start.unwrap_or(Path::new(".")))?;
    let manifest = read_manifest(&found.index_dir)?;
    let root = found.root.filter(|_| !manifest.store_docs);
    Ok((found.index_dir, manifest, root))
}

/// `printing` widens a search with no mode flag from wire counts to matched
/// lines.
fn execute(cmd: &Command, printing: bool, sink: &mut dyn MatchSink) -> Result<Outcome, String> {
    match cmd {
        Command::Index(a) => {
            crate::set_verbose(a.verbose);
            run_index(&IndexRequest {
                folder: &a.folder,
                output: a.output.as_deref(),
                store_docs: a.store_docs,
            })
            .map(Outcome::Index)
        }
        Command::Query(a) => {
            let (index_dir, manifest, root) = open_index(a.index.as_deref())?;
            run_query(
                &QueryRequest {
                    index_dir: &index_dir,
                    manifest: &manifest,
                    corpus_root: root.as_deref(),
                    regex: a.regex,
                    mode: a.mode(),
                    terms: &a.terms,
                    expr: a.expr.as_ref(),
                    output: a.output(printing),
                },
                sink,
            )
            .map(Outcome::Query)
        }
        Command::Serve(a) if a.listen.is_some() => Err(
            "error: serve over TCP needs dispatch_to (its readiness line would be discarded)"
                .to_owned(),
        ),
        Command::Serve(a) => serve(a, &mut std::io::empty(), &mut std::io::sink()),
        Command::Client(a) => run_client(a, &mut std::io::sink()),
    }
}

/// Parse and run `args` against `cli`, DISCARDING match output: the outcome
/// carries the counting report only. The test seam behind `dispatch_to`:
/// same parsing, same dispatch, no printing and no `exit`; `serve --stdio`
/// reads no requests and TCP `serve` is refused.
pub fn dispatch(cli: &Cli, args: &[String]) -> Result<Outcome, String> {
    match parse_args(cli, args)? {
        Parsed::Help => Ok(Outcome::Help),
        Parsed::Version => Ok(Outcome::Version),
        Parsed::Cmd(mut cmd) => {
            read_expr(&mut cmd, &mut std::io::empty())?;
            execute(&cmd, false, &mut Discard)
        }
    }
}

/// Fill a `--expr-json -` expression from `input`.
fn read_expr(cmd: &mut Command, input: &mut dyn BufRead) -> Result<(), String> {
    let slot = match cmd {
        Command::Query(a) if a.expr_stdin => &mut a.expr,
        Command::Client(ClientArgs {
            action:
                ClientAction::Query {
                    params,
                    expr_stdin: true,
                    ..
                },
            ..
        }) => &mut params.expr,
        _ => return Ok(()),
    };
    let mut json = String::new();
    input
        .read_to_string(&mut json)
        .map_err(|e| format!("error: cannot read --expr-json from stdin: {e}"))?;
    *slot = Some(args::parse_expr(&json)?);
    Ok(())
}

/// Parse and run `args`, writing output to `out`: matches in rg's human
/// format (a per-term counts summary under `--approx-counts` and
/// `--occurrences`), or under `--json` either rg's event stream (matches
/// mode) or the wire report.
/// `serve --stdio` reads its requests from `input`. `tty` resolves `--color
/// auto`. The seam [`run`] uses over locked stdin and stdout.
pub fn dispatch_to(
    cli: &Cli,
    args: &[String],
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    tty: bool,
) -> Result<Outcome, String> {
    let mut cmd = match parse_args(cli, args)? {
        Parsed::Help => {
            print::line(out, cli.help)?;
            return Ok(Outcome::Help);
        }
        Parsed::Version => {
            print::line(out, &format!("{NAME} {}", env!("CARGO_PKG_VERSION")))?;
            return Ok(Outcome::Version);
        }
        Parsed::Cmd(cmd) => cmd,
    };
    read_expr(&mut cmd, input)?;
    match &cmd {
        Command::Serve(a) => serve(a, input, out),
        Command::Client(a) => run_client(a, out),
        Command::Index(a) => {
            let outcome = execute(&cmd, false, &mut Discard)?;
            if let Outcome::Index(summary) = &outcome {
                print::index_summary(out, a.json, summary)?;
            }
            Ok(outcome)
        }
        Command::Query(a) if a.json && a.emits_events() => {
            let mut sink = JsonSink::new(out, a.json_config());
            let outcome = execute(&cmd, false, &mut sink)?;
            sink.finish()?;
            Ok(outcome)
        }
        Command::Query(a) if a.json => {
            let outcome = execute(&cmd, false, &mut Discard)?;
            if let Outcome::Query(report) = &outcome {
                print::query_report(out, report)?;
            }
            Ok(outcome)
        }
        // Human `--approx-counts` / `--occurrences`: no matches to print, so
        // print the report's FM numbers instead of nothing.
        Command::Query(a) if a.approx_counts || a.occurrences => {
            let outcome = execute(&cmd, true, &mut Discard)?;
            if let Outcome::Query(report) = &outcome {
                print::counts_summary(out, report)?;
            }
            Ok(outcome)
        }
        Command::Query(a) => {
            let mut writer = MatchWriter::new(out, a.config(tty));
            let outcome = execute(&cmd, true, &mut writer)?;
            writer.finish()?;
            Ok(outcome)
        }
    }
}

/// Run `cli` over the process argv and print the result.
///
/// Exit codes: 0 success, 2 bad argv / unreadable input / build refusal, 1 an
/// IO failure writing output. A closed pipe is not a failure.
pub fn run(cli: &Cli) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tty = std::io::stdout().is_terminal();
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let mut input = std::io::stdin().lock();
    let mut result = dispatch_to(cli, &args, &mut input, &mut out, tty).map(|_| ());
    // Flush on every path: a failed run must still end on a whole line.
    // A closed pipe left the writer silent; nothing else may be.
    if let Err(e) = out.flush()
        && e.kind() != std::io::ErrorKind::BrokenPipe
        && result.is_ok()
    {
        result = Err(format!("{WRITE_FAILED} output: {e}"));
    }
    if let Err(m) = result {
        eprintln!("{m}");
        std::process::exit(if m.starts_with(WRITE_FAILED) { 1 } else { 2 });
    }
}

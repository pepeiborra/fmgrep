//! argv parsing for every subcommand.

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::regex_pattern;

use super::Cli;
use super::serve::QueryParams;
use crate::events::JsonConfig;
use crate::expr::{Expr, REGEX_REFUSAL, TERMS_REFUSAL};
use crate::matching::{MatchMode, QueryOutput};
use crate::output::{ColorMode, Format, OutputConfig};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Index(IndexArgs),
    Query(QueryArgs),
    Serve(ServeArgs),
    Client(ClientArgs),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ServeArgs {
    /// Where the index lookup starts; None: the cwd.
    pub path: Option<PathBuf>,
    /// None: `--stdio`.
    pub listen: Option<SocketAddr>,
    /// `--no-watch`: a files-layout index is not watched for changes.
    pub no_watch: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ClientArgs {
    pub server: String,
    pub action: ClientAction,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClientAction {
    /// `expr_stdin`: `--expr-json -`, read into `params.expr` before sending.
    Query {
        params: QueryParams,
        json: bool,
        expr_stdin: bool,
    },
    Status,
    Shutdown,
}

#[derive(Debug, PartialEq, Eq)]
pub struct IndexArgs {
    /// Defaults to `.`.
    pub folder: PathBuf,
    pub output: Option<PathBuf>,
    /// `--store-docs`: keep a copy of the documents in `docs.shard`.
    pub store_docs: bool,
    pub json: bool,
    /// `--verbose`/`-v`: list every skipped file instead of counting them.
    pub verbose: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct QueryArgs {
    /// `--index`: where the index lookup starts; None: the cwd.
    pub index: Option<PathBuf>,
    pub regex: bool,
    /// rg `-U`: terms may match across line terminators.
    pub multiline: bool,
    /// rg `--multiline-dotall`: only read under `-U`, as in rg.
    pub dotall: bool,
    /// `-i`: every term matches case-insensitively.
    pub ignore_case: bool,
    pub json: bool,
    /// `--approx-counts`: the FM-index counts only, whatever the path.
    pub approx_counts: bool,
    /// `--occurrences`: `total_matches` only, whatever the path.
    pub occurrences: bool,
    /// The printing mode flags, last one winning. None: counts on the wire,
    /// matched lines when printing.
    pub format: Option<Format>,
    pub before: usize,
    pub after: usize,
    pub heading: bool,
    pub line_numbers: bool,
    pub color: ColorMode,
    pub terms: Vec<String>,
    /// `--expr` / `--expr-json`, or the terms under `--and` / `--or`;
    /// exclusive with `terms` and `--regex`.
    pub expr: Option<Expr>,
    /// `--expr-json -`: `expr` comes from stdin.
    pub expr_stdin: bool,
}

impl QueryArgs {
    /// A printing mode flag, which also clears `--approx-counts` and
    /// `--occurrences` so that the last mode flag on the argv wins.
    fn print_as(&mut self, format: Format) {
        self.format = Some(format);
        self.approx_counts = false;
        self.occurrences = false;
    }

    /// What the drive must produce. `--approx-counts` asks for the
    /// counts-only drive on both paths; without any mode flag a printing run
    /// wants matched lines while the wire keeps its counts-only default.
    pub fn output(&self, printing: bool) -> QueryOutput {
        if self.occurrences {
            return QueryOutput::Occurrences;
        }
        if self.approx_counts {
            return QueryOutput::Counts;
        }
        let lines = QueryOutput::Matches {
            before: self.before,
            after: self.after,
        };
        match self.format {
            Some(Format::Lines | Format::OnlyMatching) => lines,
            Some(Format::CountLines) => QueryOutput::CountLines,
            Some(Format::CountMatches) => QueryOutput::CountMatches,
            Some(Format::FilesWithMatches) => QueryOutput::FilesWithMatches,
            None if printing => lines,
            None => QueryOutput::Counts,
        }
    }

    /// Whether `--json` is rg's event stream here rather than the wire
    /// report: exactly the modes that carry lines.
    pub fn emits_events(&self) -> bool {
        matches!(self.output(false), QueryOutput::Matches { .. })
    }

    /// How every term matches.
    pub fn mode(&self) -> MatchMode {
        MatchMode {
            multiline: self.multiline,
            dotall: self.dotall,
            ignore_case: self.ignore_case,
        }
    }

    pub fn json_config(&self) -> JsonConfig {
        JsonConfig {
            line_numbers: self.line_numbers,
            multiline: self.multiline,
        }
    }

    pub fn config(&self, tty: bool) -> OutputConfig {
        OutputConfig {
            format: self.format.unwrap_or(Format::Lines),
            heading: self.heading,
            line_numbers: self.line_numbers,
            context: self.before > 0 || self.after > 0,
            multiline: self.multiline,
            color: match self.color {
                ColorMode::Always => true,
                ColorMode::Never => false,
                ColorMode::Auto => tty,
            },
            banner: self.terms.len() > 1,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Help,
    Version,
    Cmd(Command),
}

/// Parse argv (excluding argv\[0\]).
///
/// Only a first word `index` or `serve` selects a subcommand; any other argv
/// is a search, `--server ADDR` sending it to a server. No args, or `-h` /
/// `--help` anywhere before `--`, is help. Value flags take `--flag value`
/// and `--flag=value`; switches take no value. A search and `index` stop
/// flag parsing at `--`.
pub fn parse_args(cli: &Cli, args: &[String]) -> Result<Parsed, String> {
    match args.first().map(String::as_str) {
        None => Ok(Parsed::Help),
        Some("index") => parse_index_args(cli, &args[1..]),
        Some("serve") => parse_serve_args(cli, &args[1..]),
        Some(_) => parse_query_args(cli, args),
    }
}

/// Take the value of `--flag value` / `--flag=value` at `args[*i]`.
fn take_value(
    args: &[String],
    i: &mut usize,
    key: &str,
    inline: Option<&str>,
) -> Result<String, String> {
    let val = match inline {
        Some(v) => v.to_owned(),
        None => {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("error: {key} requires a value"))?
        }
    };
    if val.is_empty() {
        return Err(format!("error: {key} requires a value"));
    }
    Ok(val)
}

fn parse_index_args(cli: &Cli, args: &[String]) -> Result<Parsed, String> {
    let mut folder: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut store_docs = false;
    let mut json = false;
    let mut verbose = false;
    let mut rest_is_folder = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if rest_is_folder {
            if folder.is_some() {
                return Err(format!(
                    "error: unexpected positional argument {a:?} ({})",
                    cli.usage
                ));
            }
            folder = Some(PathBuf::from(a));
            i += 1;
            continue;
        }
        let (key, inline) = split_value(a, &["--output"]);
        match key {
            "--output" => output = Some(PathBuf::from(take_value(args, &mut i, key, inline)?)),
            "--store-docs" if inline.is_none() => store_docs = true,
            "--json" if inline.is_none() => json = true,
            "--verbose" | "-v" if inline.is_none() => verbose = true,
            "--help" | "-h" if inline.is_none() => return Ok(Parsed::Help),
            "--version" | "-V" if inline.is_none() => return Ok(Parsed::Version),
            "--" if inline.is_none() => rest_is_folder = true,
            _ if !a.starts_with('-') => {
                if folder.is_some() {
                    return Err(format!(
                        "error: unexpected positional argument {a:?} ({})",
                        cli.usage
                    ));
                }
                folder = Some(PathBuf::from(a));
            }
            _ => return Err(format!("error: unknown argument {a:?} ({})", cli.usage)),
        }
        i += 1;
    }
    let folder = folder.unwrap_or_else(|| PathBuf::from("."));
    Ok(Parsed::Cmd(Command::Index(IndexArgs {
        folder,
        output,
        store_docs,
        json,
        verbose,
    })))
}

/// Value flags of a search, plus the `-A`/`-B`/`-C` short forms, which also
/// accept an attached value (`-A3`).
const VALUE_FLAGS: [&str; 11] = [
    "--index",
    "--server",
    "--expr",
    "--expr-json",
    "--color",
    "--after-context",
    "--before-context",
    "--context",
    "-A",
    "-B",
    "-C",
];

/// Split `--flag=value` / `-A=3` / `-A3` into key and inline value.
fn split_flag(arg: &str) -> (&str, Option<&str>) {
    if let Some((k, v)) = arg.split_once('=')
        && VALUE_FLAGS.contains(&k)
    {
        return (k, Some(v));
    }
    let b = arg.as_bytes();
    if b.len() > 2 && b[0] == b'-' && matches!(b[1], b'A' | b'B' | b'C') {
        let (key, value) = arg.split_at(2);
        return (key, Some(value));
    }
    (arg, None)
}

fn parse_count(key: &str, value: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("error: invalid {key} {value:?}; expected a number"))
}

fn parse_color(value: &str) -> Result<ColorMode, String> {
    match value {
        "auto" => Ok(ColorMode::Auto),
        "always" => Ok(ColorMode::Always),
        "never" => Ok(ColorMode::Never),
        _ => Err(format!(
            "error: invalid --color {value:?}; expected auto|always|never"
        )),
    }
}

/// Flags only a local search honours; `--server` refuses them.
const LOCAL_ONLY: [&str; 22] = [
    "--index",
    "--color",
    "-A",
    "--after-context",
    "-B",
    "--before-context",
    "-C",
    "--context",
    "-n",
    "--line-number",
    "-N",
    "--no-line-number",
    "--heading",
    "--no-heading",
    "--matches",
    "-o",
    "--only-matching",
    "-c",
    "--count",
    "--count-matches",
    "-l",
    "--files-with-matches",
];

fn parse_query_args(cli: &Cli, args: &[String]) -> Result<Parsed, String> {
    let mut q = QueryArgs {
        index: None,
        regex: false,
        multiline: false,
        dotall: false,
        ignore_case: false,
        json: false,
        approx_counts: false,
        occurrences: false,
        format: None,
        before: 0,
        after: 0,
        heading: true,
        line_numbers: true,
        color: ColorMode::Auto,
        terms: Vec::new(),
        expr: None,
        expr_stdin: false,
    };
    // rg's precedence: `-A`/`-B` win over `-C` whatever the argv order.
    let (mut before, mut after, mut context) = (None, None, None);
    let mut op = None;
    let mut rest_are_terms = false;
    let (mut text, mut json) = (false, false);
    let mut server = None;
    let (mut status, mut shutdown) = (false, false);
    // The first flag `--server` would refuse.
    let mut local: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if rest_are_terms {
            q.terms.push(a.clone());
            i += 1;
            continue;
        }
        let (key, inline) = split_flag(a);
        let value = |i: &mut usize| take_value(args, i, key, inline);
        if local.is_none() && LOCAL_ONLY.contains(&key) {
            local = Some(key);
        }
        match key {
            "--index" => q.index = Some(PathBuf::from(value(&mut i)?)),
            "--server" => server = Some(value(&mut i)?),
            "--color" => q.color = parse_color(&value(&mut i)?)?,
            "-A" | "--after-context" => after = Some(parse_count(key, &value(&mut i)?)?),
            "-B" | "--before-context" => before = Some(parse_count(key, &value(&mut i)?)?),
            "-C" | "--context" => context = Some(parse_count(key, &value(&mut i)?)?),
            "--expr" => (q.expr, text) = (Some(expr_text(&value(&mut i)?)?), true),
            "--expr-json" => {
                (q.expr, q.expr_stdin) = expr_json(&value(&mut i)?)?;
                json = true;
            }
            "--regex" => q.regex = true,
            "-U" | "--multiline" => q.multiline = true,
            "--multiline-dotall" => q.dotall = true,
            "-i" | "--ignore-case" => q.ignore_case = true,
            "--and" => op_flag(&mut op, "--and", cli)?,
            "--or" => op_flag(&mut op, "--or", cli)?,
            "--json" => q.json = true,
            "-n" | "--line-number" => q.line_numbers = true,
            "-N" | "--no-line-number" => q.line_numbers = false,
            "--heading" => q.heading = true,
            "--no-heading" => q.heading = false,
            "--matches" => q.print_as(Format::Lines),
            "-o" | "--only-matching" => q.print_as(Format::OnlyMatching),
            "-c" | "--count" => q.print_as(Format::CountLines),
            "--count-matches" => q.print_as(Format::CountMatches),
            "-l" | "--files-with-matches" => q.print_as(Format::FilesWithMatches),
            "--approx-counts" => (q.approx_counts, q.occurrences, q.format) = (true, false, None),
            "--occurrences" => (q.approx_counts, q.occurrences, q.format) = (false, true, None),
            "--status" => status = true,
            "--shutdown" => shutdown = true,
            "--help" | "-h" => return Ok(Parsed::Help),
            "--version" | "-V" => return Ok(Parsed::Version),
            "--" => rest_are_terms = true,
            _ if !a.starts_with('-') => q.terms.push(a.clone()),
            _ => return Err(format!("error: unknown argument {a:?} ({})", cli.usage)),
        }
        i += 1;
    }
    let Some(server) = server else {
        if status || shutdown {
            return Err(format!(
                "error: --status and --shutdown need --server ADDR ({})",
                cli.usage
            ));
        }
        q.before = before.or(context).unwrap_or(0);
        q.after = after.or(context).unwrap_or(0);
        settle_expr(
            cli,
            op,
            (text, json),
            &mut q.expr,
            q.expr_stdin,
            &mut q.terms,
            &mut q.regex,
        )?;
        return Ok(Parsed::Cmd(Command::Query(q)));
    };
    if let Some(flag) = local {
        return Err(format!(
            "error: {flag} needs a local index; --server answers counts only ({})",
            cli.usage
        ));
    }
    let action = server_action(cli, q, op, (text, json), (status, shutdown))?;
    Ok(Parsed::Cmd(Command::Client(ClientArgs { server, action })))
}

/// The request a `--server` search sends.
fn server_action(
    cli: &Cli,
    mut q: QueryArgs,
    op: Option<&str>,
    given: (bool, bool),
    manage: (bool, bool),
) -> Result<ClientAction, String> {
    let expr = q.expr.is_some() || q.expr_stdin;
    match manage {
        (false, false) => {
            settle_expr(
                cli,
                op,
                given,
                &mut q.expr,
                q.expr_stdin,
                &mut q.terms,
                &mut q.regex,
            )?;
            Ok(ClientAction::Query {
                params: QueryParams {
                    terms: q.terms,
                    expr: q.expr,
                    regex: q.regex,
                    ignore_case: q.ignore_case,
                    multiline: q.multiline,
                    dotall: q.dotall,
                    approx_counts: true,
                    occurrences: q.occurrences,
                },
                json: q.json,
                expr_stdin: q.expr_stdin,
            })
        }
        (true, true) => Err(format!(
            "error: --status and --shutdown are exclusive ({})",
            cli.usage
        )),
        _ if !q.terms.is_empty() => Err(format!(
            "error: --status and --shutdown take no terms ({})",
            cli.usage
        )),
        _ if expr => {
            let flag = if given.0 { "--expr" } else { "--expr-json" };
            Err(format!(
                "error: --status and --shutdown take no {flag} ({})",
                cli.usage
            ))
        }
        _ if op.is_some() => Err(format!(
            "error: --status and --shutdown take no --and/--or ({})",
            cli.usage
        )),
        (true, false) => Ok(ClientAction::Status),
        (false, true) => Ok(ClientAction::Shutdown),
    }
}

/// `--expr-json` JSON, or `-` for stdin: `(None, true)`.
fn expr_json(value: &str) -> Result<(Option<Expr>, bool), String> {
    match value {
        "-" => Ok((None, true)),
        json => Ok((Some(parse_expr(json)?), false)),
    }
}

fn expr_text(src: &str) -> Result<Expr, String> {
    src.parse::<Expr>()
        .map_err(|e| format!("error: invalid --expr: {}", e.render(src)))
}

/// An `--expr-json` document, checked.
pub fn parse_expr(json: &str) -> Result<Expr, String> {
    let bad = |e: String| format!("error: invalid --expr-json: {e}");
    let expr: Expr = serde_json::from_str(json).map_err(|e| bad(e.to_string()))?;
    expr.check().map_err(bad)?;
    Ok(expr)
}

/// Refuses `--and` with `--or`.
fn op_flag(slot: &mut Option<&'static str>, key: &'static str, cli: &Cli) -> Result<(), String> {
    match slot.replace(key) {
        Some(other) if other != key => Err(format!(
            "error: --and and --or are exclusive ({})",
            cli.usage
        )),
        _ => Ok(()),
    }
}

/// Folds the terms under `op` into `expr`, each leaf spelled as the term
/// would be; then refuses an expression given terms or `--regex`. `given`:
/// whether `--expr` and `--expr-json` were.
fn settle_expr(
    cli: &Cli,
    op: Option<&str>,
    given: (bool, bool),
    expr: &mut Option<Expr>,
    stdin: bool,
    terms: &mut Vec<String>,
    regex: &mut bool,
) -> Result<(), String> {
    let (text, json) = given;
    if text && json {
        return Err(format!(
            "error: --expr and --expr-json are exclusive ({})",
            cli.usage
        ));
    }
    let any = expr.is_some() || stdin;
    let Some(op) = op else {
        return if any {
            expr_exclusive(cli, !terms.is_empty(), *regex)
        } else {
            Ok(())
        };
    };
    if any {
        let flag = if text { "--expr" } else { "--expr-json" };
        return Err(format!(
            "error: {op} and {flag} are exclusive ({})",
            cli.usage
        ));
    }
    if terms.len() < 2 {
        return Err(format!(
            "error: {op} needs at least two terms ({})",
            cli.usage
        ));
    }
    let leaf = |t: String| match (*regex, regex_pattern(&t)) {
        (true, _) => Expr::Regex(t),
        (false, Some(inner)) => Expr::Regex(inner.to_owned()),
        (false, None) => Expr::Term(t),
    };
    let leaves = std::mem::take(terms).into_iter().map(leaf).collect();
    *expr = Some(if op == "--and" {
        Expr::And(leaves)
    } else {
        Expr::Or(leaves)
    });
    *regex = false;
    Ok(())
}

fn expr_exclusive(cli: &Cli, terms: bool, regex: bool) -> Result<(), String> {
    match (terms, regex) {
        (true, _) => Err(format!("error: {TERMS_REFUSAL} ({})", cli.usage)),
        (_, true) => Err(format!("error: {REGEX_REFUSAL} ({})", cli.usage)),
        _ => Ok(()),
    }
}

/// Split `--flag=value` for the value flags in `keys`.
fn split_value<'a>(arg: &'a str, keys: &[&str]) -> (&'a str, Option<&'a str>) {
    match arg.split_once('=') {
        Some((k, v)) if keys.contains(&k) => (k, Some(v)),
        _ => (arg, None),
    }
}

fn parse_serve_args(cli: &Cli, args: &[String]) -> Result<Parsed, String> {
    let mut path: Option<PathBuf> = None;
    let mut listen: Option<SocketAddr> = None;
    let mut stdio = false;
    let mut no_watch = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let (key, inline) = split_value(a, &["--listen"]);
        match key {
            "--listen" => {
                let v = take_value(args, &mut i, key, inline)?;
                listen = Some(
                    v.parse()
                        .map_err(|_| format!("error: invalid --listen {v:?}; expected IP:PORT"))?,
                );
            }
            "--stdio" => stdio = true,
            "--no-watch" => no_watch = true,
            "--help" | "-h" => return Ok(Parsed::Help),
            "--version" | "-V" => return Ok(Parsed::Version),
            _ if !a.starts_with('-') && path.is_none() => path = Some(PathBuf::from(a)),
            _ if !a.starts_with('-') => {
                return Err(format!(
                    "error: unexpected positional argument {a:?} ({})",
                    cli.usage
                ));
            }
            _ => return Err(format!("error: unknown argument {a:?} ({})", cli.usage)),
        }
        i += 1;
    }
    let listen = match (listen, stdio) {
        (Some(_), true) => {
            return Err(format!(
                "error: --listen and --stdio are exclusive ({})",
                cli.usage
            ));
        }
        (_, true) => None,
        (addr, false) => Some(addr.unwrap_or(SocketAddr::from(([127, 0, 0, 1], 0)))),
    };
    Ok(Parsed::Cmd(Command::Serve(ServeArgs {
        path,
        listen,
        no_watch,
    })))
}

#[cfg(test)]
mod tests;

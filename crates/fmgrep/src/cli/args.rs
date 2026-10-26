//! The command line.

use std::io::BufRead;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::builder::NonEmptyStringValueParser;
use clap::{Args, ColorChoice, Parser, Subcommand};

use super::params::{Color, Context, Mode, QueryParams};
use crate::expr::Expr;
use crate::regex_pattern;

const SEARCH_NOTE: &str = "\
A search reads the .fmgrep of the nearest enclosing directory. Put -- first
to search for a TERM named index or serve. Trailing TERMs naming files or
directories are PATHs, as in rg; with --expr every argument is one.";

const ENV: &str = "\
Environment:
  FMGREP_SHARD_BYTES    index shard size (default 100MiB)
  FMGREP_VERBOSE=1      as -v";

/// The mode flags: the last one given wins.
const MODES: [&str; 6] = [
    "count",
    "count_matches",
    "files_with_matches",
    "only_matching",
    "matches",
    "approx_counts",
];

#[derive(Parser)]
#[command(
    bin_name = "fmgrep",
    version,
    about = "ripgrep-style search over a persistent FM-index",
    after_help = SEARCH_NOTE,
    no_binary_name = true,
    propagate_version = true,
    disable_help_subcommand = true,
    args_conflicts_with_subcommands = true,
    arg_required_else_help = true,
    args_override_self = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    #[command(flatten)]
    pub search: Search,
}

#[derive(Subcommand)]
pub enum Command {
    /// Index PATH into PATH/.fmgrep
    #[command(display_name = "fmgrep", after_help = ENV)]
    Index(IndexArgs),
    /// Answer searches over JSON-RPC
    #[command(display_name = "fmgrep")]
    Serve(ServeArgs),
}

#[derive(Args)]
pub struct IndexArgs {
    /// The tree to index
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Write the index to DIR
    #[arg(long, value_name = "DIR")]
    pub output: Option<PathBuf>,
    /// Keep a copy of the files; searches never read them
    #[arg(long)]
    pub store_docs: bool,
    /// Print the summary as JSON
    #[arg(long)]
    pub json: bool,
    /// List skipped binary files
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Args)]
pub struct ServeArgs {
    /// Serve the index at or above PATH
    pub path: Option<PathBuf>,
    /// TCP address, printed once up
    #[arg(
        long,
        value_name = "ADDR",
        default_value = "127.0.0.1:0",
        conflicts_with = "stdio"
    )]
    pub listen: SocketAddr,
    /// Requests on stdin, responses on stdout
    #[arg(long)]
    pub stdio: bool,
    /// Do not watch the tree for changed or new files
    #[arg(long)]
    pub no_watch: bool,
}

#[derive(Args)]
pub struct Search {
    /// A literal, or /REGEX/
    #[arg(value_name = "TERM")]
    pub terms: Vec<String>,
    /// Read every TERM as a regex
    #[arg(long)]
    pub regex: bool,
    /// Match case-insensitively
    #[arg(short, long)]
    pub ignore_case: bool,
    /// Let a match span lines
    #[arg(short = 'U', long)]
    pub multiline: bool,
    /// Under -U, let . match a line terminator
    #[arg(long)]
    pub multiline_dotall: bool,
    /// Documents holding every TERM
    #[arg(long, conflicts_with = "or")]
    pub and: bool,
    /// Documents holding any TERM
    #[arg(long)]
    pub or: bool,
    /// Documents an expression picks: 'a AND (b OR /c+/)'
    #[arg(
        long,
        value_name = "EXPR",
        conflicts_with_all = ["regex", "and", "or", "expr_json"]
    )]
    pub expr: Option<String>,
    /// Use the index at or above PATH
    #[arg(long, value_name = "PATH", conflicts_with = "server")]
    pub index: Option<PathBuf>,

    /// Include or exclude (!GLOB) paths; the last match wins
    #[arg(
        short = 'g',
        long = "glob",
        value_name = "GLOB",
        help_heading = "Filter"
    )]
    pub glob: Vec<String>,
    /// As -g, ignoring case
    #[arg(long, value_name = "GLOB", help_heading = "Filter")]
    pub iglob: Vec<String>,
    /// Make every -g ignore case
    #[arg(long, help_heading = "Filter")]
    pub glob_case_insensitive: bool,

    /// Search through a running fmgrep serve
    #[arg(
        long,
        value_name = "ADDR",
        value_parser = NonEmptyStringValueParser::new(),
        help_heading = "Server"
    )]
    pub server: Option<String>,
    /// Report on the server
    #[arg(
        long,
        requires = "server",
        conflicts_with_all = ["shutdown", "terms", "expr", "expr_json", "and", "or"],
        help_heading = "Server"
    )]
    pub status: bool,
    /// Stop the server
    #[arg(
        long,
        requires = "server",
        conflicts_with_all = ["terms", "expr", "expr_json", "and", "or"],
        help_heading = "Server"
    )]
    pub shutdown: bool,

    /// Count matching lines per file
    #[arg(short, long, overrides_with_all = MODES, help_heading = "Output")]
    pub count: bool,
    /// Count matches per file
    #[arg(long, overrides_with_all = MODES, help_heading = "Output")]
    pub count_matches: bool,
    /// List matching files
    #[arg(short = 'l', long, overrides_with_all = MODES, help_heading = "Output")]
    pub files_with_matches: bool,
    /// Print only the matched parts
    #[arg(short, long, overrides_with_all = MODES, help_heading = "Output")]
    pub only_matching: bool,
    /// Lines of context after each match
    #[arg(short = 'A', long, value_name = "NUM", help_heading = "Output")]
    pub after_context: Option<usize>,
    /// Lines of context before each match
    #[arg(short = 'B', long, value_name = "NUM", help_heading = "Output")]
    pub before_context: Option<usize>,
    /// Lines of context around each match
    #[arg(short = 'C', long, value_name = "NUM", help_heading = "Output")]
    pub context: Option<usize>,
    /// Show line numbers (default)
    #[arg(
        short = 'n',
        long,
        overrides_with = "no_line_number",
        help_heading = "Output"
    )]
    pub line_number: bool,
    /// Hide line numbers
    #[arg(
        short = 'N',
        long,
        overrides_with = "line_number",
        help_heading = "Output"
    )]
    pub no_line_number: bool,
    /// Group matches per file (default)
    #[arg(long, overrides_with = "no_heading", help_heading = "Output")]
    pub heading: bool,
    /// One match per line, prefixed by its path
    #[arg(long, overrides_with = "heading", help_heading = "Output")]
    pub no_heading: bool,
    /// auto (default), always or never
    #[arg(
        long,
        value_name = "WHEN",
        default_value = "auto",
        hide_default_value = true,
        hide_possible_values = true,
        help_heading = "Output"
    )]
    pub color: ColorChoice,

    /// rg's JSON events, or a counts report
    #[arg(long, help_heading = "Protocol (unstable)")]
    pub json: bool,
    /// Print matched lines (the default without --json)
    #[arg(long, overrides_with_all = MODES, help_heading = "Protocol (unstable)")]
    pub matches: bool,
    /// The index's own counts per TERM
    #[arg(long, overrides_with_all = MODES, help_heading = "Protocol (unstable)")]
    pub approx_counts: bool,
    /// The expression as JSON; - reads stdin
    #[arg(
        long,
        value_name = "JSON",
        conflicts_with_all = ["regex", "and", "or"],
        help_heading = "Protocol (unstable)"
    )]
    pub expr_json: Option<String>,
}

impl Search {
    /// The search as a request. `printing`: with no mode flag, matched lines
    /// (counts under `--json` or when false).
    pub fn params(
        &self,
        input: &mut dyn BufRead,
        printing: bool,
        tty: bool,
    ) -> Result<QueryParams, String> {
        let mode = match () {
            _ if self.approx_counts => Mode::Counts,
            _ if self.count => Mode::Count,
            _ if self.count_matches => Mode::CountMatches,
            _ if self.files_with_matches => Mode::Files,
            _ if self.json && (self.matches || self.only_matching) => Mode::JsonMatches,
            _ if self.only_matching => Mode::Only,
            _ if self.matches || (printing && !self.json) => Mode::Lines,
            _ => Mode::Counts,
        };
        let (terms, paths) = self.split()?;
        let mut p = QueryParams {
            terms,
            expr: self.expr(input)?,
            regex: self.regex,
            ignore_case: self.ignore_case,
            multiline: self.multiline,
            dotall: self.multiline_dotall,
            mode,
            globs: self.glob.clone(),
            iglobs: self.iglob.clone(),
            glob_case_insensitive: self.glob_case_insensitive,
            paths,
            ..QueryParams::default()
        };
        p.filter()?;
        if self.and || self.or {
            let op = if self.and { "--and" } else { "--or" };
            if p.terms.len() < 2 {
                return Err(format!("error: {op} needs at least two terms"));
            }
            let leaf = |t: String| match (p.regex, regex_pattern(&t)) {
                (true, _) => Expr::Regex(t),
                (false, Some(inner)) => Expr::Regex(inner.to_owned()),
                (false, None) => Expr::Term(t),
            };
            let leaves = std::mem::take(&mut p.terms).into_iter().map(leaf).collect();
            p.expr = Some(if self.and {
                Expr::And(leaves)
            } else {
                Expr::Or(leaves)
            });
            p.regex = false;
        }
        if mode != Mode::Counts {
            p.context = Context {
                before: self.before_context.or(self.context).unwrap_or(0),
                after: self.after_context.or(self.context).unwrap_or(0),
            };
            p.line_numbers = self.line_number || !self.no_line_number;
            p.heading = self.heading || !self.no_heading;
            p.color = match (self.color, tty) {
                (ColorChoice::Always, _) | (ColorChoice::Auto, true) => Color::Always,
                _ => Color::Never,
            };
        }
        Ok(p)
    }

    /// The positionals as TERMs and PATHs: under an expression all are
    /// PATHs, else the trailing ones after the first that name files or
    /// directories and are not spelled /REGEX/.
    fn split(&self) -> Result<(Vec<String>, Vec<String>), String> {
        if self.expr.is_some() || self.expr_json.is_some() {
            if let Some(t) = self.terms.iter().find(|t| !Path::new(t).exists()) {
                return Err(format!("error: {t}: No such file or directory"));
            }
            return Ok((Vec::new(), self.terms.clone()));
        }
        let n = (self.terms.iter().skip(1).rev())
            .take_while(|t| regex_pattern(t).is_none() && Path::new(t).exists())
            .count();
        let (terms, paths) = self.terms.split_at(self.terms.len() - n);
        Ok((terms.to_vec(), paths.to_vec()))
    }

    fn expr(&self, input: &mut dyn BufRead) -> Result<Option<Expr>, String> {
        if let Some(src) = &self.expr {
            let expr = src.parse::<Expr>();
            return expr
                .map(Some)
                .map_err(|e| format!("error: invalid --expr: {}", e.render(src)));
        }
        let Some(json) = &self.expr_json else {
            return Ok(None);
        };
        let mut read = String::new();
        let json = if json == "-" {
            input
                .read_to_string(&mut read)
                .map_err(|e| format!("error: cannot read --expr-json from stdin: {e}"))?;
            &read
        } else {
            json
        };
        let bad = |e: String| format!("error: invalid --expr-json: {e}");
        let expr: Expr = serde_json::from_str(json).map_err(|e| bad(e.to_string()))?;
        expr.check().map_err(bad)?;
        Ok(Some(expr))
    }
}

#[cfg(test)]
mod tests;

//! The command line.

use std::io::BufRead;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::builder::NonEmptyStringValueParser;
use clap::{Args, ColorChoice, Parser, Subcommand};

use super::params::{Color, Context, Mode, QueryParams};
use crate::expr::Expr;
use crate::pattern::{plain_literal, smart_case_folds};

const SEARCH_NOTE: &str = "\
A search reads the .fmgrep of the nearest enclosing directory. The first
argument is the PATTERN, a regex as in rg; -F makes it a literal. Every
other argument is a PATH, and one that does not exist is an error, as in
rg. -e gives the PATTERNs instead, and then every argument is a PATH. Put
-- first to search for a PATTERN named index, serve or status. With --expr
every argument is a PATH, a word is a literal and /x/ is a regex.
Without a PATH only the cwd's subtree is searched, as in rg; from outside the
indexed tree, all of it.

Environment:
  FMGREP_SERVER=HOST:PORT  with no --server, --index or enclosing .fmgrep,
                           search through that server";

/// Why an index cannot honour rg's `flag`, which asks for what it does not
/// hold, as the error of that flag; None for any other flag.
pub fn unindexable(flag: &str) -> Option<String> {
    let name = flag.split('=').next().unwrap_or(flag);
    let reason = match name {
        "--hidden" | "-." => "hidden files are not indexed",
        "-u" | "--unrestricted" => "hidden and ignored files are not indexed",
        "--no-ignore"
        | "--no-ignore-vcs"
        | "--no-ignore-dot"
        | "--no-ignore-parent"
        | "--no-ignore-global"
        | "--no-ignore-exclude"
        | "--no-ignore-files" => "ignored files are not indexed",
        "-L" | "--follow" => "symlinks are not indexed",
        "-a" | "--text" | "--binary" => "binary files are not indexed",
        "-z" | "--search-zip" => "files are indexed as they are on disk, not decompressed",
        "--pre" | "--pre-glob" => "files are indexed as they are on disk, not preprocessed",
        "-E" | "--encoding" => "files are indexed as bytes, and fmgrep decodes none",
        _ => return None,
    };
    Some(format!(
        "error: {name} cannot work on an index: {reason}; use rg"
    ))
}

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
    override_usage = "fmgrep [OPTIONS] PATTERN [PATH]...\n       fmgrep [OPTIONS] -e PATTERN... [PATH]...\n       fmgrep <COMMAND>",
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
    /// Say which index or server a search in PATH uses, and what changed
    #[command(display_name = "fmgrep")]
    Status(StatusArgs),
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
    /// With --store-docs: fail, and leave no index, unless it records its
    /// served commit
    #[arg(long)]
    pub require_served_commit: bool,
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
    /// Serve a tree that does not change: check it once at start, ask
    /// watchman nothing
    #[arg(long)]
    pub no_watch: bool,
}

#[derive(Args)]
pub struct StatusArgs {
    /// Report on what a search started in PATH uses
    pub path: Option<PathBuf>,
    /// Print the manifest, or the server's reply, as JSON
    #[arg(long)]
    pub json: bool,
    /// Report on the server at ADDR, as a search's --server
    #[arg(long, value_name = "ADDR", value_parser = NonEmptyStringValueParser::new())]
    pub server: Option<String>,
}

#[derive(Args)]
pub struct Search {
    /// A regex, as in rg, then the PATHs to search
    #[arg(value_name = "PATTERN|PATH")]
    pub positionals: Vec<String>,
    /// A PATTERN; every -e is one, and a line matching any is printed
    #[arg(
        short = 'e',
        long = "regexp",
        value_name = "PATTERN",
        allow_hyphen_values = true,
        conflicts_with_all = ["expr", "expr_json"]
    )]
    pub patterns: Vec<String>,
    /// Read every PATTERN as a literal
    #[arg(
        short = 'F',
        long,
        conflicts_with_all = ["regex", "expr", "expr_json"]
    )]
    pub fixed_strings: bool,
    /// Accepted for v0.1.0: every PATTERN is a regex now
    #[arg(long, hide = true)]
    pub regex: bool,
    /// Match case-insensitively
    #[arg(short, long, overrides_with_all = ["case_sensitive", "smart_case"])]
    pub ignore_case: bool,
    /// Match case-sensitively (the default)
    #[arg(short = 's', long, overrides_with_all = ["ignore_case", "smart_case"])]
    pub case_sensitive: bool,
    /// Ignore case unless a PATTERN holds an uppercase letter
    #[arg(short = 'S', long, overrides_with_all = ["ignore_case", "case_sensitive"])]
    pub smart_case: bool,
    /// Let a match span lines
    #[arg(short = 'U', long)]
    pub multiline: bool,
    /// Under -U, let . match a line terminator
    #[arg(long)]
    pub multiline_dotall: bool,
    /// Documents holding every -e PATTERN
    #[arg(long, conflicts_with = "or")]
    pub and: bool,
    /// Documents holding any -e PATTERN
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
    /// Stop the server (--server only)
    #[arg(
        long,
        conflicts_with_all = ["index", "positionals", "patterns", "expr", "expr_json", "and", "or"],
        help_heading = "Server"
    )]
    pub shutdown: bool,
    /// Print the server's answer alone: no look for local changes, no warning
    #[arg(
        long,
        conflicts_with_all = ["index", "shutdown"],
        help_heading = "Server"
    )]
    pub ignore_local_changes: bool,

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
    /// Show line numbers (default on a terminal)
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
    /// Group matches per file (default on a terminal)
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
    /// Print rg's statistics after the matched lines
    #[arg(
        long,
        conflicts_with_all = ["count", "count_matches", "files_with_matches"],
        help_heading = "Output"
    )]
    pub stats: bool,

    /// rg's JSON events, or a counts report
    #[arg(long, help_heading = "Protocol (unstable)")]
    pub json: bool,
    /// Print matched lines (the default without --json)
    #[arg(long, overrides_with_all = MODES, help_heading = "Protocol (unstable)")]
    pub matches: bool,
    /// The index's own counts per PATTERN
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
        let (terms, paths) = self.split();
        let mut p = QueryParams {
            terms,
            expr: self.expr(input)?,
            regex: !self.fixed_strings && self.expr.is_none() && self.expr_json.is_none(),
            ignore_case: self.ignore_case,
            multiline: self.multiline,
            dotall: self.multiline_dotall,
            mode,
            globs: self.glob.clone(),
            iglobs: self.iglob.clone(),
            glob_case_insensitive: self.glob_case_insensitive,
            filename: !matches!(&paths[..], [one] if !Path::new(one).is_dir()),
            ..QueryParams::default()
        };
        p.filter()?;
        if self.smart_case {
            p.ignore_case = smart_case_folds(self.sources(&p).iter().map(String::as_str));
        }
        // Several -e patterns are one search of any of them, as in rg.
        if self.and || self.or || p.terms.len() > 1 {
            let op = if self.and { "--and" } else { "--or" };
            if p.terms.len() < 2 {
                return Err(format!(
                    "error: {op} needs at least two patterns, each given with -e: fmgrep {op} -e A -e B"
                ));
            }
            // A pattern that is nothing but a literal is a term, as the
            // server plans it: the report names it as typed.
            // The empty pattern matches every line; the term of it, none.
            let leaf = |t: String| match (self.fixed_strings, plain_literal(&t)) {
                (true, _) if !t.is_empty() => Expr::Term(t),
                (false, Some(literal)) => Expr::Term(literal),
                (_, _) => Expr::Regex(t),
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
            p.line_numbers =
                self.line_number || (!self.no_line_number && (tty || mode == Mode::JsonMatches));
            p.heading = self.heading || (tty && !self.no_heading);
            p.color = match (self.color, tty) {
                (ColorChoice::Always, _) | (ColorChoice::Auto, true) => Color::Always,
                _ => Color::Never,
            };
            // -c, --count-matches and -l refuse --stats; --json and
            // --approx-counts print what they print without it.
            p.stats = self.stats && matches!(mode, Mode::Lines | Mode::Only);
        }
        Ok(p)
    }

    /// The patterns the search reads, as regexes: the TERMs, the leaves of an
    /// expression, a literal escaped.
    fn sources(&self, p: &QueryParams) -> Vec<String> {
        let escaped = |t: &String| match p.regex {
            true => t.clone(),
            false => regex::escape(t),
        };
        let mut sources: Vec<String> = p.terms.iter().map(escaped).collect();
        for leaf in p.expr.iter().flat_map(Expr::leaves) {
            match leaf {
                Expr::Term(t) => sources.push(regex::escape(t)),
                Expr::Regex(r) => sources.push(r.clone()),
                Expr::And(_) | Expr::Or(_) => {}
            }
        }
        sources
    }

    /// The PATHs, as typed, those that name no file too.
    pub fn paths(&self) -> Vec<String> {
        self.split().1
    }

    /// The PATTERNs, as typed; none under an expression.
    pub fn typed_terms(&self) -> Vec<String> {
        self.split().0
    }

    /// The positionals as PATTERNs and PATHs, as rg reads them. With -e, or
    /// under an expression, every positional is a PATH. Else the first is
    /// the PATTERN, whatever it is, and the rest are PATHs.
    fn split(&self) -> (Vec<String>, Vec<String>) {
        if self.expr.is_some() || self.expr_json.is_some() || !self.patterns.is_empty() {
            return (self.patterns.clone(), self.positionals.clone());
        }
        match self.positionals.split_first() {
            Some((pattern, paths)) => (vec![pattern.clone()], paths.to_vec()),
            None => (Vec::new(), Vec::new()),
        }
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

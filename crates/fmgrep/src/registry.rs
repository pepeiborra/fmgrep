use crate::Cli;

const USAGE: &str = "usage: fmgrep [OPTIONS] TERM... | fmgrep index [PATH] | fmgrep serve [PATH]; see fmgrep --help";

const HELP: &str = "fmgrep - ripgrep-style search over a persistent FM-index

Usage:
  fmgrep [OPTIONS] TERM...        search; a TERM is a literal, /../ a regex
  fmgrep [OPTIONS] --expr EXPR    documents an AND/OR expression picks
  fmgrep index [PATH] [OPTIONS]   index PATH (default .) into PATH/.fmgrep
  fmgrep serve [PATH] [OPTIONS]   answer searches over JSON-RPC
  fmgrep -h | -V                  this help / the version
A search reads the .fmgrep of the nearest enclosing directory. Put -- first
to search for a TERM named index or serve.

Search:
  --regex                   read every TERM as a regex
  -i, --ignore-case         match case-insensitively
  -U, --multiline           let a match span lines
  --multiline-dotall        under -U, let . match a line terminator
  --and, --or               documents holding every / any TERM
  --expr EXPR               e.g. 'HscEnv AND (DynFlags OR /Dyn[A-Z]\\w+/)'
  --index PATH              use the index at or above PATH
  --server ADDR             ask a running fmgrep serve (counts only)
Output (as in rg):
  -c, --count               count matching lines per file
  --count-matches           count matches per file
  -l, --files-with-matches  list matching files
  -o, --only-matching       print only the matched parts
  -A, -B, -C NUM            lines of context after / before / around
  -n, -N                    show (default) / hide line numbers
  --heading, --no-heading   group matches per file (default) or not
  --color WHEN              auto (default), always or never
Index:
  --store-docs              keep a copy of the files; searches never read them
  --output DIR              write the index to DIR
  -v, --verbose             list skipped binary files
Serve:
  --listen ADDR             TCP address (default 127.0.0.1:0), printed once up
  --stdio                   requests on stdin, responses on stdout
  --no-watch                do not watch the tree for changed files
  --status, --shutdown      with --server: report on or stop the server
Harness and protocol (unstable):
  --json                    rg's JSON events, or a counts report
  --matches                 print matched lines (the default without --json)
  --approx-counts           the index's own counts per TERM
  --occurrences             occurrence totals per TERM
  --expr-json JSON|-        the expression as JSON; - reads stdin
Environment:
  FMGREP_SHARD_BYTES        index shard size (default 100MiB)
  FMGREP_BUILD_THREADS      shards built at once
  FMGREP_VERBOSE=1          as index -v
Unlike rg, a search with no match exits 0.";

pub fn cli_spec() -> Cli {
    Cli {
        usage: USAGE,
        help: HELP,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_for(words: &[&str]) -> String {
        let argv: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        match crate::dispatch(&cli_spec(), &argv) {
            Err(e) => e,
            Ok(_) => panic!("argv {words:?} must be rejected"),
        }
    }

    #[test]
    fn rejects_bad_argv_against_the_real_spec() {
        // Parsed: the refusal is the missing folder's, not the flag's.
        assert!(err_for(&["index", "no/such/dir", "--store-docs"]).contains("cannot list folder"));
        for words in [
            vec!["index", "c", "--bogus"],
            vec!["index", "c", "--store-docs=x"],
            vec!["--bogus"],
            vec!["serve", "--listen", "127.0.0.1:0", "--stdio"],
            vec!["--status"],
            vec!["--server", "a:1", "-l", "x"],
            vec!["--and", "x"],
        ] {
            assert!(err_for(&words).contains(USAGE), "argv {words:?}");
        }
    }
}

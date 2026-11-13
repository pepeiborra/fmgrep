# fmgrep

fmgrep is ripgrep over a persistent index with efficient support for AND/OR expressions.
`fmgrep index` builds an FM-index of a source tree once; after that a search prints what `rg` would
print, but without scanning the whole tree.  The index is mapped from disk and used in place.

## Status

fmgrep is experimental software:  
* the index format, the serve protocol and the `--json` output can change in any release.
* the `.fmgrep` directory holds the indexed text unencrypted: treat it as a copy of the source.  
* `fmgrep serve` has no authentication and listens on 127.0.0.1 by default ([SECURITY.md](SECURITY.md)). 
* Linux is the measured platform; macOS works, Windows is unsupported. 

fmgrep is provided as is, under the MIT license, without warranty.

## Install

```sh
cargo install fmgrep
```

or, from a checkout, `cargo install --path crates/fmgrep`. It needs Rust
1.98 or later.

## Dependencies

To build fmgrep you need only Rust (above). To run a search you need no
other program: fmgrep does not call `rg`.

[Watchman](https://facebook.github.io/watchman/), the file watcher, is a
soft dependency of a search: a search works without it, with the same
answers. `fmgrep serve` needs it, unless given `--no-watch` or a
`--store-docs` index ([Usage](#usage)).

* **What fmgrep uses it for.** A search must notice the files changed
  since the index was built. On a tree of 50,000 files or more, it asks
  watchman which files changed, instead of checking every file. `fmgrep
  index` and a search of such a tree start the watchman daemon when none
  runs, in CI and in containers too, and ask it to watch the tree. The
  daemon keeps running after fmgrep exits, with its watch: on AOSP
  (1.25 M files), about 710 MB of memory and 312,000 inotify watches. A
  search keeps a small record per tree in `~/.cache/fmgrep`. Below 50,000
  files a search never contacts watchman: checking every file is faster.
  `fmgrep serve` checks before every query, not once, so it asks watchman
  at any tree size, unless given `--no-watch`: it starts the daemon and
  asks it to watch even a small tree. On Linux that makes each served
  query 1.1 to 3.2 ms slower: a query that took 0.4 ms takes 2.1 ms. On
  macOS the ask costs each served query about 38 ms (on a tree of 60,000
  files, where a query with `--no-watch` took 0.17 ms). Details:
  [docs/index-format.md](docs/index-format.md#watchman).
* **Without watchman**, each search of such a tree checks every file
  itself and prints one warning line, `warning: watchman cannot say what
  changed (cannot run watchman (...); install watchman); checked every
  file`. The answers are the same; the search is slower. Whole search for
  an identifier found in 8 to 19 files, p50 of 15, files in the page
  cache, on an AWS i4i.4xlarge (Ubuntu 24.04, watchman v2026.11.09,
  November 2026):

  | search | Linux kernel, 96k files | Chromium, 461k | AOSP, 1.25 M |
  |---|--:|--:|--:|
  | asks watchman | 41 ms | 141 ms | 574 ms |
  | checks every file | 114 ms | 182 ms | 1.57 s |

  The second row is the first search after an index: it asks watchman
  for its clock, a few milliseconds, then checks every file. A search
  without watchman checks every file the same way. `fmgrep serve`
  without watchman exits 2 at its start, and says why, unless given
  `--no-watch` or a `--store-docs` index.
* To keep fmgrep away from watchman, set `WATCHMAN_SOCK` to a path where
  no daemon listens: fmgrep then starts no daemon, and each search checks
  every file and prints a warning that gives the socket
  ([docs/tuning.md](docs/tuning.md#search)). fmgrep no longer reads
  `FMGREP_JOURNAL`, which fmgrep 0.1.0 used for this. `fmgrep serve
  --no-watch` contacts no watchman at any tree size; without `--no-watch`,
  a server refuses to start with such a socket.

## Usage

Index a tree once, at its root:

```console
$ fmgrep index .
skipped 70 non-text file(s) (binary or NUL); set FMGREP_VERBOSE=1 or pass --verbose to list them
indexed 26787 files (88781005 bytes) -> ./.fmgrep in 1997 ms
```

The index lives in `./.fmgrep`; add it to `.gitignore`. Search from
anywhere below the root, as with `rg`:

```console
$ fmgrep initHscEnv
compiler/GHC.hs
615:    initHscEnv mb_top_dir

compiler/GHC/Driver/Main.hs
46:    , initHscEnv

compiler/GHC/Driver/Main/Hsc.hs
17:    , initHscEnv
142:initHscEnv :: Maybe FilePath -> IO HscEnv
143:initHscEnv mb_top_dir = do
```

* The nearest enclosing `.fmgrep` index is used.
* Without a PATH only the cwd's subtree is searched, and paths print and
  `-g` globs match relative to the cwd, as in rg; from outside the indexed
  tree the whole tree is searched, relative to its root. PATH arguments are
  rg's: relative to the cwd, and their files print as rg prints them.
* Common rg flags work (`-i`, `-l`, `-c`, `-o`, `-A/-B/-C`, `-U`, `-g`, `--iglob`, PATH arguments); `fmgrep --help` lists them.

For many searches in a row, keep the index loaded in a server:

```console
$ fmgrep serve --listen 127.0.0.1:7777 &
127.0.0.1:7777
$ fmgrep --server 127.0.0.1:7777 -l initHscEnv
compiler/GHC.hs
compiler/GHC/Driver/Main.hs
compiler/GHC/Driver/Main/Hsc.hs
$ fmgrep --server 127.0.0.1:7777 --shutdown
```

`--server` takes every search flag but `--index` and prints what the local
search would; the server answers ([details](docs/protocol.md#--server)).
`FMGREP_SERVER=127.0.0.1:7777` does the same for every search that finds
no `.fmgrep` by walking up from the cwd. An index found wins, and the
search says so on stderr with a line that starts `warning: Using local
index`.
Before each query the server asks [watchman](https://facebook.github.io/watchman/)
what changed in the tree, so it needs watchman installed; when no daemon
runs, watchman's client starts one. To serve a tree that does not change
(a fixed commit), or without watchman, pass `--no-watch`. A
`--store-docs` index never reads the tree, so serving it never needs
watchman.

## Expressions

```sh
fmgrep HscEnv                     # a TERM is a literal: 'a.b' is three bytes
fmgrep '/Dyn[A-Z]\w+/'            # /../ is a regex
fmgrep --regex 'Dyn[A-Z]\w+'      # --regex makes every TERM a regex
fmgrep --and HscEnv DynFlags      # files holding every TERM
fmgrep --or HscEnv DynFlags       # files holding any TERM
fmgrep --expr 'HscEnv AND (DynFlags OR /Dyn[A-Z]\w+/)'
```

`AND` and `OR` are capitals, `AND` binds tighter, and quoting is as in sh.
They pick files; fmgrep then prints the lines of every TERM in those files.

## Indexing

```sh
fmgrep index .              # index the current folder
```

`index` takes the files `rg --files` lists (`.gitignore`, `.ignore` and
`.rgignore` apply; hidden files and symlinks are skipped), minus binary
files. A search never reads a file the index skips: where rg searches a
hidden or ignored file named as a PATH or matched by a `-g` glob, fmgrep
finds nothing.

A search reads every file it prints, and a regex every candidate. By
default it opens them in the tree, so answers stay current: edited and new
files are read in full; after many edits, remove `.fmgrep` and re-index.

| fmgrep / rg | linux | chromium | aosp |
|---|--:|--:|--:|
| index size | 1.11 GB / - | 2.22 GB / - | 12.00 GB / - |
| files read per literal (mean) | 798 / all 95926 | 14219 / all 460698 | 1239 / all 1253798 |
| literal p50 | 4.09 / 114 ms | 24.0 / 510 ms | 9.89 / 1560 ms |
| regex p50 | 9.06 / 116 ms | 30.2 / 496 ms | 15.0 / 1580 ms |

aosp is Android's platform source: 17.4 GB of text in 1.25 M files from
about a thousand repositories. AWS i4i.4xlarge, Linux; method and full
tables in [BENCHMARKS.md](BENCHMARKS.md).
[scripts/ec2-bench.sh](scripts/ec2-bench.sh) runs the benchmark on such an
instance ([docs/benchmark.md](docs/benchmark.md#ec2)).

## How it works

The goal of a regexp search engine is to avoid scanning a regexp over all the documents in the source tree, which is infeasible for very large corpuses. To achieve this, most modern regexp search engines build an inverted index of posting lists keyed by n-grams. Instead, fmgrep builds an [FM-index](https://en.wikipedia.org/wiki/FM-index), a kind of compressed suffix array; this idea was originally coined and pursued by Nathan Hawkes (@nhawkes) at Meta. 

An FM-index provides two main operations: 

* `locate`: returns the posting list of a pattern in `O(ocurrences)` time,
* `count`: how many times the pattern occurs in the tree in `O(pattern)` time,

`locate` is faster than scanning when the number of ocurrences is smaller than the number of documents, which we can find out with `count`; if not, we fall back to a regexp scan of all the candidates. `count` is also useful for planning AND and OR queries, e.g. start from the least common side for ANDs.

A count-augmented n-gram index provides the same two operations at similar cost, with one crucial difference: patterns need to be broken in n-grams. With an FM index this is not the case for literals and most regular expressions, and therefore a match is never a false positive, which means that:
 * we end up scanning far fewer documents than the n-gram based approach 
 * we never need to intersect posting lists (other than for AND queries)
 * `-l` literal queries do not scan any documents at all
 * the index is never lowercased to support `-i` queries
 * in a client/server setup, the server could never scan any documents at all, and instead tell the client what to scan.

 The tradeoff is a larger index (1.5X to 2X) and a higher RSS.


## Links

- [AGENTS.md](AGENTS.md): using fmgrep from a coding agent
- [docs/protocol.md](docs/protocol.md): the serve protocol and `--json`
  report (unstable)
- [BENCHMARKS.md](BENCHMARKS.md): method and full results
- [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md)

## License

MIT; see [LICENSE](LICENSE).

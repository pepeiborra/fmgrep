# fmgrep

fmgrep is ripgrep over a persistent index with efficient support for AND/OR expressions.

`rg` reads every file of the tree on every search. With the files in the page cache, that takes a
median 503 ms on the Chromium repo: not too bad for one search, but a lot for a coding agent that
runs hundreds. fmgrep indexes the tree once, allowing it to resolve queries much faster following
the same format and conventions as `rg`.

An `fmgrep` server answers a query in the Chromium repo in a median 12.8 ms, 39X faster than `rg`,
16 times faster than [tgrep](https://github.com/microsoft/tgrep), and 4.6 times faster than
[elgrep](https://github.com/eliosai/sngram). With no server, the median query takes 38 ms. The index
takes 35 s to build and is 0.78X the size of the repo (medians over all queries; method and every
row in [BENCHMARKS.md](BENCHMARKS.md)).

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

To run a search you need nothing beyond fmgrep. It does not call `rg`.
Two programs are optional.

* [Watchman](https://facebook.github.io/watchman/) is a file watcher.
  On a tree of 50,000 files or more, a search asks it which files
  changed since the index. Without it, such a search checks every file
  and prints a warning. The answers are the same. On the Linux kernel,
  the search takes 76 ms instead of 7.5 ms. `fmgrep serve` needs
  watchman, unless given `--no-watch` or a `--store-docs` index. See
  [docs/watchman.md](docs/watchman.md).
* fmgrep needs `git` only for a shared server of a git checkout.
  `fmgrep index --store-docs` runs it to learn which commit it indexed.
  A search from a checkout runs it to find the local changes. Without
  git, the index records no commit. A client without git prints the
  server's answer without its own local changes, and warns. See
  [docs/shared-server.md](docs/shared-server.md).

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
* Common rg flags work (`-i`, `-l`, `-c`, `-o`, `-A/-B/-C`, `-U`, `-g`, `--iglob`, `--stats`, PATH arguments); `fmgrep --help` lists them.
* `--stats` prints rg's statistics after the matched lines of each TERM.
  Files and bytes searched count the indexed files that the PATHs, globs
  and cwd keep; a narrowed search also counts their UTF-8 byte-order marks,
  which rg leaves out. The total time runs from the start of the query, not
  of the process. `-c`, `--count-matches` and `-l` refuse `--stats`; with
  `--json` it prints nothing more.
* In colour, the matches of a file's first 1,000 matched lines are
  coloured, as rg colours them; past those, lines print with their path
  and line number in colour and their matches plain. `-o` colours every
  match. Under `-U`, adjacent matched lines print as one block, and a
  block that starts in the first 1,000 matched lines is coloured whole.
* `index`, `serve` and `status` are commands when they are the first word.
  To search for one of these words, put `--` or any flag first:
  `fmgrep -- status`, `fmgrep -n status`.

To see which index or server a search here uses, what that index holds,
and what changed since it was built:

```console
$ fmgrep status
a search here uses the local index /home/me/ghc/.fmgrep
format: 9
store_docs: false
num_docs: 26787
num_bytes: 88781005
bom_bytes: 6
index_size_bytes: 76771936
build_time_us: 1997204
index_started_ns: 1763061773123456789
shards: 1
corpus_root: /home/me/ghc
changes: fewer than 50000 files: checked every file
since the index: 2 stale, 1 added, 0 gone
ignore files changed since the index: none
record: none saved
```

Status runs the check of what changed that a search runs, as a search
runs it, and prints what that check did, in the search's words: on a
tree of 50,000 files or more it may take a watchman token, start
watchman, ask it to watch the tree and save the record of the search
([watchman](docs/index-format.md#watchman)); where watchman cannot say,
it checks every file. The lines a search warns about it print once, on
stdout. Status does not load the index's shards, so it does not find a
damaged shard; a search does.

`fmgrep status --json` prints the manifest as one JSON line, with the file
list ([fields](docs/index-format.md#manifestbin)). `fmgrep status` chooses
as a search does: a local index found by walking up wins over
`FMGREP_SERVER`, and the first line gives the note the search warns.
Where a server answers, status prints its reply (`--json`: as one JSON
line). `fmgrep status --server ADDR` takes `--server` as a search does.

`fmgrep index --store-docs` of the top of a git checkout records the
served commit, or says in a note why it cannot
([Dependencies](#dependencies)); the manifest keeps that reason, and
`fmgrep status` of the index prints it as `why_no_commit`. With
`--require-served-commit` it fails with that reason instead, exits 2, and
leaves no index. A reason that git alone can tell, such as an edited file,
fails before the index reads a file. Build a server's index with it when
every client must be able to search its own changes.

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

### A shared server for a team's checkouts

One machine can serve a large git tree to a whole team: it builds the
index of a clean checkout with `fmgrep index --store-docs` and runs
`fmgrep serve`. Each developer sets `FMGREP_SERVER` and searches from
their own checkout; the client searches the files that differ from the
served commit itself and takes the rest from the server.
[docs/shared-server.md](docs/shared-server.md) gives the setup, the
warning that local changes are ignored, and the limits.

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
| literal p50 | 4.08 / 114 ms | 24.9 / 494 ms | 10.6 / 1560 ms |
| regex p50 | 8.23 / 118 ms | 30.1 / 500 ms | 13.2 / 1580 ms |

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
- [docs/watchman.md](docs/watchman.md): what fmgrep asks of watchman, and
  what a search costs without it
- [docs/shared-server.md](docs/shared-server.md): a shared server, searched
  from each developer's checkout
- [docs/protocol.md](docs/protocol.md): the serve protocol and `--json`
  report (unstable)
- [BENCHMARKS.md](BENCHMARKS.md): method and full results
- [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md)

## License

MIT; see [LICENSE](LICENSE).

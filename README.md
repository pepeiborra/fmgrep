# fmgrep

fmgrep is ripgrep over a persistent index with efficient support for AND/OR expressions.

`rg` scans every file of the tree on every search. With the files in the page cache, that takes a
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

Index a tree once, at its root. Then search from anywhere below it, and
the nearest `.fmgrep` index is used. Add `.fmgrep` to `.gitignore`.

```console
$ fmgrep index .
skipped 70 non-text file(s) (binary or NUL); set FMGREP_VERBOSE=1 or pass --verbose to list them
indexed 26787 files (88781005 bytes) -> ./.fmgrep in 1997 ms
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

`fmgrep` behaves like `rg`, with the same output and exit status. These
are the differences.

* **It needs an index, and searches only what the index holds.** It
  never finds a hidden, ignored, symlinked or binary file, even when you
  name it as a PATH or match it with `-g`. A PATH outside the indexed
  tree is an error. A search also finds matches in a file edited or
  added since the index.
* **A TERM is a literal, not a regex.** `'a.b'` is three bytes. Write
  `/a.b/` for a regex, or pass `--regex` to make every TERM one. Several
  TERMs print one `== TERM ==` section each. `--and`, `--or` and
  `--expr` combine TERMs to choose the files, then print the lines of
  every TERM ([Expressions](#expressions)).
* **A word that names no file is a TERM, not a PATH.**
  `fmgrep HscEnv nope.txt` also searches for `nope.txt`. `rg` fails with
  exit 2.
* **Fewer flags.** These work: `-i`, `-U`, `-g`, `--iglob`, `-c`, `-l`,
  `-o`, `-A`, `-B`, `-C`, `-n`, `-N`, `--heading`, `--color` and
  `--stats`. Other flags of `rg`, such as `-F`, `-w`, `-v`, `-e`, `-t`,
  `-m`, `--hidden`, `--no-ignore` and `--sort`, fail with exit 2.
  `fmgrep --help` lists the flags that work.
* **It does not search stdin or read a `RIPGREP_CONFIG_PATH` file.**
* **`--json` prints a counts report.** `--json --matches` prints the
  events of `rg`.
* **`index`, `serve` and `status` are commands** when they are the first
  word. Put `--` first to search for one of them.

[docs/usage.md](docs/usage.md) gives each difference with the commands
that show it. It also covers `fmgrep status`, `--stats` and colour.
`status` says which index or server a search uses, and what changed
since.

To search many times in a row, keep the index loaded in a server.

```console
$ fmgrep serve --listen 127.0.0.1:7777 &
127.0.0.1:7777
$ fmgrep --server 127.0.0.1:7777 -l initHscEnv
compiler/GHC.hs
compiler/GHC/Driver/Main.hs
compiler/GHC/Driver/Main/Hsc.hs
$ fmgrep --server 127.0.0.1:7777 --shutdown
```

With `FMGREP_SERVER=127.0.0.1:7777` set, every search that finds no
`.fmgrep` index goes to that server. To share one server of a git
checkout with a team, index it with `fmgrep index --store-docs`. See
[docs/shared-server.md](docs/shared-server.md).

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
files.

A search scans every file it prints, and a regex every candidate. By
default it opens them in the tree, so answers stay current: edited and new
files are scanned in full; after many edits, remove `.fmgrep` and re-index.

| fmgrep / rg | linux | chromium | aosp |
|---|--:|--:|--:|
| index size | 1.11 GB / - | 2.22 GB / - | 12.00 GB / - |
| files scanned per literal (mean) | 798 / all 95926 | 14219 / all 460698 | 1239 / all 1253798 |
| literal p50 | 4.08 / 114 ms | 24.9 / 494 ms | 10.6 / 1560 ms |
| regex p50 | 8.23 / 118 ms | 30.1 / 500 ms | 13.2 / 1580 ms |

aosp is Android's platform source: 17.4 GB of text in 1.25 M files from
about a thousand repositories. AWS i4i.4xlarge, Linux; method and full
tables in [BENCHMARKS.md](BENCHMARKS.md).
[scripts/ec2-bench.sh](scripts/ec2-bench.sh) runs the benchmark on such an
instance ([docs/benchmark.md](docs/benchmark.md#ec2)).

## How it works

An indexed search engine avoids scanning a regex over every file of a
large tree. Most build an inverted index of posting lists keyed
by n-grams. fmgrep builds an
[FM-index](https://en.wikipedia.org/wiki/FM-index) instead. This idea was
originally coined and pursued by Nathan Hawkes (@nhawkes) at Meta. 
fmgrep keeps the Burrows-Wheeler transform of the tree, with the id of the 
file at sampled rows ([crates/docfm](crates/docfm/README.md)). The
index is mapped from disk and used in place, so a search touches only
the pages it needs.

An FM-index provides two main operations.

* `count` says how many times a pattern occurs in the tree. It takes
  time proportional to the length of the pattern, whatever the size of
  the tree.
* `locate` says which files hold the pattern. It takes time proportional
  to the number of occurrences of the pattern in the tree. 

fmgrep answers a literal with `locate`, which also gives its count in
each file. An AND `count`s each side and `locate`s the rarest, 
It then either `locate`s or scans each other side, whichever costs less. 
When a PATH or a glob narrows a literal search, fmgrep scans those files if
that costs less than locating.

To answer a regex from the index, fmgrep takes the longest literal that every
match holds, or walks the regex over the index as an automaton. Either
gives candidate files, which are then scanned: in the worst case, a regex
that gives the index nothing to narrow makes every file a candidate.

By comparison, an n-gram index must break a pattern into n-grams. The files it 
finds hold the n-grams, not necessarily the pattern, so it scans each one to
confirm. An FM-index looks up a literal whole, so every file it finds
holds the literal. That has three consequences.

* A literal search scans only the files it prints lines from, and `-l`
  scans none. The exception is a file edited since the index, which is
  scanned in full.
* There are no posting lists to intersect, except for the sides of an AND.
* The index is never lowercased to support `-i`. It holds the text as
  written, and a `-i` term walks its case variants through the index.
  The case variants are those of Unicode simple case folding.

## Links

- [AGENTS.md](AGENTS.md): using fmgrep from a coding agent
- [docs/usage.md](docs/usage.md): how a search differs from rg, and what
  `status` and `serve` do
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

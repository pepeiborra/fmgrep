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
1.98 or later and a C compiler (for the bundled libsais), nothing else.

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

A search reads every file it prints, and a regex every candidate. By
default it opens them in the tree, so answers stay current: edited and new
files are read in full; after many edits, remove `.fmgrep` and re-index.

| fmgrep / rg | linux | chromium |
|---|--:|--:|
| index size | 2.35 GB / - | 4.61 GB / - |
| files read per literal (mean) | 798 / all 95553 | 14219 / all 460957 |
| literal p50 | 5.25 / 132 ms | 37.9 / 571 ms |
| regex p50 | 14.8 / 133 ms | 39.7 / 567 ms |

AWS i4i.4xlarge, Linux; method and full tables in [BENCHMARKS.md](BENCHMARKS.md).

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

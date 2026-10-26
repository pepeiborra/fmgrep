# fmgrep

fmgrep is ripgrep over a persistent index. `fmgrep index` builds an
FM-index of a source tree once; after that a search prints what `rg` would
print, but typically answers in milliseconds, without scanning the whole tree.
The index is mapped from disk and used in place.

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
* Paths are relative to the indexed root.
* Common rg flags work (`-i`, `-l`, `-c`, `-o`, `-A/-B/-C`, `-U`); `fmgrep --help` lists them.

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
fmgrep index . --store-docs # also keep a copy of every file
```

A search reads every file it prints, and a regex every candidate. By
default it opens them in the tree, so answers stay current: edited and new
files are read in full; after many edits, remove `.fmgrep` and re-index.
`--store-docs` reads them from the copy instead, for about 1.8 times the
disk: it never reads the tree and answers as of indexing time. Details:
[docs/index-format.md](docs/index-format.md#staleness).

| | GHC | linux |
|---|--:|--:|
| index size, default / `--store-docs` | 113 / 197 MB | 1.89 / 3.42 GB |
| files read per literal (mean) | 122 | 798 |
| literal p50, default / `--store-docs` | 4.7 / 4.3 ms | 10.1 / 7.0 ms |
| regex p50, default / `--store-docs` | 9.3 / 8.6 ms | 15.4 / 10.0 ms |

Apple M5, median of repeated warm queries (the
Performance table below is one spawn-to-exit pass). A file costs an
`open(2)` of about 5.5 µs by default and a `pread` of 1.4-2 µs with
`--store-docs`, so the gap grows with the files read; it is several times
larger when the tree is out of the page cache or the filesystem is busy. A
lower `FMGREP_SAMPLE_RATE` speeds the index probe, not the reads
([docs/tuning.md](docs/tuning.md)).

## Performance

Apple M5 (10 cores, 16 GiB), macOS 26.5.1, fmgrep 0.1.0, ripgrep 15.2.0,
2026-09-24 (chromium: 1 run, loaded box). Latency is the per-query
median, spawn to exit, of one `fmgrep --server` or `rg` process; footprint
is the peak anonymous memory while answering. [BENCHMARKS.md](BENCHMARKS.md)
has the method, every column and the index built with `--store-docs`.

| corpus | files / text | index build / size | fmgrep literal / regex | rg literal / regex | footprint fmgrep / rg |
|---|--:|--:|--:|--:|--:|
| GHC | 26787 / 85 MB | 1.7 s / 113 MB | 2.8 / 9.4 ms | 448 / 447 ms | 13 / 78 MB |
| linux | 95540 / 1.5 GB | 13 s / 1.9 GB | 4.5 / 5.1 ms | 2.0 / 2.3 s | 22 / 446 MB |
| chromium | 460957 / 2.8 GB | 72 s / 3.7 GB | 64 ms / 2.6 s | 12.6 / 13.9 s | 63 / 789 MB |

Regexes cost more on chromium because every candidate file is read from
the tree.

## How it works

`fmgrep index` concatenates the text files of the tree and builds one
FM-index per 100 MiB shard with [docfm](crates/docfm), a byte-alphabet
FM-index stored as a single blob. It keeps each file's size and mtime,
not its text (`--store-docs` keeps a copy of the text too). A search maps
the blob and uses it in place: backward search counts a literal and locates
each occurrence's file without touching the tree.

A regex is narrowed by the index first, through the literals every match
must contain or a walk of its reversed DFA over the index, and the regex
crate then verifies only the candidate files, read from the tree. Details:
[docs/index-format.md](docs/index-format.md).

## Environment

| variable | default | effect |
|---|---|---|
| `FMGREP_SHARD_BYTES` | `100MB` | text per index shard |
| `FMGREP_VERBOSE` | off | as `index -v`: list skipped binary files |

Developer knobs are in [docs/tuning.md](docs/tuning.md).

## Links

- [AGENTS.md](AGENTS.md): using fmgrep from a coding agent
- [docs/protocol.md](docs/protocol.md): the serve protocol and `--json`
  report (unstable)
- [BENCHMARKS.md](BENCHMARKS.md): method and full results
- [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md)

## License

MIT; see [LICENSE](LICENSE).

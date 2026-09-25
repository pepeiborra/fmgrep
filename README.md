# fmgrep

fmgrep is ripgrep over a persistent index. `fmgrep index` builds an
FM-index of a source tree once; after that a search prints what `rg` would
print, but typically answers in milliseconds, with tens of MB of memory,
instead of scanning the whole tree. The index is mapped from disk and used
in place rather than loaded.

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
indexed 26787 files (88781005 bytes) -> ./.fmgrep (fmgrep) in 1997 ms
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

The nearest enclosing `.fmgrep` is used, found as git finds `.git`, and
paths are relative to the indexed root. The common rg flags work (`-i`,
`-l`, `-c`, `-o`, `-A/-B/-C`, `-U`); `fmgrep --help` lists them.
Unlike rg, a search with no match exits 0.

For many searches in a row, keep the index loaded in a server:

```console
$ fmgrep serve --listen 127.0.0.1:7777 &
127.0.0.1:7777
$ fmgrep --server 127.0.0.1:7777 HscEnv '/Dyn[A-Z]\w+/'
HscEnv: 605 matches in 85 files
/Dyn[A-Z]\w+/: 2279 matches in 345 files
$ fmgrep --server 127.0.0.1:7777 --shutdown
```

`--server` answers counts only; search locally for lines.

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

## Staleness

An edited file is read in full by every search, so answers stay current;
after many edits, remove `.fmgrep` and re-index. A deleted file is skipped
with a warning, new files are invisible until the re-index, and an
`index --store-docs` index answers as of index time.

## Performance

Apple M5 (10 cores, 16 GiB), macOS 26.5.1, fmgrep 0.1.0, ripgrep 15.2.0,
2026-09-24; median of 3 runs of `scripts/bench.sh` (chromium: 1 run, on a
loaded box). Latency is the per-query median, spawn to exit: one
`fmgrep --server` process against a running `fmgrep serve`, or one `rg`
scanning the tree. Footprint is the peak anonymous memory while answering.

| corpus | files / text | index build / size | fmgrep literal / regex | rg literal / regex | footprint fmgrep / rg |
|---|--:|--:|--:|--:|--:|
| GHC | 26787 / 85 MB | 1.7 s / 113 MB | 2.8 / 9.4 ms | 448 / 447 ms | 13 / 78 MB |
| linux | 95540 / 1.5 GB | 13 s / 1.9 GB | 4.5 / 5.1 ms | 2.0 / 2.3 s | 22 / 446 MB |
| chromium | 460957 / 2.8 GB | 72 s / 3.7 GB | 64 ms / 2.6 s | 12.6 / 13.9 s | 63 / 789 MB |

Regexes cost more on chromium because every candidate file is read from
the tree. [BENCHMARKS.md](BENCHMARKS.md) has the method, every column and
the index built with `--store-docs`.

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
[docs/index-format.md](docs/index-format.md) and
[docs/design](docs/design/decisions.md).

## Environment

| variable | default | effect |
|---|---|---|
| `FMGREP_SHARD_BYTES` | `100MB` | text per index shard |
| `FMGREP_BUILD_THREADS` | machine parallelism, capped by memory | shards built at once |
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

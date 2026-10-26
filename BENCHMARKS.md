# Benchmarks

fmgrep against ripgrep on three pinned source trees, on one machine.

## Method

Every run is `scripts/bench.sh` ([docs/benchmark.md](docs/benchmark.md)).
Every row searches the same staged copy of the corpus, one query at a time,
and prints each matching line as `path:line:text`; a run fails unless all
rows print the same lines, by file and line number, for every query they
answer. A query `A AND B` is an intersection: the lines matching `A` or `B`
in the files that hold both.

- **rg**: one `rg -n --no-heading` process per query; an intersection is
  one pipe, `rg -l --null A | xargs -0 -r rg -l --null B |
  xargs -0 -r rg -n --no-heading -e A -e B`.
- **fmgrep**: an index (`index build`, `index size`), one `fmgrep serve`
  (`ready`), then one `fmgrep --server ADDR -n --no-heading` process per
  query (`--and A B` for an intersection).
- **tgrep**, when installed: `tgrep index`, one `tgrep serve --no-watch`,
  then one `tgrep -n --no-heading` client per query; an intersection is one
  `tgrep -l` per side, then `tgrep -n --no-heading -e A -e B --type-add
  and:FILE ... -t and` over the files on every list (`-g` and path arguments
  would bypass the index). This is Microsoft's tgrep, built
  from [github.com/microsoft/tgrep](https://github.com/microsoft/tgrep)
  (`cargo install --locked --git https://github.com/microsoft/tgrep
  tgrep-cli`); the crates.io crate named `tgrep` is an unrelated tool, so do
  not `cargo install tgrep`. See [docs/benchmark.md](docs/benchmark.md#tgrep-optional).

Latency is spawn to exit, process start-up and printing included. `p50` is
the lower median over the literal, the regex or the intersection queries,
beside their mean; `all` is over every query. `query footprint` is the
peak anonymous memory of the process answering (the server, or each rg);
`index peak RSS` is the index build's peak resident memory. `load1` is the
1-minute load average at the row's start and end (the harness marks 4 or
more with `!`). Each cell is the median of 3 runs (chromium: 1 run). MB
and GB are 2^20 and 2^30 bytes.

Each run was started at load1 below 3 with no other build or benchmark
running, except where noted; rg keeps every core busy, so the rows after it
start at a load1 of 4 to 18. The linux checkout sits on a case-insensitive
file system, where 13 files of the pinned tree collide; every row sees the
same 95540 files.

Machine: Apple M5, 10 cores (4P + 6E), 16 GiB, macOS 26.5.1. fmgrep 0.1.0
(release build), ripgrep 15.2.0. Measured 2026-10-05, UTC.

## GHC

26787 files, 84.67 MB, at ghc `4cf7628`; 40 queries (30 literal, 5 regex,
5 intersection).

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 479 ms | 516 ms | 476 ms | 521 ms | 507 ms | 499 ms | 479 ms | 514 ms | 45.58 MB | - | 3.08->6.54, 2.99->4.64, 2.62->5.50 |
| fmgrep | 1.98 s | 112.66 MB | 26.8 ms | 4.37 ms | 5.35 ms | 10.3 ms | 9.79 ms | 3.03 ms | 3.29 ms | 4.37 ms | 5.65 ms | 26.06 MB | 686.09 MB | 6.54->7.38, 4.64->4.64, 5.50->5.50 |
| fmgrep `--store-docs` | 2.16 s | 197.35 MB | 6.38 ms | 3.74 ms | 3.89 ms | 7.67 ms | 6.90 ms | 3.26 ms | 3.33 ms | 3.78 ms | 4.20 ms | 29.06 MB | 700.23 MB | 7.38->7.67, 4.64->4.75, 5.50->5.14 |

Printing a line reads its file from the tree, so the slowest fmgrep
queries are those that touch the most files, such as
`/\{-# INLINE \w+ #-\}/` (3102 lines in 426 files, 14.8 ms). With
`--store-docs` no query takes more than 9.2 ms. An intersection costs rg a pass over the whole tree for its
first side, then one over the files left per further side.

## linux

95540 files, 1.53 GB, at linux `5878583`; 107 queries (92 literal, 10
regex, 5 intersection). Runs 1, 3 and 4; run 2 is left out because builds
in other checkouts started during it and took load1 to 40.

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 2.85 s | 2.80 s | 3.13 s | 3.13 s | 4.08 s | 4.04 s | 3.01 s | 2.89 s | 208.89 MB | - | 3.88->13.19, 2.90->6.51, 6.15->10.70 |
| fmgrep | 26.93 s | 1.89 GB | 107 ms | 16.5 ms | 28.8 ms | 22.5 ms | 35.4 ms | 17.9 ms | 19.4 ms | 17.2 ms | 29.0 ms | 67.30 MB | 4.84 GB | 13.19->17.35, 6.51->7.95, 10.70->15.19 |
| fmgrep `--store-docs` | 25.05 s | 3.42 GB | 21.8 ms | 7.78 ms | 17.2 ms | 8.90 ms | 12.4 ms | 12.9 ms | 12.4 ms | 8.17 ms | 16.5 ms | 64.44 MB | 4.33 GB | 17.35->17.35, 7.95->11.42, 15.19->14.58 |

The first queries meet a cold page cache: `netdev_err`, the first, took
164 ms (3710 lines in 519 files), and `atomic_set`, the third, 223 ms
(3946 lines in 1499 files). An intersection costs rg three passes, the
first over the whole tree.

## chromium

460957 files, 2.84 GB, at chromium `e2c5a77`; 40 queries (30 literal, 5
regex, 5 intersection). One run, started at load1 2.50.

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 14.78 s | 14.56 s | 15.18 s | 15.27 s | 17.08 s | 17.03 s | 14.91 s | 14.96 s | 688.31 MB | - | 3.45->5.92 |
| fmgrep | 71.83 s | 3.73 GB | 2.81 s | 141 ms | 408 ms | 408 ms | 709 ms | 7.59 ms | 8.97 ms | 92.6 ms | 396 ms | 480.50 MB | 4.53 GB | 5.92->6.41 |
| fmgrep `--store-docs` | 72.65 s | 6.57 GB | 40.0 ms | 40.3 ms | 147 ms | 101 ms | 110 ms | 7.70 ms | 7.50 ms | 34.1 ms | 125 ms | 481.58 MB | 3.75 GB | 6.41->7.53 |

Here the files read from the tree dominate. `ready` is the stat of every
file before the first answer. The first query, `base::BindOnce` (55162
lines in 14079 files), took 1.32 s against a cold cache. The regexes
verify every candidate file: `/#include "base/\w+\.h"/` (78449 lines in
46770 files) took 1.90 s, against rg's 15.54 s; with `--store-docs`,
225 ms. An intersection touches few files, so it costs fmgrep under 13 ms
and rg over 16 s.

## tgrep

[tgrep](https://github.com/microsoft/tgrep), a trigram-index grep, is an
optional row ([Method](#method); install:
[docs/benchmark.md](docs/benchmark.md)). Same machine, tgrep 1.0.8 (commit
`b1d0fc2`). The fmgrep rows are from the same runs.

GHC, three runs on 2026-10-05 (UTC):

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| fmgrep | 2.18 s | 112.66 MB | 26.1 ms | 5.62 ms | 6.78 ms | 12.3 ms | 11.9 ms | 3.77 ms | 4.17 ms | 5.49 ms | 6.97 ms | 29.08 MB | 695.92 MB | 3.21->3.28, 3.18->3.33, 2.77->3.03 |
| tgrep | 1.37 s | 87.61 MB | 172 ms | 7.94 ms | 8.39 ms | 13.4 ms | 22.7 ms | 134 ms | 97.5 ms | 8.85 ms | 21.2 ms | 138.02 MB | 128.80 MB | 3.28->3.28, 3.33->3.33, 3.03->3.03 |

An intersection with a common side, such as `import AND zonkTcType`
(161 ms), costs tgrep a `-l` over every file holding that side.

linux, the runs above:

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| fmgrep | 26.93 s | 1.89 GB | 107 ms | 16.5 ms | 28.8 ms | 22.5 ms | 35.4 ms | 17.9 ms | 19.4 ms | 17.2 ms | 29.0 ms | 67.30 MB | 4.84 GB | 13.19->17.35, 6.51->7.95, 10.70->15.19 |
| tgrep | 15.44 s | 1.02 GB | 756 ms | 29.1 ms | 46.9 ms | 35.0 ms | 74.0 ms | - | - | - | - | 1.25 GB | 273.59 MB | 17.35->14.15, 11.42->10.37, 14.58->11.81 |

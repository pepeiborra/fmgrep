# Benchmarks

fmgrep against ripgrep and tgrep on two pinned source trees, on a Linux
server.
## Method

Every run is `scripts/bench.sh` ([docs/benchmark.md](docs/benchmark.md)).
Every row searches the same staged copy of the corpus, one query at a time,
and prints each matching line as `path:line:text`; a run fails unless all
rows print the same lines, by file and line number, for every query they
answer.

- **rg**: one `rg -n --no-heading` process per query.
- **fmgrep**: an index (`index build`, `index size`), one `fmgrep serve`
  (`ready`: spawn to its readiness line, the load and its
  [check of the tree](docs/index-format.md#staleness) included), then one
  `fmgrep --server ADDR -n --no-heading` process per query.
- **fmgrep `--store-docs`**: the same over `index --store-docs`, which keeps
  a copy of every file: a printed line is read from the copy, and `serve`
  does not check the tree.
- **tgrep**, when installed: `tgrep index`, one `tgrep serve --no-watch`
  (`ready`: until it reports the tree reconciled), then one
  `tgrep -n --no-heading` client per query. This is Microsoft's tgrep 1.0.8
  (commit `b1d0fc2`), built
  from [github.com/microsoft/tgrep](https://github.com/microsoft/tgrep)
  (`cargo install --locked --git https://github.com/microsoft/tgrep
  tgrep-cli`). See [docs/benchmark.md](docs/benchmark.md#tgrep-optional).

Latency is spawn to exit, process start-up and printing included. `p50` is
the lower median over the literal, the regex or the intersection queries,
beside their mean; `all` is over every query. `query footprint` is the
peak anonymous memory of the process answering (the server, or each rg);
`query RSS` is its peak resident set, file-backed pages such as the mapped
index included; `index peak RSS` is the index build's peak resident
memory. `load1` is the 1-minute load average at the row's start and end
(the harness marks 4 or more with `!`). MB and GB are 2^20 and 2^30
bytes.

| corpus | commit | files | text | queries (literal / regex / intersection) |
|---|---|--:|--:|--:|
| linux | `5878583` | 95553 | 1.53 GB | 107 (92 / 10 / 5) |
| chromium | `e2c5a77` | 460957 | 2.84 GB | 40 (30 / 5 / 5) |


## Intersections

A query `A AND B` prints the lines matching `A` or `B` in the files that
hold both.

- **fmgrep**: one `fmgrep --server ADDR -n --no-heading --and A B`
  request; the index gives each side's files and intersects them before any
  file is read.
- **rg**: one pipe, `rg -l --null A | xargs -0 -r rg -l --null B |
  xargs -0 -r rg -n --no-heading -e A -e B`: a pass over the whole tree for
  the first side, then one over the files left per further side.
- **tgrep**: one `tgrep -l` per side, then `tgrep -n --no-heading -e A -e B
  --type-add and:FILE ... -t and` over the files on every list (`-g` and
  path arguments would bypass its index).

### Hardware and version numbers

AWS i4i.4xlarge spot instance (16 vCPUs, Intel Xeon Platinum 8375C at 2.90
GHz, 123 GiB, local NVMe formatted ext4), Ubuntu 24.04, kernel 7.0. fmgrep
0.1.0 (release build), ripgrep 15.2.0, tgrep 1.0.8. One run per corpus,
measured 2026-10-23 (UTC).

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 128 ms | 128 ms | 128 ms | 130 ms | 247 ms | 257 ms | 128 ms | 134 ms | 40.09 MB | 43.85 MB | - | 10.03->9.31! |
| fmgrep | 11.50 s | 2.18 GB | 199 ms | 4.74 ms | 5.94 ms | 7.63 ms | 10.4 ms | 17.6 ms | 17.5 ms | 5.04 ms | 6.89 ms | 50.24 MB | 1.92 GB | 8.16 GB | 9.31->10.61! |
| fmgrep `--store-docs` | 11.50 s | 3.71 GB | 18.5 ms | 4.10 ms | 4.90 ms | 6.15 ms | 7.93 ms | 16.7 ms | 16.8 ms | 4.30 ms | 5.74 ms | 57.48 MB | 1.93 GB | 9.33 GB | 10.61->11.44! |
| tgrep | 16.32 s | 1.02 GB | 1.86 s | 18.5 ms | 29.9 ms | 37.7 ms | 35.5 ms | 901 ms | 747 ms | 20.4 ms | 64.0 ms | 2.07 GB | 2.22 GB | 189.32 MB | 11.44->9.08! |

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 553 ms | 561 ms | 571 ms | 571 ms | 571 ms | 567 ms | 556 ms | 563 ms | 213.30 MB | 217.06 MB | - | 9.74->9.09! |
| fmgrep | 23.10 s | 4.26 GB | 942 ms | 38.7 ms | 81.2 ms | 108 ms | 120 ms | 4.47 ms | 4.56 ms | 36.4 ms | 76.5 ms | 228.47 MB | 4.12 GB | 8.47 GB | 9.09->11.12! |
| fmgrep `--store-docs` | 22.24 s | 7.10 GB | 55.0 ms | 28.9 ms | 62.5 ms | 79.8 ms | 77.8 ms | 5.45 ms | 5.60 ms | 25.6 ms | 57.3 ms | 277.45 MB | 4.19 GB | 9.57 GB | 11.12->12.03! |
| tgrep | 39.57 s | 2.70 GB | 10.18 s | 162 ms | 418 ms | 642 ms | 554 ms | 484 ms | 446 ms | 231 ms | 438 ms | 3.67 GB | 3.69 GB | 899.19 MB | 12.03->4.90! |

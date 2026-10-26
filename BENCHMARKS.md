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
measured 2026-10-09 (UTC).

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 149 ms | 149 ms | 150 ms | 152 ms | 323 ms | 341 ms | 149 ms | 159 ms | 38.55 MB | 42.05 MB | - | 4.94->5.78! |
| fmgrep | 16.05 s | 2.18 GB | 243 ms | 7.93 ms | 9.92 ms | 12.9 ms | 16.3 ms | 29.3 ms | 28.4 ms | 8.85 ms | 11.4 ms | 51.98 MB | 1.85 GB | 8.11 GB | 5.78->7.16! |
| fmgrep `--store-docs` | 16.25 s | 3.71 GB | 15.2 ms | 5.75 ms | 6.59 ms | 7.64 ms | 10.1 ms | 19.0 ms | 18.8 ms | 5.97 ms | 7.49 ms | 53.98 MB | 1.85 GB | 9.30 GB | 7.16->8.36! |
| tgrep | 17.14 s | 1.02 GB | 2.24 s | 22.3 ms | 34.7 ms | 45.7 ms | 41.5 ms | 1.05 s | 848 ms | 23.1 ms | 73.3 ms | 2.08 GB | 2.23 GB | 207.72 MB | 8.36->6.53! |

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 638 ms | 649 ms | 645 ms | 656 ms | 662 ms | 660 ms | 642 ms | 651 ms | 207.48 MB | 210.98 MB | - | 6.74->6.88! |
| fmgrep | 30.87 s | 4.26 GB | 1.11 s | 52.2 ms | 112 ms | 153 ms | 167 ms | 7.09 ms | 7.00 ms | 48.5 ms | 106 ms | 224.42 MB | 4.20 GB | 8.52 GB | 6.88->9.14! |
| fmgrep `--store-docs` | 27.39 s | 7.10 GB | 54.4 ms | 41.0 ms | 90.0 ms | 115 ms | 116 ms | 7.36 ms | 8.71 ms | 35.8 ms | 83.0 ms | 280.18 MB | 4.27 GB | 9.53 GB | 9.14->11.09! |
| tgrep | 45.56 s | 2.70 GB | 14.71 s | 223 ms | 554 ms | 920 ms | 772 ms | 714 ms | 667 ms | 303 ms | 596 ms | 3.67 GB | 3.69 GB | 923.72 MB | 11.09->3.60! |

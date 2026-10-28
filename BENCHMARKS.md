# Benchmarks

fmgrep against ripgrep, tgrep and elgrep on two pinned source trees, on a
Linux server.
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
- **elgrep**, when installed: one `eg-indexd`, started and stopped by the
  harness, keeps an index in the row's temporary directory
  (`eg --index-dir`). The first query waits while it builds the index
  (`index build`: the wait eg reports; `ready`: from the daemon's start
  until that query is answered from the index, so at least `index build`;
  `index peak RSS`: the daemon's build process). Then one
  `eg -n --no-heading` process per query reads the index itself, after the
  daemon vouches that the tree has not changed. Every query runs with
  `--debug`, which names any query eg scanned without the index; a note
  under the table counts them. The index answers only on Linux (it needs
  inotify); elsewhere every query is a scan. eg searches a file holding a
  NUL byte up to the NUL where rg skips the file; the harness stages no
  such file, so the rows still agree. This is elgrep 1.0.0, from
  [github.com/eliosai/sngram](https://github.com/eliosai/sngram)
  (`cargo install --locked elgrep --version 1.0.0`). See
  [docs/benchmark.md](docs/benchmark.md#elgrep-optional).

Latency is spawn to exit, process start-up and printing included. `p50` is
the lower median over the literal, the regex or the intersection queries,
beside their mean; `all` is over every query. `query footprint` is the
peak anonymous memory of the process answering (the server, or each rg or
eg); `query RSS` is its peak resident set, file-backed pages such as the
mapped index included; `index peak RSS` is the index build's peak resident
memory. `load1` is the 1-minute load average at the row's start and end
(the harness marks 4 or more with `!`). MB and GB are 2^20 and 2^30
bytes.

| corpus | commit | files | text | queries (literal / regex / intersection) |
|---|---|--:|--:|--:|
| linux | `5878583` | 95553 | 1.53 GB | 113 (92 / 13 / 8) |
| chromium | `e2c5a77` | 460957 | 2.84 GB | 46 (30 / 8 / 8) |


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
- **elgrep**: rg's pipe with `eg`, `eg -l --null A | xargs -0 -r eg -l
  --null B | xargs -0 -r eg -n --no-heading -e A -e B`. eg answers file
  arguments from the same index, restricted to those files, and has no AND
  of its own.

### Hardware and version numbers

AWS i4i.4xlarge spot instance (16 vCPUs, Intel Xeon Platinum 8375C at 2.90
GHz, 123 GiB, local NVMe formatted ext4), Ubuntu 24.04, kernel 7.0. fmgrep
0.1.0 (release build), ripgrep 15.2.0, tgrep 1.0.8, elgrep 1.0.0. One run per corpus,
measured 2026-10-28 (UTC).

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 137 ms | 137 ms | 137 ms | 138 ms | 146 ms | 225 ms | 137 ms | 144 ms | 39.52 MB | 43.22 MB | - | 10.79->10.15! |
| fmgrep | 12.95 s | 2.18 GB | 215 ms | 5.64 ms | 6.94 ms | 16.1 ms | 39.4 ms | 24.3 ms | 27.6 ms | 6.56 ms | 12.1 ms | 200.54 MB | 2.19 GB | 8.11 GB | 10.15->10.84! |
| fmgrep `--store-docs` | 13.19 s | 3.71 GB | 18.9 ms | 5.40 ms | 6.10 ms | 11.4 ms | 31.6 ms | 23.1 ms | 27.1 ms | 5.62 ms | 10.5 ms | 211.49 MB | 2.20 GB | 9.36 GB | 10.84->11.44! |
| tgrep | 17.39 s | 1.02 GB | 2.15 s | 21.5 ms | 33.8 ms | 49.4 ms | 71.1 ms | 119 ms | 550 ms | 24.3 ms | 74.7 ms | 2.08 GB | 2.23 GB | 192.69 MB | 11.44->9.07! |
| elgrep | 28.31 s | 1.47 GB | 28.32 s | 9.93 ms | 11.4 ms | 27.1 ms | 24.6 ms | 41.0 ms | 1.19 s | 11.0 ms | 96.1 ms | 10.46 MB | 404.53 MB | 7.27 GB | 9.07->9.11! |

elgrep answered 4 of the 113 queries by scanning: its index refuses a pattern that selects over 30% of the files.

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 562 ms | 566 ms | 559 ms | 564 ms | 566 ms | 568 ms | 563 ms | 566 ms | 213.07 MB | 216.83 MB | - | 11.13->9.25! |
| fmgrep | 22.82 s | 4.26 GB | 962 ms | 38.6 ms | 82.4 ms | 56.4 ms | 115 ms | 5.30 ms | 9.88 ms | 27.7 ms | 75.5 ms | 228.41 MB | 4.20 GB | 8.50 GB | 9.25->11.33! |
| fmgrep `--store-docs` | 23.72 s | 7.10 GB | 51.8 ms | 29.1 ms | 64.3 ms | 56.7 ms | 80.7 ms | 5.81 ms | 10.2 ms | 23.3 ms | 57.7 ms | 287.65 MB | 4.28 GB | 9.57 GB | 11.33->12.47! |
| tgrep | 40.15 s | 2.70 GB | 10.44 s | 163 ms | 421 ms | 472 ms | 812 ms | 159 ms | 338 ms | 174 ms | 475 ms | 5.03 GB | 5.17 GB | 884.25 MB | 12.47->6.10! |
| elgrep | 55.41 s | 3.45 GB | 55.42 s | 44.2 ms | 80.4 ms | 49.6 ms | 123 ms | 149 ms | 995 ms | 49.6 ms | 247 ms | 61.34 MB | 620.60 MB | 15.15 GB | 6.10->8.58! |

elgrep answered every query from its index.

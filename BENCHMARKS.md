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
- **fmgrep, no server** (`--rows fmgrep-direct`): the same index, then one
  `fmgrep --index DIR -n --no-heading` process per query from the corpus
  root, as an agent without a server runs it; each process loads the index
  and checks the tree itself, so there is no `ready`, and the footprint
  and RSS are the largest query's.
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
measured 2026-11-09 (UTC).

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 127 ms | 127 ms | 128 ms | 129 ms | 133 ms | 208 ms | 127 ms | 133 ms | 41.84 MB | 45.59 MB | - | 1.11->2.95 |
| fmgrep | 25.10 s | 1.16 GB | 196 ms | 4.23 ms | 5.35 ms | 12.7 ms | 28.1 ms | 16.9 ms | 18.7 ms | 4.85 ms | 8.92 ms | 253.43 MB | 1.36 GB | 6.94 GB | 3.60->6.91! |
| fmgrep `--store-docs` | 25.61 s | 2.68 GB | 38.9 ms | 3.57 ms | 4.34 ms | 8.54 ms | 18.8 ms | 16.1 ms | 27.0 ms | 3.97 ms | 7.61 ms | 260.02 MB | 1.37 GB | 7.15 GB | 6.91->8.92! |
| fmgrep, no server | 25.21 s | 1.16 GB | - | 68.9 ms | 69.5 ms | 75.5 ms | 91.8 ms | 75.4 ms | 80.6 ms | 69.1 ms | 72.9 ms | 251.32 MB | 1.05 GB | 6.83 GB | 8.92->8.92! |
| tgrep | 16.57 s | 1.02 GB | 1.98 s | 20.3 ms | 31.6 ms | 46.7 ms | 66.5 ms | 113 ms | 522 ms | 22.7 ms | 70.3 ms | 2.07 GB | 2.22 GB | 190.61 MB | 11.56->8.32! |
| elgrep | 27.96 s | 1.47 GB | 27.97 s | 9.91 ms | 11.1 ms | 25.0 ms | 23.5 ms | 42.1 ms | 1.19 s | 11.0 ms | 95.9 ms | 10.34 MB | 404.20 MB | 7.29 GB | 8.32->8.77! |

elgrep answered 4 of the 113 queries by scanning: its index refuses a pattern that selects over 30% of the files.

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 550 ms | 561 ms | 548 ms | 555 ms | 545 ms | 550 ms | 550 ms | 558 ms | 199.71 MB | 203.22 MB | - | 7.05->6.95! |
| fmgrep | 38.54 s | 2.29 GB | 936 ms | 35.5 ms | 74.4 ms | 37.5 ms | 94.8 ms | 4.11 ms | 4.72 ms | 23.3 ms | 65.8 ms | 224.17 MB | 2.45 GB | 14.60 GB | 6.32->10.83! |
| fmgrep `--store-docs` | 39.29 s | 5.12 GB | 97.3 ms | 26.8 ms | 56.3 ms | 26.7 ms | 65.3 ms | 5.05 ms | 5.43 ms | 19.7 ms | 49.0 ms | 286.61 MB | 2.52 GB | 14.42 GB | 10.83->11.59! |
| fmgrep, no server | 38.62 s | 2.29 GB | - | 319 ms | 341 ms | 326 ms | 373 ms | 299 ms | 301 ms | 314 ms | 340 ms | 149.85 MB | 2.23 GB | 14.61 GB | 11.59->14.26! |
| tgrep | 39.62 s | 2.70 GB | 11.65 s | 179 ms | 444 ms | 477 ms | 854 ms | 174 ms | 367 ms | 185 ms | 502 ms | 5.02 GB | 5.16 GB | 871.89 MB | 12.69->4.47! |
| elgrep | 55.58 s | 3.45 GB | 55.58 s | 44.1 ms | 78.7 ms | 51.6 ms | 122 ms | 146 ms | 991 ms | 50.5 ms | 245 ms | 61.34 MB | 628.96 MB | 15.51 GB | 4.47->8.64! |

elgrep answered every query from its index.

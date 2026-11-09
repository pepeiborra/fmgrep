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
| fmgrep | 19.37 s | 1.11 GB | 197 ms | 4.64 ms | 5.72 ms | 11.6 ms | 20.1 ms | 16.5 ms | 18.1 ms | 5.38 ms | 8.25 ms | 204.93 MB | 1.28 GB | 6.26 GB | 2.95->5.41! |
| fmgrep `--store-docs` | 22.40 s | 2.64 GB | 39.3 ms | 3.53 ms | 4.33 ms | 8.53 ms | 13.4 ms | 16.1 ms | 22.4 ms | 4.06 ms | 6.64 ms | 207.40 MB | 1.29 GB | 6.55 GB | 5.41->7.85! |
| fmgrep, no server | 17.85 s | 1.11 GB | - | 70.6 ms | 71.7 ms | 78.1 ms | 87.8 ms | 79.1 ms | 81.4 ms | 71.6 ms | 74.2 ms | 195.39 MB | 930.86 MB | 7.08 GB | 7.85->11.56! |
| tgrep | 16.57 s | 1.02 GB | 1.98 s | 20.3 ms | 31.6 ms | 46.7 ms | 66.5 ms | 113 ms | 522 ms | 22.7 ms | 70.3 ms | 2.07 GB | 2.22 GB | 190.61 MB | 11.56->8.32! |
| elgrep | 27.96 s | 1.47 GB | 27.97 s | 9.91 ms | 11.1 ms | 25.0 ms | 23.5 ms | 42.1 ms | 1.19 s | 11.0 ms | 95.9 ms | 10.34 MB | 404.20 MB | 7.29 GB | 8.32->8.77! |

elgrep answered 4 of the 113 queries by scanning: its index refuses a pattern that selects over 30% of the files.

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 550 ms | 561 ms | 548 ms | 555 ms | 545 ms | 550 ms | 550 ms | 558 ms | 199.71 MB | 203.22 MB | - | 7.05->6.95! |
| fmgrep | 35.52 s | 2.23 GB | 920 ms | 36.0 ms | 74.1 ms | 39.2 ms | 84.0 ms | 4.61 ms | 5.07 ms | 23.8 ms | 63.8 ms | 223.32 MB | 2.38 GB | 9.05 GB | 6.95->14.57! |
| fmgrep `--store-docs` | 41.07 s | 5.06 GB | 99.1 ms | 26.5 ms | 56.3 ms | 29.1 ms | 55.9 ms | 5.58 ms | 5.92 ms | 20.1 ms | 47.5 ms | 295.14 MB | 2.45 GB | 8.28 GB | 14.57->14.04! |
| fmgrep, no server | 37.86 s | 2.23 GB | - | 316 ms | 343 ms | 321 ms | 360 ms | 295 ms | 299 ms | 310 ms | 338 ms | 142.85 MB | 2.19 GB | 8.70 GB | 14.04->12.69! |
| tgrep | 39.62 s | 2.70 GB | 11.65 s | 179 ms | 444 ms | 477 ms | 854 ms | 174 ms | 367 ms | 185 ms | 502 ms | 5.02 GB | 5.16 GB | 871.89 MB | 12.69->4.47! |
| elgrep | 55.58 s | 3.45 GB | 55.58 s | 44.1 ms | 78.7 ms | 51.6 ms | 122 ms | 146 ms | 991 ms | 50.5 ms | 245 ms | 61.34 MB | 628.96 MB | 15.51 GB | 4.47->8.64! |

elgrep answered every query from its index.

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
measured 2026-10-29 (UTC).

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 132 ms | 133 ms | 133 ms | 135 ms | 145 ms | 223 ms | 133 ms | 139 ms | 39.88 MB | 43.63 MB | - | 10.87->9.47! |
| fmgrep | 18.99 s | 2.35 GB | 206 ms | 5.25 ms | 6.44 ms | 14.8 ms | 26.2 ms | 12.3 ms | 18.5 ms | 6.09 ms | 9.57 ms | 205.49 MB | 2.32 GB | 9.80 GB | 9.47->10.45! |
| fmgrep `--store-docs` | 18.66 s | 3.88 GB | 20.6 ms | 4.34 ms | 5.21 ms | 10.2 ms | 19.2 ms | 17.8 ms | 19.9 ms | 4.95 ms | 7.85 ms | 211.61 MB | 2.34 GB | 10.42 GB | 10.45->10.90! |
| tgrep | 16.98 s | 1.02 GB | 2.14 s | 20.8 ms | 32.6 ms | 43.7 ms | 66.2 ms | 131 ms | 540 ms | 23.4 ms | 72.4 ms | 2.07 GB | 2.23 GB | 185.94 MB | 10.90->9.49! |
| elgrep | 28.31 s | 1.47 GB | 28.32 s | 9.93 ms | 11.4 ms | 25.0 ms | 23.7 ms | 41.0 ms | 1.19 s | 11.0 ms | 96.3 ms | 10.14 MB | 405.75 MB | 7.21 GB | 9.49->9.39! |

elgrep answered 4 of the 113 queries by scanning: its index refuses a pattern that selects over 30% of the files.

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 571 ms | 581 ms | 567 ms | 579 ms | 576 ms | 584 ms | 572 ms | 581 ms | 162.96 MB | 166.59 MB | - | 11.17->8.98! |
| fmgrep | 35.65 s | 4.61 GB | 965 ms | 37.9 ms | 78.0 ms | 39.7 ms | 88.3 ms | 4.86 ms | 5.00 ms | 23.5 ms | 67.1 ms | 224.64 MB | 4.30 GB | 9.75 GB | 8.98->12.13! |
| fmgrep `--store-docs` | 36.20 s | 7.45 GB | 50.7 ms | 29.9 ms | 63.1 ms | 28.7 ms | 62.7 ms | 6.12 ms | 9.89 ms | 23.2 ms | 53.8 ms | 278.23 MB | 4.37 GB | 10.56 GB | 12.13->14.72! |
| tgrep | 42.36 s | 2.70 GB | 12.37 s | 186 ms | 462 ms | 513 ms | 889 ms | 175 ms | 379 ms | 198 ms | 522 ms | 5.03 GB | 5.17 GB | 916.69 MB | 14.72->5.03! |
| elgrep | 57.39 s | 3.45 GB | 57.40 s | 45.4 ms | 82.2 ms | 55.1 ms | 129 ms | 162 ms | 1.03 s | 51.8 ms | 254 ms | 61.40 MB | 614.29 MB | 15.92 GB | 5.03->8.53! |

elgrep answered every query from its index.

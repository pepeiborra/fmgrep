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
- **elgrep**: rg's pipe with `eg`, `eg -l --null A | xargs -0 -r eg -l
  --null B | xargs -0 -r eg -n --no-heading -e A -e B`. eg answers file
  arguments from the same index, restricted to those files, and has no AND
  of its own.

### Hardware and version numbers

AWS i4i.4xlarge spot instance (16 vCPUs, Intel Xeon Platinum 8375C at 2.90
GHz, 123 GiB, local NVMe formatted ext4), Ubuntu 24.04, kernel 7.0. fmgrep
0.1.0 (release build), ripgrep 15.2.0, tgrep 1.0.8, elgrep 1.0.0. One run per corpus,
measured 2026-10-24 (UTC).

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 129 ms | 129 ms | 129 ms | 129 ms | 243 ms | 259 ms | 129 ms | 135 ms | 41.08 MB | 44.84 MB | - | 11.09->10.05! |
| fmgrep | 11.55 s | 2.18 GB | 206 ms | 4.89 ms | 6.14 ms | 8.11 ms | 10.7 ms | 17.8 ms | 18.5 ms | 5.35 ms | 7.14 ms | 51.47 MB | 1.92 GB | 8.12 GB | 10.05->11.58! |
| fmgrep `--store-docs` | 11.58 s | 3.71 GB | 18.0 ms | 4.18 ms | 4.98 ms | 6.00 ms | 7.90 ms | 17.2 ms | 16.9 ms | 4.41 ms | 5.81 ms | 52.53 MB | 1.92 GB | 9.36 GB | 11.58->12.26! |
| tgrep | 16.31 s | 1.02 GB | 1.88 s | 19.4 ms | 31.2 ms | 39.1 ms | 36.7 ms | 930 ms | 763 ms | 20.8 ms | 65.9 ms | 2.08 GB | 2.23 GB | 203.38 MB | 12.26->8.67! |
| elgrep | 27.65 s | 1.47 GB | 27.66 s | 8.81 ms | 10.7 ms | 14.0 ms | 18.8 ms | 1.59 s | 1.88 s | 9.81 ms | 98.8 ms | 10.46 MB | 224.29 MB | 7.00 GB | 8.67->8.60! |

elgrep answered 4 of the 107 queries by scanning: its index refuses a pattern that selects over 30% of the files.

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 564 ms | 576 ms | 582 ms | 585 ms | 592 ms | 590 ms | 571 ms | 579 ms | 204.98 MB | 208.49 MB | - | 11.40->9.42! |
| fmgrep | 24.28 s | 4.26 GB | 977 ms | 40.4 ms | 86.8 ms | 117 ms | 128 ms | 5.55 ms | 5.54 ms | 38.9 ms | 81.8 ms | 225.21 MB | 4.13 GB | 8.42 GB | 9.42->11.78! |
| fmgrep `--store-docs` | 23.47 s | 7.10 GB | 57.3 ms | 30.9 ms | 66.7 ms | 84.7 ms | 82.7 ms | 6.25 ms | 6.29 ms | 26.5 ms | 61.2 ms | 281.88 MB | 4.20 GB | 9.57 GB | 11.78->13.12! |
| tgrep | 42.06 s | 2.70 GB | 12.10 s | 179 ms | 452 ms | 700 ms | 599 ms | 547 ms | 505 ms | 244 ms | 477 ms | 3.67 GB | 3.69 GB | 888.83 MB | 13.12->6.14! |
| elgrep | 56.88 s | 3.45 GB | 56.89 s | 45.2 ms | 83.0 ms | 141 ms | 187 ms | 1.73 s | 1.58 s | 56.1 ms | 283 ms | 61.42 MB | 622.04 MB | 16.10 GB | 6.14->8.62! |

elgrep answered every query from its index.

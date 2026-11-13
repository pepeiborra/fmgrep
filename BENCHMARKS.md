# Benchmarks

fmgrep against ripgrep, tgrep and elgrep on three pinned source trees, on a
Linux server.
## Method

Every run is `scripts/bench.sh` ([docs/benchmark.md](docs/benchmark.md)).
Every row searches the checkout itself, one query at a time, and prints
each matching line as `path:line:text`. rg is the reference: a run fails
if fmgrep (served, `--store-docs` or with no server) prints other lines
than rg from a file both searched, by line number; a tgrep or elgrep row
that does is noted under its table with the queries and files.
Files only some rows searched (hidden, symlinked, gitignored, binary) are
listed under the run's results, not failures.

- **rg**: one `rg -n --no-heading` process per query.
- **fmgrep**: an index (`index build`, `index size`), one `fmgrep serve
  --no-watch`, a server of a tree that does not change, as a shared server
  of one commit is (`ready`: spawn to its readiness line, the load and its
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
  NUL byte up to the NUL where rg skips the file; such a file is listed as
  one rg did not search. This is elgrep 1.0.0, from
  [github.com/eliosai/sngram](https://github.com/eliosai/sngram)
  (`cargo install --locked elgrep --version 1.0.0`). See
  [docs/benchmark.md](docs/benchmark.md#elgrep-optional).

Before its timed queries every row runs the set's first query once as a
warm-up, after `ready` where the row has one: its output is checked, but
it is neither timed nor kept, nor counted in the query footprint or
RSS, so no row times the first query a client sends.
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
| chromium | `e2c5a77` | 461027 | 2.84 GB | 46 (30 / 8 / 8) |
| aosp | `android-17.0.0_r1` | 1264964 | 17.57 GB | 46 (30 / 8 / 8) |

aosp is the Android Open Source Project's platform source at release tag
`android-17.0.0_r1`, assembled by the repo tool from the platform
manifest, with the projects of prebuilt binaries left out. `scripts/bench.sh`
never clones it; see [docs/benchmark.md](docs/benchmark.md#aosp).


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
  --type-add and:NAME ... -t and` over the base names of the files on every
  list (`-g` and path arguments would bypass its index), keeping the lines
  of those files.
- **elgrep**: rg's pipe with `eg`, `eg -l --null A | xargs -0 -r eg -l
  --null B | xargs -0 -r eg -n --no-heading -e A -e B`. eg answers file
  arguments from the same index, restricted to those files, and has no AND
  of its own.

### Hardware and version numbers

AWS i4i.4xlarge spot instance (16 vCPUs, Intel Xeon Platinum 8375C at 2.90
GHz, 123 GiB, local NVMe formatted ext4), Ubuntu 24.04, kernel 7.0. fmgrep
0.1.0 (release build), ripgrep 15.2.0, tgrep 1.0.8, elgrep 1.0.0. One run per corpus,
measured 2026-11-09 (UTC), searching the checkout in place. The fmgrep
rows come from a later run on an instance of the same type (rg, tgrep and
elgrep do not use fmgrep's index).

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 121 ms | 121 ms | 123 ms | 125 ms | 127 ms | 244 ms | 121 ms | 130 ms | 14.72 MB | 18.42 MB | - | 7.70->8.66! |
| fmgrep | 22.33 s | 1.11 GB | 115 ms | 3.89 ms | 4.72 ms | 9.79 ms | 18.4 ms | 8.91 ms | 12.9 ms | 4.05 ms | 6.87 ms | 207.56 MB | 1.28 GB | 6.57 GB | 9.97->11.56! |
| fmgrep `--store-docs` | 21.31 s | 2.64 GB | 42.3 ms | 3.26 ms | 3.96 ms | 7.70 ms | 13.2 ms | 8.44 ms | 17.4 ms | 3.72 ms | 5.97 ms | 207.99 MB | 1.28 GB | 6.70 GB | 11.56->12.33! |
| fmgrep, no server | 21.25 s | 1.11 GB | - | 51.2 ms | 51.7 ms | 57.8 ms | 66.6 ms | 57.1 ms | 61.2 ms | 51.5 ms | 54.1 ms | 202.47 MB | 936.52 MB | 6.23 GB | 12.33->11.94! |
| tgrep | 14.12 s | 1.02 GB | 1.84 s | 20.5 ms | 30.0 ms | 41.5 ms | 59.4 ms | 120 ms | 610 ms | 23.0 ms | 74.4 ms | 2.22 GB | 2.28 GB | 282.62 MB | 10.15->8.40! |
| elgrep | 27.61 s | 1.39 GB | 27.62 s | 9.79 ms | 11.0 ms | 23.7 ms | 23.0 ms | 38.5 ms | 1.27 s | 10.8 ms | 101 ms | 10.45 MB | 394.08 MB | 7.06 GB | 8.40->8.85! |

elgrep answered 4 of the 113 queries by scanning: its index refuses a pattern that selects over 30% of the files.

elgrep differed from rg on 2 intersections: `static AND
devm_platform_ioremap_resource` (14700 lines missed in 429 files) and
`MARGINING AND PERFCOUNTER` (197 lines missed in 1 file).

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 569 ms | 592 ms | 560 ms | 587 ms | 558 ms | 603 ms | 569 ms | 593 ms | 112.61 MB | 116.37 MB | - | 12.14->11.96! |
| fmgrep | 38.73 s | 2.23 GB | 629 ms | 24.4 ms | 51.3 ms | 32.1 ms | 69.8 ms | 4.24 ms | 4.79 ms | 16.0 ms | 46.4 ms | 164.37 MB | 2.33 GB | 8.80 GB | 10.98->15.99! |
| fmgrep `--store-docs` | 36.08 s | 5.06 GB | 135 ms | 17.1 ms | 37.2 ms | 27.3 ms | 47.3 ms | 5.10 ms | 5.64 ms | 11.3 ms | 33.4 ms | 208.33 MB | 2.37 GB | 9.34 GB | 15.99->16.12! |
| fmgrep, no server | 47.23 s | 2.23 GB | - | 212 ms | 240 ms | 220 ms | 257 ms | 190 ms | 193 ms | 206 ms | 235 ms | 170.49 MB | 2.22 GB | 8.79 GB | 16.12->12.83! |
| tgrep | 38.90 s | 2.77 GB | 12.28 s | 207 ms | 454 ms | 514 ms | 890 ms | 157 ms | 407 ms | 220 ms | 522 ms | 6.55 GB | 6.57 GB | 1.08 GB | 12.96->4.42! |
| elgrep | 60.83 s | 3.23 GB | 60.84 s | 47.3 ms | 89.0 ms | 52.5 ms | 134 ms | 193 ms | 1.45 s | 53.6 ms | 333 ms | 67.44 MB | 608.20 MB | 16.58 GB | 4.42->8.01! |

elgrep answered every query from its index. It differed from rg on 5
intersections, missing 2 to 1020 lines each: `std::unique_ptr AND
blink::LocalFrame` (1020 lines in 69 files), `TODO AND blink::LocalFrame`
(228 in 36), `std::vector AND net::URLRequest` (170 in 24), `DCHECK AND
net::URLRequest` (35 in 8) and `CurrencyExtendedData AND
UnitsDisplayNames` (2 in 1).

### aosp

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 2.38 s | 2.38 s | 2.36 s | 2.40 s | 2.31 s | 2.31 s | 2.36 s | 2.37 s | 368.68 MB | 372.44 MB | - | 4.01->8.04! |
| fmgrep | 201.21 s | 12.13 GB | 3.83 s | 10.6 ms | 35.5 ms | 12.1 ms | 97.0 ms | 14.2 ms | 19.8 ms | 11.6 ms | 43.5 ms | 1.03 GB | 10.78 GB | 12.05 GB | 9.68->12.55! |
| fmgrep `--store-docs` | 204.48 s | 29.71 GB | 542 ms | 9.25 ms | 29.5 ms | 9.57 ms | 89.8 ms | 14.3 ms | 19.7 ms | 9.57 ms | 38.3 ms | 979.47 MB | 10.72 GB | 10.51 GB | 12.55->11.35! |
| fmgrep, no server | 203.31 s | 12.13 GB | - | 1.66 s | 1.67 s | 1.64 s | 1.72 s | 1.66 s | 1.65 s | 1.66 s | 1.68 s | 884.64 MB | 8.41 GB | 11.15 GB | 11.35->14.83! |
| tgrep | 159.16 s | 9.87 GB | 34.59 s | 137 ms | 249 ms | 488 ms | 822 ms | 566 ms | 673 ms | 187 ms | 423 ms | 28.12 GB | 28.18 GB | 2.63 GB | 11.01->2.73! |
| elgrep | 294.54 s | 12.23 GB | 294.55 s | 52.6 ms | 1.07 s | 47.4 ms | 65.8 ms | 162 ms | 307 ms | 59.9 ms | 761 ms | 310.67 MB | 1020.09 MB | 59.55 GB | 2.73->8.71! |

elgrep answered 12 of the 46 queries by scanning, before its daemon
vouched that the tree had not changed since the index was published.

elgrep differed from rg on 2 queries: `ndk::ScopedAStatus AND
android::base::unique_fd` (227 lines missed in 3 files) and `MCK_ AND
C_Check` (40 lines missed in 1 file). The second file is indexed; eg 1.0.0
anchors the index at the common parent of its search paths under
`--index-dir` (here `external/`) and drops every candidate, while a scan
finds it, so the miss is intermittent.

## A served query with default flags

The served rows above run `fmgrep serve --no-watch`. With default flags
the server asks [watchman](docs/index-format.md#watchman) what changed
before each query, which costs time and makes each answer current.

Measured on an AWS i4i.4xlarge (Ubuntu 24.04, ext4 on NVMe, watchman
v2026.11.09) on 2026-11-13, files in the page cache. "Before" is the
server that ran its own file watcher and "after" the one that asks
watchman; both served the same index at once, in alternating blocks, 120
queries per class each, each timed as the client's wall time over a new
connection with nothing changing in the tree. `serve --no-watch` took
the same time before and after, within noise, on every tree and class,
so the tables above stand.

p50 in ms, before → after, first run. The last column covers both runs
and every class.

| tree | rare identifier (`-l`) | no match | AND of two terms | regex | common word (`-c`) | after minus before |
|---|---|---|---|---|---|---|
| linux | 0.42 → 2.10 | 0.51 → 2.52 | 1.27 → 3.11 | 42.7 → 45.0 | 20.9 → 22.9 | +1.7 to +2.3 ms |
| chromium | 0.45 → 2.11 | 0.50 → 2.41 | 1.48 → 3.42 | 85.5 → 87.5 | 39.2 → 41.3 | +1.7 to +2.1 ms (one regex run +0.2) |
| aosp | 0.52 → 2.78 | 0.55 → 3.33 | 3.29 → 5.68 | 6.84 → 9.28 | 152 → 154 | +2.2 to +3.2 ms (one `-c` run +0.4) |
| fmgrep | 0.22 → 1.30 | 0.22 → 1.32 | 0.58 → 1.66 | 1.04 → 2.13 | 0.77 → 1.87 | +1.1 ms |

Where the time goes, from the server's `--verbose` line:

| tree | the ask (p50 / p95) | the re-check (p50 / p95) | paths watchman reports per query |
|---|---|---|---|
| linux | 1.1 / 1.3-1.4 ms | 0.54-0.57 / 0.67-0.75 ms | 1 |
| chromium | 1.1-1.2 / 1.4 ms | 0.42-0.43 / 0.49-0.52 ms | 1 |
| aosp | 1.2 / 1.4-1.5 ms | 1.5 / 2.2-2.7 ms | 0 |
| fmgrep | 0.97 / 1.1 ms | 0.03 / 0.04 ms | 0 |

An edit, then a query sent at once:

| tree | before misses the edit | after misses it | before shows it after (p50 / p95 / max) |
|---|---|---|---|
| linux | 290 of 300 | 0 of 300 | 50 / 74 / 76 ms |
| chromium | 196 of 200 | 0 of 200 | 51 / 75 / 78 ms |
| aosp | 200 of 200 | 0 of 200 | 63 / 71 / 72 ms |

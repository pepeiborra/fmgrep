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
- **fmgrep**: an index (`index build`, `index size`), one `fmgrep serve`
  (`ready`: spawn to its readiness line, the load and its
  [check of the tree](docs/index-format.md#staleness) included), then one
  `fmgrep --server ADDR -n --no-heading` process per query.
- **fmgrep, no server** (`--rows fmgrep-direct`): the same index, then one
  `fmgrep --index DIR -n --no-heading` process per query from the corpus
  root, as an agent without a server runs it; each process loads the index
  and checks the tree itself, so there is no `ready`, and the footprint
  and RSS are the largest query's. On these three trees the check asks
  watchman what changed since the token in the record that the first
  search left in the cache directory. The warm-up query is that first
  search: it checks every file and takes the token, so the row times
  only later searches.
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
Before the rows, `scripts/bench.sh` has watchman watch the checkout and
waits until watchman has crawled it
([File monitor](docs/benchmark.md#file-monitor)); fmgrep runs with its
defaults.
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
| linux | `5878583` | 95926 | 1.53 GB | 113 (92 / 13 / 8) |
| chromium | `e2c5a77` | 460698 | 2.84 GB | 46 (30 / 8 / 8) |
| aosp | `android-17.0.0_r1` | 1253798 | 17.37 GB | 46 (30 / 8 / 8) |

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
0.1.0 (release build, commit `c58fb4b`), ripgrep 15.2.0, tgrep 1.0.8,
elgrep 1.0.0, watchman v2026.11.09.00 (the prebuilt release). One run of
every row on one instance with
[scripts/ec2-bench.sh](scripts/ec2-bench.sh), measured 2026-11-13 (UTC),
searching the checkout in place.

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 114 ms | 115 ms | 116 ms | 118 ms | 125 ms | 239 ms | 115 ms | 124 ms | 10.15 MB | 16.07 MB | - | 6.16->7.45! |
| fmgrep | 22.04 s | 1.11 GB | 160 ms | 4.09 ms | 4.95 ms | 9.06 ms | 17.0 ms | 8.61 ms | 13.0 ms | 4.51 ms | 6.90 ms | 196.45 MB | 1.27 GB | 6.15 GB | 7.45->9.70! |
| fmgrep `--store-docs` | 20.28 s | 2.64 GB | 42.9 ms | 3.68 ms | 4.22 ms | 6.98 ms | 13.4 ms | 6.16 ms | 11.2 ms | 3.93 ms | 5.77 ms | 206.75 MB | 1.28 GB | 7.19 GB | 9.70->10.78! |
| fmgrep, no server | 18.20 s | 1.11 GB | - | 47.3 ms | 47.9 ms | 51.6 ms | 60.3 ms | 51.4 ms | 55.9 ms | 48.2 ms | 49.9 ms | 197.52 MB | 936.34 MB | 6.17 GB | 10.78->9.78! |
| tgrep | 14.67 s | 1.02 GB | 2.13 s | 23.1 ms | 30.8 ms | 44.8 ms | 66.1 ms | 130 ms | 634 ms | 25.9 ms | 77.5 ms | 2.21 GB | 2.27 GB | 284.12 MB | 9.78->7.97! |
| elgrep | 31.42 s | 1.39 GB | 31.43 s | 9.88 ms | 11.3 ms | 23.8 ms | 23.2 ms | 40.8 ms | 1.23 s | 11.0 ms | 99.0 ms | 10.14 MB | 391.46 MB | 7.19 GB | 7.97->8.49! |

elgrep answered 4 of the 113 queries by scanning: its index refuses a pattern that selects over 30% of the files.

elgrep differed from rg on 1 intersection: `MARGINING AND PERFCOUNTER`
(197 lines missed in 1 file).

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 510 ms | 519 ms | 496 ms | 509 ms | 483 ms | 513 ms | 501 ms | 516 ms | 96.39 MB | 102.01 MB | - | 1.63->4.72! |
| fmgrep | 46.06 s | 2.22 GB | 442 ms | 24.0 ms | 48.9 ms | 30.2 ms | 64.6 ms | 4.52 ms | 5.18 ms | 15.7 ms | 44.0 ms | 181.54 MB | 2.35 GB | 8.99 GB | 4.72->11.08! |
| fmgrep `--store-docs` | 41.97 s | 5.06 GB | 138 ms | 16.3 ms | 32.8 ms | 22.2 ms | 43.1 ms | 5.20 ms | 5.95 ms | 11.8 ms | 29.9 ms | 209.09 MB | 2.37 GB | 8.83 GB | 11.08->13.16! |
| fmgrep, no server | 38.63 s | 2.22 GB | - | 169 ms | 200 ms | 177 ms | 216 ms | 150 ms | 150 ms | 162 ms | 194 ms | 168.14 MB | 2.20 GB | 8.89 GB | 13.16->12.98! |
| tgrep | 40.62 s | 2.77 GB | 13.24 s | 210 ms | 462 ms | 523 ms | 906 ms | 164 ms | 420 ms | 219 ms | 532 ms | 6.53 GB | 6.55 GB | 1.07 GB | 12.98->4.37! |
| elgrep | 70.69 s | 3.24 GB | 70.70 s | 47.5 ms | 89.2 ms | 52.8 ms | 134 ms | 195 ms | 1.37 s | 54.8 ms | 320 ms | 67.56 MB | 604.49 MB | 16.46 GB | 4.37->7.16! |

elgrep answered every query from its index. It differed from rg on 5
intersections, missing 2 to 995 lines each: `std::unique_ptr AND
blink::LocalFrame` (995 lines in 63 files), `TODO AND blink::LocalFrame`
(290 in 44), `std::vector AND net::URLRequest` (257 in 30), `DCHECK AND
net::URLRequest` (64 in 10) and `CurrencyExtendedData AND
UnitsDisplayNames` (2 in 1).

### aosp

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 1.56 s | 1.56 s | 1.58 s | 1.61 s | 1.51 s | 1.51 s | 1.56 s | 1.56 s | 309.01 MB | 314.70 MB | - | 5.39->9.88! |
| fmgrep | 196.80 s | 12.00 GB | 2.82 s | 9.89 ms | 34.0 ms | 15.0 ms | 76.3 ms | 14.2 ms | 21.0 ms | 11.1 ms | 39.1 ms | 986.38 MB | 10.50 GB | 11.98 GB | 9.88->13.78! |
| fmgrep `--store-docs` | 201.04 s | 29.38 GB | 553 ms | 9.16 ms | 31.9 ms | 12.5 ms | 75.9 ms | 18.1 ms | 59.9 ms | 11.0 ms | 44.4 ms | 882.16 MB | 10.45 GB | 10.37 GB | 13.78->11.98! |
| fmgrep, no server | 212.90 s | 12.00 GB | - | 590 ms | 614 ms | 593 ms | 658 ms | 599 ms | 604 ms | 590 ms | 620 ms | 836.33 MB | 8.28 GB | 11.71 GB | 11.98->8.03! |
| tgrep | 168.62 s | 9.87 GB | 37.63 s | 167 ms | 265 ms | 518 ms | 792 ms | 578 ms | 717 ms | 206 ms | 436 ms | 28.14 GB | 28.20 GB | 2.58 GB | 8.03->3.02! |
| elgrep | - | 12.23 GB | - | 43.1 ms | 130 ms | 55.0 ms | 70.9 ms | 163 ms | 309 ms | 47.5 ms | 151 ms | 259.75 MB | 1019.23 MB | 60.95 GB | 3.02->9.52! |

elgrep's `index build` and `ready` on aosp are not given (`-`). The
aosp root holds no `.git` directory, so watchman, which watches the tree
for fmgrep, writes its short-lived cookie files (`.watchman-cookie-*`)
in the root. eg-indexd counted them as changes and restarted its index
build, again and again, until the watch was removed by hand after 28
minutes. On 2026-11-09, before the harness had watchman watch the tree,
they were 294.54 s and 294.55 s.

elgrep answered 1 of the 46 queries by scanning: its daemon reported
that the tree had changed since the index was published.

elgrep differed from rg on 2 queries: `ndk::ScopedAStatus AND
android::base::unique_fd` (9 lines missed in 1 file) and `MCK_ AND
C_Check` (40 lines missed in 1 file). The second file is indexed; eg 1.0.0
anchors the index at the common parent of its search paths under
`--index-dir` (here `external/`) and drops every candidate, while a scan
finds it, so the miss is intermittent.

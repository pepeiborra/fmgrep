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
searching the checkout in place. The fmgrep rows come from a later run of
`scripts/bench.sh` on an instance of the same type, on 2026-11-15, at the
commit of pull request 44 that checks the shards of an index in parallel
(rg, tgrep and elgrep do not use fmgrep's index).

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 114 ms | 115 ms | 116 ms | 118 ms | 125 ms | 239 ms | 115 ms | 124 ms | 10.15 MB | 16.07 MB | - | 6.16->7.45! |
| fmgrep | 21.16 s | 1.11 GB | 108 ms | 4.99 ms | 6.01 ms | 10.7 ms | 19.3 ms | 9.90 ms | 14.4 ms | 5.55 ms | 8.14 ms | 190.03 MB | 1.26 GB | 6.80 GB | 5.12->7.07! |
| fmgrep `--store-docs` | 24.84 s | 2.64 GB | 13.9 ms | 4.35 ms | 4.83 ms | 7.37 ms | 14.3 ms | 6.81 ms | 12.2 ms | 4.69 ms | 6.45 ms | 207.62 MB | 1.28 GB | 6.19 GB | 7.07->8.69! |
| fmgrep, no server | 24.65 s | 1.11 GB | - | 18.8 ms | 20.3 ms | 23.8 ms | 34.5 ms | 20.6 ms | 27.7 ms | 19.3 ms | 22.5 ms | 193.45 MB | 932.25 MB | 6.64 GB | 8.69->8.79! |
| tgrep | 14.67 s | 1.02 GB | 2.13 s | 23.1 ms | 30.8 ms | 44.8 ms | 66.1 ms | 130 ms | 634 ms | 25.9 ms | 77.5 ms | 2.21 GB | 2.27 GB | 284.12 MB | 9.78->7.97! |
| elgrep | 31.42 s | 1.39 GB | 31.43 s | 9.88 ms | 11.3 ms | 23.8 ms | 23.2 ms | 40.8 ms | 1.23 s | 11.0 ms | 99.0 ms | 10.14 MB | 391.46 MB | 7.19 GB | 7.97->8.49! |

elgrep answered 4 of the 113 queries by scanning: its index refuses a pattern that selects over 30% of the files.

elgrep differed from rg on 1 intersection: `MARGINING AND PERFCOUNTER`
(197 lines missed in 1 file).

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 510 ms | 519 ms | 496 ms | 509 ms | 483 ms | 513 ms | 501 ms | 516 ms | 96.39 MB | 102.01 MB | - | 1.63->4.72! |
| fmgrep | 40.14 s | 2.22 GB | 135 ms | 26.8 ms | 52.4 ms | 32.6 ms | 69.3 ms | 4.98 ms | 5.60 ms | 16.9 ms | 47.2 ms | 164.89 MB | 2.31 GB | 7.24 GB | 9.97->10.57! |
| fmgrep `--store-docs` | 40.68 s | 5.06 GB | 48.9 ms | 16.5 ms | 32.6 ms | 22.5 ms | 42.5 ms | 5.31 ms | 6.48 ms | 12.0 ms | 29.8 ms | 205.69 MB | 2.37 GB | 7.91 GB | 10.57->12.41! |
| fmgrep, no server | 36.08 s | 2.22 GB | - | 80.6 ms | 109 ms | 87.0 ms | 125 ms | 56.2 ms | 57.6 ms | 68.6 ms | 103 ms | 155.61 MB | 2.19 GB | 8.95 GB | 12.41->14.11! |
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
| fmgrep | 217.76 s | 12.00 GB | 1.10 s | 11.3 ms | 37.6 ms | 16.4 ms | 77.1 ms | 15.1 ms | 22.2 ms | 12.8 ms | 41.8 ms | 841.55 MB | 10.38 GB | 11.19 GB | 8.65->11.49! |
| fmgrep `--store-docs` | 203.23 s | 29.38 GB | 146 ms | 9.28 ms | 31.3 ms | 12.3 ms | 74.5 ms | 17.6 ms | 59.5 ms | 10.7 ms | 43.7 ms | 839.56 MB | 10.46 GB | 10.13 GB | 11.49->13.39! |
| fmgrep, no server | 199.16 s | 12.00 GB | - | 185 ms | 208 ms | 191 ms | 255 ms | 192 ms | 196 ms | 186 ms | 214 ms | 805.49 MB | 8.24 GB | 11.99 GB | 13.39->10.99! |
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

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

A `-i` query is a literal or an intersection searched case-insensitively:
every row passes `-i` to its tool (`rg -i`, `fmgrep -i`, `tgrep -i`,
`eg -i`), and an intersection runs as under [Intersections](#intersections),
with `-i` on every command. Its terms are ASCII, so that no tool's own
rules for other letters change what a term means. fmgrep and rg fold case
alike (Unicode simple case folding: `k` also matches the Kelvin sign `K`),
so rg checks a `-i` query as it checks the others. No query has a path or
a `-g` filter.

Before its timed queries every row runs the set's first query once as a
warm-up, after `ready` where the row has one: its output is checked, but
it is neither timed nor kept, nor counted in the query footprint or
RSS, so no row times the first query a client sends.
Before the rows, `scripts/bench.sh` has watchman watch the checkout and
waits until watchman has crawled it
([File monitor](docs/benchmark.md#file-monitor)); fmgrep runs with its
defaults.
Latency is spawn to exit, process start-up and printing included. `p50` is
the lower median over the literal, the regex, the intersection or the
`-i` queries, beside their mean; `all` is over every query.
`query footprint` is the peak anonymous memory of the process answering
(the server, or each rg or eg); `query RSS` is its peak resident set,
file-backed pages such as the mapped index included; `index peak RSS` is
the index build's peak resident memory. `load1` is the 1-minute load
average at the row's start and end (the harness marks 4 or more with
`!`). MB and GB are 2^20 and 2^30 bytes.

| corpus | commit | files | text | queries (literal / regex / intersection / -i) |
|---|---|--:|--:|--:|
| linux | `5878583` | 95926 | 1.53 GB | 121 (92 / 13 / 8 / 8) |
| chromium | `e2c5a77` | 460698 | 2.84 GB | 54 (30 / 8 / 8 / 8) |
| aosp | `android-17.0.0_r1` | 1253798 | 17.37 GB | 54 (30 / 8 / 8 / 8) |

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
0.1.0 (release build, with the planner that counts `-i` terms), ripgrep
15.2.0, tgrep 1.0.8, elgrep 1.0.0, watchman v2026.11.09.00 (the prebuilt
release). Every row of the three tables comes from one run of
`scripts/bench.sh` per tree, all on one instance, measured 2026-11-15
(UTC), searching the checkout in place. The page cache was dropped before
each tree and the tree read once.
[scripts/ec2-bench.sh](scripts/ec2-bench.sh) runs the benchmark on such
an instance.

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 110 ms | 111 ms | 113 ms | 116 ms | 116 ms | 223 ms | 129 ms | 180 ms | 111 ms | 123 ms | 10.12 MB | 15.73 MB | - | 2.28->3.63 |
| fmgrep | 17.41 s | 1.11 GB | 93.3 ms | 3.74 ms | 4.72 ms | 8.26 ms | 16.7 ms | 8.13 ms | 12.0 ms | 19.9 ms | 20.4 ms | 4.55 ms | 7.52 ms | 173.65 MB | 1.25 GB | 6.70 GB | 3.63->6.30! |
| fmgrep `--store-docs` | 21.13 s | 2.64 GB | 9.99 ms | 3.24 ms | 3.85 ms | 6.95 ms | 12.9 ms | 6.37 ms | 15.6 ms | 13.5 ms | 13.7 ms | 3.71 ms | 6.26 ms | 204.75 MB | 1.28 GB | 6.07 GB | 6.30->8.93! |
| fmgrep, no server | 17.82 s | 1.11 GB | - | 13.0 ms | 13.6 ms | 17.9 ms | 27.3 ms | 16.2 ms | 22.4 ms | 30.6 ms | 32.0 ms | 13.1 ms | 16.9 ms | 187.59 MB | 1.03 GB | 6.47 GB | 8.93->11.42! |
| tgrep | 14.16 s | 1.02 GB | 1.81 s | 19.6 ms | 28.5 ms | 41.1 ms | 59.2 ms | 118 ms | 587 ms | 140 ms | 487 ms | 25.8 ms | 99.0 ms | 2.21 GB | 2.27 GB | 288.20 MB | 11.42->8.42! |
| elgrep | 27.55 s | 1.39 GB | 27.56 s | 9.76 ms | 10.8 ms | 22.6 ms | 22.5 ms | 38.5 ms | 1.22 s | 53.6 ms | 911 ms | 10.8 ms | 152 ms | 9.92 MB | 392.73 MB | 6.73 GB | 8.42->6.33! |

tgrep printed the same lines as rg on all 121 queries.

elgrep answered 7 of the 121 queries by scanning: its index refuses a
pattern that selects over 30% of the files.

elgrep differed from rg on 4 intersections: `static AND
devm_platform_ioremap_resource` (1611 lines missed in 44 files), `-i
static AND devm_platform_ioremap_resource` (1503 in 79), `int AND
kmem_cache_create` (1027 in 3) and `MARGINING AND PERFCOUNTER` (197 in
1).

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 483 ms | 498 ms | 479 ms | 498 ms | 474 ms | 501 ms | 510 ms | 517 ms | 489 ms | 501 ms | 75.78 MB | 81.18 MB | - | 6.32->8.01! |
| fmgrep | 35.02 s | 2.22 GB | 99.2 ms | 24.1 ms | 49.1 ms | 30.1 ms | 65.5 ms | 3.94 ms | 4.71 ms | 9.39 ms | 15.2 ms | 14.2 ms | 40.0 ms | 109.48 MB | 2.32 GB | 9.37 GB | 8.01->14.56! |
| fmgrep `--store-docs` | 37.94 s | 5.06 GB | 22.1 ms | 16.0 ms | 32.2 ms | 21.5 ms | 42.7 ms | 4.82 ms | 5.76 ms | 8.29 ms | 11.0 ms | 9.59 ms | 26.7 ms | 156.25 MB | 2.37 GB | 8.48 GB | 14.56->15.50! |
| fmgrep, no server | 35.29 s | 2.22 GB | - | 49.4 ms | 78.3 ms | 55.6 ms | 94.1 ms | 27.4 ms | 28.2 ms | 33.8 ms | 39.0 ms | 39.2 ms | 67.4 ms | 111.27 MB | 2.19 GB | 8.92 GB | 15.50->15.26! |
| tgrep | 38.93 s | 2.77 GB | 11.71 s | 188 ms | 431 ms | 494 ms | 849 ms | 154 ms | 394 ms | 142 ms | 576 ms | 197 ms | 509 ms | 6.54 GB | 6.56 GB | 1.07 GB | 15.26->4.57! |
| elgrep | 69.38 s | 3.24 GB | 69.39 s | 47.3 ms | 86.4 ms | 50.6 ms | 130 ms | 194 ms | 1.36 s | 79.4 ms | 877 ms | 60.0 ms | 399 ms | 67.59 MB | 612.22 MB | 16.06 GB | 4.57->6.20! |

tgrep printed the same lines as rg on all 54 queries.

elgrep answered 1 of the 54 queries by scanning: its index refuses a
pattern that selects over 30% of the files. It differed from rg on 8
intersections, missing 2 to 1242 lines each: `std::unique_ptr AND
blink::LocalFrame` (1242 lines in 80 files), `-i DCHECK AND
net::URLRequest` (303 in 17), `std::vector AND net::URLRequest` (265 in
30), `-i std::vector AND net::URLRequest` (265 in 30), `TODO AND
blink::LocalFrame` (228 in 30), `DCHECK AND net::URLRequest` (49 in 6),
`CurrencyExtendedData AND UnitsDisplayNames` (2 in 1) and `-i
CurrencyExtendedData AND UnitsDisplayNames` (2 in 1).

### aosp

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 1.54 s | 1.53 s | 1.53 s | 1.56 s | 1.46 s | 1.46 s | 1.61 s | 1.61 s | 1.53 s | 1.54 s | 317.22 MB | 322.73 MB | - | 7.44->10.61! |
| fmgrep | 196.28 s | 12.00 GB | 957 ms | 10.1 ms | 34.3 ms | 14.6 ms | 74.1 ms | 13.9 ms | 20.3 ms | 13.4 ms | 32.2 ms | 12.0 ms | 37.8 ms | 724.52 MB | 10.58 GB | 11.56 GB | 10.61->12.75! |
| fmgrep `--store-docs` | 199.99 s | 29.38 GB | 57.4 ms | 8.27 ms | 28.5 ms | 11.2 ms | 72.8 ms | 16.2 ms | 56.5 ms | 13.3 ms | 28.1 ms | 10.7 ms | 39.2 ms | 722.97 MB | 10.62 GB | 10.43 GB | 12.75->13.37! |
| fmgrep, no server | 192.29 s | 12.00 GB | - | 94.7 ms | 117 ms | 97.6 ms | 161 ms | 99.3 ms | 107 ms | 102 ms | 117 ms | 97.2 ms | 122 ms | 676.55 MB | 8.47 GB | 11.84 GB | 13.37->13.58! |
| tgrep | 161.78 s | 9.87 GB | 35.15 s | 154 ms | 280 ms | 526 ms | 731 ms | 530 ms | 681 ms | 511 ms | 659 ms | 287 ms | 462 ms | 28.12 GB | 28.17 GB | 2.62 GB | 13.58->2.24! |
| elgrep | 302.97 s | 12.23 GB | 302.98 s | 46.1 ms | 1.05 s | 46.4 ms | 64.0 ms | 156 ms | 298 ms | 303 ms | 365 ms | 90.7 ms | 693 ms | 313.85 MB | 1.00 GB | 59.54 GB | 2.24->9.63! |

tgrep printed the same lines as rg on all 54 queries.

The aosp root holds no `.git` directory, so watchman, which watches the
tree for fmgrep, writes its short-lived cookie files
(`.watchman-cookie-*`) in the root, and eg-indexd counts them as changes
and restarts its index build. In this run the watch was removed when the
tgrep row started, the row before elgrep's, so eg-indexd built its index
once.

elgrep answered 12 of the 54 queries by scanning, 12 of its first 13
queries, all literals: its daemon reported that the tree had changed
since the index was published. A scan takes about 2.5 s here, so these
12 queries make elgrep's literal mean 1.05 s and its all mean 693 ms.

elgrep differed from rg on 1 query: `MCK_ AND C_Check` (40 lines missed
in 1 file). The file is indexed; eg 1.0.0
anchors the index at the common parent of its search paths under
`--index-dir` (here `external/`) and drops every candidate, while a scan
finds it, so the miss is intermittent.

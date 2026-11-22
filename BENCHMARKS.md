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
  `fmgrep --server ADDR -n --no-heading` process per query from the corpus
  root, as a user runs it: where the index records a served commit (an index
  built with `--store-docs` from a clean git checkout that git can compare;
  chromium's, with submodules, records none), each query includes the client's
  check of the checkout for local changes, which asks watchman and, without
  it, runs git.
- **fmgrep, no server** (`--rows fmgrep-direct`): the same index, then one
  `fmgrep --index DIR -n --no-heading` process per query from the corpus
  root, as an agent without a server runs it; each process loads the index
  and checks the tree itself, so there is no `ready`, and the footprint
  and RSS are the largest query's. On these three trees the check asks
  watchman what changed since the token in the record that the first
  search left in the cache directory. Before its timed queries the row
  runs `fmgrep status DIR` from the corpus root, which runs that first
  search's check without a query: it checks every file, takes the token
  and saves the record, so the row times only later searches.
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
  (`eg --index-dir`). For the row the harness removes watchman's watch of
  the tree, and puts it back after: watchman writes a cookie file about
  once a minute into a tree it watches, and eg-indexd counts each as a
  change. A probe query waits while it builds the index (`index build`:
  the wait eg reports; `index peak RSS`: the daemon's build process).
  Then the probe runs every 200 ms until 3 in a row are answered from the
  index (`ready`: from the daemon's start until then, so at least `index
  build`, with the probes and the 200 ms pauses between them: 0.4 s
  between the last three). fmgrep's and tgrep's `ready` start after
  their index is built, so elgrep's `ready` minus its `index build` is
  the number to compare with theirs: on aosp 34.00 s, against 971 ms for
  fmgrep and 36.04 s for tgrep. Then one
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
RSS, so no row times the first query a client sends. The fmgrep row
with no server runs `fmgrep status DIR` instead, as above.
Before the rows, `scripts/bench.sh` has watchman watch the checkout and
waits until watchman has crawled it
([File monitor](docs/benchmark.md#file-monitor)); fmgrep runs with its
defaults. The elgrep row alone runs without that watch.
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

- **fmgrep**: one `fmgrep --server ADDR -n --no-heading --and -e A -e B`
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
GHz, 123 GiB, local NVMe formatted ext4), Ubuntu 24.04, kernel 7.0 with its
default inotify limits. fmgrep 0.1.0 (release build of the tip of the open
pull requests, up to the one this commit is in, with the command line of
rg: one PATTERN, a regex by default, `-F` for a literal, `-e` for several),
ripgrep 15.2.0, tgrep 1.0.8, elgrep 1.0.0, watchman v2026.11.09.00 (the
prebuilt release). Every row of
the three tables comes from one run of `scripts/bench.sh` per tree, all on
one instance, measured 2026-11-22 (UTC), searching the checkout in place.
The page cache was dropped before each tree and the tree read once.
[scripts/ec2-bench.sh](scripts/ec2-bench.sh) runs the benchmark on such
an instance.

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 112 ms | 113 ms | 115 ms | 117 ms | 119 ms | 229 ms | 131 ms | 183 ms | 113 ms | 126 ms | 10.11 MB | 15.61 MB | - | 12.83->12.57! |
| fmgrep | 17.93 s | 1.11 GB | 99.8 ms | 4.07 ms | 5.11 ms | 7.98 ms | 14.0 ms | 6.67 ms | 10.2 ms | 18.5 ms | 19.2 ms | 4.90 ms | 7.34 ms | 174.24 MB | 1.25 GB | 6.54 GB | 12.57->12.47! |
| fmgrep `--store-docs` | 27.09 s | 2.64 GB | 9.72 ms | 4.84 ms | 5.51 ms | 8.50 ms | 13.2 ms | 6.96 ms | 10.7 ms | 13.6 ms | 14.3 ms | 5.47 ms | 7.26 ms | 186.20 MB | 1.26 GB | 6.08 GB | 12.47->11.98! |
| fmgrep, no server | 17.39 s | 1.11 GB | - | 13.0 ms | 13.8 ms | 17.9 ms | 25.1 ms | 17.3 ms | 20.6 ms | 27.8 ms | 30.7 ms | 13.1 ms | 16.6 ms | 187.70 MB | 1.03 GB | 7.61 GB | 11.98->13.90! |
| tgrep | 14.41 s | 1.02 GB | 1.85 s | 21.1 ms | 28.0 ms | 41.2 ms | 64.8 ms | 136 ms | 617 ms | 147 ms | 501 ms | 26.2 ms | 102 ms | 2.21 GB | 2.27 GB | 296.66 MB | 13.90->9.61! |
| elgrep | 27.80 s | 1.39 GB | 28.23 s | 9.80 ms | 10.7 ms | 23.7 ms | 22.6 ms | 38.6 ms | 1.23 s | 53.4 ms | 922 ms | 10.8 ms | 153 ms | 10.45 MB | 389.22 MB | 7.22 GB | 9.61->8.09! |

tgrep printed the same lines as rg on all 121 queries.

elgrep answered 7 of the 121 queries by scanning: its index refuses a
pattern that selects over 30% of the files.

elgrep differed from rg on 6 intersections, missing lines in files that
hold both sides: `-i static AND devm_platform_ioremap_resource` (26339
lines missed in 748 files), `static AND devm_platform_ioremap_resource`
(17520 in 438), `int AND kmem_cache_create` (3250 in 38), `return AND
kvzalloc` (1004 in 13), `include AND alloc_pages_node` (299 in 15) and
`MARGINING AND PERFCOUNTER` (197 in 1).
Which intersections it misses, and how much, changes from run to run.

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 494 ms | 509 ms | 490 ms | 508 ms | 480 ms | 510 ms | 509 ms | 524 ms | 498 ms | 511 ms | 99.10 MB | 104.58 MB | - | 8.23->8.33! |
| fmgrep | 38.04 s | 2.22 GB | 103 ms | 24.6 ms | 49.8 ms | 30.0 ms | 62.9 ms | 4.45 ms | 4.91 ms | 9.95 ms | 15.2 ms | 12.1 ms | 40.0 ms | 106.64 MB | 2.32 GB | 8.66 GB | 8.33->10.94! |
| fmgrep `--store-docs` | 44.20 s | 5.06 GB | 20.7 ms | 18.4 ms | 32.6 ms | 19.1 ms | 40.3 ms | 5.29 ms | 5.50 ms | 8.64 ms | 11.0 ms | 9.74 ms | 26.5 ms | 147.33 MB | 2.36 GB | 7.26 GB | 10.94->11.94! |
| fmgrep, no server | 43.09 s | 2.22 GB | - | 50.8 ms | 79.7 ms | 55.6 ms | 91.5 ms | 28.6 ms | 28.8 ms | 35.0 ms | 39.6 ms | 37.7 ms | 68.0 ms | 112.55 MB | 2.19 GB | 8.96 GB | 11.94->11.02! |
| tgrep | 39.29 s | 2.77 GB | 12.02 s | 191 ms | 436 ms | 510 ms | 859 ms | 161 ms | 401 ms | 143 ms | 585 ms | 206 ms | 515 ms | 6.53 GB | 6.55 GB | 1.06 GB | 11.02->4.03! |
| elgrep | 61.02 s | 3.23 GB | 61.46 s | 47.5 ms | 88.7 ms | 54.9 ms | 134 ms | 188 ms | 1.37 s | 77.1 ms | 882 ms | 58.2 ms | 403 ms | 67.52 MB | 608.77 MB | 16.07 GB | 4.03->6.91! |

tgrep printed the same lines as rg on all 54 queries.

elgrep answered 1 of the 54 queries by scanning: its index refuses a
pattern that selects over 30% of the files. It differed from rg on 8
intersections, missing 2 to 416 lines each: `std::unique_ptr AND
blink::LocalFrame` (416 lines in 39 files), `DCHECK AND net::URLRequest`
(311 in 20), `-i DCHECK AND net::URLRequest` (278 in 17), `TODO AND
blink::LocalFrame` (248 in 35), `std::vector AND net::URLRequest` (246
in 29), `-i std::vector AND net::URLRequest` (246 in 29),
`CurrencyExtendedData AND UnitsDisplayNames` (2 in 1) and `-i
CurrencyExtendedData AND UnitsDisplayNames` (2 in 1).

elgrep's index build, 61.02 s, ran without watchman's watch of the tree.
With the watch, a cookie file that watchman wrote into `.git` during the
build made eg-indexd build a second time before it answered, about 9 s
more.

### aosp

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 1.52 s | 1.52 s | 1.53 s | 1.56 s | 1.48 s | 1.48 s | 1.62 s | 1.60 s | 1.52 s | 1.53 s | 336.97 MB | 342.59 MB | - | 6.99->10.93! |
| fmgrep | 191.37 s | 12.00 GB | 971 ms | 10.8 ms | 33.2 ms | 13.1 ms | 31.1 ms | 14.9 ms | 15.1 ms | 12.6 ms | 31.5 ms | 11.9 ms | 30.0 ms | 736.41 MB | 10.48 GB | 11.30 GB | 10.93->13.36! |
| fmgrep `--store-docs` | 195.02 s | 29.38 GB | 54.9 ms | 8.90 ms | 28.4 ms | 10.7 ms | 31.9 ms | 15.3 ms | 15.8 ms | 14.5 ms | 28.5 ms | 10.7 ms | 27.1 ms | 705.04 MB | 10.60 GB | 10.36 GB | 13.36->12.67! |
| fmgrep, no server | 200.07 s | 12.00 GB | - | 94.5 ms | 121 ms | 98.4 ms | 120 ms | 102 ms | 103 ms | 103 ms | 118 ms | 96.7 ms | 118 ms | 677.50 MB | 8.47 GB | 10.95 GB | 12.67->12.44! |
| tgrep | 164.73 s | 9.87 GB | 36.04 s | 143 ms | 241 ms | 520 ms | 739 ms | 519 ms | 664 ms | 535 ms | 671 ms | 293 ms | 441 ms | 28.13 GB | 28.19 GB | 2.62 GB | 12.44->1.96! |
| elgrep | 299.62 s | 12.23 GB | 333.62 s | 40.8 ms | 45.6 ms | 53.8 ms | 65.2 ms | 157 ms | 303 ms | 303 ms | 368 ms | 53.8 ms | 134 ms | 113.71 MB | 1.00 GB | 57.74 GB | 1.96->6.86! |

tgrep printed the same lines as rg on all 54 queries.

elgrep's row ran without watchman's watch of the tree. The aosp root
holds no `.git` directory, so watchman writes its cookie files
(`.watchman-cookie-*`) into the root, about once a minute; eg-indexd
counts each as a change. With the watch, every build of the tree (about
5 min) raced some, and the row's first query, which waits for a build
that raced none, still waited after 10 min.

After its first build (299.62 s), eg-indexd scanned its 12 probe searches
over 31.39 s: it then drops the watches of the directories its walk skipped (about
37,000: `.repo` and each project's `.git`), their events overflow the
kernel's inotify queue (16,384 events by default), and it counts every
path as changed until its next build. eg gives "the corpus changed since
the index was published" as its reason, but nothing in the tree
changed: the queue overflowed. The row waited for that before it
timed a query (`ready` 333.62 s); a user would see those scans, about
2.6 s each. No timed query scanned.

elgrep differed from rg on 1 query, missing lines in a file that holds
both sides: `MCK_ AND C_Check` (40 lines missed in 1 file). The file is
indexed; eg 1.0.0
anchors the index at the common parent of its search paths under
`--index-dir` (here `external/`) and drops every candidate, while a scan
finds it, so the miss is intermittent.

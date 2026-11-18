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
  the number to compare with theirs: on aosp 34.67 s, against 936 ms for
  fmgrep and 36.59 s for tgrep. Then one
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
GHz, 123 GiB, local NVMe formatted ext4), Ubuntu 24.04, kernel 7.0 with its
default inotify limits. fmgrep 0.1.0 (release build, with the lines search
that counts each match only under `--stats`, the regex walk that, when
it gives up after walking its anchor, locates its last whole frontier, the
remote client of pull requests 55 to 59, 63 and 64, and the walk back of a
large locate over the search's threads, pull request 62),
ripgrep 15.2.0, tgrep 1.0.8, elgrep 1.0.0, watchman v2026.11.09.00 (the
prebuilt release). Every row of
the three tables comes from one run of `scripts/bench.sh` per tree, all on
one instance, measured 2026-11-18 (UTC), searching the checkout in place.
The page cache was dropped before each tree and the tree read once.
[scripts/ec2-bench.sh](scripts/ec2-bench.sh) runs the benchmark on such
an instance.

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 114 ms | 115 ms | 118 ms | 119 ms | 120 ms | 236 ms | 133 ms | 184 ms | 115 ms | 128 ms | 9.49 MB | 15.17 MB | - | 11.73->11.79! |
| fmgrep | 22.41 s | 1.11 GB | 97.1 ms | 4.08 ms | 5.10 ms | 8.23 ms | 14.6 ms | 7.30 ms | 10.7 ms | 17.9 ms | 19.6 ms | 5.07 ms | 7.44 ms | 194.22 MB | 1.27 GB | 6.29 GB | 11.79->13.46! |
| fmgrep `--store-docs` | 27.37 s | 2.64 GB | 10.0 ms | 5.29 ms | 5.97 ms | 8.35 ms | 12.6 ms | 7.52 ms | 11.2 ms | 15.2 ms | 15.4 ms | 5.93 ms | 7.65 ms | 192.80 MB | 1.27 GB | 6.85 GB | 13.46->11.47! |
| fmgrep, no server | 22.01 s | 1.11 GB | - | 13.1 ms | 14.4 ms | 17.9 ms | 25.2 ms | 16.3 ms | 20.1 ms | 25.8 ms | 30.4 ms | 13.9 ms | 17.0 ms | 187.57 MB | 1.03 GB | 5.99 GB | 11.47->11.10! |
| tgrep | 14.21 s | 1.02 GB | 2.00 s | 22.0 ms | 28.8 ms | 43.1 ms | 61.0 ms | 112 ms | 607 ms | 152 ms | 505 ms | 27.1 ms | 102 ms | 2.20 GB | 2.27 GB | 325.86 MB | 11.10->7.98! |
| elgrep | 27.91 s | 1.39 GB | 28.34 s | 9.81 ms | 11.2 ms | 23.7 ms | 22.8 ms | 39.8 ms | 1.25 s | 54.7 ms | 919 ms | 10.9 ms | 154 ms | 10.45 MB | 392.96 MB | 7.00 GB | 7.98->8.23! |

tgrep printed the same lines as rg on all 121 queries.

elgrep answered 7 of the 121 queries by scanning: its index refuses a
pattern that selects over 30% of the files.

elgrep differed from rg on 6 intersections, missing lines in files that
hold both sides: `static AND devm_platform_ioremap_resource` (15333 lines
missed in 436 files), `int AND kmem_cache_create` (2815 in 26), `-i
return AND kvzalloc` (448 in 6), `-i static AND
devm_platform_ioremap_resource` (294 in 6), `MARGINING AND PERFCOUNTER`
(197 in 1) and `return AND kvzalloc` (63 in 3).
Which intersections it misses, and how much, changes from run to run.

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 494 ms | 511 ms | 500 ms | 514 ms | 484 ms | 508 ms | 514 ms | 526 ms | 503 ms | 513 ms | 90.83 MB | 96.29 MB | - | 7.44->9.28! |
| fmgrep | 35.48 s | 2.22 GB | 105 ms | 24.9 ms | 50.1 ms | 30.1 ms | 64.1 ms | 4.47 ms | 4.83 ms | 10.3 ms | 15.3 ms | 12.8 ms | 40.3 ms | 96.76 MB | 2.31 GB | 9.05 GB | 9.28->14.21! |
| fmgrep `--store-docs` | 41.65 s | 5.06 GB | 21.6 ms | 17.0 ms | 33.4 ms | 19.9 ms | 40.8 ms | 5.26 ms | 5.60 ms | 8.75 ms | 11.8 ms | 10.2 ms | 27.2 ms | 153.30 MB | 2.37 GB | 8.96 GB | 14.21->13.74! |
| fmgrep, no server | 35.62 s | 2.22 GB | - | 51.4 ms | 79.8 ms | 55.4 ms | 90.8 ms | 29.9 ms | 30.1 ms | 34.2 ms | 40.8 ms | 38.0 ms | 68.3 ms | 111.93 MB | 2.19 GB | 9.06 GB | 13.74->16.91! |
| tgrep | 39.94 s | 2.77 GB | 12.72 s | 203 ms | 448 ms | 485 ms | 900 ms | 164 ms | 413 ms | 148 ms | 613 ms | 208 ms | 534 ms | 6.53 GB | 6.55 GB | 1.06 GB | 16.91->6.27! |
| elgrep | 60.13 s | 3.23 GB | 60.56 s | 50.5 ms | 90.0 ms | 53.0 ms | 135 ms | 198 ms | 1.37 s | 79.6 ms | 894 ms | 58.8 ms | 406 ms | 67.43 MB | 605.94 MB | 16.23 GB | 6.27->6.78! |

tgrep printed the same lines as rg on all 54 queries.

elgrep answered 1 of the 54 queries by scanning: its index refuses a
pattern that selects over 30% of the files. It differed from rg on 8
intersections, missing 2 to 1348 lines each: `std::unique_ptr AND
blink::LocalFrame` (1348 lines in 99 files), `TODO AND blink::LocalFrame`
(281 in 43), `std::vector AND net::URLRequest` (132 in 24), `-i
std::vector AND net::URLRequest` (132 in 24), `DCHECK AND
net::URLRequest` (74 in 9), `-i DCHECK AND net::URLRequest` (72 in 11),
`CurrencyExtendedData AND UnitsDisplayNames` (2 in 1) and `-i
CurrencyExtendedData AND UnitsDisplayNames` (2 in 1).

elgrep's index build, 60.13 s, ran without watchman's watch of the tree.
With the watch, a cookie file that watchman wrote into `.git` during the
build made eg-indexd build a second time before it answered, about 9 s
more.

### aosp

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 1.56 s | 1.56 s | 1.58 s | 1.60 s | 1.50 s | 1.50 s | 1.64 s | 1.64 s | 1.56 s | 1.57 s | 301.21 MB | 306.73 MB | - | 6.61->10.26! |
| fmgrep | 195.47 s | 12.00 GB | 936 ms | 10.6 ms | 33.8 ms | 13.2 ms | 33.6 ms | 14.8 ms | 15.4 ms | 14.8 ms | 32.8 ms | 12.4 ms | 30.9 ms | 737.54 MB | 10.54 GB | 12.34 GB | 10.26->13.42! |
| fmgrep `--store-docs` | 207.37 s | 29.38 GB | 58.6 ms | 8.86 ms | 29.7 ms | 10.9 ms | 32.0 ms | 14.7 ms | 18.6 ms | 13.0 ms | 28.2 ms | 10.9 ms | 28.2 ms | 722.04 MB | 10.65 GB | 10.44 GB | 13.42->11.81! |
| fmgrep, no server | 214.15 s | 12.00 GB | - | 99.1 ms | 124 ms | 102 ms | 123 ms | 104 ms | 105 ms | 108 ms | 121 ms | 102 ms | 120 ms | 674.61 MB | 8.47 GB | 10.82 GB | 11.81->10.21! |
| tgrep | 164.29 s | 9.87 GB | 36.59 s | 154 ms | 303 ms | 538 ms | 873 ms | 582 ms | 793 ms | 562 ms | 796 ms | 300 ms | 533 ms | 28.10 GB | 28.15 GB | 2.62 GB | 10.21->2.32! |
| elgrep | 299.71 s | 12.22 GB | 334.38 s | 40.9 ms | 45.5 ms | 53.7 ms | 66.5 ms | 160 ms | 302 ms | 309 ms | 377 ms | 53.7 ms | 136 ms | 113.71 MB | 1.00 GB | 58.64 GB | 2.32->8.24! |

tgrep printed the same lines as rg on all 54 queries.

elgrep's row ran without watchman's watch of the tree. The aosp root
holds no `.git` directory, so watchman writes its cookie files
(`.watchman-cookie-*`) into the root, about once a minute; eg-indexd
counts each as a change. With the watch, every build of the tree (about
5 min) raced some, and the row's first query, which waits for a build
that raced none, still waited after 10 min.

After its first build (299.71 s), eg-indexd scanned its 12 probe searches
over 31.92 s: it then drops the watches of the directories its walk skipped (about
37,000: `.repo` and each project's `.git`), their events overflow the
kernel's inotify queue (16,384 events by default), and it counts every
path as changed until its next build. eg gives "the corpus changed since
the index was published" as its reason, but nothing in the tree
changed: the queue overflowed. The row waited for that before it
timed a query (`ready` 334.38 s); a user would see those scans, about
2.7 s each. No timed query scanned.

elgrep differed from rg on 3 queries, missing lines in files that hold
both sides: `MCK_ AND C_Check` (40 lines missed in 1 file), `ndk::ScopedAStatus
AND android::base::unique_fd` (2 lines in 1 file) and `-i
ndk::ScopedAStatus AND android::base::unique_fd` (2 in 1). For the first,
the file is indexed; eg 1.0.0
anchors the index at the common parent of its search paths under
`--index-dir` (here `external/`) and drops every candidate, while a scan
finds it, so the miss is intermittent.

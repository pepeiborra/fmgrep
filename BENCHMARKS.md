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
  the number to compare with theirs: on aosp 33.76 s, against 940 ms for
  fmgrep and 35.92 s for tgrep. Then one
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
that counts each match only under `--stats`), ripgrep 15.2.0, tgrep 1.0.8,
elgrep 1.0.0, watchman v2026.11.09.00 (the prebuilt release). Every row of
the three tables comes from one run of `scripts/bench.sh` per tree, all on
one instance, measured 2026-11-16 (UTC), searching the checkout in place.
The page cache was dropped before each tree and the tree read once.
[scripts/ec2-bench.sh](scripts/ec2-bench.sh) runs the benchmark on such
an instance.

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 114 ms | 115 ms | 119 ms | 119 ms | 119 ms | 228 ms | 134 ms | 181 ms | 115 ms | 127 ms | 10.18 MB | 15.64 MB | - | 3.73->4.68! |
| fmgrep | 17.90 s | 1.11 GB | 97.0 ms | 3.91 ms | 4.88 ms | 8.26 ms | 17.4 ms | 6.81 ms | 11.6 ms | 17.6 ms | 18.7 ms | 4.59 ms | 7.58 ms | 196.50 MB | 1.27 GB | 6.61 GB | 4.68->7.12! |
| fmgrep `--store-docs` | 21.16 s | 2.64 GB | 9.83 ms | 3.30 ms | 3.90 ms | 6.62 ms | 12.7 ms | 5.40 ms | 15.5 ms | 12.1 ms | 12.8 ms | 3.83 ms | 6.20 ms | 203.02 MB | 1.28 GB | 5.89 GB | 7.12->8.16! |
| fmgrep, no server | 17.49 s | 1.11 GB | - | 13.1 ms | 14.2 ms | 18.1 ms | 28.3 ms | 15.8 ms | 21.9 ms | 29.1 ms | 29.9 ms | 13.1 ms | 17.3 ms | 187.55 MB | 1.03 GB | 7.14 GB | 8.16->10.94! |
| tgrep | 14.31 s | 1.02 GB | 1.89 s | 20.7 ms | 27.5 ms | 41.0 ms | 58.9 ms | 125 ms | 595 ms | 145 ms | 498 ms | 25.4 ms | 99.5 ms | 2.21 GB | 2.27 GB | 295.34 MB | 10.94->8.33! |
| elgrep | 27.61 s | 1.39 GB | 28.03 s | 9.78 ms | 10.7 ms | 23.7 ms | 22.5 ms | 39.7 ms | 1.23 s | 53.6 ms | 918 ms | 10.8 ms | 152 ms | 10.45 MB | 394.91 MB | 7.05 GB | 8.33->6.84! |

tgrep printed the same lines as rg on all 121 queries.

elgrep answered 7 of the 121 queries by scanning: its index refuses a
pattern that selects over 30% of the files.

elgrep differed from rg on 6 intersections, missing lines in files that
hold both sides: `-i static AND devm_platform_ioremap_resource` (48629
lines missed in 1371 files), `-i return AND kvzalloc` (22126 in 237),
`int AND kmem_cache_create` (8662 in 72), `return AND kvzalloc` (275 in
1), `MARGINING AND PERFCOUNTER` (197 in 1) and `static AND
devm_platform_ioremap_resource` (189 in 4). Which intersections it misses,
and how much, changes from run to run.

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 492 ms | 504 ms | 487 ms | 505 ms | 482 ms | 506 ms | 512 ms | 527 ms | 495 ms | 508 ms | 99.06 MB | 104.50 MB | - | 7.66->10.75! |
| fmgrep | 37.33 s | 2.22 GB | 110 ms | 24.1 ms | 48.8 ms | 29.0 ms | 64.5 ms | 4.19 ms | 4.89 ms | 9.76 ms | 15.0 ms | 13.7 ms | 39.6 ms | 120.24 MB | 2.32 GB | 8.07 GB | 10.75->15.97! |
| fmgrep `--store-docs` | 37.97 s | 5.06 GB | 24.0 ms | 15.7 ms | 31.7 ms | 22.1 ms | 40.7 ms | 5.32 ms | 5.78 ms | 8.20 ms | 10.9 ms | 9.59 ms | 26.1 ms | 153.12 MB | 2.37 GB | 8.08 GB | 15.97->15.51! |
| fmgrep, no server | 35.16 s | 2.22 GB | - | 50.3 ms | 78.2 ms | 54.6 ms | 91.4 ms | 28.4 ms | 29.2 ms | 35.1 ms | 40.1 ms | 38.9 ms | 67.3 ms | 111.88 MB | 2.19 GB | 8.75 GB | 15.51->16.51! |
| tgrep | 39.32 s | 2.77 GB | 12.13 s | 195 ms | 435 ms | 521 ms | 869 ms | 155 ms | 404 ms | 143 ms | 585 ms | 201 ms | 517 ms | 6.54 GB | 6.56 GB | 1.07 GB | 16.51->4.45! |
| elgrep | 61.78 s | 3.24 GB | 62.21 s | 48.4 ms | 89.4 ms | 52.7 ms | 134 ms | 190 ms | 1.34 s | 79.1 ms | 882 ms | 55.8 ms | 399 ms | 67.50 MB | 607.44 MB | 16.51 GB | 4.45->6.82! |

tgrep printed the same lines as rg on all 54 queries.

elgrep answered 1 of the 54 queries by scanning: its index refuses a
pattern that selects over 30% of the files. It differed from rg on 8
intersections, missing 2 to 1268 lines each: `std::unique_ptr AND
blink::LocalFrame` (1268 lines in 99 files), `DCHECK AND
net::URLRequest` (297 in 21), `-i DCHECK AND net::URLRequest` (284 in
18), `std::vector AND net::URLRequest` (272 in 33), `-i std::vector AND
net::URLRequest` (272 in 33), `TODO AND blink::LocalFrame` (261 in 38),
`CurrencyExtendedData AND UnitsDisplayNames` (2 in 1) and `-i
CurrencyExtendedData AND UnitsDisplayNames` (2 in 1).

elgrep's index build, 61.78 s, ran without watchman's watch of the tree.
With the watch, a cookie file that watchman wrote into `.git` during the
build made eg-indexd build a second time before it answered, about 9 s
more.

### aosp

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 1.57 s | 1.57 s | 1.57 s | 1.60 s | 1.49 s | 1.50 s | 1.65 s | 1.65 s | 1.57 s | 1.58 s | 326.73 MB | 332.28 MB | - | 6.43->10.79! |
| fmgrep | 198.93 s | 12.00 GB | 940 ms | 9.90 ms | 33.9 ms | 13.4 ms | 63.2 ms | 14.3 ms | 20.6 ms | 14.7 ms | 32.2 ms | 12.0 ms | 36.0 ms | 737.42 MB | 10.50 GB | 11.62 GB | 10.79->13.04! |
| fmgrep `--store-docs` | 195.49 s | 29.38 GB | 58.7 ms | 9.17 ms | 30.5 ms | 10.6 ms | 65.1 ms | 16.8 ms | 57.6 ms | 15.9 ms | 29.0 ms | 10.6 ms | 39.4 ms | 730.62 MB | 10.58 GB | 10.51 GB | 13.04->14.46! |
| fmgrep, no server | 197.06 s | 12.00 GB | - | 105 ms | 132 ms | 107 ms | 161 ms | 109 ms | 116 ms | 110 ms | 126 ms | 107 ms | 133 ms | 674.99 MB | 8.47 GB | 11.01 GB | 14.46->12.68! |
| tgrep | 170.03 s | 9.87 GB | 35.92 s | 157 ms | 247 ms | 455 ms | 740 ms | 565 ms | 703 ms | 580 ms | 685 ms | 307 ms | 453 ms | 28.12 GB | 28.18 GB | 2.57 GB | 12.68->1.57! |
| elgrep | 300.35 s | 12.24 GB | 334.11 s | 39.8 ms | 45.4 ms | 54.8 ms | 64.7 ms | 158 ms | 301 ms | 305 ms | 372 ms | 54.7 ms | 135 ms | 113.72 MB | 1004.52 MB | 58.01 GB | 1.57->8.60! |

tgrep printed the same lines as rg on all 54 queries.

elgrep's row ran without watchman's watch of the tree. The aosp root
holds no `.git` directory, so watchman writes its cookie files
(`.watchman-cookie-*`) into the root, about once a minute; eg-indexd
counts each as a change. With the watch, every build of the tree (about
5 min) raced some, and the row's first query, which waits for a build
that raced none, still waited after 10 min.

After its first build (300.35 s), eg-indexd scanned every query for 31
s: it then drops the watches of the directories its walk skipped (about
37,000: `.repo` and each project's `.git`), their events overflow the
kernel's inotify queue (16,384 events by default), and it counts every
path as changed until its next build. eg gives "the corpus changed since
the index was published" as its reason, but nothing in the tree
changed: the queue overflowed. The row waited for that before it
timed a query (`ready` 334.11 s); a user would see those scans, about
2.5 s each. No timed query scanned.

elgrep differed from rg on 1 query: `MCK_ AND C_Check` (40 lines missed
in 1 file). The file is indexed; eg 1.0.0
anchors the index at the common parent of its search paths under
`--index-dir` (here `external/`) and drops every candidate, while a scan
finds it, so the miss is intermittent.

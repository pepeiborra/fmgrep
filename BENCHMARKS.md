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
  the number to compare with theirs: on aosp 31.81 s, against 932 ms for
  fmgrep and 35.40 s for tgrep. Then one
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
that counts each match only under `--stats`, and the regex walk that, when
it gives up after walking its anchor, locates its last whole frontier),
ripgrep 15.2.0, tgrep 1.0.8, elgrep 1.0.0, watchman v2026.11.09.00 (the
prebuilt release). Every row of
the three tables comes from one run of `scripts/bench.sh` per tree, all on
one instance, measured 2026-11-16 (UTC), searching the checkout in place.
The page cache was dropped before each tree and the tree read once.
[scripts/ec2-bench.sh](scripts/ec2-bench.sh) runs the benchmark on such
an instance.

### Results

### linux

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 114 ms | 115 ms | 118 ms | 118 ms | 121 ms | 229 ms | 133 ms | 181 ms | 114 ms | 127 ms | 10.20 MB | 15.69 MB | - | 8.09->9.12! |
| fmgrep | 21.55 s | 1.11 GB | 99.8 ms | 3.97 ms | 4.95 ms | 8.49 ms | 14.8 ms | 6.65 ms | 11.6 ms | 18.3 ms | 19.1 ms | 4.75 ms | 7.38 ms | 192.69 MB | 1.27 GB | 6.25 GB | 9.12->10.74! |
| fmgrep `--store-docs` | 21.56 s | 2.64 GB | 10.6 ms | 3.41 ms | 4.03 ms | 6.84 ms | 11.0 ms | 5.55 ms | 16.3 ms | 12.6 ms | 12.8 ms | 3.96 ms | 6.17 ms | 185.22 MB | 1.27 GB | 6.56 GB | 10.74->11.22! |
| fmgrep, no server | 17.72 s | 1.11 GB | - | 13.2 ms | 14.5 ms | 18.3 ms | 26.2 ms | 16.9 ms | 22.2 ms | 28.0 ms | 31.1 ms | 14.1 ms | 17.4 ms | 187.61 MB | 1.03 GB | 6.75 GB | 11.22->12.39! |
| tgrep | 14.62 s | 1.02 GB | 1.99 s | 22.4 ms | 28.8 ms | 41.9 ms | 63.1 ms | 114 ms | 609 ms | 146 ms | 515 ms | 27.2 ms | 103 ms | 2.21 GB | 2.26 GB | 296.06 MB | 12.39->9.24! |
| elgrep | 27.86 s | 1.39 GB | 28.28 s | 9.82 ms | 11.1 ms | 23.7 ms | 23.1 ms | 41.8 ms | 1.22 s | 54.7 ms | 915 ms | 10.9 ms | 152 ms | 10.45 MB | 398.17 MB | 7.03 GB | 9.24->9.12! |

tgrep printed the same lines as rg on all 121 queries.

elgrep answered 7 of the 121 queries by scanning: its index refuses a
pattern that selects over 30% of the files.

elgrep differed from rg on 5 intersections, missing lines in files that
hold both sides: `-i return AND kvzalloc` (17018 lines missed in 194
files), `return AND kvzalloc` (11783 in 156), `-i static AND
devm_platform_ioremap_resource` (3314 in 97), `include AND
alloc_pages_node` (594 in 29) and `MARGINING AND PERFCOUNTER` (197 in 1).
Which intersections it misses, and how much, changes from run to run.

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 494 ms | 506 ms | 492 ms | 508 ms | 485 ms | 516 ms | 515 ms | 525 ms | 497 ms | 511 ms | 77.93 MB | 83.31 MB | - | 9.52->10.39! |
| fmgrep | 39.03 s | 2.22 GB | 101 ms | 24.6 ms | 49.5 ms | 29.8 ms | 64.3 ms | 4.50 ms | 5.30 ms | 9.76 ms | 15.1 ms | 14.2 ms | 40.0 ms | 114.73 MB | 2.32 GB | 8.86 GB | 10.39->12.32! |
| fmgrep `--store-docs` | 43.18 s | 5.06 GB | 21.9 ms | 16.0 ms | 32.0 ms | 22.2 ms | 42.0 ms | 5.17 ms | 5.86 ms | 8.88 ms | 11.3 ms | 9.84 ms | 26.6 ms | 152.86 MB | 2.37 GB | 8.90 GB | 12.32->14.15! |
| fmgrep, no server | 42.28 s | 2.22 GB | - | 51.3 ms | 80.3 ms | 58.7 ms | 93.8 ms | 29.7 ms | 30.4 ms | 36.3 ms | 40.6 ms | 40.8 ms | 69.0 ms | 112.17 MB | 2.19 GB | 8.22 GB | 14.15->12.20! |
| tgrep | 39.52 s | 2.77 GB | 12.41 s | 192 ms | 438 ms | 484 ms | 870 ms | 159 ms | 403 ms | 142 ms | 588 ms | 208 ms | 519 ms | 6.53 GB | 6.55 GB | 1.09 GB | 12.20->3.89! |
| elgrep | 59.98 s | 3.23 GB | 60.41 s | 47.3 ms | 87.4 ms | 51.7 ms | 132 ms | 195 ms | 1.36 s | 82.5 ms | 891 ms | 59.9 ms | 402 ms | 67.48 MB | 618.80 MB | 15.91 GB | 3.89->6.37! |

tgrep printed the same lines as rg on all 54 queries.

elgrep answered 1 of the 54 queries by scanning: its index refuses a
pattern that selects over 30% of the files. It differed from rg on 8
intersections, missing 2 to 1162 lines each: `std::unique_ptr AND
blink::LocalFrame` (1162 lines in 79 files), `TODO AND
blink::LocalFrame` (297 in 47), `DCHECK AND net::URLRequest` (291 in
20), `-i DCHECK AND net::URLRequest` (285 in 20), `std::vector AND
net::URLRequest` (244 in 27), `-i std::vector AND net::URLRequest` (244
in 27), `CurrencyExtendedData AND UnitsDisplayNames` (2 in 1) and `-i
CurrencyExtendedData AND UnitsDisplayNames` (2 in 1).

elgrep's index build, 59.98 s, ran without watchman's watch of the tree.
With the watch, a cookie file that watchman wrote into `.git` during the
build made eg-indexd build a second time before it answered, about 9 s
more.

### aosp

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 1.57 s | 1.57 s | 1.56 s | 1.60 s | 1.51 s | 1.51 s | 1.66 s | 1.65 s | 1.57 s | 1.58 s | 319.95 MB | 325.51 MB | - | 7.04->10.89! |
| fmgrep | 198.74 s | 12.00 GB | 932 ms | 10.3 ms | 34.9 ms | 12.5 ms | 54.8 ms | 14.7 ms | 20.5 ms | 13.2 ms | 30.7 ms | 11.6 ms | 35.1 ms | 732.78 MB | 10.54 GB | 10.13 GB | 10.89->12.16! |
| fmgrep `--store-docs` | 200.73 s | 29.38 GB | 53.7 ms | 8.41 ms | 28.8 ms | 10.4 ms | 53.6 ms | 14.9 ms | 55.3 ms | 14.0 ms | 26.8 ms | 10.4 ms | 36.1 ms | 719.17 MB | 10.63 GB | 10.09 GB | 12.16->12.25! |
| fmgrep, no server | 201.54 s | 12.00 GB | - | 97.1 ms | 121 ms | 97.9 ms | 142 ms | 102 ms | 108 ms | 104 ms | 118 ms | 97.9 ms | 122 ms | 677.02 MB | 8.47 GB | 11.20 GB | 12.25->11.60! |
| tgrep | 165.34 s | 9.87 GB | 35.40 s | 145 ms | 241 ms | 459 ms | 715 ms | 538 ms | 675 ms | 506 ms | 649 ms | 285 ms | 436 ms | 28.15 GB | 28.21 GB | 2.63 GB | 11.60->1.50! |
| elgrep | 298.66 s | 12.23 GB | 330.47 s | 39.7 ms | 47.2 ms | 54.7 ms | 66.3 ms | 160 ms | 304 ms | 303 ms | 368 ms | 54.7 ms | 136 ms | 113.71 MB | 1002.11 MB | 57.92 GB | 1.50->8.08! |

tgrep printed the same lines as rg on all 54 queries.

elgrep's row ran without watchman's watch of the tree. The aosp root
holds no `.git` directory, so watchman writes its cookie files
(`.watchman-cookie-*`) into the root, about once a minute; eg-indexd
counts each as a change. With the watch, every build of the tree (about
5 min) raced some, and the row's first query, which waits for a build
that raced none, still waited after 10 min.

After its first build (298.66 s), eg-indexd scanned every query for 29
s: it then drops the watches of the directories its walk skipped (about
37,000: `.repo` and each project's `.git`), their events overflow the
kernel's inotify queue (16,384 events by default), and it counts every
path as changed until its next build. eg gives "the corpus changed since
the index was published" as its reason, but nothing in the tree
changed: the queue overflowed. The row waited for that before it
timed a query (`ready` 330.47 s); a user would see those scans, about
2.5 s each. No timed query scanned.

elgrep differed from rg on 1 query: `MCK_ AND C_Check` (40 lines missed
in 1 file). The file is indexed; eg 1.0.0
anchors the index at the common parent of its search paths under
`--index-dir` (here `external/`) and drops every candidate, while a scan
finds it, so the miss is intermittent.

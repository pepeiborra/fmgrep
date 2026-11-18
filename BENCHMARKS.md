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
  the number to compare with theirs: on aosp 32.26 s, against 936 ms for
  fmgrep and 35.88 s for tgrep. Then one
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
it gives up after walking its anchor, locates its last whole frontier, and the
remote client of pull requests 55 to 59, 63 and 64),
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
| rg | - | - | - | 113 ms | 114 ms | 115 ms | 117 ms | 119 ms | 228 ms | 131 ms | 182 ms | 113 ms | 126 ms | 10.27 MB | 15.87 MB | - | 5.02->5.75! |
| fmgrep | 21.11 s | 1.11 GB | 100 ms | 4.06 ms | 5.05 ms | 8.88 ms | 14.5 ms | 7.23 ms | 11.7 ms | 18.6 ms | 19.4 ms | 4.89 ms | 7.45 ms | 190.53 MB | 1.27 GB | 5.82 GB | 5.75->8.10! |
| fmgrep `--store-docs` | 23.55 s | 2.64 GB | 10.0 ms | 5.11 ms | 5.60 ms | 8.30 ms | 12.6 ms | 6.79 ms | 12.4 ms | 14.9 ms | 15.0 ms | 5.42 ms | 7.42 ms | 184.53 MB | 1.28 GB | 6.89 GB | 8.10->8.89! |
| fmgrep, no server | 21.62 s | 1.11 GB | - | 12.3 ms | 14.0 ms | 18.4 ms | 24.2 ms | 15.2 ms | 21.4 ms | 28.1 ms | 30.3 ms | 13.3 ms | 16.7 ms | 187.79 MB | 1.03 GB | 6.38 GB | 8.89->9.40! |
| tgrep | 14.16 s | 1.02 GB | 1.89 s | 21.0 ms | 27.7 ms | 40.5 ms | 58.4 ms | 108 ms | 595 ms | 143 ms | 500 ms | 25.9 ms | 99.7 ms | 2.21 GB | 2.26 GB | 299.51 MB | 9.40->6.16! |
| elgrep | 28.01 s | 1.39 GB | 28.44 s | 9.79 ms | 10.9 ms | 23.7 ms | 22.4 ms | 41.8 ms | 1.23 s | 52.5 ms | 920 ms | 10.9 ms | 153 ms | 10.45 MB | 397.44 MB | 7.10 GB | 6.16->7.04! |

tgrep printed the same lines as rg on all 121 queries.

elgrep answered 7 of the 121 queries by scanning: its index refuses a
pattern that selects over 30% of the files.

elgrep differed from rg on 5 intersections, missing lines in files that
hold both sides: `static AND devm_platform_ioremap_resource` (7432 lines
missed in 245 files), `-i static AND devm_platform_ioremap_resource`
(15708 in 351), `int AND kmem_cache_create` (2500 in 23), `return AND
kvzalloc` (1593 in 9) and `MARGINING AND PERFCOUNTER` (197 in 1).
Which intersections it misses, and how much, changes from run to run.

### chromium

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 496 ms | 509 ms | 493 ms | 509 ms | 481 ms | 509 ms | 516 ms | 531 ms | 499 ms | 512 ms | 84.43 MB | 89.92 MB | - | 8.06->9.19! |
| fmgrep | 39.28 s | 2.22 GB | 102 ms | 24.3 ms | 48.9 ms | 29.6 ms | 63.5 ms | 4.15 ms | 5.14 ms | 9.36 ms | 15.1 ms | 14.4 ms | 39.6 ms | 121.63 MB | 2.31 GB | 9.13 GB | 9.19->14.95! |
| fmgrep `--store-docs` | 37.04 s | 5.06 GB | 21.0 ms | 16.1 ms | 32.9 ms | 22.0 ms | 41.8 ms | 5.43 ms | 5.96 ms | 8.72 ms | 11.1 ms | 10.6 ms | 27.0 ms | 156.51 MB | 2.37 GB | 9.15 GB | 14.95->15.68! |
| fmgrep, no server | 38.34 s | 2.22 GB | - | 51.1 ms | 80.2 ms | 55.4 ms | 93.3 ms | 28.5 ms | 29.2 ms | 35.0 ms | 39.9 ms | 40.0 ms | 68.6 ms | 112.30 MB | 2.19 GB | 8.85 GB | 15.68->14.72! |
| tgrep | 39.15 s | 2.77 GB | 11.93 s | 194 ms | 434 ms | 461 ms | 850 ms | 164 ms | 399 ms | 144 ms | 588 ms | 199 ms | 513 ms | 6.54 GB | 6.56 GB | 1.06 GB | 14.72->4.37! |
| elgrep | 60.60 s | 3.23 GB | 61.03 s | 47.4 ms | 90.0 ms | 52.9 ms | 134 ms | 191 ms | 1.36 s | 76.0 ms | 880 ms | 61.1 ms | 401 ms | 67.46 MB | 612.41 MB | 16.49 GB | 4.37->6.99! |

tgrep printed the same lines as rg on all 54 queries.

elgrep answered 1 of the 54 queries by scanning: its index refuses a
pattern that selects over 30% of the files. It differed from rg on 8
intersections, missing 2 to 1337 lines each: `std::unique_ptr AND
blink::LocalFrame` (1337 lines in 90 files), `-i DCHECK AND
net::URLRequest` (286 in 19), `std::vector AND net::URLRequest` (252 in
30), `-i std::vector AND net::URLRequest` (252 in 30), `TODO AND
blink::LocalFrame` (215 in 29), `DCHECK AND net::URLRequest` (69 in 11),
`CurrencyExtendedData AND UnitsDisplayNames` (2 in 1) and `-i
CurrencyExtendedData AND UnitsDisplayNames` (2 in 1).

elgrep's index build, 60.60 s, ran without watchman's watch of the tree.
With the watch, a cookie file that watchman wrote into `.git` during the
build made eg-indexd build a second time before it answered, about 9 s
more.

### aosp

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | -i p50 | -i mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 1.55 s | 1.55 s | 1.58 s | 1.60 s | 1.49 s | 1.50 s | 1.64 s | 1.63 s | 1.55 s | 1.56 s | 296.50 MB | 302.02 MB | - | 7.19->10.72! |
| fmgrep | 192.46 s | 12.00 GB | 936 ms | 10.1 ms | 34.3 ms | 13.3 ms | 54.4 ms | 13.7 ms | 20.8 ms | 13.2 ms | 31.5 ms | 11.1 ms | 34.9 ms | 762.08 MB | 10.51 GB | 11.90 GB | 10.72->15.88! |
| fmgrep `--store-docs` | 189.38 s | 29.38 GB | 56.0 ms | 8.83 ms | 28.7 ms | 10.5 ms | 53.2 ms | 14.5 ms | 55.9 ms | 13.5 ms | 27.4 ms | 10.5 ms | 36.1 ms | 721.87 MB | 10.59 GB | 10.63 GB | 15.88->14.20! |
| fmgrep, no server | 185.84 s | 12.00 GB | - | 97.1 ms | 120 ms | 99.7 ms | 142 ms | 101 ms | 107 ms | 103 ms | 119 ms | 97.8 ms | 121 ms | 676.85 MB | 8.47 GB | 11.83 GB | 14.20->13.99! |
| tgrep | 163.27 s | 9.86 GB | 35.88 s | 159 ms | 266 ms | 531 ms | 730 ms | 547 ms | 694 ms | 521 ms | 704 ms | 291 ms | 463 ms | 28.07 GB | 28.13 GB | 2.63 GB | 13.99->3.50! |
| elgrep | 302.50 s | 12.24 GB | 334.76 s | 39.8 ms | 45.6 ms | 53.8 ms | 65.8 ms | 158 ms | 300 ms | 303 ms | 368 ms | 53.8 ms | 134 ms | 113.71 MB | 997.05 MB | 60.32 GB | 3.50->7.74! |

tgrep printed the same lines as rg on all 54 queries.

elgrep's row ran without watchman's watch of the tree. The aosp root
holds no `.git` directory, so watchman writes its cookie files
(`.watchman-cookie-*`) into the root, about once a minute; eg-indexd
counts each as a change. With the watch, every build of the tree (about
5 min) raced some, and the row's first query, which waits for a build
that raced none, still waited after 10 min.

After its first build (302.50 s), eg-indexd scanned its 11 probe searches
over 29.54 s: it then drops the watches of the directories its walk skipped (about
37,000: `.repo` and each project's `.git`), their events overflow the
kernel's inotify queue (16,384 events by default), and it counts every
path as changed until its next build. eg gives "the corpus changed since
the index was published" as its reason, but nothing in the tree
changed: the queue overflowed. The row waited for that before it
timed a query (`ready` 334.76 s); a user would see those scans, about
2.7 s each. No timed query scanned.

elgrep differed from rg on 3 queries, missing lines in files that hold
both sides: `@SystemApi AND @RequiresPermission` (4344 lines missed in
179 files), `-i @SystemApi AND @RequiresPermission` (4368 in 181) and
`MCK_ AND C_Check` (40 lines in 1 file). For the last, the file is indexed; eg 1.0.0
anchors the index at the common parent of its search paths under
`--index-dir` (here `external/`) and drops every candidate, while a scan
finds it, so the miss is intermittent.

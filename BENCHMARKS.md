# Benchmarks

fmgrep against ripgrep on three pinned source trees, on one machine.

## Method

Every run is `scripts/bench.sh` ([docs/benchmark.md](docs/benchmark.md)).
Every row searches the same staged copy of the corpus, one query at a time,
and a run fails unless all rows agree on every query's match and file
counts.

- **rg**: one `rg --count-matches` process per query.
- **fmgrep**: an index (`index build`, `index size`), one `fmgrep serve`
  (`ready`), then one `fmgrep --server` process per query.
- **tgrep**, when installed: `tgrep index`, one `tgrep serve --no-watch`,
  then one `tgrep --count-matches` client per query.

Latency is spawn to exit, process start-up included. `p50` is the lower
median over the literal or the regex queries, beside their mean. `query
footprint` is the peak anonymous memory of the process answering (the
server, or each rg); `index peak RSS` is the index build's peak resident
memory. `load1` is the 1-minute load average at the row's start and end
(the harness marks 4 or more with `!`). Each cell is the median of 3 runs
(chromium: 1 run). MB and GB are 2^20 and 2^30 bytes.

Each run started with load1 below 3 and no other build or benchmark
running, except where noted; rg keeps every core busy, so the rows after it
start at a load1 of 4 to 12. The linux checkout sits on a case-insensitive
file system, where 13 files of the pinned tree collide; every row sees the
same 95540 files.

Machine: Apple M5, 10 cores (4P + 6E), 16 GiB, macOS 26.5.1. fmgrep 0.1.0
(release build), ripgrep 15.2.0. Measured 2026-09-24 (UTC).

## GHC

26787 files, 84.67 MB, at ghc `4cf7628`; 35 queries (30 literal, 5 regex).

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | query footprint | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 448 ms | 449 ms | 447 ms | 448 ms | 77.55 MB | - | 1.85->4.15, 2.84->4.80, 3.08->5.86 |
| fmgrep | 1.71 s | 112.66 MB | 21.7 ms | 2.76 ms | 2.94 ms | 9.43 ms | 8.65 ms | 12.63 MB | 713.14 MB | 4.15->4.15, 4.80->4.65, 5.86->5.63 |
| fmgrep `--store-docs` | 2.17 s | 197.35 MB | 4.03 ms | 2.77 ms | 2.96 ms | 7.24 ms | 6.90 ms | 12.80 MB | 720.00 MB | 4.15->4.15, 4.65->4.65, 5.63->5.63 |

The slowest fmgrep queries, about 11 ms, are the regexes whose candidate
files must all be read and verified: `/\{-# INLINE \w+ #-\}/` (3102
matches in 426 files) and `/stg_\w+_info/` (1084 matches in 87 files).

## linux

95540 files, 1.53 GB, at linux `5878583`; 102 queries (92 literal, 10
regex). Runs 2 and 3 started after a 30-minute wait with another checkout's
idle `cargo test` (0% CPU) still present, at load1 1.7 and 1.3.

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | query footprint | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 1.96 s | 2.06 s | 2.29 s | 2.28 s | 445.84 MB | - | 3.15->6.90, 2.07->11.76, 1.45->5.84 |
| fmgrep | 12.94 s | 1.89 GB | 69.4 ms | 4.50 ms | 13.4 ms | 5.09 ms | 21.1 ms | 21.94 MB | 5.23 GB | 6.90->7.83, 11.76->11.76, 5.84->7.68 |
| fmgrep `--store-docs` | 12.91 s | 3.42 GB | 19.5 ms | 4.72 ms | 12.9 ms | 13.7 ms | 16.5 ms | 22.70 MB | 5.14 GB | 7.83->7.96, 11.76->10.63, 7.68->8.39 |

Run 2 ran at load1 near 12, and its rg row is 40% slower than the other two.

## chromium

460957 files, 2.84 GB, at chromium `e2c5a77`; 35 queries (30 literal, 5
regex). One run, rows rg and fmgrep only. It started after a 30-minute wait
with the same idle process present, at load1 1.35, and the fmgrep row ended
at load1 14.9, so treat its timings as indicative.

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | query footprint | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rg | - | - | - | 12.62 s | 13.87 s | 13.87 s | 13.87 s | 789.49 MB | - | 3.03->6.64 |
| fmgrep | 71.65 s | 3.73 GB | 2.24 s | 64.3 ms | 315 ms | 2.63 s | 3.62 s | 62.50 MB | 5.97 GB | 6.64->14.92 |

Here the files read from the tree dominate. `ready` is the stat of every
file before the first answer. The first query, `base::BindOnce` (55196
matches in 14079 files), took 3.47 s against a cold cache. The regexes
verify every candidate file: `/#include "base/\w+\.h"/` reads 46770 of
them and took 8.82 s, against rg's 13.85 s. `index --store-docs` avoids
these reads for about 2.84 GB more index (not measured here).

## tgrep

[tgrep](https://github.com/microsoft/tgrep), a trigram-index grep, is an
optional row ([Method](#method); install:
[docs/benchmark.md](docs/benchmark.md)). Same machine, tgrep 1.0.8 (commit
`b1d0fc2`), measured 2026-09-25 (UTC) on a shared box, so treat the timings
as indicative. The fmgrep rows are from the same runs.

GHC, median of 3 runs:

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | query footprint | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| fmgrep | 2.02 s | 112.66 MB | 36.1 ms | 2.87 ms | 3.11 ms | 13.3 ms | 11.9 ms | 13.05 MB | 710.98 MB | 6.72->6.72, 6.92->6.77, 6.36->6.33 |
| tgrep | 1.34 s | 87.62 MB | 159 ms | 5.53 ms | 6.57 ms | 12.3 ms | 20.1 ms | 109.44 MB | 126.38 MB | 6.72->6.72, 6.77->6.77, 6.33->6.33 |

linux, single run:

| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | query footprint | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| fmgrep | 15.84 s | 1.89 GB | 70.4 ms | 4.49 ms | 12.1 ms | 5.17 ms | 22.2 ms | 21.38 MB | 5.32 GB | 6.50->7.69 |
| tgrep | 12.10 s | 1.02 GB | 537 ms | 17.3 ms | 24.3 ms | 26.0 ms | 32.9 ms | 1.25 GB | 266.48 MB | 7.69->6.87 |

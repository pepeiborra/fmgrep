# Benchmarks

fmgrep against ripgrep on three pinned source trees, on one machine.

## Method

Every run is `scripts/bench.sh` with `--verify`; [docs/benchmark.md](docs/benchmark.md)
explains how to run it and what each column means. In short:

- Both rows search the same staged copy of the corpus, one query at a time.
  rg runs one `rg --count-matches` process per query. fmgrep builds its
  index, starts one `fmgrep serve`, and runs one `fmgrep --server` process
  per query. Latency is spawn to exit, process start-up included.
- `--verify` passed on every run: all rows agreed on every query's match
  and file counts.
- `p50` is the lower median over the queries. Each cell below is the
  median of 3 runs (chromium: 1 run). MB and GB are 2^20 and 2^30 bytes.
- Each run started with load1 below 3 and no other build or benchmark
  running, except where noted. rg keeps every core busy, so the rows after
  it start at a load1 of 4 to 12; `load1` lists each run's start and end.
- The linux checkout sits on a case-insensitive file system, where 13 files
  of the pinned tree collide; both rows see the same 95540 files.

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

[tgrep](https://github.com/microsoft/tgrep) (a trigram-index grep, at
commit `b1d0fc2`) is not a harness row. These figures were measured
locally with an earlier version of the harness, not in CI: Apple M5,
2026-09-24, quiet box, median of 3. p50 is per query; `fp` is the query
footprint; `idx` is index bytes over text bytes. The fmgrep index then
kept a copy of the documents, as `--store-docs` does now, and `idx`
includes it.

| row | GHC p50 / fp / idx | linux p50 / fp / idx / ready | chromium p50 / fp / idx / ready / build |
|---|---|---|---|
| fmgrep | 2.37 ms / 13 MB / 2.33x | 4.24 ms / 25 MB / 2.24x / 10 ms | 34.0 ms / 68 MB / 2.31x / 38 ms / 71 s |
| tgrep | 5.70 ms / 114 MB / 1.03x | 14.9 ms / 1.35 GB / 0.67x / 0.59 s | 451 ms / 3.9 GB / 0.95x / 9.6 s / 47 s |

Other FM-index engines measured the same way are in
[docs/design/history.md](docs/design/history.md).

## Projection to 100x chromium

Not measured: a linear extrapolation of the chromium run above to a tree
of 284 GB (305 x 10^9 bytes) in 46 million files, assuming each figure scales
with the quantity named.

| figure | scales with | projection |
|---|---|---|
| index size | text | 373 GB, or 657 GB with `--store-docs` |
| index build | text (40 MB/s here) | about 2 hours |
| serve footprint | files (a stamp and a path each) | about 6.1 GB |
| serve ready | files (one stat each) | about 4 minutes |
| rg per query | text; a lower bound, as 284 GB is not cached | about 21 minutes |

Search latency is not projected: every term probes every shard, so
per-term work grows with the shard count, and verification grows with the
candidate files each query reads.

# Benchmark

How to measure fmgrep against ripgrep on a pinned corpus and read the result.

Measured results are in [BENCHMARKS.md](../BENCHMARKS.md).

## Running

```sh
scripts/bench.sh                          # GHC, cloned into target/bench/ghc
scripts/bench.sh --corpus linux --repo-path ~/src/linux
scripts/bench.sh --results bench.md -- --rows rg,fmgrep,fmgrep-store-docs
```

`scripts/bench.sh` needs `git`, `rg` and `cargo`. Without `--repo-path` it
shallow-clones the commit the corpus is pinned to and reuses the clone while
it is clean. It builds release binaries (`--skip-build` skips that), runs the
harness with `--verify`, and prints the markdown results, by default
`target/bench/results-<corpus>.md`. Arguments after `--` go to the harness.
It exits non-zero when rg and fmgrep disagree on any count.

A `--repo-path` checkout at another commit still runs, with a warning: its
numbers are not comparable with the pinned set.

## What is measured

Both rows search the same staged copy of the corpus, one query at a time.

- **rg** runs one `rg --count-matches` process per query.
- **fmgrep** builds an index (`index build`, `index size`), starts one
  `fmgrep serve` (`ready`), then runs one `fmgrep --server` process per query.

Latency is spawn to exit, so fmgrep's includes process start-up and one
request. The summary reports the median (lower median, `p50`) and mean
separately for literal and regex queries. `query footprint` is the peak
anonymous memory of the process answering the queries (the fmgrep server, or
each rg); `index peak RSS` is the peak
resident memory of the index build. `load1` is the 1-minute load average at
the row's start and end; `!` marks 4 or more, where timings are likely
inflated. The per-query table lists every query's match and file counts and
both latencies.

## Queries

Each corpus's queries and pinned commit are in
`crates/bench/queries/<corpus>.json`; `/.../` entries are regexes. To add a
corpus, add a query file and register the corpus in
`crates/bench/src/corpora.rs` and `crates/bench/src/terms.rs`.

## Running the harness directly

```sh
cargo build --release -p fmgrep -p bench
FMGREP_CORPUS_GHC=/path/to/ghc target/release/bench --verify --results out.md
```

`target/release/bench --help` lists every flag. Environment:

| variable | meaning |
|---|---|
| `FMGREP_CORPUS_GHC`, `FMGREP_CORPUS_LINUX`, `FMGREP_CORPUS_CHROMIUM` | corpus checkout; default `target/bench/<corpus>`, and a set path must exist |
| `FMGREP_BENCH_BIN` | fmgrep binary; default next to `bench`, then `PATH` |
| `FMGREP_BENCH_RG_BIN` | rg binary; default `rg` on `PATH` |
| `FMGREP_BENCH_CHILD_TIMEOUT_SECS` | per-child timeout, default 300 |
| `FMGREP_BENCH_RSS_OFF`, `FMGREP_BENCH_RSS_INTERVAL_MS` | memory sampling off, or its interval |

The harness refuses to run fmgrep binaries older than their sources
(`--allow-stale` overrides) and a corpus that does not fit on the staging
volume.

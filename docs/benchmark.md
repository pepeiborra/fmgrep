# Benchmark

How to run the benchmark of fmgrep against ripgrep on a pinned corpus.

What each row and column means, and the measured results, are in
[BENCHMARKS.md](../BENCHMARKS.md#method).

## Running

```sh
scripts/bench.sh                          # GHC, cloned into target/bench/ghc
scripts/bench.sh --corpus linux --repo-path ~/src/linux
scripts/bench.sh --results bench.md -- --rows rg,fmgrep,fmgrep-store-docs
```

`scripts/bench.sh` needs `git`, `rg` and `cargo`. Without `--repo-path` it
shallow-clones the commit the corpus is pinned to and reuses the clone while
it is clean; a `--repo-path` checkout at another commit runs with a warning
that its numbers are not comparable. It builds release binaries, runs the
harness and prints its markdown, by default
`target/bench/results-<corpus>.md`, with a JSON line per query, stamped
with its kind, and per row in `target/bench/trace-<corpus>.jsonl`. Every
row prints each query's matching lines, and the harness exits 1 unless all
rows print the same lines. Arguments after `--` go to the harness;
`target/release/bench --help` lists them. The corpus checkout is
`FMGREP_CORPUS_GHC`, `FMGREP_CORPUS_LINUX` or `FMGREP_CORPUS_CHROMIUM`,
by default `target/bench/<corpus>`; the harness exits 2 when it is missing.
`--fixture` searches one file of the query set's literals instead, as a smoke
test.

## tgrep (optional)

The tgrep row runs Microsoft's indexed-search
[tgrep](https://github.com/microsoft/tgrep): `FMGREP_BENCH_TGREP_BIN`, else the
first `tgrep` on `PATH`, provided `tgrep index --help` and `tgrep serve --help`
show `--index-path` and `--no-watch`. Otherwise the row is skipped, with the
reason on stderr and in the results. CI does not install it. Build it from
source: `cargo install --locked --git https://github.com/microsoft/tgrep
tgrep-cli`, or clone that repository and `cargo build --release`. Do not
`cargo install tgrep`: the crates.io crate named `tgrep` is an unrelated grep
clone without an index.

## Queries

Each corpus's queries and pinned commit are in
`crates/bench/queries/<corpus>.json`; `/.../` entries are regexes and
`A AND B` entries intersections. To add a corpus, add a query file and name
it in `crates/bench/src/main.rs`.

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
`target/bench/results-<corpus>.md`, with a JSON line per query and per row
in `target/bench/trace-<corpus>.jsonl`. Arguments after `--` go to the
harness; `target/release/bench --help` lists them. The corpus checkout is
`FMGREP_CORPUS_GHC`, `FMGREP_CORPUS_LINUX` or `FMGREP_CORPUS_CHROMIUM`,
by default `target/bench/<corpus>`; the harness exits 2 when it is missing.
`--fixture` searches one file of the query set's literals instead, as a smoke
test.

## tgrep (optional)

The tgrep row runs when [tgrep](https://github.com/microsoft/tgrep) is on
`PATH` and `tgrep --version` and `tgrep serve --help` succeed; otherwise the
results say why on one line. CI does not install it. To add it:
`cargo install --locked --git https://github.com/microsoft/tgrep tgrep-cli`,
or `brew install tgrep`. The crates.io crate named `tgrep` is another program.

## Queries

Each corpus's queries and pinned commit are in
`crates/bench/queries/<corpus>.json`; `/.../` entries are regexes. To add a
corpus, add a query file and name it in `crates/bench/src/main.rs`.

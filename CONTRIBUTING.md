# Contributing

Bug reports, fixes and measurements are welcome.

## Before a pull request

- `scripts/check.sh` (format, clippy, tests) must pass; CI runs it on Linux
  and macOS.
- Update the docs with the code. `crates/bench/tests/docs.rs` fails when an
  `FMGREP_*` variable is missing from [docs/tuning.md](docs/tuning.md), a
  manifest field from [docs/index-format.md](docs/index-format.md), or a
  relative link points nowhere.
- A change to search output runs `scripts/rg-oracle.sh`, which diffs fmgrep
  against rg, with and without `--store-docs`, and with `--corpus DIR` on a
  GHC checkout; `--serve` repeats every case through `--server`. It needs
  `rg` (and `jq` for `--corpus`), so it is not part of `scripts/check.sh`.

## Performance

The Benchmark workflow runs GHC against ripgrep on every pull request and
posts the table as the job summary. Shared runners are noisy, so it gates
nothing. A pull request that claims a speedup includes a `scripts/bench.sh`
table from a quiet machine, before and after; see
[docs/benchmark.md](docs/benchmark.md). For the optional tgrep row, build
Microsoft's tgrep from https://github.com/microsoft/tgrep; `cargo install
tgrep` installs an unrelated crate, which the harness skips.

## License

Contributions are licensed under the MIT license, as the project is.

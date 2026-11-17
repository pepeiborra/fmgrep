# Contributing

Bug reports, fixes and measurements are welcome.

## Before a pull request

- `scripts/check.sh` (format, clippy, tests) must pass; CI runs it on Linux
  and macOS.
- A test that needs watchman starts a private daemon of its own, from
  `$FMGREP_TEST_WATCHMAN` or else `watchman` on `PATH`, and never uses
  yours. Without watchman it prints one `skipped` line. CI's Watchman job
  runs these tests on Linux, then `scripts/watchman-race.sh`, then
  `scripts/watchman-spawn.sh`, which starts the runner's own daemon and so
  runs only where `CI=true`.
- Tests of git checkouts need `git` on `PATH`. They build real
  repositories in temporary directories, with `GIT_CONFIG_GLOBAL` and
  `GIT_CONFIG_SYSTEM` set to empty files and `HOME` and
  `XDG_CONFIG_HOME` set to temporary directories, so that your git
  config and global excludes file never reach them.
- `fmgrep serve` asks watchman before each query. A test that starts a
  server of a tree it does not change passes `--no-watch`; a test of what a
  server sees change asks a private daemon.
- Update the docs with the code. `crates/bench/tests/docs.rs` fails when an
  `FMGREP_*` variable is missing from [docs/tuning.md](docs/tuning.md), a
  manifest field from [docs/index-format.md](docs/index-format.md), or a
  relative link points nowhere.
- A change to search output runs `scripts/rg-oracle.sh`, which diffs fmgrep
  against rg, with and without `--store-docs`, and with `--corpus DIR` on a
  GHC checkout; `--serve` repeats every case through `--server`, with a
  private watchman daemon when it finds one as the tests do; `--remote`
  runs every case from a clone of the tree through a server of its
  `--store-docs` index, with the stale pass made in the clone. It needs
  `rg` (and `jq` for `--corpus`), so it is not part of `scripts/check.sh`.
- To run CI on a commit without a pull request, push it to a branch whose
  name starts with `ci/`, or start the CI workflow from the Actions tab.
  The run has every job of a pull request's CI run.

## Performance

The Benchmark workflow runs GHC against ripgrep on every pull request and
posts the table as the job summary. Shared runners are noisy, so it gates
nothing. A pull request that claims a speedup includes a `scripts/bench.sh`
table from a quiet machine, before and after; see
[docs/benchmark.md](docs/benchmark.md). For the optional tgrep row, build
Microsoft's tgrep from https://github.com/microsoft/tgrep; `cargo install
tgrep` installs an unrelated crate, which the harness skips. For the
optional elgrep row, `cargo install --locked elgrep --version 1.0.0`; its
index answers only on Linux.

## License

Contributions are licensed under the MIT license, as the project is.

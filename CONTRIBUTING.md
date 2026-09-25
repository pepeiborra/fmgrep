# Contributing

Bug reports, fixes and measurements are welcome.

## Before a pull request

`scripts/check.sh` must pass. It runs what CI runs on Linux and macOS:
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked
-- -D warnings` and `cargo test --workspace --locked`.

- Keep a pull request to one change, with tests named for the behaviour
  they pin.
- Update the docs with the code. `crates/bench/tests/docs.rs` fails when an
  `FMGREP_*` variable is missing from [docs/tuning.md](docs/tuning.md), a
  manifest field from [docs/index-format.md](docs/index-format.md), or a
  relative link points nowhere.
- A change to the index bytes bumps the manifest format and says so in the
  pull request.
- Comments state what the code guarantees, briefly; history belongs in the
  commit message.
- A change to search output runs `scripts/rg-oracle.sh`,
  `scripts/rg-oracle.sh --store-docs` and `scripts/rg-bool-oracle.sh`,
  which diff fmgrep against rg. They need `rg` (and `jq`), so they are not
  part of `scripts/check.sh`.
- New dependencies need a reason in the pull request.

## Performance

The Benchmark workflow runs GHC against ripgrep on every pull request and
posts the table as the job summary. Shared runners are noisy, so it gates
nothing. A pull request that claims a speedup includes a `scripts/bench.sh`
table from a quiet machine, before and after; see
[docs/benchmark.md](docs/benchmark.md).

## License

Contributions are licensed under the MIT license, as the project is.

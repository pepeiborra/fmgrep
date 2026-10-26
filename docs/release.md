# Releasing

How to publish docfm and fmgrep to crates.io.

## First upload

Once, before the first release:

1. Create the GitHub repository and make `workspace.package.repository`
   in `Cargo.toml` its URL.
2. Push `main`. Enable Actions if the repository has them off, and watch
   the first `ci.yml` run to green on both Linux and macOS.
3. Dispatch `bench.yml` once on `main` (Actions, Benchmark, Run workflow), so
   pull requests start from a warm cache.
4. Enable private vulnerability reporting (Settings, Security):
   `SECURITY.md` points reporters to it.

## Each release

1. Version. Set `workspace.package.version` in `Cargo.toml` and the
   `version` of fmgrep's `docfm` dependency to the same value. Check
   `workspace.package.repository`.

2. Gates, all clean:

   ```sh
   scripts/check.sh
   RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
   cargo package --list -p docfm
   cargo package --list -p fmgrep
   cargo publish --dry-run --workspace
   ```

   Each list holds `LICENSE`, `README.md`, `Cargo.lock`, the manifests,
   `.cargo_vcs_info.json`, `src/` and (fmgrep) `tests/`: no scripts, no
   query sets, no file over 1 MB. The dry run packages docfm, then builds
   fmgrep against the packaged docfm; bench has `publish = false`.
   `cargo publish --dry-run -p fmgrep` alone fails until that docfm
   version is on crates.io.

3. Publish, docfm first, since fmgrep resolves docfm from crates.io:

   ```sh
   cargo publish -p docfm
   cargo publish -p fmgrep
   ```

   `cargo publish --workspace` does the same in that order. A published
   version cannot be replaced, only yanked: if fmgrep fails after docfm is
   up, fix and re-run the fmgrep step.

4. Check both crates.io pages: the README renders and its links resolve.

5. Tag the published commit `v<version>` and push the tag.

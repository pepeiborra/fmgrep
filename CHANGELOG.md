# Changelog

Releases of the fmgrep and docfm crates, which share one version.

## 0.1.0

First release.

### Features

- `fmgrep index [PATH]` writes a sharded FM-index to `PATH/.fmgrep/`; a
  search finds it by walking up from the current directory, like git.
  `--store-docs` keeps a copy of the files so searches never read the tree.
- Search is the default command: literal TERMs, `/regex/` or `--regex`,
  `-i`, `-U` and `--multiline-dotall`, with rg's output: `-c`,
  `--count-matches`, `-l`, `-o`, `-A`/`-B`/`-C`, `-n`/`-N`, `--heading`
  and `--color`.
- `--and`, `--or` and `--expr` pick documents by AND / OR over the terms.
- Changed files are detected by a `(size, mtime)` stamp and scanned in full,
  so answers equal a fresh index of the current tree.
- `fmgrep serve` answers over TCP or stdio and watches the tree;
  `--server ADDR` sends a search to it.
- Knobs `FMGREP_SHARD_BYTES`, `FMGREP_BUILD_THREADS` and `FMGREP_VERBOSE`.
- docfm, the index as a stand-alone library: build, count, locate, and a
  zero-copy view over any `AsRef<[u8]>` (blob version 6).

### Limitations

- Files added after `index` are not searched until the next `index`.
- A missing, unreadable or now-binary file is left out, with one warning
  per process.
- On filesystems with 1-second mtimes, a same-size edit within that second
  goes unnoticed.
- A `--store-docs` index answers as of index time.
- `--server` answers counts only.
- A search with no match exits 0, unlike rg.
- Binary files are not indexed; a shard text is at most 2^31-1 bytes.
- Older index formats are refused, never upgraded: re-run `fmgrep index`.
- The serve protocol, `--json` output and fmgrep's library items are
  unstable.
- No Windows support.

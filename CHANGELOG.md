# Changelog

## Unreleased (0.2.0)

This release breaks the command line and the index of v0.1.0. Rebuild every
index, and read the table below before you script a search. Details are in
[docs/usage.md](docs/usage.md).

### Breaking changes

- An index of v0.1.0 is refused: remove `.fmgrep` and run `fmgrep index` again. The format is now 9.
- The manifest is `manifest.bin` now: read it with `fmgrep status --json`, not `jq`.
- An index holds the files that `rg --files` lists, minus binary files. Ignored, hidden and symlinked files are left out.
- A PATTERN is a regex, as in rg. `-F` makes it text. The slashes of `/re/` are now text.
- The first argument is the PATTERN and every other is a PATH, as in rg. Give several patterns with `-e`.
- A PATH that does not exist, or that the index lacks, is an error (exit 2). The other PATHs are still searched.
- `--no-ignore`, `-u`, `--hidden`, `-L`, `-a`, `-z`, `--pre` and `-E` are refused with a reason.
- An empty pattern matches every line, as in rg.
- Piped output prints `path:text`, with no headings and no line numbers, as rg does.
- `fmgrep status` is a command: search for the word with `fmgrep -- status`. The flag `--status` is gone.
- `fmgrep serve` needs watchman, and exits 2 without it. Pass `--no-watch` to serve a tree that does not change.
- On a tree of 50,000 files or more, fmgrep starts a watchman daemon when none runs ([docs/watchman.md](docs/watchman.md)).
- `FMGREP_JOURNAL` is ignored. Point `WATCHMAN_SOCK` at a path where no daemon listens to keep fmgrep away from watchman.
- The serve protocol changed: use one fmgrep version on client and server ([docs/protocol.md](docs/protocol.md)).
- `--json` and `--approx-counts` of several `-e` patterns report one expression, with a line for each pattern.
- The library no longer exports `regex_pattern`.

| v0.1.0 | now |
|---|---|
| `fmgrep 'a.b'` finds the text `a.b` | `fmgrep -F 'a.b'`; `'a.b'` also finds `axb` |
| `fmgrep '/Dyn[A-Z]\w+/'` | `fmgrep 'Dyn[A-Z]\w+'` |
| `fmgrep A B` searched for `A` and for `B` | `fmgrep -e A -e B`: one search, the lines with either |
| `fmgrep --and A B` | `fmgrep --and -e A -e B` |
| `fmgrep A nope.txt` also searched for `nope.txt` | `error: nope.txt: No such file or directory`, exit 2 |
| `fmgrep --server ADDR --status` | `fmgrep status --server ADDR --json` |

`--expr` does not change: a word is a literal and `/re/` a regex.

### New

- `-e`/`--regexp`, `-F`/`--fixed-strings`, `-s`/`--case-sensitive` and `-S`/`--smart-case`, as in rg.
- `cargo install fmgrep` needs no C compiler.
- `FMGREP_SERVER=HOST:PORT` searches a shared `fmgrep serve` from your own checkout, with your local edits, new files and commits included ([docs/shared-server.md](docs/shared-server.md)).
- `--ignore-local-changes` prints the server's answer alone.
- A search warns when it must ignore your local changes, and says why.
- A local `.fmgrep` above the cwd wins over `FMGREP_SERVER`, with a warning. It is also used when `--server` cannot be reached.
- `fmgrep index --store-docs` of a git checkout records the commit it holds. `--require-served-commit` makes that an error when it cannot.
- `fmgrep status` says which index or server a search uses, what it holds and what changed since the build ([docs/usage.md](docs/usage.md#status)).
- `--stats` prints rg's statistics after the matched lines.
- Files that start with a UTF-16 byte order mark are indexed and searched, as in rg.
- A file or directory that a search cannot read gets a warning. It used to drop out silently.
- A search that cannot use watchman says why, once, and checks every file.
- An ignore file edited after the index applies to the next search, with a warning.
- `FMGREP_VERBOSE=1` prints how long each phase of a search took.

### Faster

- The index is about half the size: 2.2 GB on Chromium, down from 4.6 GB ([BENCHMARKS.md](BENCHMARKS.md)).
- A regex with a literal in it reads far fewer files.
- A `-i` search with a PATH or a glob is faster.
- A search without a server opens its index sooner.
- A local or served search waits at most 100 ms for watchman's answer, not 1 to 5 s.
- A search from a checkout through a shared server waits at most 500 ms for it.
- Finding the daemon can add up to 500 ms, when no earlier search left a record of it.

### Fixed

- Output matches rg for adjacent coloured matches, empty `-o` matches, `--json -U` and `-o '\A...'`.
- `fmgrep index` no longer fails on a file name with a backslash in it.
- A file over 2 GiB is skipped with a warning. It failed the whole index.
- `fmgrep serve` asks watchman before each query, so a query no longer misses a change saved just before it.
- A search could miss a change while watchman was still reading the tree.
- A tree on NFS, or shared by a virtual machine, is checked in full: watchman does not see edits made elsewhere.
- `fmgrep index` of a big tree no longer waits 5 s for watchman, and later searches stay fast.
- A served search that fails mid-way prints what it found, then the error, as a local search does.
- An idle `fmgrep serve` on Linux no longer keeps 8 cores busy and grows by 550 MB.

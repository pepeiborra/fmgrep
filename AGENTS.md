# fmgrep for coding agents

How an agent uses fmgrep to search a tree, and the few places where it
behaves differently from rg.

## Mental model

fmgrep is ripgrep over a prebuilt FM-index. Index once, then search from
anywhere below the indexed root:

```sh
fmgrep index .                  # once: writes ./.fmgrep (add it to .gitignore)
fmgrep HscEnv                   # lines, grouped per file, as rg prints them
fmgrep -l HscEnv                # files only; -c counts lines per file
fmgrep -i hscenv                # case-insensitive
fmgrep '/Dyn[A-Z]\w+/'          # /../ is a regex; --regex makes every TERM one
fmgrep --expr 'HscEnv AND (DynFlags OR /Dyn[A-Z]\w+/)' -l
```

For many searches in a row, keep the index loaded:

```sh
fmgrep serve --listen 127.0.0.1:7777 &     # prints the address once ready
fmgrep --server 127.0.0.1:7777 HscEnv      # the lines a local search prints
fmgrep --server 127.0.0.1:7777 -l HscEnv   # any search flag
fmgrep --server 127.0.0.1:7777 --shutdown
```

## Pitfalls

- A TERM is a **literal**, not a regex: `fmgrep 'a.b'` finds the three
  bytes `a.b`. Write `/a.b/` or pass `--regex` for a pattern.
- Every search covers the **whole indexed tree**, and paths are relative to
  the directory holding `.fmgrep`, not the cwd.
- Exit status is rg's: 0 on a match, 1 on no match, 2 on an error.
- `--json` alone prints a counts report. `--json --matches` gives rg's JSON
  event stream.
- `--and`, `--or` and `--expr` are **document-level**: `A AND B` picks files
  holding both, and then prints the lines of either. `AND` and `OR` must be
  capitals.
- Put `--` first to search for a term named `index` or `serve`.

## Freshness

An edited or new file is read in full by every search: answers stay
correct, but each costs a read per search, so re-index after many edits.
After `fmgrep index --store-docs` (faster on large trees, about 1.7 times
the disk), searches never read the tree and answer as of index time.
Details: [docs/index-format.md](docs/index-format.md#staleness).

## Troubleshooting

| message | meaning |
|---|---|
| `no .fmgrep index in DIR or any parent` | run `fmgrep index` at the root |
| `unsupported manifest format` | index from another version: remove `.fmgrep`, re-index |
| `warning: excluding PATH: is missing` | deleted since indexing; skipped |
| `note: N file(s) changed since indexing` | searches read them in full; re-index |

Contributing: see [CONTRIBUTING.md](CONTRIBUTING.md); design notes live in `docs/`.

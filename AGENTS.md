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

For many counts in a row, keep the index loaded:

```sh
fmgrep serve --listen 127.0.0.1:7777 &   # prints the address once ready
fmgrep --server 127.0.0.1:7777 HscEnv    # HscEnv: N matches in M files
fmgrep --server 127.0.0.1:7777 --shutdown
```

## Pitfalls

- A TERM is a **literal**, not a regex: `fmgrep 'a.b'` finds the three
  bytes `a.b`. Write `/a.b/` or pass `--regex` for a pattern.
- Every search covers the **whole indexed tree**, and paths are relative to
  the directory holding `.fmgrep`, not the cwd.
- **No match exits 0.** Read the output, not the exit code. Errors exit 2.
- `--json` alone prints a counts report. `--json --matches` gives rg's JSON
  event stream.
- `--server` answers counts only. `-l`, `-c`, `-o`, context and line flags
  are refused with it; search locally for lines.
- `--and`, `--or` and `--expr` are **document-level**: `A AND B` picks files
  holding both, and then prints the lines of either. `AND` and `OR` must be
  capitals.
- Put `--` first to search for a term named `index` or `serve`.

## Freshness

- An edited file is scanned in full by every search. Answers stay correct,
  but each stale file costs a read per search, so re-index after many edits.
- **New files are invisible** until you remove `.fmgrep` and re-index.
- `fmgrep index --store-docs` keeps a copy of every file. Searches then
  never read the tree, and answers are as of index time.

## Troubleshooting

| message | meaning |
|---|---|
| `no .fmgrep index in DIR or any parent` | run `fmgrep index` at the root |
| `unsupported manifest format` | index from another version: remove `.fmgrep`, re-index |
| `warning: excluding PATH: is missing` | deleted since indexing; skipped |
| `note: N file(s) changed since indexing` | searches read them in full; re-index |

Contributing: see [CONTRIBUTING.md](CONTRIBUTING.md); design notes live in `docs/`.

# Tuning

Every `FMGREP_*` environment variable, and rayon's `RAYON_NUM_THREADS`,
with its default and what it trades.

No variable changes an answer, and only `FMGREP_SHARD_BYTES` changes the
blobs.

Value grammar: a byte size is `<n>`, `<n>KB`, `<n>MB` or `<n>GB` over
zero, in 1024-based units; a count is an integer of `0` or more. Unset or
empty is the default, and any other value that does not parse is an error.

## Index (public)

| variable | default | trades |
|---|---|---|
| `FMGREP_SHARD_BYTES` | `100MB` | Text per shard. A term probes every shard its PATHs, globs and cwd leave, so small shards add per-term work; large ones build slower in more memory. 100 MiB keeps a 90 MB tree in one shard. |
| `FMGREP_VERBOSE` | `0`; a count | `1`, as `index -v`: lists each skipped binary file, and `serve` prints the read and locate costs it measured. `2`: a search also prints its plan on stderr (each leaf's probe, what decided it and the costs; a served search on the server's). |

## Index (developer)

| variable | default | trades |
|---|---|---|
| `FMGREP_BUILD_BYTES` | half of physical memory, else `8GB` | The build's memory budget. Shards built at once are this over `8 x FMGREP_SHARD_BYTES` (a shard needs about 8 bytes per text byte while it builds), at most the build pool size: build wall against peak memory. |
| `RAYON_NUM_THREADS` | machine parallelism | Threads for the build, and for a search's shard probes and directory walk. |

## Search (developer)

| variable | default | trades |
|---|---|---|
| `FMGREP_JOURNAL` | unset: a running watchman, where it pays off | `off` writes and reads no `docs.journal`, so every load stats every file; `on` asks watchman even on a small tree. A watchman that misses its deadline warns on stderr and falls back to the stat pass. |

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
| `FMGREP_VERBOSE` | `0`; a count | `1`, as `index -v`: lists each skipped binary file, `serve` prints the read and locate costs it measured, and a search without a server ends with its [spans](#spans) on stderr. `2`: a search also prints its plan on stderr (each leaf's probe, what decided it and the costs; a served search on the server's). |

## Index (developer)

| variable | default | trades |
|---|---|---|
| `FMGREP_BUILD_BYTES` | half of physical memory, else `8GB` | The build's memory budget. Shards built at once are this over `8 x FMGREP_SHARD_BYTES` (a shard needs about 8 bytes per text byte while it builds), at most the build pool size: build wall against peak memory. |
| `RAYON_NUM_THREADS` | machine parallelism | Threads for the build, and for a search's shard probes and directory walk. |

## Search

No variable. A search asks watchman what changed when the tree holds
50,000 files or more, starting the daemon and adding the watch when they
are missing, and says on stderr when watchman cannot answer; watchman's
own `WATCHMAN_SOCK` names its socket. To keep fmgrep from watchman, set
`WATCHMAN_SOCK` to a path where no daemon listens: fmgrep then starts no
daemon, asks for no watch, uses no record of an earlier search and checks
every file. `fmgrep serve` then refuses to start, unless given
`--no-watch`. See [index-format.md](index-format.md#watchman).

## Spans

Under `FMGREP_VERBOSE=1` a search without a server ends with a tree of
its spans on stderr, after its output. For a search of 50,000 files or
more, read from the files, when watchman does not answer, the tree has
this shape (each time, in ms with two decimals, is shown here as `t`):

```text
note: span                 thread  start ms  took ms
note: search                    0         t        t
note:   locate                  0         t        t
note:   manifest                0         t        t
note:   load                    0         t        t
note:     open                  0         t        t
note:     index                 0         t        t
note:     check                 1         t        t
note:       watchman            1         t        t
note:       stat                1         t        t
note:       discovery           2         t        t
note:     wait                  0         t        t
note:   query                   0         t        t
note:   save                    0         t        t
note:   drop                    0         t        t
note: flush                     0         t        t
note: total                               t
```

Each row is one span: the thread it ran on (0 is the main thread), its
start in ms from the start of the program, and how long it took. A span
is inside the span above it with one less indent. The spans of one
thread under one parent follow each other, so they add up to the parent,
less the code between them. A span on another thread runs beside them.
At the top, `search` and `flush` follow each other the same way, and
`flush` ends just before `total`. A row is there only when its code ran:
with `--store-docs`, `check` holds no `watchman`, `stat` or `discovery`.

| span | what it times |
|---|---|
| `search` | the search, from the end of the argv parse |
| `locate` | find the index directory |
| `manifest` | read and parse `manifest.json` |
| `load` | `open`, `index` and `wait`, then note the files that changed |
| `open` | open the documents and the shard files |
| `index` | load each shard's FM-index |
| `check` | check which files changed, on its own thread |
| `watchman` | ask watchman what changed, and re-check what it names; at 50,000 files or more |
| `stat` | stat every indexed file, when watchman cannot say |
| `discovery` | find the files added since indexing, on its own thread, when watchman cannot say |
| `wait` | after the index load, wait for `check` and `discovery` to end |
| `query` | answer the terms and write their matches |
| `save` | save the watchman token and what differs |
| `drop` | free the loaded index and documents |
| `flush` | write the rest of the output; all of it with `--approx-counts`, or with `--json` when it prints no matches |

`total` is the time from the start of the program to the tree. The
process start and exit are outside it: the wall time of the process
minus `total` is their cost. A search that fails prints the spans it
opened, before the error. A search with `--server`, `serve` and `index`
print no spans. Without the variable no span is recorded, and stdout,
stderr and the exit status do not change.

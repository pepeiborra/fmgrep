# Tuning

Every `FMGREP_*` environment variable, with its default and what it
trades.

No variable changes an answer, and only `FMGREP_SHARD_BYTES` changes index
bytes.

Value grammar: a byte size is `<n>`, `<n>KB`, `<n>MB` or `<n>GB`, in
1024-based units, and junk or zero is an error. A count is a positive
integer, and junk or zero falls back to the default.

## Index (public)

| variable | default | trades |
|---|---|---|
| `FMGREP_SHARD_BYTES` | `100MB` | Text per shard. Every term probes every shard, so small shards add per-term work; large ones build slower in more memory (measurements: `docs/design/history.md` §3). 100 MiB keeps a 90 MB tree in one shard. |
| `FMGREP_BUILD_THREADS` | machine parallelism, capped at `FMGREP_BUILD_BYTES / (8 x FMGREP_SHARD_BYTES)` and at the shard count | Shards built at once (K): build wall against peak memory. |
| `FMGREP_VERBOSE` | off; any value but empty or `0` turns it on | As `index -v`: lists each skipped binary file. |

## Index (developer)

| variable | default | trades |
|---|---|---|
| `FMGREP_BUILD_BYTES` | `8GB` | The build's memory budget, assuming a shard needs 8 bytes per text byte while it builds. It is a constant, not a reading of this machine's RAM. |
| `FMGREP_BUILD_SHARD_THREADS` | machine parallelism / K | Threads inside one shard build (T). K x T above the core count oversubscribes the machine. |
| `FMGREP_READ_THREADS` | machine parallelism, at most 8 | Files read at once while indexing. Reads are latency-bound. |

## Search (developer)

These tune how far the index narrows the candidate documents before the
regex crate verifies them. When a budget runs out, the walk gives up and
keeps more candidates, so it widens and never narrows. Raising a budget
buys fewer candidates for more walking.

| variable | default | trades |
|---|---|---|
| `FMGREP_AUTOMATON` | on; `off` disables | The reversed-DFA walk for patterns. Off, a pattern prunes by its required literal alone. |
| `FMGREP_AUTOMATON_MAX_ANCHOR` | `3` | A pattern whose required literal is at most this many bytes walks the automaton instead of pruning by the literal. |
| `FMGREP_AUTOMATON_MIN_ANCHOR_COUNT` | `256` | A longer literal that occurs at least this often in a shard walks the pattern there, instead of locating every occurrence. |
| `FMGREP_AUTOMATON_MAX_LIVE` | `256` | Live FM intervals one walk frontier may hold. |
| `FMGREP_AUTOMATON_MAX_DEPTH` | `64` | Bytes one branch may walk backward. |
| `FMGREP_AUTOMATON_MAX_NODES` | `20000` | Node expansions per walk. |
| `FMGREP_AUTOMATON_CLASS_BYTES` | `64` | Distinct bytes one DFA state may admit. A wider first step is refused, and a wider step later ends the walk. |
| `FMGREP_AUTOMATON_DFA_BYTES` | `8388608` | Largest compiled DFA, in bytes (a count, not a byte size). A bigger pattern is not walked. |
| `FMGREP_FOLD` | on; `off` disables | The case-variant walk for `-i` literals. |
| `FMGREP_FOLD_MAX_LIVE` | `256` | Live case variants one `-i` walk may hold. |

The measurements behind these defaults are in `docs/design/history.md`.

## Build feature

`cargo install fmgrep --features jemalloc` makes jemalloc the global
allocator. On macOS it can lower the peak footprint of multi-GB builds,
where the system allocator keeps freed buffers cached, for a slightly
larger binary and a longer clean build. It is off by default
(measurements: `docs/design/history.md` §10).

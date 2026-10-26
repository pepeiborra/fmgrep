# Tuning

Every `FMGREP_*` environment variable, and rayon's `RAYON_NUM_THREADS`,
with its default and what it trades.

No variable changes an answer, and only `FMGREP_SHARD_BYTES` and
`FMGREP_SAMPLE_RATE` change index bytes.

Value grammar: a byte size is `<n>`, `<n>KB`, `<n>MB` or `<n>GB` over
zero, in 1024-based units; a count is an integer of `0` or more. Unset or
empty is the default, and any other value that does not parse is an error.

## Index (public)

| variable | default | trades |
|---|---|---|
| `FMGREP_SHARD_BYTES` | `100MB` | Text per shard. Every term probes every shard, so small shards add per-term work; large ones build slower in more memory. 100 MiB keeps a 90 MB tree in one shard. |
| `FMGREP_VERBOSE` | `0`; a count over `0` turns it on | As `index -v`: lists each skipped binary file. |

## Index (developer)

| variable | default | trades |
|---|---|---|
| `FMGREP_BUILD_BYTES` | half of physical memory, else `8GB` | The build's memory budget. Shards built at once are this over `8 x FMGREP_SHARD_BYTES` (a shard needs about 8 bytes per text byte while it builds), at most the build pool size: build wall against peak memory. |
| `FMGREP_SAMPLE_RATE` | `16`; a count over `0` | A document id is stored every this many text positions, so locating an occurrence takes fewer than this many steps: lower can answer literals and regexes faster, from a larger index (below). A search reads the rate from the index. |
| `RAYON_NUM_THREADS` | machine parallelism | Threads for the build, and for a search's shard probes and directory walk. |

`FMGREP_SAMPLE_RATE` against blob size and the mean warm served search
(Apple M5, files layout):

| rate | GHC blob | GHC literal / regex | linux blob | linux literal / regex |
|--:|--:|--:|--:|--:|
| 32 | 113 MiB | 4.99 / 9.05 ms | 1.89 GiB | 11.4 / 19.1 ms |
| 16 | 129 MiB | 4.42 / 7.75 ms | 2.18 GiB | 11.0 / 18.5 ms |
| 8 | 160 MiB | 4.34 / 7.26 ms | 2.75 GiB | 10.4 / 20.4 ms |
| 4 | 224 MiB | 4.20 / 6.77 ms | 3.90 GiB | 10.3 / 17.1 ms |

16 is the default. 32 saves about 12% of the index for about 1.5 µs more
per matched document; 8 buys most of the remaining latency for 1.25 times
the index; 4 is not worth it (the last 9% of the gain for 1.4 times). On
chromium (Linux, i4i.4xlarge), per matched document, 32 to 16 saves
1.49 µs, 16 to 8 0.42 µs and 8 to 4 0.18 µs. Below 16 an intersection with
a very common side can get slower. The rate only moves the probe: a lower
rate recovers about half of the regex gap to an index that stores its
documents, and none of the per-document `open(2)` cost of reading matches
from the tree, which on linux (about 800 documents per literal) dwarfs it.
`--store-docs` is the lever for that half.

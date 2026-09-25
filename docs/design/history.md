# Design history

Why fmgrep and its docfm engine look the way they do: the measurements
behind each choice, condensed from the development log.

Every table sits under a stamp naming the machine and date. "Indicative"
marks a box that was shared at the time (load1 given); treat those as
ratios, not absolutes. Engines other than docfm are external projects,
named with the versions measured:

- **genedex-small**, **genedex-fast-build**: two tunings of genedex 0.2.2.
- **fm-index-rl**: fm-index 0.3.1's run-length FM-index, patched to persist.
- **tgrep**: a trigram-index grep at b1d0fc2 (reference row).
- **rg**: ripgrep 15.2.0, scanning the tree (reference row).

Latency is per term: `p50` is a client round-trip to a server holding the
index (process spawn included), `fp` the server's anonymous footprint and
`idx` the index bytes over the text bytes.

## 1. Why a new FM-index

The first engines were off-the-shelf FM-indexes behind one sharded
harness. They answered fast, but each held its whole structure in anonymous
memory, several times the text. docfm was written so that the index file
itself is the query structure, mapped and used in place.

Apple M5, 10 cores (4P+6E), 16 GiB, macOS 26; 2026-09-24; quiet box,
median of 3; p50 in ms.

| row | GHC p50 / fp / idx | linux p50 / fp / idx / ready | chromium p50 / fp / idx / ready / build |
|---|---|---|---|
| docfm | 2.37 / 13 MB / 2.33x | 4.24 / 25 MB / 2.24x / 10 ms | 34.0 / 68 MB / 2.31x / 38 ms / 71 s |
| genedex-small | 3.02 / 208 MB / 3.17x | 4.33 / 2.96 GB / 2.79x / 0.35 s | 34.1 / 6.4 GB / 3.09x / 1.1 s / 74 s |
| genedex-fast-build | 2.48 / 275 MB / 3.92x | 2.75 / 4.19 GB / 3.53x / 0.58 s | 18.8 / 8.7 GB / 3.84x / 1.6 s / 72 s |
| fm-index-rl | 1.72 / 360 MB / 4.86x | 2.28 / 6.25 GB / 4.78x / 2.4 s | 6.4 / 11.2 GB / 4.79x / 5.7 s / 79 s |
| tgrep | 5.70 / 114 MB / 1.03x | 14.9 / 1.35 GB / 0.67x / 0.59 s | 451 / 3.9 GB / 0.95x / 9.6 s / 47 s |

These indexes still held a copy of the documents (`idx` includes it);
section 6 removes it. docfm ties genedex-small on p50 with 1-6% of its
footprint; the faster engines hold 20-250x its memory.
Linear projection to 100x Chromium (305 GB, 46M files): docfm
705 GB on disk (401 GB without the document copy) and a query footprint of
~5-7 GB, against 943 GB / 645 GB for genedex-small and 1.46 TB / 1.1 TB for
fm-index-rl; about 2 h to build for all of them.

## 2. Blob layout, up to version 6

**Mapped wavelet matrix (v2).** Rank lines of 64 bytes per wavelet level,
mapped in place. The anonymous footprint fell to 3% of the structure, but
an LF step touched one line in each of 8 level sections, so a term mapped
many pages.

**Bit-sliced occurrence blocks (v4).** One block per 4096 BWT positions:
u16 counts at the block midpoint, then the code bits of each 64 positions
side by side, plus u32 counts every 65536 positions. One backward-search or
LF step reads one block.

Apple M5, 10 cores (4P+6E), 16 GiB, macOS 26; 2026-09-23; indicative
(load1 20-35). Resident = pages of `blobs.shard` mapped after N terms,
page cache evicted first; warm = sum of per-term medians.

| corpus | blob | blobs.shard | resident after 1 / 10 / all terms | warm total |
|---|---|---|---|---|
| GHC (35) | v2 | 128.8 MB | 69 / 101 / 121 MB | 153 ms |
| GHC (35) | v4 | 135.3 MB | 30 / 75 / 113 MB | 78 ms |
| linux (102) | v2 | 2312 MB | 207 / 1396 / 1718 MB | 108 ms |
| linux (102) | v4 | 2351 MB | 89 / 786 / 1611 MB | 73 ms |

Rejected: 2048-position blocks (13.7% larger than v2 on GHC for the same
warm latency).

**Document-id samples (v5).** Samples hold the document id, not a text
position, and a row is sampled at every multiple of the rate and at every
document start. A locate stops inside its own document, so no position
table is needed. The rate trades size against locate steps.

Apple M5, 10 cores (4P+6E), 16 GiB, macOS 26; 2026-09-23; indicative
(contended throughout). `blobs.shard` and warm in-process latency, sum of
per-term medians.

| corpus | rate 16 | rate 32 | rate 64 |
|---|---|---|---|
| GHC | 128.8 MB, 171 ms | 117.8 MB (-8.5%), 316 ms | 112.2 MB (-12.8%), 652 ms |
| linux | 2.31 GB, 383 ms | 2.11 GB (-8.9%), 864 ms | 2.00 GB (-13.3%), 1589 ms |
| chromium | 4.42 GB | 4.04 GB (-8.6%) | 3.85 GB (-12.9%) |

These were measured on the wavelet layout; on the v4 blocks, rate-32
samples cost 1.57x the warm latency of v4 at rate 16 on GHC for -8.1% bytes.
Rate 32 was kept.

**Sparse marks (v6).** The sampled-row marks went from 64-byte rank lines
(1.07 bits per row) to a u32 count per 4096 rows plus a u16 offset per
marked row (0.51 bits per row).

Apple M5, 10 cores (4P+6E), 16 GiB, macOS 26; 2026-09-23; load1 3-4. Serve
mode, sum of per-term medians, 5 reps x 2.

| | GHC v5 | GHC v6 | linux v5 | linux v6 |
|---|---|---|---|---|
| `blobs.shard` | 124.3 MB | 118.1 MB (-4.9%) | 2146 MB | 2032 MB (-5.3%) |
| warm, 35 / 102 terms | 65.1 ms | 69.6 ms (+6.9%) | 268 ms | 284 ms (+5.9%) |
| RSS after all terms | 127 MB | 121 MB | 1601 MB | 1516 MB |

On chromium the layout alone cost +7.5-8.5% latency for -5.0% size. That
was accepted. Rejected: the marks as a ninth bit plane in each occurrence
block, which was 3.5-6% faster warm but only 0.6% smaller.

## 3. Construction

**Threads per shard.** Each shard builds on T threads. Writing each row's
BWT code into its suffix-array slot freed the text early: live heap per
text byte went from 6.43 to 5.43 on 100 MiB linux shards. The rest of the
10.6x footprint peak was the macOS allocator's large cache holding freed
build buffers (`MallocLargeCache=0` brings the peak down to the live heap).
This is accepted for single-shard builds. The build budget assumes 8 bytes
per text byte, so linux builds 10 shards at once within 8 GiB.

**K shards x T threads.** K is machine parallelism under the build budget,
and T = machine / K. On genedex at K = 8, CPU time looked inflated; the
cause was E-cores, where an E-core second buys 1/4 to 1/3 of a P-core
second, not thread control. On linux at 100 MiB
shards, one shard with the whole machine costs ~15% less CPU and ~35% less
peak memory than 8 single-threaded shards at the same wall. At 20 MiB
shards the ordering reverses.

**BWT without a suffix array.** libsais writes the BWT directly and returns
the row of every r-th text position (r the largest power of two up to the
sample rate, at most 64). The other sampled rows are LF walks of fewer than
r steps over the finished blocks. The blob bytes are unchanged.

Apple M5, 10 cores (4P+6E), 16 GiB, macOS 26; 2026-09-24; indicative
(load1 5-14).

| build | before | after |
|---|---|---|
| one 100 MiB linux shard, T = 1 | 2.06 s | 1.65 s |
| live heap per text byte | 5.25x | 5.13x |
| linux, 3 alternating reps | 14.1 / 14.6 / 17.3 s | 11.6 / 13.7 / 15.1 s |
| chromium, one rep | 49.3 s | 46.1 s |

The remaining peak (~5.1x text) is libsais's `i32` temporary array. Going
lower needs a construction without libsais.

**Shard cap.** 100 MiB is the smallest cap that keeps GHC in one shard.
Halving the cap from 200 MiB cut fm-index-rl's linux index footprint from
8.19 to 7.44 GB at equal build time (2026-09-19). docfm on linux with 200 /
400 MiB caps (2026-09-24, contended): index +1.4% / +2.6%, build 14.3 ->
21.7 / 26.3 s, p50 4.3 -> 4.7 / 5.0 ms. The default stays 100 MiB.

## 4. OpenMP dropped

libsais can use an OpenMP team, but shard-level parallelism already fills
the machine, and requiring libomp would break `cargo install` on a stock
Mac. The bytes are identical either way (linux `blobs.shard` checked).

Apple M5, 10 cores (4P+6E), 16 GiB, macOS 26; 2026-09-24; two reps each.

| corpus | libsais with OpenMP | single-threaded libsais |
|---|---|---|
| GHC build wall | 2065 / 1714 ms | 1977 / 1990 ms |
| linux build wall | 12123 / 11090 ms | 10976 / 11327 ms |

## 5. The mapped blob

`blobs.shard` is mapped once at load, with default read-ahead
(`MADV_NORMAL`). Measured 2026-09-23: `MADV_RANDOM` was 20% slower on the first cold pass with
the same resident set. `MADV_WILLNEED` at load made cold passes 5-11x
faster but pulled the whole file into memory, so it is not the default.

## 6. Documents read from the tree

Keeping a copy of every document doubled the index, so the default index
keeps only a per-file `(size, mtime)` stamp and reads matched files from
the tree. `index --store-docs` keeps the copy.

Apple M5, 10 cores (4P+6E), 16 GiB, macOS 26; 2026-09-24; GHC, medians of 3,
load1 3.5-6.

| | with copy | without |
|---|---|---|
| index bytes | 206.9 MB | 118.1 MB |
| build wall | 1.82 s | 1.86 s |
| serve p50 / mean per term | 2.33 / 3.92 ms | 2.33 / 4.56 ms |
| serve ready | 3.1 ms | 20.3 ms |

On linux the index went from 3.67 to 2.03 GB, and the mean per term from
12.7 to 16.8 ms. The cost is one `open(2)` per regex candidate file.

**Staleness**, in three steps:

1. A term that touched a changed file failed with an error naming it.
2. Files were statted lazily, the first time an answer touched them. This
   made serve ready (2026-09-24) in 4.7 ms instead of 26 ms on GHC, and in 33 ms instead
   of 3.2 s on chromium.
3. The current rule: every file is statted at load, on 16 threads, before
   the first answer. A changed file is scanned in full for every term, so
   answers equal a fresh index of the current tree. A missing file is
   excluded, with one warning per process. `serve` watches the root.

The cost of step 3 on a loaded box (2026-09-24): serve ready went from 4-7 to 38-44 ms
on GHC and from 16-22 to 140-190 ms on linux (~2 s on chromium). On linux
with N stale files, literal p50 was 1.37 ms at N = 1 and 2.03 ms at
N = 1000. The watcher costs no measurable idle CPU. FSEvents delivered
changes in 0.03-0.8 s.

## 7. Query pruning

The regex crate is the only matcher; the index only narrows the candidate
documents, and every budget below widens when it runs out, never narrows.

**`-i`** walks the case variants of a folded literal as a branching
backward search. The fold classes are the regex crate's simple folding (`k`
also matches U+212A). On GHC (2026-09-21) the candidates equaled the matching documents
for all 20 designed terms, and latency fell from ~100 ms to 0.1-5 ms.

**Automaton walk.** A pattern without a literal longer than 3 bytes steps a
reversed byte DFA while the backward search branches, and a branch dies as
soon as its interval empties. On GHC (2026-09-21) the candidates on the
four routed terms were exact: `fo+bar` 1334 -> 56, `Int(32|64)#` 8415 -> 94,
`(foo|bar)Baz` 146 -> 0, `[A-Z][a-z]+Env` 743 -> 444. Latency went from
5.5 to 0.5 ms and from 33.4 to 2.1 ms. `[A-Z][a-z]+Env`, whose literal was
already selective, lost (12.6 -> 15.4 ms).

**Deferred plan for common anchors.** A longer literal is counted per
shard first. Where it occurs at least 256 times, the shard walks the
longest run of the pattern that holds the literal (no look-around, NUL or
wide classes) instead of locating every occurrence.

Apple M5, 10 cores (4P+6E), 16 GiB, macOS 26; 2026-09-24; indicative
(load1 3-8), medians of 3.

| term | candidates | latency |
|---|---|---|
| linux `/union[ \t]+\{/` | 6281 -> 2854 | 190.3 -> 83.8 ms |
| linux `/typedef[ \t]+struct/` | 2797 -> 1302 | 176.2 -> 61.4 ms |
| linux `/__attribute__[ \t]*\(\(/` | 1178 -> 1175 | 30.4 -> 32.3 ms |
| GHC `/import GHC\.(Core\|Stg\|Cmm)\.[\w.]+/` | 3649 -> 485 | 49.9 -> 8.7 ms |
| GHC `/foreign import ccall (unsafe\|safe)/` | 379 -> 201 | 6.5 -> 4.7 ms |

The threshold 256 is the largest within 3% of the best summed regex latency
on each of GHC, linux and chromium (swept 1 to 2048); 512 loses linux
`typedef`.

**Interleaved locate.** Locate runs 16 LF walks at once, which cut literal
p50 by 17% on GHC and 12% on linux (2026-09-24).

**`--occurrences`.** It reports totals without locating: a literal is
counted from its FM intervals alone. On linux (102 terms, 92 literals,
2026-09-24, indicative, load1 5-6) the literal sum went from 152 to 8.2 ms
(18.5x) and the process wall from 295 to 152 ms.

## 8. AND / OR

Every leaf is probed in one pass. Literals give exact documents and
patterns give candidates; AND intersects and OR unites these before any
file is read, and a single `RegexSet` pass verifies only the undecided
documents.

Apple M5, 10 cores (4P+6E), 16 GiB, macOS 26; 2026-09-24; indicative
(load1 4.4-5.9), 7 reps. Median latency of the expression over its slower
leaf alone.

| expression | GHC | linux |
|---|---|---|
| literal AND literal | 1.2-1.4x | ~2x |
| literal AND regex | 0.42-0.56x | 0.68x |
| OR | about the sum of the leaves | about the sum of the leaves |

## 9. Fidelity to ripgrep

Output was checked byte for byte against rg 15.2.0 by an oracle script over
staged fixtures: 812 cases, covering every output mode, `-i`, `-U`, BOMs,
unterminated last lines and expressions. A second oracle checks 20 AND/OR
pairs on GHC against rg pipelines. Choices this forced:

- Every mode that prints matches counts as rg does: line-anchored and
  non-overlapping. Only `--approx-counts` reports the index's own counts,
  and `counts_exact` flags any term where the two can differ.
- A leading UTF-8 BOM is stripped before matching, as rg's searcher does.
  `bytes_searched` excludes it, and `^`-anchored patterns see line 1.
- A zero-width match at the end of an unterminated last line reports that
  line with no span, as rg's printers do.
- `-U` takes the whole-document path only for a term that can match a line
  terminator. This is rg's own test, so `\A` and `\z` anchor exactly where
  rg anchors them.
- `--json --matches` is rg's event stream; `docs/protocol.md` lists the
  deviations.

## 10. Measured, not adopted

- **A 4-gram shard prefilter** (2026-09-24). For absent terms p50 fell to
  6% of the baseline, and for rare terms to 60%, at +1.3% index and +3.2%
  build. It failed the cold-CPU gate (75% against a 60% target).
- **Word-at-a-time bit-plane packing** in the occurrence writer
  (2026-09-24): -5% build, under the 30% bar.
- **jemalloc as the global allocator** (2026-09-19). A wash at GHC scale.
  It helped only multi-GB builds on macOS, and it is still available as a
  feature (`docs/tuning.md`).
- **Four background stat threads instead of two** in serve (2026-09-24):
  -2.9% chromium mean, under the 5% bar.
- **Shard caps above 100 MiB**: see section 3.
- **Memory-residency caps for queries**: they had no effect on a mapped
  index and were removed.

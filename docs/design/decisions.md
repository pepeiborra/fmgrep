# Design decisions

The standing decisions behind fmgrep, each with its reason and a pointer to
the measurements in [history.md](history.md).

**docfm is the only engine.** It ties genedex-small on latency with 1-6%
of its query footprint; the faster engines hold 20-250x its memory. The
mapped index file is the query structure. There is no engine plug-in seam.
(history §1)

**Documents are read from the tree by default; `index --store-docs` keeps a
copy.** The copy nearly doubled the index. Without it an index stores a
`(size, mtime)` stamp per file, and matched files are read from the tree.
A `--store-docs` index never touches the tree again, so its answers are as
of index time. (history §6)

**Doc-id sample rate 32.** Rate 32 is 8.5-8.9% smaller than 16. Rate 64
saves only another 4-5% and doubles locate steps again. (history §2)

**Blob v6 sparse marks.** -5% size for +6-8% warm latency, accepted.
(history §2)

**Staleness.** A load stats every file against its stamp before the first
answer. A changed file is scanned in full for every term, so answers equal
a fresh index of the current tree. A missing, unreadable or now-binary file
is excluded, with one warning per process. `serve` watches the tree
(`--no-watch` opts out). Known limits: files added after `index` are not
searched, and on filesystems with 1-second mtimes a same-size edit within
that second goes unnoticed. Re-index after many edits. (history §6)

**`--occurrences`.** Totals only, with no locate: a literal is counted from
its FM intervals. `docs_matched` is absent (not null, not 0), and AND is
refused, because AND needs documents. (history §7)

**No shard prefilter.** A 4-gram prefilter helped absent and rare terms but
failed the cold-CPU gate. (history §10)

**AND / OR are document-level.** A document satisfies a leaf when the leaf
matches anywhere in it. In the text form `AND` and `OR` are capitals only,
`AND` binds tighter, and nesting is capped at 60, within the JSON
recursion limit that serve parses under. `-l` lists the satisfying
documents. The line modes print the alternation of every leaf within them,
as `rg -e a -e b` would. The counting modes add per-leaf `leaves` totals.
(history §8)

**OpenMP dropped.** Shard-level parallelism already fills the machine, and
plain `cargo install` must work on a stock Mac. (history §4)

**The build budget is a constant.** `FMGREP_BUILD_BYTES` defaults to 8 GiB
rather than a reading of physical RAM, which would need a platform
dependency. (history §3)

**The automaton threshold stays a knob.** `FMGREP_AUTOMATON_MIN_ANCHOR_COUNT`
(256) was tuned on three corpora; other corpora may prefer another value.
(history §7)

**No `MADV_WILLNEED` by default.** Cold passes are 5-11x faster with it,
but the whole blob becomes resident. (history §5)

**The macOS large-cache footprint of a one-shard build is accepted.** It is
the allocator's cache, not live memory. (history §3)

**Deliberate differences from rg.** A search with no match exits 0.
Every search covers the whole indexed tree, and paths are relative to the
directory holding `.fmgrep`, not the cwd.
Heading and line numbers stay on when stdout is not a tty. `-U` uses rg's
own test for multiline terms. `--json` streams deviate as listed in
[protocol.md](../protocol.md). (history §9)

**rg's own semantics are kept where they surprise.** The last output-mode
flag (`-c`, `--count-matches`, `-l`, `-o`, `--matches`, ...) wins. Under
`-U`, for a term that can match a line terminator, `-c` counts matches
rather than lines, and a line reached only through its terminator prints
with no span. (history §9)

**The serve protocol is unstable.** It is documented, but its methods and
fields may change in any release.

**No Windows support.** There is no CI for it and no Windows-specific code.

**CLI shape.** Search is the default command. `index` writes `.fmgrep/` at
the root of the tree, and a search finds it by walking up from the current
directory, like git. `--server ADDR` sends a search to a running `serve`.

**Knobs.** The public knobs are `FMGREP_SHARD_BYTES`, `FMGREP_BUILD_THREADS`
and `FMGREP_VERBOSE`. Developer knobs tune construction and pruning speed.
Only `FMGREP_SHARD_BYTES` changes index bytes, and no knob changes an
answer. Test hooks are not knobs. The list is in [tuning.md](../tuning.md).

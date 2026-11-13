# Index format

The on-disk layout `fmgrep index` writes (manifest format 7): what each
file holds and the rules a reader relies on.

## Directory

`fmgrep index PATH` writes `PATH/.fmgrep/` (`--output DIR` elsewhere). The
directory must not already exist. `manifest.json` is written last, so an
interrupted build leaves nothing queryable, and a failed build removes the
directory.

| file | present | holds |
|---|---|---|
| `manifest.json` | always | the document list, length tables and blob inventory |
| `blobs.shard` | always | one docfm blob per shard, concatenated |
| `docs.stamps` | default | one mtime per document, and the file length of each UTF-16 one |
| `dirs.stamps` | default | every walked directory's `(size, mtime)` |
| `ignores.stamps` | default | every ignore file's `(size, mtime)` |
| `docs.shard` | `--store-docs` | the documents, concatenated |

A search opens the index at `--index PATH` (default: the current
directory) if one is there, else the `.fmgrep` of the nearest directory
above it. A default index reads documents from the parent of its `.fmgrep`
directory, so a tree moved with its index keeps working. An index written
with `--output` reads them from the manifest's `corpus_root`.

## manifest.json

One JSON object on one line, with the fields in this order. Unknown fields
are refused.

| field | type | meaning |
|---|---|---|
| `format` | u32 | `7` |
| `store_docs` | bool | documents are in `docs.shard`, not read from the tree |
| `num_docs` | usize | documents indexed |
| `num_bytes` | u64 | sum of `doc_lens` |
| `bom_bytes` | u64 | leading UTF-8 BOM bytes over all documents |
| `index_size_bytes` | u64 | the structures' size, plus the document text with `store_docs`; an accounting figure, not a file size |
| `build_time_us` | u64 | the index wall: walk, construction and blob flush |
| `index_started_ns` | i64 | when the index began, before it listed or read anything, in ns since the epoch |
| `files` | [string] | corpus-relative path per document, by id |
| `blobs` | [string] | one `shard-<start>-<end>.bin` label per shard |
| `doc_lens` | [u64] | byte length per document, parallel to `files` |
| `blob_lens` | [u64] | byte length per blob, parallel to `blobs` |
| `corpus_root` | string | absolute path of the tree; present iff not `store_docs` |

`num_bytes - bom_bytes` is the `bytes_searched` that rg reports for the
tree, because rg's searcher strips a leading BOM.

`fmgrep status [PATH]` reads the manifest of the index at or above PATH
(default: the current directory) as a search does, and refuses what a
search refuses. It prints one `name: value` line per scalar field, plus
`index` (the index directory) and `shards` (the number of blobs).
`fmgrep status --json` prints the manifest itself: the fields above, in
that order, on one line. A script reads the paths with
`fmgrep status --json | jq -r '.files[]'`.

A load refuses, with exit 2:

- a `format` other than 7, naming the remedy: re-run `fmgrep index`. Old
  indexes are refused, never upgraded;
- a `corpus_root` that is present with `store_docs`, or that is absent or
  relative without it;
- `bom_bytes > num_bytes`;
- `files` / `doc_lens` or `blobs` / `blob_lens` of different lengths;
- labels that do not partition `0..num_docs`, or a `blobs.shard` whose
  length is not the sum of `blob_lens`.

## Documents

The walk under PATH defines the document set:

- every file `rg --files` lists, sorted by relative path; ids are positions
  in that order. As in rg, `.gitignore` (inside a git repository),
  `.ignore`, `.rgignore`, `.git/info/exclude` and the global git excludes
  file apply, including those of the directories above PATH; hidden names
  are skipped unless an ignore file whitelists them; symlinks are not
  followed. `.fmgrep` is never read;
- a file starting with a UTF-16LE or UTF-16BE BOM is read as the UTF-8
  that rg decodes it to; a document is that text, else the file's bytes;
- binary files are skipped, with one summary line on stderr (`-v` lists
  them): a document holding a NUL byte, starting with `%PDF-`, or whose
  first 8 KiB are over 5% control bytes. rg searches the last two and
  prints their bytes as unreadable lines;
- a file whose size or text is over 2^31-2 bytes (docfm's bound less a
  separator) is skipped, with a warning;
- paths are stored as UTF-8, with `/` separators; a non-UTF-8 name is
  converted lossily.

## Staleness

A files-layout index answers over the tree as it is now. Before the first
answer, a load stats every document against its `(size, mtime)` stamp
(the stat pass):

- a changed file is scanned in full by every search, so answers equal a
  fresh index of the current tree;
- a missing or unreadable file, or one the walk would now skip, is
  excluded, with one warning per process;
- a file the walk's rules find that the index does not list is added:
  scanned in full and printed in path order among the indexed files, with
  no warning. The load stats the directories in `dirs.stamps`, lists only
  those whose stamp moved, and walks new directories below them in full;
- a file the walk skipped as non-text is sniffed again whenever its
  directory is listed: one rewritten in place as text is found once its
  directory's stamp moves, or when watchman reports it as changed;
- when an ignore file in `ignores.stamps` changes, or one appears in a
  listed directory, the load walks the directory it governs in full (the
  root for those above it): an indexed file the rules now skip is excluded
  with no warning, and a file they now allow is added. This repeats at
  every load until the next `index`, and in a served query that watchman
  tells of the change. Such a load prints one line on stderr per process,
  with the number of ignore files new, edited or deleted since the index,
  for example `warning: 2 ignore files changed since the index; each
  search reads their directories again (re-run index to make searches
  fast again)`; a server that asks watchman before each query says `each
  query` instead. Without `dirs.stamps`, every load walks the whole tree,
  and prints no such line.

`serve` asks watchman before each query (see Watchman below). With
`--no-watch`, for a tree that does not change, it contacts no watchman:
it runs the stat pass at its start, reads and saves no record, misses
files added after it is ready, and sees a changed file only when a query
reads it.
On a filesystem with 1-second mtimes, a same-size edit within that second
goes unnoticed.
The directory check relies on the filesystem updating a directory's mtime
when an entry is created, removed or renamed, as POSIX requires; on one
that does not, delete `dirs.stamps` to make every load walk the tree.
Re-index after many edits.

On a tree of 50,000 files or more, a search asks watchman instead (see
Watchman below). Below that, the stat pass is cheaper and a search never
asks watchman. `serve` asks it at any size, unless `--no-watch` is given.

A `--store-docs` index never reads the tree: it answers as of index time,
and edits, new files and deletions show up only after the next `index`;
`serve` neither stats the tree nor asks watchman. Written with `--output`, it has
no tree: a PATH is refused and the cwd does not narrow the search.

## Watchman

`index` takes no token and writes no record. Once its walk has listed
50,000 files or more, before it reads them, it finds the daemon and asks
it to watch the tree when no watch covers it, as a search does (below),
and waits at most 100 ms for the reply; watchman then crawls the tree
while `index` reads the files and builds.
A search of a files-layout index of 50,000 documents or more
keeps a record between searches, in the user's cache directory
(`$XDG_CACHE_HOME/fmgrep`, else `~/.cache/fmgrep`), one per tree and
index: `<tree name>-<16 hex digits>.json`. A search never writes into the
index directory. The record is one line of JSON:

| field | holds |
|---|---|
| `v` | `1` |
| `root` | the canonical tree |
| `index` | the canonical index directory |
| `index_started_ns` | the manifest's, so a rebuilt index does not use it |
| `token` | `sock`, `watch`, `relative`, `clock`: a watchman clock of the watch on the tree, and the daemon's socket |
| `baseline` | the paths that may differ from the index, ascending; `null` when more than an eighth of the documents (at least 64) do |

A **token** is the second of two watchman clocks synced within 100 ms,
kept only when it is a later tick than the first. A clock answered during
a crawl, its tail included, is the crawl's tick, which files changed then
get too, so `since` it misses them; such clocks share the tick, and a
watch still crawling gives no token. A watch whose `ignore_dirs` reach
into the tree, or above it, gives no token.

The token comes from the watch on the tree or on a directory above it.
Where no watch covers the tree, the search asks watchman to watch it
(`watch-project`). Watchman answers that only once its crawl of the tree
ends, so the search stops waiting at its 100 ms limit; the watch stays,
and watchman goes on crawling. That search checks every file and says
`fmgrep asked watchman to watch /src; later searches ask it once its
crawl ends`; each search during the crawl says `watchman is still
crawling /src`. A search with a record whose watch was removed asks for
it again in the same way.

**A search cannot use a record** when there is none yet, when it was
written for another index build, when `WATCHMAN_SOCK` is set to another
socket than the one it stores, when no watchman daemon answers on that
socket, or when watchman reports that it restarted, removed and added the
watch again, or forgot deleted files since the record's token.
Such a search finds the daemon from `WATCHMAN_SOCK`, else by running
`watchman get-sockname` within 500 ms: when no daemon runs, watchman's
client starts one and answers once it listens. Where `WATCHMAN_SOCK` is
set, fmgrep starts no daemon. The search then takes a token (or keeps the
one it just took from the record's daemon), checks every file (the stat
pass), and says so once on stderr, with the reason: `warning: checked
every file once (no record of an earlier search); later searches ask
watchman`.

**With a record it can use**, a search takes a newer token from the
daemon on the socket the record stores, then asks it what changed since
the record's token (the token question), within 100 ms for both. It
re-checks the paths that watchman reports as changed plus the baseline,
and skips the stat pass and the directory check: a reported file is
statted, and a reported directory and the parent of any other reported
path are listed again one level (a recorded subdirectory whose stamp
moved is listed in turn, a new one is walked). A reported directory that
the index did not record, or a path under one, makes the search walk the
topmost such directory in full, with the ignore files in it. The ignore
files are checked as the stat pass checks them.

After it answers, a search saves its token and what differs now as the new
baseline: the stale and gone files, each added file, or its topmost
directory the index did not record, and each ignore file the index did not
record, so that later searches walk its directory again and print the
warning above. The write is a rename, so two searches at once leave one
whole record.

Without `--no-watch`, `serve` reads the record once, at its start, as a
search does, whatever the size of the tree. Before each query it takes
a newer token and asks
watchman what changed since the token of the query before, and
re-checks the paths that watchman reports as changed. It keeps its
tokens in memory and saves the record once, after its first query with
a token. When watchman cannot answer for a query, that query checks
every file, as a new load would, and the server says why on stderr each
time the reason changes; `note: each query re-checks only what watchman
reports again` follows once watchman answers. The server's lines speak
of its next query, not of later searches: `warning: checked every file
once (no record of an earlier search); the next query asks watchman`.

A server that cannot use watchman at all exits 2 before its readiness
line, unless `--no-watch` is given: when watchman is not installed, when
`WATCHMAN_SOCK` names a socket where no daemon answers, when the tree is
on a network filesystem or `/proc/self/mountinfo` cannot be read, when
watchman's `ignore_dirs` hide part of the tree or watchman refuses to
watch it, or when the index has no `dirs.stamps`:

    error: cannot use watchman for /src (cannot run watchman (No such file or directory (os error 2)); install watchman); `fmgrep serve` asks it before each query (pass --no-watch to serve a tree that does not change)

A `--store-docs` index never needs watchman. A late daemon (one that
hangs, or reads the tree) does not stop the server, at its start or
later: the queries check every file until watchman answers, and the
server keeps its last token, or at its start the record's token, since
the stat pass runs after it was taken. A daemon gone from its socket, a
restart or a removed watch makes the server take a new token.

On Linux the ask makes each served query 1.1 to 3.2 ms slower (p50,
measured on trees of 161 to 1.25 M files): a query that took 0.4 ms takes
2.1 ms. On linux, chromium and aosp the question to watchman takes 1.1 to
1.2 ms, and the re-check of what it reports 0.4 to 1.5 ms. In a git
checkout watchman reports `.git` at every query, because its sync cookie
changes that directory's mtime, so every query re-checks it. On macOS
each of its three synced calls (two clocks and the question) waits for
FSEvents, and the ask takes about 38 ms (35 to 41 ms on a tree of 60,000
files); a loaded Mac can pass the 100 ms limit, and that query checks
every file. Serve a tree that does not change with `--no-watch`, which
asks watchman nothing.

When watchman cannot answer, the search checks every file and prints one
line on stderr per process that says why and what to do; stdout does not
change:

- `warning: watchman cannot say what changed (<why>); checked every file`,
  where `<why>` is one of: `fmgrep asked watchman to watch /src; later
  searches ask it once its crawl ends`, `watchman is still crawling
  /src`, `cannot run watchman (<error>); install watchman`, `watchman
  get-sockname did not answer within 500 ms`, `watchman get-sockname
  failed (<exit status>)`, `no watchman daemon answers on <socket>
  (<error>)` (the socket in `WATCHMAN_SOCK`),
  `watchman did not answer within 100 ms`, `watchman's ignore_dirs hide
  part of /src`, `/src is on nfs4, a network or shared filesystem:
  watchman does not see changes made there from other machines`,
  `cannot read /proc/self/mountinfo to tell the tree's filesystem
  (<error>)`, `the
  index has no dirs.stamps; re-run index`, `the manifest lists files out
  of order; re-run index`, `cannot resolve /src (<error>)`, or
  `watchman: <its own error>`. A late answer is never used; the record is
  kept, with a newer token when one was taken.
- `warning: more than <n> paths may differ from the index; checked every file (re-run index)`:
  re-checking them would cost more than the stat pass (about 7 us a path
  against 1 us a file). The record then holds no baseline, and each
  search checks every file, until no more than that many differ again
  (then the record holds a baseline again) or the next `index`.
- `warning: more than <n> paths changed since the last query; checked
  every file`: the same limit, for one served query.
- `warning: cannot save <record> (<error>); the next search checks again`.

An edit made while a directory above the root is renamed away and back,
and a hard-linked file edited outside the root, are missed by watchman.

On a network or shared filesystem (NFS, SMB, 9p, Ceph, AFS, Lustre, GPFS
or virtiofs, as `/proc/self/mountinfo` says) watchman sees only the changes
made from this machine, so a token would miss a file changed from
another one. There `index` asks for no watch, and a search takes no
token and saves no record: every search checks every file and prints
the `watchman cannot say what changed` line. The same holds when
`/proc/self/mountinfo` cannot be read (no `/proc` in a container or a
sandbox, for example): the type is then unknown, and fmgrep takes it as
a network filesystem. This check is made on Linux only; elsewhere the
type of the filesystem is not read.

## Shards

Documents are cut into shards greedily, whole documents only, in id order.
Each non-empty document costs its length plus one NUL separator. A shard
closes when the next document would take it past `FMGREP_SHARD_BYTES`
(default 100 MiB), and a document over the cap forms its own shard. Empty
documents index nothing and belong to whichever shard spans them. Labels
are `shard-%08d-%08d.bin`, over the id range `start..end`. A search
re-derives the plan from the labels and `doc_lens`, never from the
environment. A shard text over 2^31-1 bytes (docfm's bound) is refused
before construction.

## blobs.shard

The blobs, in shard order, with no separators; `blob_lens` slices them.
Each blob's length is a multiple of 16 KiB, so every blob starts
page-aligned. The file is mapped at load and used in place, never decoded.

Blob version 8, little-endian. The code-side contract is the table in
`crates/docfm/src/layout.rs`, with `src/wavelet.rs`, `src/wavelet/digits.rs`
and `src/marks.rs` of `crates/docfm-bits` for the structures it holds.

| section | holds |
|---|---|
| header, 2368 bytes | magic `DOCFM\0\x08\0`; text bytes `n`; sentinel row; sampling gaps `min` 4, `max` 8, `ws` 32 and walk bound 31; alphabet bitmap and size; digits per block (256); `c` (first row per byte code); where each code starts below the last level; `num_docs`, `num_samples`; digit starts and payload bytes per level; bits per digit (4) |
| levels | one per 4-bit digit of the codes (2 for more than 16 distinct bytes), each 64-byte aligned: per 16384 digits a u32 payload offset and a u32 count per digit; per 256-digit block a u16 kind and payload offset and a u16 count per digit from its super; then the payloads: none for a block of one digit, plain digits, or runs (a digit and an Elias-gamma length each) |
| marks | the sampled rows as Elias-Fano: their low bits packed, their high bits in unary, a u32 position per 64th zero |
| samples | document id per sampled row, in row order, packed in `ceil(log2 num_docs)` bits (at least 1) |

A row is sampled iff its text position starts a document, or starts a
word, subword or line at least 4 bytes past the previous sample, or is 8
bytes past it (32 on whitespace). A locate walks LF to the nearest sampled
row, in at most 31 steps, without leaving its document. The load checks
the header, each level's zeros against its ones, each code's total, the
marks total and the sentinel row's sample. A blob that passes these checks
but is corrupt elsewhere can miscount or panic, but never reads out of
bounds.

## docs.stamps

`8 x num_docs` bytes: the mtime of each document at index time, as a
little-endian i64 in nanoseconds since the epoch, by id. Then 16 bytes per
document read from UTF-16, ascending by id: its id and its file length, as
little-endian u64s. With `doc_lens`, or that file length, it forms the
`(size, mtime)` stamp that a load checks every file against. Any other
size, order or id is refused with exit 2.

## dirs.stamps

Optional: an index without it walks the whole tree for added files at
every load. An 8-byte magic, `fmdirs\0\x01`, then records to the end of
the file, strictly ascending by path:

| bytes | holds |
|---|---|
| 8 | `len`, u64 LE: the size |
| 8 | `mtime_ns`, i64 LE: nanoseconds since the epoch |
| 4 | `n`, u32 LE |
| `n` | the corpus-relative path, UTF-8, `/`-separated |

`dirs.stamps` starts with the root, the empty path. Each directory is
stamped before it is listed, so any later change to its entries moves its
stamp. The stamp `len = u64::MAX, mtime_ns = i64::MIN` matches no stat, so
its directory is listed at every load. It is written for a directory
stamped within 2 s of the index start (a change in the same timestamp tick
would not move it; writing `.fmgrep` makes the root of a default index
one), one that could not be statted, and a path that collides once decoded
lossily, which is written once.

A file with another magic or version is ignored, as if absent. One with
the right magic that is truncated, not ascending, not UTF-8 or not rooted
is refused with exit 2, like a damaged `docs.stamps`. A `docs.skipped`
left by an older `fmgrep` is ignored.

## ignores.stamps

Optional, with `dirs.stamps`: without it, every load walks the whole tree.
Magic `fmigns\0\x01`, then records as in `dirs.stamps`, strictly
ascending by path: every `.gitignore`, `.ignore`, `.rgignore` and
`.git/info/exclude` in a walked directory, stamped before it is read; the
same four of the root and of each directory above it, prefixed `../` once
per level; and the global git excludes file, by absolute path. The stamp
`len = u64::MAX, mtime_ns = i64::MAX` records an absent file; the other
sentinel and the 2 s rule are those of `dirs.stamps`.

## docs.shard

The documents concatenated, by id, with no separators; `doc_lens` slices
them. They are stored as indexed: UTF-8 BOMs included, UTF-16 decoded.

## Determinism

The same tree, the same ignore files outside it (see Documents) and `FMGREP_SHARD_BYTES` give the same `blobs.shard` and
`docs.shard` bytes, whatever the thread counts. `docs.stamps`,
`dirs.stamps` and `ignores.stamps` follow the mtimes. Only
`build_time_us` and `index_started_ns` differ in the manifest, besides
`corpus_root` when the tree moves.

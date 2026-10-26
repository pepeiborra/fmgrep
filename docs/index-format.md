# Index format

The on-disk layout `fmgrep index` writes (manifest format 2): what each
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
| `docs.stamps` | default | one mtime per document |
| `dirs.stamps` | default | every walked directory's `(size, mtime)` |
| `docs.journal` | default, when a change source marks | change-source tokens, rewritten by each search |
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
| `format` | u32 | `2` |
| `store_docs` | bool | documents are in `docs.shard`, not read from the tree |
| `num_docs` | usize | documents indexed |
| `num_bytes` | u64 | sum of `doc_lens` |
| `bom_bytes` | u64 | leading UTF-8 BOM bytes over all documents |
| `index_size_bytes` | u64 | the structures' size, plus the document text with `store_docs`; an accounting figure, not a file size |
| `build_time_us` | u64 | the index wall: walk, construction and blob flush |
| `files` | [string] | corpus-relative path per document, by id |
| `blobs` | [string] | one `shard-<start>-<end>.bin` label per shard |
| `doc_lens` | [u64] | byte length per document, parallel to `files` |
| `blob_lens` | [u64] | byte length per blob, parallel to `blobs` |
| `corpus_root` | string | absolute path of the tree; present iff not `store_docs` |

`num_bytes - bom_bytes` is the `bytes_searched` that rg reports for the
tree, because rg's searcher strips a leading BOM.

A load refuses, with exit 2:

- a `format` other than 2, naming the remedy: re-run `fmgrep index`. Old
  indexes are refused, never upgraded;
- a `corpus_root` that is present with `store_docs`, or that is absent or
  relative without it;
- `bom_bytes > num_bytes`;
- `files` / `doc_lens` or `blobs` / `blob_lens` of different lengths;
- labels that do not partition `0..num_docs`, or a `blobs.shard` whose
  length is not the sum of `blob_lens`.

## Documents

The walk under PATH defines the document set:

- every regular file, sorted by relative path; ids are positions in that
  order;
- names starting with `.` are pruned, files and directories alike, so
  `.git`, `.sl` and `.fmgrep` are never read;
- symlinked directories are not descended; symlinked files are included;
- files holding a NUL byte, or that look binary (a known binary magic
  number, or over 5% control bytes in the first 8 KiB), are skipped, with
  one summary line on stderr (`-v` lists them);
- paths are stored as UTF-8, with `/` separators; a non-UTF-8 name is
  converted lossily.

## Staleness

A files-layout index answers over the tree as it is now. Before the first
answer, a load stats every document against its `(size, mtime)` stamp:

- a changed file is scanned in full by every search, so answers equal a
  fresh index of the current tree;
- a missing, unreadable or now-binary file is excluded, with one warning
  per process;
- a file the walk's rules find that the index does not list is added:
  scanned in full and printed in path order among the indexed files, with
  no warning. The load stats the directories in `dirs.stamps`, lists only
  those whose stamp moved, and walks new directories below them in full;
- a file the walk skipped as non-text is sniffed again whenever its
  directory is listed: one rewritten in place as text is found once its
  directory's stamp moves, or when a change source or `serve`'s watch
  names it.

`serve` watches the tree, following new files and directories; with
`--no-watch` it misses files added after its first answer. On a filesystem
with 1-second mtimes, a same-size edit within that second goes unnoticed.
The directory check relies on the filesystem updating a directory's mtime
when an entry is created, removed or renamed, as POSIX requires; on one
that does not, delete `dirs.stamps` to make every load walk the tree.
Re-index after many edits.

`index` also asks a running watchman daemon (never started; asked to watch
the tree) for a token before the walk. The token goes to `docs.journal`
(JSON), with the symlinked documents and the directories holding a
dangling symlink. A load asks watchman what changed since its token,
re-checks only those paths, the symlinked documents and those directories,
and skips the stat pass and the directory check; a search then rewrites
the token with what differs now. Watchman is asked from 50,000 documents.
A watchman that answers too slowly or names too many paths, and an index
without `docs.journal`, fall back to the stat pass. An edit made while a
directory above the root is renamed away and back, and a hard-linked file
edited outside the root, are missed by the journal. `FMGREP_JOURNAL`
forces or disables it (`docs/tuning.md`).

A `--store-docs` index never reads the tree: it answers as of index time,
and edits, new files and deletions show up only after the next `index`;
`serve` neither stats nor watches the tree. Written with `--output`, it has
no tree: a PATH is refused and the cwd does not narrow the search.

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
Each blob's length is a multiple of 16 KiB, so every blob, and every
occurrence block inside one, starts page-aligned. The file is mapped at
load and used in place, never decoded.

Blob version 6, little-endian. The code-side contract is the table in
`crates/docfm/src/layout.rs`, with `occ.rs` and `marks.rs` beside it.

| section | holds |
|---|---|
| header, 2176 bytes | magic `DOCFM\0\x06\0`; text bytes `n`; sentinel row; sample rate (default 16); alphabet bitmap and size; `c` (first row per byte code); `num_docs`, `num_samples` |
| supers | per 65536 BWT positions: u32 counts per code, padded to 64 bytes |
| occurrence blocks | from the next 16 KiB boundary; per 4096 positions: u16 counts at the block midpoint, then the code bits of each 64 positions as bit planes |
| marks | a u32 count per 4096 rows, then a u16 in-bucket offset per sampled row |
| samples | u32 document id per sampled row, in row order |

A row is sampled iff its text position is a multiple of the sample rate or
starts a document. A locate walks LF to the nearest sampled row, in fewer
steps than the sample rate, without leaving its document. The load checks
the header, each code's total against the last block, the marks total and
the sentinel row's sample. A blob that passes these checks but is corrupt
elsewhere can miscount or panic, but never reads out of bounds.

## docs.stamps

`8 x num_docs` bytes: the mtime of each document at index time, as a
little-endian i64 in nanoseconds since the epoch, by id. With `doc_lens` it
forms the `(size, mtime)` stamp that a load checks every file against.

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
one), one that could not be statted, one holding a symlink to no file, and
a path that collides once decoded lossily, which is written once.

A file with another magic or version is ignored, as if absent. One with
the right magic that is truncated, not ascending, not UTF-8 or not rooted
is refused with exit 2, like a damaged `docs.stamps`. A `docs.skipped`
left by an older `fmgrep` is ignored.

## docs.shard

The documents concatenated, by id, with no separators; `doc_lens` slices
them. They are stored as read, BOMs included.

## Determinism

The same tree and `FMGREP_SHARD_BYTES` give the same `blobs.shard` and
`docs.shard` bytes, whatever the thread counts. `docs.stamps` and
`dirs.stamps` follow the mtimes. Only `build_time_us`
differs in the manifest, besides `corpus_root` when the tree moves.

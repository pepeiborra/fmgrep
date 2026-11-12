# Index format

The on-disk layout `fmgrep index` writes (manifest format 6): what each
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
| `format` | u32 | `6` |
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

- a `format` other than 6, naming the remedy: re-run `fmgrep index`. Old
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
answer, a load stats every document against its `(size, mtime)` stamp:

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
  directory's stamp moves, or when a change source or `serve`'s watch
  names it;
- when an ignore file in `ignores.stamps` changes, or one appears in a
  listed directory, the load walks the directory it governs in full (the
  root for those above it): an indexed file the rules now skip is excluded
  with no warning, and a file they now allow is added. This repeats at
  every load until the next `index`. `serve`'s watch does the same when it
  sees an ignore file change inside the tree.

`serve` watches the tree, following new files and directories, and exits
2 when the watch cannot start (on Linux, out of inotify watches: raise
`fs.inotify.max_user_watches`). With `--no-watch` it misses files added
after it is ready, and sees a changed file only when a query reads it.
On a filesystem with 1-second mtimes, a same-size edit within that second
goes unnoticed.
The directory check relies on the filesystem updating a directory's mtime
when an entry is created, removed or renamed, as POSIX requires; on one
that does not, delete `dirs.stamps` to make every load walk the tree.
Re-index after many edits.

`index` also asks a running watchman daemon (never started) for a token
before the walk, from the watch on the tree or on a directory above it.
Where watchman watches neither, fmgrep asks it to watch the tree
(`watch-project`). Watchman answers that once its crawl of the tree ends;
`index` waits at most 5 s, and a search at most 100 ms, and the watch
stays when they stop waiting. A watch with `ignore_dirs` gives no token.
The token goes to `docs.journal` (JSON). A load asks watchman what
changed since its token, re-checks only those paths and the ignore files,
and skips the stat pass and the directory check; a search then rewrites
the token with what differs now. Watchman is asked from 50,000
documents.
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
`dirs.stamps` and `ignores.stamps` follow the mtimes. Only `build_time_us`
differs in the manifest, besides `corpus_root` when the tree moves.

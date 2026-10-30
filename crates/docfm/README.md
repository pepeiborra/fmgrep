# docfm

A stand-alone byte-alphabet FM-index for document search: counts by backward
search, the document of each occurrence from doc ids sampled per row. 

## How it works

An FM-index answers `count` by backward search: one rank query per pattern
byte, whatever the size of the text. Answering `locate`, which documents
hold the pattern, is where implementations differ, and docfm is built for
that question alone: it never needs the position of a match, only its
document.

* Samples are document ids, not positions. Most FM-indexes sample the suffix
  array, locate a text position and then look up its document; docfm stores
  the document id at every sampled row and every document start, so a
  locate walk ends at a document id and never crosses into another document.
* Samples sit where patterns start. Rows are sampled at document, word,
  subword and line starts, at least 8 bytes apart, and forced 16 bytes past
  the previous sample (64 in whitespace), so most occurrences of an
  identifier hold their own sample.
* Locate walks both ways. An occurrence reads its own sample first; the
  backward search passes through the rows of every tail of the pattern, so
  a sample a few bytes into the pattern is reached from there and verified
  by a few LF steps back; the rest walk back to the nearest sample. At most
  63 steps per occurrence, about 2 on average for identifiers.
* No suffix array is ever built. libsais writes the BWT directly and returns
  the rows the samples need; peak memory is about six times the text,
  well under what building and then sampling a suffix array takes.
* One read per rank. Occurrences are bit-sliced blocks over the byte
  alphabet rather than a wavelet tree, so a backward-search or LF step reads
  one block and a small hot table instead of one level per bit of the
  alphabet.
* The blob is the index. `FmIndex::view` works over any `AsRef<[u8]>`, a
  mapped file included, copying nothing: a search touches only the pages its
  walk needs, and a server that maps the whole blob is ready as soon as the
  file is open.
* `Cursor` exposes the backward search one byte at a time, with `count` at
  every step, so a regex automaton or a case-insensitive search branches
  over the index natively and drops every branch that counts zero.

The trade-off is size: about 1.55 bytes per byte of text on C code, 1.65 on
Haskell.

## Layout

The text is documents, each followed by one `DOC_END` (0x00). A row is
sampled when its text position starts a document, or starts a word,
subword or line at least 8 bytes past the previous sample, or is 16 bytes
past it (64 on whitespace), and stores that document's id. `locate` reads
the occurrences' own samples, resolves more from samples a few bytes into
the pattern, then walks LF from the rest to the nearest sample, at most 63
steps; none crosses into another document.

Construction builds no suffix array: libsais writes the BWT over the text
and returns the row of every 4th position; the sampled rows are LF walks
of at most 3 steps from those over the finished occurrence blocks. Peak
live heap is about six times the text, mostly libsais's `i32` temporary
array.

The blob (`src/layout.rs`) is the query structure:
`FmIndex::view` works over any `AsRef<[u8]>`, a mapped file slice included,
copying nothing. BWT occurrences are bit-sliced blocks of 4096 positions
(`src/occ.rs`): per-block u16 counts at the midpoint, then the code bits of
64 positions side by side, so a backward-search or LF step reads one block
(one or two 16 KiB pages) plus a small hot table of per-65536 u32 counts.
The sample marks are sparse (`src/marks.rs`): a u32 count per 4096 rows and
a u16 in-bucket offset per marked row, about 0.5 bits per row. Blocks
start on a 16 KiB boundary and the blob length is a multiple of 16 KiB.
`view` checks the header, each code's total against the last block, and the
marks total; a blob consistent there but wrong elsewhere can miscount or
trip the locate invariant (a panic), never read out of bounds. Blobs of
other versions are refused.

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
  subword and line starts, at least 4 bytes apart, and forced 8 bytes past
  the previous sample (32 in whitespace), so most occurrences of an
  identifier hold their own sample.
* Locate walks both ways. An occurrence reads its own sample first; the
  backward search passes through the rows of every tail of the pattern, so
  a sample a few bytes into the pattern is reached from there and verified
  by a few LF steps back; the rest walk back to the nearest sample. At most
  31 steps per occurrence, about half a step on average for identifiers.
* No suffix array is ever built. msbwt, a pure-Rust multi-string BWT by
  induced sorting, writes the BWT directly and returns the rows the samples
  need; peak memory is about six times the text, well under what building
  and then sampling a suffix array takes. Each document sorts as a cyclic
  string, so no row reads into another document.
* A compressed BWT. The BWT is a 16-ary wavelet matrix, two levels of
  4-bit digits for a byte alphabet, each cut into blocks of 256 digits
  stored as nothing (one digit throughout), plain digits or runs: about 3.2
  bits per text byte on source code. A backward-search or LF step reads one
  block per level.
* Compact samples. The sampled rows are Elias-Fano coded and their doc ids
  packed in `ceil(log2 docs)` bits.
* The blob is the index. `FmIndex::view` works over any `AsRef<[u8]>`, a
  mapped file included, copying nothing: a search touches only the pages its
  walk needs, and a server that maps the whole blob is ready as soon as the
  file is open.
* `Cursor` exposes the backward search one byte at a time, with `count` at
  every step, so a regex automaton or a case-insensitive search branches
  over the index natively and drops every branch that counts zero.

The trade-off is size: about 0.73 bytes per byte of text on C code, 0.86 on
Haskell.

## Layout

The text is documents, each followed by one `DOC_END` (0x00). A row is
sampled when its text position starts a document, or starts a word,
subword or line at least 4 bytes past the previous sample, or is 8 bytes
past it (32 on whitespace), and stores that document's id. `locate` reads
the occurrences' own samples, resolves more from samples a few bytes into
the pattern, then walks LF from the rest to the nearest sample, at most 31
steps; none crosses into another document.

Construction builds no suffix array: msbwt (`crates/msbwt`) sorts the
rotations of every document, each a cyclic string, by induced sorting and
returns the BWT with the row of every sampled position, which it carries
down its levels as it sorts. Peak live heap is about six times the text,
msbwt's working arrays.

The blob (`src/layout.rs`) is the query structure:
`FmIndex::view` works over any `AsRef<[u8]>`, a mapped file slice included,
copying nothing. The BWT is a wavelet matrix (`src/wavelet.rs`) over `d`-bit
digits of the codes (`d` = 4 by default; 1 and 2 also read): one level per
digit, most significant first, each holding the digits of the codes in the
order the level above left them, stably sorted by digit. Each level
(`src/digits.rs`) is cut into blocks of 256 digits: a head per block (its
kind, its payload's offset, a u16 count per digit from its super) and a
u32 offset and counts per super, then the payloads: none for a block of one
digit, plain digits, or runs (a digit and an Elias-gamma length each),
whichever is smallest.
The sample marks are Elias-Fano (`src/marks.rs`), the doc ids packed in
`ceil(log2 num_docs)` bits each. The blob length is a multiple of
16 KiB. `view` checks the header, each level's digit counts against its
digit starts, each code's total, and the marks total; a blob consistent
there but wrong elsewhere can miscount or trip the locate invariant (a
panic), never read out of bounds. Blobs of other versions are refused.

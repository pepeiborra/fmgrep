# docfm

A stand-alone byte-alphabet FM-index for document search: counts by backward
search, the document of each occurrence from doc ids sampled per row. Its
dependencies are `libsais`, for BWT construction with aux rows, and
`rayon`: libsais runs single-threaded and the other build phases run on the
caller's rayon pool.

The text is documents, each followed by one `DOC_END` (0x00). A row is
sampled when its text position is a multiple of the sample rate or starts a
document, and stores that document's id; `locate` walks LF to the nearest
sample, fewer than rate steps, and never crosses into another document.

Construction builds no suffix array: libsais writes the BWT over the text
and returns the row of every `r`-th position, `r` the largest power of two
up to the sample rate (at most 64); the other sampled rows are LF walks over
the finished occurrence blocks. Peak live heap is about five times the text,
mostly libsais's `i32` temporary array.

The blob (version 6, layout in `src/layout.rs`) is the query structure:
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

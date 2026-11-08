# msbwt

The multi-string BWT of NUL-terminated documents, in safe Rust, by induced
sorting without a suffix array: Holt's
[msbwt-is](https://github.com/holtjma/msbwt-is) over a byte alphabet, in
memory, with its parallel passes on the current rayon pool.

Each document with its terminator is one cyclic string, so no row reads
into another document. fmgrep builds its index with it, which takes libsais,
a C dependency, out of fmgrep's build.

```rust
let text = b"banana\0ananas\0".to_vec();
let bwt = msbwt::bwt(text.clone())?;
// The rows of chosen positions too: a bitmap, bit p % 64 of word p / 64.
let (bwt2, rows) = msbwt::bwt_sampled(text, &[1 | 1 << 7])?;
assert_eq!(bwt, bwt2);
for r in rows {
    let (row, position) = (r >> 32, r as u32);
}
```

## Measured

- Peak heap during a build: about 4.3 times the text, or 5.9 times with
  sampled rows.
- Indexing all of Chromium on a Ryzen 5 3600, two 250 MB shards at a time:
  290 s against 297 s with libsais, one run each.
- Indexing all of Chromium on 10-cpu EPYC workers: from 3% faster to 6%
  slower than libsais, one run each.

## Testing

The tests check the BWT and sampled rows against a naive sort of every
rotation, and larger inputs by inverting the BWT, with forced name widths,
piece counts and switch levels so that small inputs run every code path.

```sh
cargo test -p msbwt --release
```

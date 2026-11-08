# docfm-bits

The succinct structures under docfm's blob, over plain `&[u8]`, knowing
nothing of documents, samples or the blob header. Reads clamp to the bytes
given: tampered input miscounts, never reads out of bounds.

The crate is layered by file, each layer using only the ones below it:

| layer | file | holds |
|---|---|---|
| public | `src/wavelet.rs` | a `k`-ary wavelet matrix over byte codes |
| private | `src/wavelet/digits.rs` | a digit sequence in blocks of plain digits, runs or one digit |
| public | `src/marks.rs` | Elias-Fano marks over rows |
| public | `src/packed.rs` | integers of one width, packed end to end |
| private | `src/word.rs` | clamped little-endian reads, slot equality, the bit writer, Elias-gamma, `select` in a word |

The public modules are the whole surface. Their preconditions are types:
`wavelet::Params` holds only digit widths and block sizes the format allows,
`wavelet::Shape` and `wavelet::Encoded` have private fields, and
`packed::Packed` has only widths its reads can serve.

## Testing

Each layer has its own tests against naive oracles, in the `tests.rs` beside
it.

```sh
cargo test -p docfm-bits
```

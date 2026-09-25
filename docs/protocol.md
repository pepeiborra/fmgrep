# Serve protocol and JSON output

The JSON that fmgrep speaks: the `serve` protocol, `--server`, and the
`--json` output of `index` and of a search.

> **UNSTABLE.** Methods, params and fields may change in any release.

The examples below were captured from a release build over a three-file
tree; `*_us` timings vary from run to run.

## Transport

`fmgrep serve [PATH]` loads the index once, the same way a search does, and
then answers newline-delimited JSON-RPC 2.0: one request per line, one
response line each.

- `--listen ADDR` (default `127.0.0.1:0`): TCP. The bound address is the
  only line on stdout, printed once the index is loaded, so it doubles as
  the readiness signal. Connections are served one at a time, each until
  EOF, until a `shutdown`.
- `--stdio`: requests on stdin, responses on stdout, ending at EOF.
- A load that fails (no index, an old format, a damaged index) exits 2
  before the readiness line, with the message a search would print.
- `serve` watches the tree for changed files unless `--no-watch` is given
  (see `docs/index-format.md`).

## Methods

| method | params | result |
|---|---|---|
| `query` | below | an `OutputReport` |
| `status` | none | `{index_name, store_docs, files, num_bytes, index_size_bytes, load_us}` |
| `shutdown` | none | `{"status":"shutting down"}`, then the server exits 0 |

`query` params; unknown fields are refused:

| field | type | default | meaning |
|---|---|---|---|
| `terms` | [string] | required unless `expr` is given | each a literal, or `/re/` for a regex |
| `expr` | Expr | absent | one expression instead of `terms` |
| `regex` | bool | false | every term is a regex (`--regex`) |
| `ignore_case` | bool | false | `-i` |
| `multiline` | bool | false | `-U` |
| `dotall` | bool | false | `--multiline-dotall` |
| `approx_counts` | bool | true | must be true: only counts are served |
| `occurrences` | bool | false | `--occurrences` totals instead |

`expr` with non-empty `terms`, or with `regex: true`, is refused.

```text
> {"jsonrpc":"2.0","id":1,"method":"query","params":{"terms":["greet","/hel+o/"]}}
< {"jsonrpc":"2.0","id":1,"result":{"index_name":"fmgrep","index_time_us":1268,"index_size_bytes":34448,"num_files_indexed":3,"queries":[{"term":"greet","latency_us":11,"total_matches":3,"docs_matched":1},{"term":"/hel+o/","latency_us":270,"total_matches":3,"docs_matched":2,"counts_exact":false}]}}
> {"jsonrpc":"2.0","id":4,"method":"status"}
< {"jsonrpc":"2.0","id":4,"result":{"index_name":"fmgrep","store_docs":false,"files":3,"num_bytes":228,"index_size_bytes":34448,"load_us":1268}}
```

## Expressions

An Expr is `{"term": s}` (a literal, verbatim), `{"regex": s}`, `{"and":
[Expr, ...]}` or `{"or": [Expr, ...]}`, nested freely, and every `and` /
`or` needs at least one operand. A document satisfies a leaf when the leaf
matches anywhere in it. `ignore_case`, `multiline` and `dotall` apply to
every leaf. The report names the query in the text form, with `AND` /
`OR` in capitals. A request nesting `and` / `or` more than 62 deep exceeds
serde_json's recursion limit and fails with -32700. The CLI's `--expr`
caps nesting at 60.

```text
> {"jsonrpc":"2.0","id":2,"method":"query","params":{"expr":{"and":[{"term":"greet"},{"or":[{"term":"hello"},{"regex":"sort[A-Z]"}]}]}}}
< {"jsonrpc":"2.0","id":2,"result":{"index_name":"fmgrep","index_time_us":1268,"index_size_bytes":34448,"num_files_indexed":3,"queries":[{"term":"greet AND (hello OR /sort[A-Z]/)","latency_us":84,"total_matches":5,"docs_matched":1,"counts_exact":false,"leaves":[{"term":"greet","total_matches":3},{"term":"hello","total_matches":2},{"term":"/sort[A-Z]/","total_matches":0}]}]}}
```

## Reports

`OutputReport`:

| field | meaning |
|---|---|
| `index_name` | `"fmgrep"` |
| `index_time_us` | the search's index load wall; not the manifest's `build_time_us` |
| `index_size_bytes` | as in the manifest |
| `num_files_indexed` | documents in the index |
| `queries` | one `QueryReport` per term, in order, or one for the expression |

`QueryReport`:

| field | meaning |
|---|---|
| `term` | the term as given, or the expression's text form |
| `latency_us` | this term's probe and fetch |
| `total_matches` | matches (see below) |
| `docs_matched` | documents matched; absent under `occurrences` |
| `counts_exact` | present, as `false`, only when the counts may differ from rg's |
| `leaves` | expressions only, in counting modes: `[{term, total_matches}]` per leaf |

Counting semantics:

- **`--approx-counts`**, which is also all that serve answers, gives the
  index's own numbers: document-wide, with overlapping literal matches,
  and a pattern may match across lines. `counts_exact` is false for every
  pattern, and for a literal that is empty, overlaps itself or holds `\n`.
- **`--occurrences`** gives the same `total_matches` with no
  `docs_matched`. A literal is counted from the index alone. AND is
  refused; an OR sums its leaves.
- **`-c`, `--count-matches` and `-l`** with `--json` print the report
  with rg's counts, line-anchored and non-overlapping. `-l` proves
  membership only, so its `counts_exact` is false.
- **An expression** has an exact `docs_matched`. Its `total_matches` is
  the sum of the leaves' counts within those documents, exact only for
  plain literals that cannot overlap.

## Errors

| code | when | example message |
|---|---|---|
| -32700 | the line is not JSON (or is nested too deep) | `parse error: expected ident at line 1 column 2` |
| -32600 | a batch, a notification (no id), or a bad `jsonrpc` / `method` | `batch requests are not supported` |
| -32601 | unknown method | `unknown method "nope"` |
| -32602 | bad params | `invalid params: approx_counts must be true` |
| -32000 | the search failed | `error: --occurrences places no match in a document, so it cannot answer AND` |

The `id` is echoed, or null when the request has none that can be read.
Serving continues after any error.

## --server

`fmgrep --server ADDR TERM...` sends one `query` (all terms, or the
expression from `--expr` / `--and` / `--or` / `--expr-json`) and prints the
result as `--approx-counts` (or `--occurrences`) would locally: one
`<term>: N matches in M files` line per term, or the report with `--json`.
`--status` prints the status object and `--shutdown` stops the server.
Flags only a local search honours are refused with exit 2: `--index`,
`--color`, context and line-number flags, heading flags, `--matches`, `-o`,
`-c`, `--count-matches` and `-l`. An error response exits 2, as
`error: server: <message>`.

## CLI --json

| command | stdout |
|---|---|
| `fmgrep index --json` | one `IndexSummary` line |
| a search with `--json`, alone or with a counting mode | one `OutputReport` line |
| a search with `--json --matches` or `--json -o` | rg's JSON event stream |

`IndexSummary` is `{index_name, store_docs, path, num_files, num_bytes,
index_size_bytes, build_time_us}`:

```text
$ fmgrep index --json
{"index_name":"fmgrep","store_docs":false,"path":"./.fmgrep","num_files":3,"num_bytes":228,"index_size_bytes":34448,"build_time_us":840}
```

The event stream is rg 15.2.0's, byte for byte: `begin`, one `match` or
`context` object per line (`lines.text` keeps the terminator; non-UTF-8
text becomes `{"bytes": <base64>}`), `end` with the file's stats, and a
final `summary`. It streams, one fetch batch at a time. Deviations, all
deliberate:

1. N terms produce N concatenated streams in argv order, each closed by
   its own `summary`. A foreign event type would break rg-schema consumers
   worse than a repeated one.
2. Each file's `stats.elapsed` is zero, because the scan reads the index,
   not the file. `summary.stats.elapsed` is the term's latency, and
   `elapsed_total` its wall clock.
3. `binary_offset` is always null, because binary files are skipped at
   index time.
4. `searches` and `bytes_searched` cover the whole index, which is what rg
   over the indexed file list reports. Leading UTF-8 BOMs are excluded, as
   rg's searcher strips them.
5. `path` is always `{"text": ...}`, because manifest paths are UTF-8.
6. `-c`, `--count-matches` and `-l` with `--json` print the report; rg
   ignores `--json` there and prints plain counts.
7. No match still exits 0, and no terms prints nothing.

Under `-U`, a term matched over the whole document reports one `match`
event per block of consecutive matched lines, and its submatches are the
whole matches, terminators included.

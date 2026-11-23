# Serve protocol and JSON output

The JSON that fmgrep speaks: the `serve` protocol, `--server`, and the
`--json` output of `index` and of a search.

> **UNSTABLE.** Methods, params and fields may change in any release.

The examples below were captured from a release build over a three-file
tree; `*_us` timings vary from run to run.

## Transport

`fmgrep serve [PATH]` loads the index once, the same way a search does, and
then answers newline-delimited JSON-RPC 2.0: one request per line, one
response line each, after any [frames](#streamed-output).

- `--listen ADDR` (default `127.0.0.1:0`): TCP. The bound address is the
  only line on stdout, printed once the index is loaded and the tree
  checked, so it doubles as the readiness signal. `fmgrep serve` answers
  one connection at a time, each until EOF, until a `shutdown`. A client
  that reads its answer slowly, or keeps its connection open, delays the
  other clients.
- `--stdio`: requests on stdin, responses on stdout, ending at EOF.
- A load that fails (no index, an old format, a damaged index) exits 2
  before the readiness line, with the message a search would print.
- Before each query, `serve` asks watchman what changed in the tree since
  the query before ([Watchman](index-format.md#watchman)). With
  `--no-watch`, for a tree that does not change, it contacts no watchman
  and checks the tree once, at its start. When watchman cannot answer, that
  query checks every file, and the server says why on its stderr; the
  response does not. Without `--no-watch`, a server that cannot use
  watchman at all exits 2 before the readiness line, with the cause. A
  `--store-docs` index never reads the tree and never needs watchman.
- `serve` needs its documents: `docs.shard`, or the tree with
  `docs.stamps` and at most half the indexed files. Otherwise it exits 2
  before the readiness line.

## Methods

| method | params | result |
|---|---|---|
| `query` | below | an `OutputReport`, `{text, report}`, or frames then `{report}` |
| `status` | none | `{format, store_docs, files, num_bytes, index_size_bytes, load_us, root, commit}` |
| `shutdown` | none | `{"status":"shutting down"}`, then the server exits 0 |

`query` params; unknown fields are refused:

| field | type | default | meaning |
|---|---|---|---|
| `terms` | [string] | required unless `expr` is given | each a literal, or a regex when `regex` is true; the CLI sends one, and several `-e` as an `or` expression |
| `expr` | Expr | absent | one expression instead of `terms` |
| `regex` | bool | false | every term is a regex; the CLI sends true, and false for `-F` |
| `ignore_case` | bool | false | `-i` |
| `multiline` | bool | false | `-U` |
| `dotall` | bool | false | `--multiline-dotall` |
| `mode` | string | `counts` | what to answer, below |
| `context` | `{before, after}` | 0, 0 | `-B`, `-A` |
| `line_numbers` | bool | true | false: `-N` |
| `heading` | bool | true | false: `--no-heading` |
| `color` | string | `never` | `always`: `--color always`; a file's matches past its first 1,000 matched lines print without colour, except under `only` |
| `globs` | [string] | [] | `-g`, in order; `!` excludes |
| `iglobs` | [string] | [] | `--iglob`, after `globs` |
| `glob_case_insensitive` | bool | false | `--glob-case-insensitive` |
| `paths` | [{path, name}] | [] | PATH arguments: `path` relative to the indexed root (`""` the root), `name` as typed (`""`: the cwd of a search without PATH), which its files print under |
| `cwd` | string | `""` | the client's working directory: stripped as by rg from printed paths before `globs` match them; without `paths` and `commit`, the server searches only below it when it lies in the served tree |
| `filename` | bool | true | false: one file PATH; human output omits its path except under `-l` |
| `stream` | bool | false | send the output as [frames](#streamed-output) |
| `rendered` | bool | false | with `stream`: frames of the printed bytes, not one per event; refused without `stream` (-32602) |
| `stats` | bool | false | `--stats`: modes `lines` and `only` print rg's statistics after each term's output, and `lines` reports matches (below) |
| `format` | u32 | absent | the manifest format the client reads; a server of another answers -32001 |
| `commit` | string | absent | the served commit (hex id) the client compared its checkout with; a server that serves another, or none, answers -32001 |
| `skip` | [string] | [] | the skip list: files the server leaves out, because the client searches them itself; each a path from the root, `d/` for every file below `d`, or `/` for every file; at most 10,000; needs `commit` and `stream`, else -32602 |

`expr` with non-empty `terms`, or with `regex: true`, is refused.

A term is read as the request says, whoever sent it. With `regex` false, it
is a literal verbatim, a `/re/` term included. With `regex` true, it is a
regex, the empty one too, which matches every line. A regex term that is
nothing but a literal, such as `HscEnv` or `a\.b`, is answered as that
literal is, from the index alone, and so is a `{"regex"}` leaf of an
expression. A server answers what its own build reads, and client and
server are one build: a client of v0.1.0 sends `/re/` with `regex` false,
which this server reads as a literal. The fields of the request, `expr`
and `--expr-json` are those of v0.1.0.

`mode` is `counts` (`--json --approx-counts`), `lines` (a plain search),
`files` (`-l`), `count` (`-c`), `count_matches`, `only` (`-o`) or
`json_matches` (`--json --matches`). `counts` answers the bare
`OutputReport`.
Every other mode answers `{"text", "report"}`: `text` is what the local
search prints, byte for byte, and `report` its `OutputReport`. When that
output is not UTF-8, `bytes` (standard base64) replaces `text`.
`json_matches` gives the matches as structured events.

```text
> {"jsonrpc":"2.0","id":1,"method":"query","params":{"terms":["greet","hel+o"],"regex":true}}
< {"jsonrpc":"2.0","id":1,"result":{"index_time_us":4305,"index_size_bytes":34456,"num_files_indexed":3,"queries":[{"term":"greet","latency_us":37,"total_matches":6,"docs_matched":3},{"term":"hel+o","latency_us":90,"total_matches":2,"docs_matched":2,"counts_exact":false}]}}
> {"jsonrpc":"2.0","id":3,"method":"query","params":{"terms":["hello"],"mode":"lines"}}
< {"jsonrpc":"2.0","id":3,"result":{"text":"README.md\n3:Say hello with greet.\n\nsrc/Lib.hs\n4:greet name = \"hello \" ++ name\n","report":{"index_time_us":4305,"index_size_bytes":34456,"num_files_indexed":3,"queries":[{"term":"hello","latency_us":47,"total_matches":2,"docs_matched":2}]}}}
> {"jsonrpc":"2.0","id":4,"method":"query","params":{"terms":["greet"],"mode":"files"}}
< {"jsonrpc":"2.0","id":4,"result":{"text":"README.md\napp/Main.hs\nsrc/Lib.hs\n","report":{"index_time_us":4305,"index_size_bytes":34456,"num_files_indexed":3,"queries":[{"term":"greet","latency_us":26,"total_matches":3,"docs_matched":3,"counts_exact":false}]}}}
> {"jsonrpc":"2.0","id":5,"method":"status"}
< {"jsonrpc":"2.0","id":5,"result":{"format":9,"store_docs":false,"files":3,"num_bytes":197,"index_size_bytes":34456,"load_us":4305,"root":"/src/demo"}}
```

`root` is where the server reads the documents: the tree, or the index
directory holding `docs.shard`. `format` is the manifest format of the
served index ([index-format.md](index-format.md#manifestbin)); a server
older than this field leaves it out. `commit`, as `{"vcs":"git","id":"<40
hex>"}`, is the served commit: the commit whose files a `--store-docs`
index holds ([index-format.md](index-format.md#served-commit)). It is
absent when the index records none, and from every index that reads the
tree. `why_no_commit` is a string: why a `--store-docs` index records no
served commit, as the note of its build said it (`the checkout holds a
submodule (agents/internal)`). It is absent when the index records a
commit, from every index that reads the tree, and from a server older
than this field; a client ignores it where it does not know it. The text
is the build's: it can name paths of the server's machine (the indexed
directory, its git checkout, the global excludes file), as `root` does.
Build with `--require-served-commit` to serve no index without a commit.
`fmgrep status --server ADDR` prints them as `format: 9`, `commit: git
<id>` and `why_no_commit: <why>`, and `none` for a field the reply leaves
out.

### Streamed output

With `stream: true`, a rendering mode sends its output while the search
runs, as frames before the response line: one frame per output event, so
that the client prints each through its own writer, and can put files of
its own between them in path order. A frame is its length in ASCII
decimal, `\n`, then that many bytes; a response line starts with `{`
instead. A frame's payload is one JSON line, then, for a file, its
printed bytes:

| event | payload |
|---|---|
| the corpus, once | `{"corpus":[docs,bytes]}`: what rg's `--json` summary and `--stats` count as searched, less the skip list's files (their bytes less their BOMs in `json_matches` and with `stats`, which print them) |
| a term starts | `{"term":"<term>"}` |
| a matching file | `{"doc":"<path from the root>","path":"<path as printed>","stats":[matches,matched_lines,bytes_printed]}`, a newline, then the file's printed bytes; the three counts are 0 in `lines` without `stats` |
| a term ends | `{"end":<QueryReport>}` |

Files come in path order within each term. Banners, headings and `--`
separators are not in the frames: the client adds them. A file's frame
holds all that the file prints, however long. The result is
`{"report"}`. A search that fails after output started sends the frames
so far, then the error. `counts` ignores `stream`. Connections are still
served one at a time.

A client with no files of its own to put between the server's has no use
for the events, and decoding one frame per file costs it CPU. With
`rendered: true` as well, the server sends what a local search prints
(its banners, headings, separators and statistics included) as frames
of at most 64 KiB, cut wherever the 64 KiB fall, and the client copies
them to its stdout. The result is the same `{"report"}`. A query that
searched no file sends what the local search prints before its error
(statistics, rg's JSON summary), and its report carries
`none_searched`. A request without `rendered` gets one frame per event,
as before: a client that puts files of its own between the server's needs
it.

```text
> {"jsonrpc":"2.0","id":7,"method":"query","params":{"terms":["hello"],"mode":"lines","stream":true,"rendered":true}}
< 74
< Lib.hs
< 1:greet name = "hello " ++ name
<
< README.md
< 1:Say hello with greet.
< {"jsonrpc":"2.0","id":7,"result":{"report":{"index_time_us":7059,"index_size_bytes":19440,"num_files_indexed":2,"queries":[{"term":"hello","latency_us":207,"total_matches":2,"docs_matched":2}]}}}
```

```text
> {"jsonrpc":"2.0","id":6,"method":"query","params":{"terms":["hello"],"mode":"lines","stream":true}}
< 18
< {"corpus":[2,52]}
< 17
< {"term":"hello"}
< 81
< {"doc":"Lib.hs","path":"Lib.hs","stats":[0,0,0]}
< 1:greet name = "hello " ++ name
< 79
< {"doc":"README.md","path":"README.md","stats":[0,0,0]}
< 1:Say hello with greet.
< 77
< {"end":{"docs_matched":2,"latency_us":348,"term":"hello","total_matches":2}}
< {"jsonrpc":"2.0","id":6,"result":{"report":{"index_time_us":171,"index_size_bytes":19494,"num_files_indexed":2,"queries":[{"term":"hello","latency_us":348,"total_matches":2,"docs_matched":2}]}}}
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
< {"jsonrpc":"2.0","id":2,"result":{"index_time_us":4305,"index_size_bytes":34456,"num_files_indexed":3,"queries":[{"term":"greet AND (hello OR /sort[A-Z]/)","latency_us":63,"total_matches":6,"docs_matched":2,"counts_exact":false,"leaves":[{"term":"greet","total_matches":4},{"term":"hello","total_matches":2},{"term":"/sort[A-Z]/","total_matches":0}]}]}}
```

## Reports

`OutputReport`:

| field | meaning |
|---|---|
| `index_time_us` | the search's index load wall; not the manifest's `build_time_us` |
| `index_size_bytes` | as in the manifest |
| `num_files_indexed` | documents in the index |
| `queries` | one `QueryReport` per term, in order, or one for the expression |
| `none_searched` | present, as `true`, only when a query with `commit` found that its PATHs, cwd and globs keep it from every file it answers for; such a query does not fail, as its client searches other files too |
| `unindexed` | present only when a PATH of the request, as typed, has no file at it or below it that the index or the added files hold: a hidden, ignored, binary or symlinked one. The query does not fail; the CLI prints an error for each and exits 2, but for a directory in which rg would read no file either (empty, or only hidden, ignored or binary files), which is no error. A client that searches files of its own drops a PATH that holds one of them |

`QueryReport`:

| field | meaning |
|---|---|
| `term` | the term as given, or the expression's text form |
| `latency_us` | this term's probe and fetch |
| `total_matches` | matches, or matched lines under `-c` and in `lines` without `stats` (see below) |
| `docs_matched` | documents matched |
| `counts_exact` | present, as `false`, only when the counts may differ from rg's |
| `leaves` | expressions only, in counting modes: `[{term, total_matches}]` per leaf |

Counting semantics:

- **`--approx-counts`** (serve's `counts` mode) gives the index's own
  numbers: document-wide, with overlapping literal matches, and a pattern
  may match across lines. `counts_exact` is false for every pattern, and
  for a literal that overlaps itself or holds `\n`. The empty pattern
  counts one match per line.
- **`--count-matches` and `-l`** with `--json` print the report with
  rg's counts, line-anchored and non-overlapping. `-l` proves
  membership only, so its `counts_exact` is false.
- **`-c`** reports in `total_matches` the sum of the counts it prints:
  matched lines, as `rg -c` counts them.
- **`lines`** reports in `total_matches` the lines it printed as matched,
  which it counts as it prints them: rg's matched lines of `--stats`.
  They equal the `-c` count except under `-U`, where `rg -c` counts a match
  over several lines once. With `stats` it finds each match of
  every printed line again and reports rg's matches, as `json_matches`
  and `only` always do.
- **An expression** has an exact `docs_matched`. Its `total_matches` is
  the sum of the leaves' counts within those documents, exact only for
  plain literals that cannot overlap.

## Errors

| code | when | example message |
|---|---|---|
| -32700 | the line is not JSON (or is nested too deep) | `parse error: expected ident at line 1 column 2` |
| -32600 | a batch, a notification (no id), or a bad `jsonrpc` / `method` | `batch requests are not supported` |
| -32601 | unknown method | `unknown method "nope"` |
| -32602 | bad params | `` invalid params: missing field `terms` `` |
| -32603 | the server hit a bug (a panic); it goes on serving | `error: internal error: corrupt docfm index: doc 7 out of range` |
| -32000 | the search failed | `` error: invalid regex pattern "(": regex parse error: ... `` |
| -32001 | `format` or `commit` is not this server's | `this server serves commit 81b12d8... of index format 9` |

The `id` is echoed, or null when the request has none that can be read.
Serving continues after any error.

## --server

`fmgrep --server ADDR [OPTIONS] PATTERN [PATH]...` (or `-e PATTERN... [PATH]...`) takes every search flag but
`--index` and prints what the local search prints. It sends one `query`
(the pattern, several `-e` as one `or` expression, or the expression) with `stream: true` and `rendered: true`,
and copies the frames to its stdout; only a search that must put files of
its own between the server's ([From a checkout](#from-a-checkout)) asks
for a frame per file, and prints each through its own writer. `--json`
alone and `--approx-counts` send `mode` `counts`, with no `rendered`. `--color auto`, line numbers and
headings follow the client's stdout, as in rg. `--shutdown` stops the
server; `fmgrep status --server ADDR --json` prints the status object.
An error response exits 2, as `error: server: <message>`, after any
output already received. When
the server cannot be reached, a `.fmgrep` found by walking up from the cwd
answers, and stderr says `warning: Using local index <dir>: cannot reach
the server at ADDR (<cause>)`; with none, the search exits 2, as `error:
cannot reach the server at ADDR: <cause>`.

The client first asks for `status`, on the connection that then carries
the query, and refuses a server whose index format is not its own, or
that reports none: use one fmgrep version on both sides. Its query
carries `format`; a server that answers it with -32001 (its index
changed after the `status`) exits 2, as `error: the server at ADDR
changed its index during this search; run the search again`. A later
search from a git checkout skips the `status`: see the pin, below.

### From a checkout

When the server reports a served commit and the cwd is in a git checkout
that holds it, PATH arguments and the cwd resolve against the top of that
checkout, as a local index of the checkout would read them, and the query
carries `commit`: the server's tree may be anywhere.

The client then finds the skip list, with `git status` in the checkout,
and `git diff-tree` between the commit and HEAD when HEAD moved: each
path that differs from the commit (an untracked directory as `d/`, and
a path that is a directory now also as `d/`), the whole directory of an
ignore file that differs, and each tracked file that the client's
`info/exclude` or global excludes file ignores. It searches the files
of that list that a local index of the checkout would hold (a path
below a symlinked directory is not one), sends the list as `skip`, and
prints its files between the
server's in path order. Counts, rg's JSON summary and the `--stats`
block cover its files and the server's.

What the client cannot search of that list it names on stderr, and the
answer lacks it: a file it cannot read, or one over 2 GiB, as `warning:
skipping PATH: why`, in every search that lists it; a path it cannot
look at, because a directory above it is closed to it, as `warning:
skipping PATH: cannot stat (why)`; a directory of the list it cannot
list, as `warning: cannot list DIR: why; files under it may be missing
from the answer`; and a directory that `git status` could not open, as
`warning: git status: could not open directory 'd/': why; untracked
files under it are not searched`. A directory below one the client walks
is named once, by the walk. A binary file is left out quietly, as
the index does.

The warnings cover the whole list, whatever the PATH or `-g` of the
search, where rg names only what it searches. Git leaves the untracked
files of a directory it cannot open out of its list, so the last line
comes only from a search that runs git: after the baseline, later
searches run none, and learn of the directory only when watchman names a
path in it. Git leaves out the tracked files of such a directory as
well, and says nothing of them (it prints `d/f.c: Permission denied`,
which the client does not read): the server's copy of each answers,
where rg leaves it out. The client reads git's messages in English: it
runs git with `LC_ALL=C`.

It asks git once and saves what git found with a watchman token in a
record ([index-format.md](index-format.md#watchman)); later searches ask
watchman what changed since, and run no git. A search that asks git for
a new baseline says why, in one line: `warning: asked git what differs
from the served commit <id> (<why>; took <time>); later searches ask
watchman`. When git's settings changed while git ran, the line ends
with `git's config files changed while git ran; the next search asks git
again` instead, and no record is saved. Without watchman every search
asks git, and the first for each reason and served commit (every one
under `FMGREP_VERBOSE=1`) prints `warning:
watchman cannot say what changed (<why>); asked git what differs from
the served commit <id> (took <time>)`. Either line prints only when the
client can search the files that git listed.

After each `status`, the client writes a pin
([index-format.md](index-format.md#watchman)): the served commit, for
this checkout and server address. A later search that finds a pin of a
commit sends no `status`. It searches as with a status of that commit,
since a search from a checkout reads only the format and the commit of
a status, and sends its query, which carries both. A server of another
commit or format refuses it with -32001 before it searches. The
client's lines on stderr wait for the server's first frame or result.
When the server refuses the query, or the search fails before an
answer, the client asks for `status` on the same connection. If the
status has the pinned commit, the lines print and the search ends as it
would have. If it has another format, the search fails as above. If it
has another commit, or none, the lines are dropped, the pin is
rewritten or removed, and the search runs as one without a pin: it
prints what a first search prints. A server without a served commit,
and a cwd in no checkout, get a `status` on every search. `fmgrep
status --server ADDR` always asks the server.

`FMGREP_VERBOSE=1` adds two notes: whether the search asked for the
`status`, and why; and where the skip list came from, the sizes, the
served commit and the record.

Where the client cannot compute its local changes soundly, it sends no
skip list, searches nothing itself, and prints one warning, `warning:
local changes are ignored (<why>); these results are the server's
alone; use --ignore-local-changes to avoid this warning`. That is the
case for assume-unchanged or skip-worktree entries, a sparse checkout, a
filter, line-ending, `ident` or encoding conversion that applies to a
tracked file, a submodule, an `.ignore` or `.rgignore` file above the
checkout with a pattern that can match a file of it, and more
than 10,000 differing paths or files to search (as when a changed ignore
file governs a large directory). It is also the case for a checkout
without the served commit, and for a served commit that is not a full
git commit id (40 or 64 lowercase hex digits; such a commit is never
handed to git): PATHs and the cwd still resolve against the top of the
checkout, and the query carries the served commit. A server that
reports no served commit makes a search fall back from any checkout
when it serves a `--store-docs` index, and from a checkout outside its
tree when it serves a files-layout index. The warning then names the
server and says why it serves no commit: `the server at ADDR reports no
served commit: <why>`, with the server's `why_no_commit`, or `only a
--store-docs index records one` for a files-layout server. A
`--store-docs` server that gives no reason (an index made by hand) gets
the words before the colon alone. A files-layout server reads
its tree, so a search from inside that tree gets the tree as it is and
does not fall back. A server that reports a served commit makes a
search from a cwd in no checkout fall back too. For a server with no
served commit, and for a cwd in no checkout, PATHs and the cwd resolve
against the served tree. Each search that must fall back finds that out
again, by the same checks, and warns again.

With `--ignore-local-changes` the client prints the server's answer
alone, and no warning about local changes: it runs no git and asks no
watchman. PATH arguments and the cwd resolve as in the fallback. A
search with the flag that a local index would answer fails, with exit 2:
with no server, `error: --ignore-local-changes needs --server ADDR or
FMGREP_SERVER`; when a local index wins over `FMGREP_SERVER`, with an
error that names the index and says to pass `--server`; and a server
that cannot be reached is an error, `error: cannot reach the server at
ADDR: <cause>`, even when a local index is found. It conflicts with
`--index` and `--shutdown`; `fmgrep status` does not take it.

Known limits of this first version:

- git only.
- Only a `--store-docs` index records a served commit.
- An ignore file that differs from the commit puts its whole directory
  on the skip list.
- Files that git ignores but rg searches are not seen: git status runs
  without `--ignored`.
- When the server moves to a new commit during a search, the search
  stops and asks to be run again.
- A pinned query carries the commit's id, not its VCS: the server
  checks the id. This fmgrep records only git commits.
- After the server moves to another commit, the next search sends three
  requests, and compares the checkout with the old commit before it
  learns of the new one.
- `--json` alone and `--approx-counts` print the server's
  `num_files_indexed` and `index_size_bytes`.
- Under `--stats` with a PATH, a glob or a cwd below the top, the bytes
  searched hold the byte-order marks of the server's files, as a local
  index's do, but not those of the client's files, as rg's do not.
- On a file system that ignores case (macOS by default), a rename that
  changes only the case of a name prints the old name: in place of the
  new one after `mv`, next to it after `git mv`.
- With git before 2.42, which cannot name its system config and
  attributes files, a setting added to a system file that held none at
  the baseline is not seen until another stamped input moves. Nor is
  another `git` on `PATH`, whose built-in system files may be others.
- fmgrep serve answers one connection at a time. A client that reads its
  answer slowly delays the other clients.

`FMGREP_SERVER=ADDR` stands for `--server ADDR` in a search that has
neither `--server` nor `--index` and finds no `.fmgrep` by walking up from
the cwd. When it finds one, that index answers, and stderr says
`warning: Using local index <dir>, not the server at ADDR
(FMGREP_SERVER)`. `--shutdown` needs `--server`. An empty value is
unset. The command `fmgrep status` reads it as a search does: where the
server answers, status prints the `status` reply, one `name: value`
line per field (`--json`: the reply). `fmgrep status --server ADDR` is
the search's `--server`.

## CLI --json

| command | stdout |
|---|---|
| `fmgrep index --json` | one `IndexSummary` line |
| a search with `--json`, alone or with a counting mode | one `OutputReport` line |
| a search with `--json --matches` or `--json -o` | rg's JSON event stream |

`IndexSummary` is `{store_docs, path, num_files, num_bytes, index_size_bytes,
build_time_us}`:

```text
$ fmgrep index --json
{"store_docs":false,"path":"./.fmgrep","num_files":3,"num_bytes":197,"index_size_bytes":34456,"build_time_us":4145}
```

The event stream is rg 15.2.0's, byte for byte: `begin`, one `match` or
`context` object per line (`lines.text` keeps the terminator; non-UTF-8
text becomes `{"bytes": <base64>}`), `end` with the file's stats, and a
final `summary`. It streams, one fetch batch at a time. Deviations, all
deliberate:

1. N terms of a request produce N concatenated streams in order, each
   closed by its own `summary`. A foreign event type would break rg-schema
   consumers worse than a repeated one. The CLI sends one pattern, or one
   expression for several `-e`, so its stream has one.
2. `summary.stats.elapsed` is the term's latency, and `elapsed_total` its
   wall clock.
3. `binary_offset` is always null, because binary files are skipped at
   index time.
4. `searches` and `bytes_searched` cover the whole index, which is what rg
   over the indexed file list reports, whatever `-g` or PATH. Leading UTF-8
   BOMs are excluded, as rg's searcher strips them.
5. `path` is always `{"text": ...}`, because manifest paths are UTF-8.
6. `-c`, `--count-matches` and `-l` with `--json` print the report; rg
   ignores `--json` there and prints plain counts.
7. No terms prints nothing.

Under `-U`, a term matched over the whole document reports one `match`
event per block of consecutive matched lines, and its submatches are the
whole matches, terminators included.

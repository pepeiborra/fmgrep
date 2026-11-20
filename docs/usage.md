# Using fmgrep

How a search differs from rg's, and what `status`, `serve` and
`--server` do.

## Where fmgrep differs from rg

A search prints what `rg` prints, on a terminal and in a pipe, with the
same exit status. The exit status is 0 on a match, 1 on none and 2 on an
error. Each difference below was checked by running both tools on one
tree.

### The index decides what is searched

`fmgrep index` takes the files `rg --files` lists, minus binary files
([Indexing](../README.md#indexing)). A search never reads a file the
index skips. `rg` searches a hidden or ignored file that you name as a
PATH or match with a `-g` glob. fmgrep says that it cannot, and exits 2.
It searches the other PATHs first.

| search | rg | fmgrep |
|---|---|---|
| `HscEnv .hidden.txt` | finds the line | `error: .hidden.txt: not in the index (...)`, exit 2 |
| `HscEnv build/out.txt` (gitignored) | finds the line | the same error, exit 2 |
| `-c HscEnv link.md` (a symlink) | counts the target's lines | the same error, exit 2 |
| `-g '.hidden*' HscEnv` | finds the line | `error: no files were searched`, exit 2 |
| `HscEnv /etc/hosts` (outside the tree) | searches it | `error: /etc/hosts is outside the indexed tree`, exit 2 |
| `--hidden HscEnv` | searches hidden files | `error: --hidden cannot work on an index`, exit 2 |

The error ends with `search it with rg`. It names a directory only when
rg would read a file in it, such as an ignored `build/`. An empty one, or
one with only hidden or binary files, is no error: the search finds
nothing and exits 1, as rg does. With a `--store-docs` index, the error
also names a file added since the index, which such a search never reads. `--no-ignore`, `-u`, `-L`, `-a`, `-z` and
`--pre` get the same refusal, each with its reason.

A search needs an index. With none above the cwd, it exits 2 with
`error: no .fmgrep index in DIR or any parent`. `fmgrep index` refuses
to overwrite an index, so remove `.fmgrep` to index again. A search
reads in full every file edited or added after the index, so its answers
match a fresh index. See [Staleness](index-format.md#staleness).

Without a PATH, a search covers the subtree of the cwd. Paths print
relative to the cwd, as in rg, and `-g` globs match relative to it. From
outside the indexed tree, a search covers the whole tree, relative to
its root. A PATH is relative to the cwd, and its files print as rg
prints them. Files print in path order.

### A PATTERN is a regex

The PATTERN is a regex, as in rg. The syntax is that of rg's default engine,
the Rust regex crate. `-F` makes it the text it is, and `-i`, `-s`, `-S`
and `-U` work as in rg.

| search | rg | fmgrep |
|---|---|---|
| `a.b` | finds `a.b` and `axb` | the same |
| `-F a.b` | finds `a.b` | the same |
| `'foo('` | `regex parse error`, exit 2 | the same, and a hint to pass `-F` |
| `-F 'foo('` | finds `foo(bar)` | the same |
| `/usr/` | finds `/usr/lib` | the same: the slashes are text |
| `''` | every line | the same |
| `'a\x00b'` (a NUL) | `pattern contains "\0" but it is impossible to match`, exit 2 | the same |

A pattern with no regex syntax in it, such as `HscEnv`, is answered
from the index as a literal is. `-F` does so for any text. A pattern
with a `.`, a bracket or a `+` reads every file that may match, so use
`-F` for code such as `foo(` or `a.b`. A search for `/a.b/` that finds
nothing says that the slashes are part of the pattern.

`--expr` and `--expr-json` keep their own syntax: a word is a literal and
`/re/` a regex, and `-F` is refused
([Expressions](../README.md#expressions)). `--regex` is accepted from v0.1.0
and does nothing.

rg wraps each pattern in a group, so it accepts a few patterns with an
unbalanced `)`, such as `)(\d#?`. fmgrep refuses them with a parse error.

`-P` and `--engine` do not exist: there is one engine. `-f`, `-w`, `-x` and
`-v` are refused.

### The PATTERN and the PATHs

The first argument is the PATTERN, and every other argument is a PATH, as
in rg. A PATH that does not exist is an error, and the other PATHs are still
searched.

| search | rg | fmgrep |
|---|---|---|
| `alpha nosuch.txt` | `No such file or directory`, exit 2 | the same |
| `alpha nosuch.txt a.txt` | the lines of `a.txt`, then the error, exit 2 | the same |
| `-e alpha -e beta a.txt` | the lines with either | the same |
| `-e alpha -- -file` | `-file` is a PATH | the same |
| `-- -x a.txt` | the PATTERN is `-x` | the same |

`-e` gives the patterns, and then every argument is a PATH. Several make
one search: the lines that match any, with no heading for each. `-F`, `-i`
and `-S` apply to every pattern.

`--and` and `--or` take their patterns with `-e`, and need two or more.
`--and A B` is a search for `A` in the PATH `B`, so it says to write
`--and -e A -e B`. They pick files, then print the lines of every pattern in
them, as `--expr` does. Under `--expr`, every argument is a PATH too.

`index`, `serve` and `status` are commands when they are the first word.
To search for one of these words, put `--` or any flag first:
`fmgrep -- status`, `fmgrep -n status`.

### Flags

These work as in rg: `-e`, `-F`, `-i`, `-s`, `-S`, `-U`,
`--multiline-dotall`, `-g`, `--iglob`, `--glob-case-insensitive`, `-c`,
`--count-matches`, `-l`, `-o`, `-A`, `-B`, `-C`, `-n`, `-N`, `--heading`,
`--no-heading`, `--color` and `--stats`. `fmgrep --help` lists them. Any
other flag of rg is refused with `error: unexpected argument` and exit 2.
These are some of them:

- patterns: `-f`, `-w`, `-x`, `-v`, `-P`, `--engine`
- files: `-t`, `-T`, `--type-add`, `-d`, `--max-filesize`, `--files`,
  `--files-without-match`
- output: `-m`, `-r`, `-H`, `-I`, `-0`, `--column`, `--vimgrep`,
  `--pretty`, `--passthru`, `--trim`, `--sort`, `-q`, `--colors`,
  `--byte-offset`, `--max-columns`, `--context-separator`
- process: `-j`, `--debug`, `--no-config`

An index cannot honour these, and each is refused with its reason, such as
`error: --hidden cannot work on an index: hidden files are not indexed; use
rg`: `--hidden`, `-.`, `--no-ignore` and its variants, `-u`, `-L`, `-a`,
`--binary`, `-z`, `--pre`, `--pre-glob` and `-E`.

fmgrep does not search stdin, and does not read a `RIPGREP_CONFIG_PATH`
file. To filter a pipe, or to use a flag above, use rg.

In rg the PATH `-` is stdin. In fmgrep it is a PATH like any other: with no
file named `-` it is the error `error: -: No such file or directory`, exit 2,
and with one that file is searched.

Under `-e` or `--expr`, every positional is a PATH. Otherwise the first
positional is always the PATTERN, so `fmgrep -- -` searches for a hyphen.

### Output

- `--json` alone prints a counts report, not rg's events.
  `--json --matches` prints rg's JSON event stream
  ([protocol.md](protocol.md#cli---json)).
- `--stats` prints rg's statistics after the matched lines.
  Files and bytes searched count the indexed files that the PATHs, globs
  and cwd keep. A narrowed search also counts their UTF-8 byte-order
  marks, which rg leaves out. The total time runs from the start of the
  query, not of the process. `-c`, `--count-matches` and `-l` refuse
  `--stats`. With `--json` it prints nothing more.
- In colour, the matches of a file's first 1,000 matched lines are
  coloured, as rg colours them. Past those, lines print with their path
  and line number in colour, and their matches plain. `-o` colours every
  match. Under `-U`, adjacent matched lines print as one block. A block
  that starts in the first 1,000 matched lines is coloured whole.

## status

`fmgrep status` says which index or server a search here uses, what that
index holds, and what changed since it was built.

```console
$ fmgrep status
a search here uses the local index /home/me/ghc/.fmgrep
format: 9
store_docs: false
num_docs: 26787
num_bytes: 88781005
bom_bytes: 6
index_size_bytes: 76771936
build_time_us: 1997204
index_started_ns: 1763061773123456789
shards: 1
corpus_root: /home/me/ghc
changes: fewer than 50000 files: checked every file
since the index: 2 stale, 1 added, 0 gone
ignore files changed since the index: none
record: none saved
```

Status checks what changed since the index, as a search does, and says
what that check did. On a tree of 50,000 files or more, it may take a
watchman token, start watchman, ask it to watch the tree and save the
record of the search ([watchman](watchman.md)). Where watchman cannot
say, it checks every file. Status prints each warning a search would
give, once, on stdout. It does not load the index's shards, so it does
not find a damaged shard. A search does.

`fmgrep status --json` prints the manifest as one JSON line, with the
file list ([fields](index-format.md#manifestbin)).

`fmgrep status` picks the index or server as a search does. A local
index found by walking up wins over `FMGREP_SERVER`. If a search here
would warn `Using local index DIR` with a note, status gives that note on
its first line. Where a server answers, status prints its reply, as one
JSON line with `--json`.
`fmgrep status --server ADDR` takes `--server` as a search does.

## A server

`fmgrep serve` keeps the index loaded for many searches in a row. A
console example is in the [README](../README.md#usage). `--server` takes
every search flag but `--index`, and prints what the local search would.
The server runs the search ([details](protocol.md#--server)). With
`FMGREP_SERVER=127.0.0.1:7777` set, every search that finds no `.fmgrep`
index by walking up from the cwd goes to that server. An index found
wins, and the search says so on stderr with a line that starts
`warning: Using local index`.

Before each query the server asks [watchman](watchman.md) what changed
in the tree, so it needs watchman installed. When no daemon runs,
watchman's client starts one. To serve a tree that does not change, such
as a fixed commit, or to serve without watchman, pass `--no-watch`. A
`--store-docs` index never reads the tree, so serving it never needs
watchman. A team can share one server of a git checkout. See
[shared-server.md](shared-server.md).

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
PATH or match with a `-g` glob. fmgrep finds nothing in it.

| search | rg | fmgrep |
|---|---|---|
| `HscEnv .hidden.txt` | finds the line | exit 1, no output |
| `HscEnv build/out.txt` (gitignored) | finds the line | exit 1, no output |
| `-g '.hidden*' HscEnv` | finds the line | `error: no files were searched`, exit 2 |
| `-c HscEnv link.md` (a symlink) | counts the target's lines | exit 1, no output |
| `HscEnv /etc/hosts` (outside the tree) | searches it | `error: /etc/hosts is outside the indexed tree`, exit 2 |

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

### A TERM is a literal

`fmgrep 'a.b'` finds the three bytes `a.b`. `rg` reads `a.b` as a regex,
so it finds `axb` too. Write `/a.b/` for a regex, or pass `--regex` to
make every TERM one. The regex syntax is that of rg's default engine,
the Rust regex crate. There is no `-P`, `-F` or `-e`.

Several TERMs are searched one after the other, and each prints under a
`== TERM ==` line. In rg, `-e A -e B` is one search. `--and`, `--or` and
`--expr` pick files, then print the lines of every TERM in them
([Expressions](../README.md#expressions)).

Trailing arguments that name an existing file or directory are PATHs, as
in rg. One that does not exist is a TERM. So `fmgrep HscEnv nope.txt`
also searches for `nope.txt`, and exits 0. `rg` exits 2 with an error.

`index`, `serve` and `status` are commands when they are the first word.
To search for one of these words, put `--` or any flag first:
`fmgrep -- status`, `fmgrep -n status`.

### Flags

These work as in rg: `-i`, `-U`, `--multiline-dotall`, `-g`, `--iglob`,
`--glob-case-insensitive`, `-c`, `--count-matches`, `-l`, `-o`, `-A`,
`-B`, `-C`, `-n`, `-N`, `--heading`, `--no-heading`, `--color` and
`--stats`. `fmgrep --help` lists them. Any other flag of rg is refused
with `error: unexpected argument` and exit 2. These are some of them:

- patterns: `-e`, `-f`, `-F`, `-w`, `-x`, `-v`, `-s`, `-S`, `-P`,
  `--engine`
- files: `--hidden`, `--no-ignore`, `-u`, `-t`, `-T`, `--type-add`,
  `-L`, `-d`, `--max-filesize`, `--files`, `--files-without-match`,
  `-a`, `-z`
- output: `-m`, `-r`, `-H`, `-I`, `-0`, `--column`, `--vimgrep`,
  `--pretty`, `--passthru`, `--trim`, `--sort`, `-q`, `--colors`,
  `--byte-offset`, `--max-columns`, `--context-separator`
- process: `-j`, `-E`, `--debug`, `--no-config`

fmgrep does not search stdin, and does not read a `RIPGREP_CONFIG_PATH`
file. To filter a pipe, or to use a flag above, use rg.

### Output

- `--json` alone prints a counts report, not rg's events.
  `--json --matches` prints rg's JSON event stream
  ([protocol.md](protocol.md#cli---json)).
- `--stats` prints rg's statistics after the matched lines of each TERM.
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

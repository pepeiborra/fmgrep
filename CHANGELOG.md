# Changelog

## Unreleased (0.2.0)

These changes break the command line of v0.1.0.

### A PATTERN is a regex, as in rg

In v0.1.0 a TERM was a literal. Now it is a regex, as in rg, and `-F`
makes it a literal. A pattern with nothing but a literal in it, such as
`HscEnv`, is answered from the index as before.

| v0.1.0 | now |
|---|---|
| `fmgrep 'a.b'` finds the three bytes `a.b` | `fmgrep -F 'a.b'`; `'a.b'` also finds `axb` |
| `fmgrep 'foo('` finds `foo(bar)` | `fmgrep -F 'foo('`; `'foo('` exits 2 with a parse error and a hint |
| `fmgrep '/Dyn[A-Z]\w+/'` | `fmgrep 'Dyn[A-Z]\w+'`; the slashes are now text |
| `fmgrep --regex 'x+'` | `fmgrep 'x+'`; `--regex` is still accepted and does nothing |
| `fmgrep --and A 'b+'` read `b+` as text | `fmgrep --and -e A -e 'b+'` reads each as a regex; `-F` for text |
| `fmgrep 'foo\.bar'` finds the text `foo\.bar` | `fmgrep -F 'foo\.bar'`; `'foo\.bar'` is the regex for `foo.bar` |
| `fmgrep 'C:\Users'` finds the text `C:\Users` | `fmgrep -F 'C:\Users'`; `'C:\Users'` exits 2 (`\U` starts a hex escape) |
| `fmgrep foo /tmp/` searched for `foo` and the regex `tmp` | `/tmp/` is a PATH, as in rg: `error: /tmp/ is outside the indexed tree` |
| `fmgrep '$HOME'` finds `echo $HOME` | `fmgrep -F '$HOME'`; `'\$HOME'` is the regex |
| `fmgrep 'a || b'` finds the line `a || b` | `fmgrep -F 'a || b'`; `'a || b'` matches every line |
| `fmgrep ''` matched no line | matches every line, as in rg |
| `fmgrep --expr 'A AND /b+/'` | unchanged: a word is a literal and `/re/` a regex |

A search that finds nothing for a pattern spelled `/x/` says so on stderr,
since rg reads the slashes as text. A pattern that does not compile exits 2
with the parse error and a hint to pass `-F`.

### One PATTERN, then PATHs, as in rg

In v0.1.0 every word after the first that named no file was another TERM,
and several TERMs printed one `== TERM ==` section each. Now the first
argument is the PATTERN and every other is a PATH, as in rg. A PATH that
does not exist is an error, and the others are still searched. `-e` gives
the patterns, several make one search, and then every argument is a PATH.

| v0.1.0 | now |
|---|---|
| `fmgrep A B` searched for `A` and `B`, one section each | `fmgrep -e A -e B`: one search, the lines with either |
| `fmgrep HscEnv nope.txt` also searched for `nope.txt`, exit 0 | `error: nope.txt: No such file or directory (os error 2)`, exit 2 |
| `fmgrep HscEnv nope.txt a.txt` searched for three terms | the lines of `a.txt`, then the error, exit 2 |
| `fmgrep --and A B` | `fmgrep --and -e A -e B`; `--and A B` exits 2 and says so |
| `fmgrep --or A B` | `fmgrep --or -e A -e B`, or just `-e A -e B` |
| `fmgrep --json A B` gave one query for each TERM | `fmgrep --json A` for each pattern; `--json -e A -e B` reports one expression, with a leaf for each; `--json A B` exits 2 |
| `fmgrep --approx-counts A B` gave one count for each TERM | `fmgrep --approx-counts A` for each pattern; `-e A -e B` gives one expression, with a line for each |
| `fmgrep a - nosuch` searched for the three words | `-` and `nosuch` are PATHs that name no file: two errors, exit 2 |
| `fmgrep HscEnv .hidden.txt` found nothing, exit 1 | `error: .hidden.txt: not in the index (...); search it with rg`, exit 2 |
| `fmgrep --hidden HscEnv`: unexpected argument | `error: --hidden cannot work on an index: hidden files are not indexed; use rg` |
| `fmgrep --expr 'A AND B' PATH` | unchanged: every argument is a PATH |

A PATH that exists, with no file of it in the index (a hidden, ignored,
binary or symlinked file, or a directory that holds such a file), is an
error too: rg searches it, an index cannot. A directory in which rg reads
nothing either (empty, or only hidden or binary files) is no error: the
search finds nothing and exits 1, as in rg. So are `--no-ignore`, `-u`, `-L`, `-a`, `-z`,
`--pre` and `-E`, each with its reason.

### New flags

- `-e`, `--regexp`: a PATTERN. It may start with a hyphen. Refused with
  `--expr` and `--expr-json`.
- `-F`, `--fixed-strings`: every PATTERN is text. Refused with `--expr` and
  `--expr-json`.
- `-s`, `--case-sensitive` and `-S`, `--smart-case`. The last of `-i`, `-s`
  and `-S` wins, as in rg. Smart case ignores case when the patterns hold a
  literal and none is uppercase.

### The serve protocol

A request with `"regex": true` reads its terms as regexes, and one without
reads them as literals, as before. The CLI sends `"regex": true` unless it
is given `-F`, so a term spelled `/re/` on the wire is now a literal, never
a regex. A regex term, or a `{"regex"}` leaf, that is nothing but a literal
is answered as the literal. `--expr` and `--expr-json` do not change. A
client and its server are one build: do not mix v0.1.0 with this version.
The CLI sends one pattern, and several `-e` as an `or` expression; no
request of it holds several `terms`, though the server still answers them
for a raw client. The report gains `unindexed`, the PATHs the index lacks.

### Smaller changes

- The counts report (`--json` alone, `--approx-counts`) of a pattern that is
  nothing but a literal is the index's count: `aa` on `aaaa` is 3, not 2, and
  `counts_exact` is false.
- The library no longer exports `regex_pattern`. `plain_literal` says what a
  pattern matches when it is one literal.
- `FMGREP_VERBOSE=2` prints one `plan:` line per pattern.
- `--approx-counts` and `--json` of several `-e` patterns report one
  expression with a line for each pattern, where several TERMs reported one
  query for each.

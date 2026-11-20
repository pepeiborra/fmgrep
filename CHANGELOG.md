# Changelog

## Unreleased (0.2.0)

These changes break the command line of v0.1.0.

### A TERM is a regex, as in rg

In v0.1.0 a TERM was a literal. Now it is a regex, as in rg, and `-F`
makes it a literal. A pattern with nothing but a literal in it, such as
`HscEnv`, is answered from the index as before.

| v0.1.0 | now |
|---|---|
| `fmgrep 'a.b'` finds the three bytes `a.b` | `fmgrep -F 'a.b'`; `'a.b'` also finds `axb` |
| `fmgrep 'foo('` finds `foo(bar)` | `fmgrep -F 'foo('`; `'foo('` exits 2 with a parse error and a hint |
| `fmgrep '/Dyn[A-Z]\w+/'` | `fmgrep 'Dyn[A-Z]\w+'`; the slashes are now text |
| `fmgrep --regex 'x+'` | `fmgrep 'x+'`; `--regex` is still accepted and does nothing |
| `fmgrep --and A 'b+'` read `b+` as text | `--and` and `--or` read each TERM as a regex; `-F` for text |
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

### New flags

- `-F`, `--fixed-strings`: every TERM is text. Refused with `--expr` and
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

### Smaller changes

- The counts report (`--json` alone, `--approx-counts`) of a pattern that is
  nothing but a literal is the index's count: `aa` on `aaaa` is 3, not 2, and
  `counts_exact` is false.
- The library no longer exports `regex_pattern`. `plain_literal` says what a
  pattern matches when it is one literal.
- `FMGREP_VERBOSE=2` prints one `plan:` line per TERM.

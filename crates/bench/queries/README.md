# Benchmark query sets

One file per corpus: `{"repo_url", "commit", "queries"}`. `commit` is the
revision the set is pinned to and `scripts/bench.sh` clones. A query spelled
`/pattern/` is a regex, `A AND B` an intersection of two literals or regexes,
anything else a literal; literals come first, intersections last.

Queries are what a developer of that codebase would search for, each
selective.

`linux.json` and `chromium.json` derive from microsoft/tgrep's
`scripts/benchmark-queries*.json` (MIT) at `b1d0fc2f`, with three kernel
patterns rewritten to use `[ \t]` instead of `\s`; the regexes of
`chromium.json` and all of `ghc.json` are authored here.

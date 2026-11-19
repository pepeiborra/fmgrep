# Watchman

What fmgrep asks of watchman, what that costs, and how to keep fmgrep
away from it.

[Watchman](https://facebook.github.io/watchman/) is a file watcher.
fmgrep uses it to learn which files changed since the index was built,
so that a search need not check every file. It is optional. On a
tree of 50,000 files or more, a search without it gives the same
answers, only slower. `fmgrep serve` needs it, unless given `--no-watch` or a `--store-docs` index.

## What fmgrep uses it for

- **A search of a local index.** On a tree of 50,000 files or more, a
  search asks watchman which files changed. Below 50,000 files it never
  contacts watchman, because checking every file is faster.
- **A search through a server from a checkout.** At any checkout size,
  it asks watchman what changed since git last compared the checkout
  with the served commit ([shared server](shared-server.md)). It starts
  the daemon when none runs, and asks it to watch the whole checkout. It
  keeps a record per checkout and served commit, and remembers each
  server's served commit per checkout
  ([details](index-format.md#watchman)).
- **`fmgrep serve`.** The server checks before every query, not once. So
  it asks watchman at any tree size, unless given `--no-watch`. It
  starts the daemon and asks it to watch even a small tree. A
  `--store-docs` index never reads the tree, so serving it never needs
  watchman.

Asking watchman makes each query of `fmgrep serve` slower. On Linux, it
adds 1.1 to 3.2 ms. A query that took 0.4 ms takes 2.1 ms. On macOS, it
adds about 38 ms, on a tree of 60,000 files, where a query with
`--no-watch` took 0.17 ms.

`fmgrep index` starts the watchman daemon when none runs, if the tree
has 50,000 files or more. So does a search of such a tree. They do so in
CI and in containers too, and ask the daemon to watch the tree. The
daemon keeps running after fmgrep exits, with its watch. On AOSP (1.25 M
files) that is about 710 MB of memory and 312,000 inotify watches. A
search keeps a small record per tree in `~/.cache/fmgrep`. The record's
fields, the token and the warnings are in
[index-format.md](index-format.md#watchman).

## Without watchman

Each search of a tree of 50,000 files or more checks every file itself.
It prints one warning line: `warning: watchman cannot say what changed
(cannot run watchman (...); install watchman); checked every file`. The
answers are the same, and the search is slower.

The table times a search for an identifier found in 8 to 19 files. Each
cell is the median of 15 searches, with the files in the page cache, on
an AWS i4i.4xlarge (Ubuntu 24.04, watchman v2026.11.09, November 2026).

| search | Linux kernel, 96k files | Chromium, 461k | AOSP, 1.25 M |
|---|--:|--:|--:|
| asks watchman | 7.5 ms | 23 ms | 104 ms |
| checks every file | 76 ms | 106 ms | 786 ms |

The second row is the first search after an index. It asks watchman for
its clock, which takes a few milliseconds, then checks every file. A
search without watchman checks every file the same way.

`fmgrep serve` without watchman exits 2 at its start and says why. Give
it `--no-watch` or a `--store-docs` index to serve without watchman.

## Keeping fmgrep away from watchman

Set `WATCHMAN_SOCK` to a path where no daemon listens. Then fmgrep
starts no daemon, and each search checks every file and prints a warning
that gives the socket ([tuning.md](tuning.md#search)). fmgrep no longer
reads `FMGREP_JOURNAL`, which fmgrep 0.1.0 used for this.

`fmgrep serve --no-watch` contacts no watchman at any tree size. Without
`--no-watch`, a server refuses to start with such a socket. Use
`--no-watch` for a tree that does not change, such as a fixed commit.

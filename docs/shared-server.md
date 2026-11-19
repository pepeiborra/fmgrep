# Searching a shared server from a checkout

How a team shares one `fmgrep serve` of a large git tree, and how a
search from a developer's own checkout accounts for the changes that
checkout holds.

## Set up the server

One machine indexes a clean checkout of one commit with `--store-docs`
and serves the index. `--store-docs` stores the files in the index. When
they are exactly the files of the checked-out commit, the index also
records that commit, the served commit
([conditions](index-format.md#served-commit)). Pass
`--require-served-commit` to make the build fail when it cannot record
one.

```console
server$ git -C /srv/repo checkout -q origin/main
server$ fmgrep index --store-docs /srv/repo
note: the index holds commit 81b12d888b2bce896b8745567cfb620cfaf73e1b
server$ fmgrep status /srv/repo | grep commit
commit: git 81b12d888b2bce896b8745567cfb620cfaf73e1b
server$ fmgrep serve /srv/repo --listen 0.0.0.0:7777 &
```

Serving a `--store-docs` index never reads the tree, so it needs no
watchman.

## Search from a checkout

Each developer sets `FMGREP_SERVER` and searches from their own
checkout, with no index of their own. A `.fmgrep` index found above the
cwd still wins, with a warning. The checkout must hold the served commit:
`git fetch` brings it.

```console
dev$ export FMGREP_SERVER=server:7777
dev$ cd ~/src/repo/compiler && fmgrep -l initHscEnv
```

The output is what a local index of the checkout would print. To print
it, the client asks git where the checkout differs from the served
commit (edits, new and deleted files, commits on top, another branch).
It searches those files itself and asks the server to leave them out.
The first search asks git; later ones ask
[watchman](watchman.md) what changed since, and without it
every search asks git. Later searches also skip the server's status
request: the client remembers the served commit in a pin
([details](index-format.md#watchman)) and asks again only when the
server has moved.

## When local changes are ignored

When git cannot list the differences soundly, the client prints the
server's answer alone, with one warning that says why:

```console
warning: local changes are ignored (git converts files (core.autocrlf=true)); these results are the server's alone; use --ignore-local-changes to avoid this warning
```

The causes include:

- the cwd is in no git checkout, or the checkout lacks the served
  commit;
- git hides or converts files: assume-unchanged or skip-worktree
  entries, a sparse checkout, a filter, line-ending, `ident` or encoding
  conversion that applies to a tracked file;
- the checkout holds a submodule, or an `.ignore` or `.rgignore` file
  above it holds a pattern that can match a file of it (a `.gitignore`
  above a repository applies to nothing in it, as in rg, nor does a
  pattern such as `/clone/` that names the checkout or a path outside it);
- more than 10,000 files differ or would be searched;
- the server reports no served commit (a `--store-docs` index that
  recorded none, or a files-layout server searched from outside its
  tree), or one that is not a full git commit id. The warning then says
  why: the reason that the index build gave (`the server at ADDR reports
  no served commit: the checkout holds a submodule (agents/internal)`),
  or that only a `--store-docs` index records one.

[protocol.md](protocol.md#from-a-checkout) has the full list. Every
search checks again, and warns again.

`--ignore-local-changes` asks for the server's answer alone: the client
runs no git or watchman and prints no warning.

## Limits

- git only; Sapling is not supported.
- Only a `--store-docs` index records a served commit, so the server
  holds a copy of the files.
- An ignore file that differs from the commit makes the client search
  its whole directory. At the top of a large tree, that passes the
  10,000-file limit, and the search falls back to the server's answer
  alone.
- Files that git ignores but rg searches (a `!` rule in `.ignore`) are
  not seen.
- When the server moves to a new commit during a search, the search
  stops and asks to be run again.
- The client and the server must be the same fmgrep version.
- The server answers one connection at a time: a client that reads its
  answer slowly delays the others.
- The server has no authentication, and its index holds the source:
  serve on a network you trust ([SECURITY.md](../SECURITY.md)).

Other limits, such as renames on a file system that ignores case:
[protocol.md](protocol.md#from-a-checkout).
#!/usr/bin/env bash
#
# scripts/watchman-spawn.sh: with no watchman daemon running and
# WATCHMAN_SOCK unset, `fmgrep index` of a tree of 50,000 files or more
# runs the watchman client, which starts the daemon, and asks the daemon to
# watch the tree. By design index waits at most 500 ms for the client and
# 100 ms for the daemon; when either runs out, it says so under --verbose,
# and a search asks for the watch instead, until one has it. Once the
# daemon has read the tree, a search takes a token and checks every file;
# the next search asks watchman. Every search is compared with rg.
#
# This starts, uses and stops the machine's own daemon, as fmgrep does on a
# user's machine, so it runs only where CI=true: a disposable machine.
# Needs watchman, python3 and rg. Exits 1 on a failed check, 2 on a usage
# error or a daemon already running.
#
# Usage: scripts/watchman-spawn.sh FMGREP

set -euo pipefail

[[ $# == 1 ]] || { sed -n 's/^# \{0,1\}//; 17p' "${BASH_SOURCE[0]}" >&2; exit 2; }
[[ ${CI:-} == true ]] || {
    echo "refusing: this starts and stops your own watchman daemon; run it only where CI=true" >&2
    exit 2
}
BIN="$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")"
for tool in watchman python3 rg; do
    command -v "$tool" >/dev/null || { echo "$tool is not on PATH" >&2; exit 2; }
done
unset WATCHMAN_SOCK
# The daemon's socket, when one answers. Without --no-local, the client
# answers get-sockname itself when no daemon runs.
running() { watchman --no-spawn --no-local --no-pretty get-sockname 2>/dev/null; }
if running >/dev/null; then
    echo "a watchman daemon runs already: this checks that fmgrep starts one" >&2
    exit 2
fi

WORK="$(cd "$(mktemp -d)" && pwd -P)"
TREE="$WORK/tree"
export XDG_CACHE_HOME="$WORK/cache"
trap 'watchman --no-spawn shutdown-server >/dev/null 2>&1 || true; rm -rf "$WORK"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

python3 -I - "$TREE" <<'EOF'
import os, sys
root = sys.argv[1]
for i in range(60000):
    d = os.path.join(root, "d%03d" % (i // 1000))
    if i % 1000 == 0:
        os.makedirs(d)
    with open(os.path.join(d, "f%d.c" % i), "w") as f:
        f.write("common id%d\n%s" % (i, "needle\n" if i % 7 == 0 else ""))
EOF

# Whether the daemon lists the tree among its watches.
watched() {
    watchman --no-spawn --no-pretty watch-list 2>/dev/null |
        python3 -I -c 'import json, sys; sys.exit(sys.argv[1] not in json.load(sys.stdin)["roots"])' "$TREE"
}
# Runs a check every 100 ms until it passes, for at most 10 s.
within_10s() {
    local _
    for _ in $(seq 100); do
        "$@" && return 0
        sleep 0.1
    done
    return 1
}
# Returns once the daemon has read the whole tree.
crawled() {
    watchman --no-spawn --no-pretty -j <<<"[\"query\", \"$TREE\", {\"expression\": [\"false\"], \"sync_timeout\": 600000}]" >/dev/null
}

"$BIN" index --verbose --output "$WORK/idx" "$TREE" 2>"$WORK/index-err" ||
    { cat "$WORK/index-err" >&2; fail "fmgrep index failed"; }
sed 's/^/  index: /' "$WORK/index-err" >&2
within_10s running >/dev/null || fail "fmgrep index started no daemon"
echo "a daemon runs: $(running)" >&2
# Index says it asked for the watch also when its 100 ms ran out after
# watch-list, before it sent watch-project: the tree is then not watched.
INDEX_ASKED="note: fmgrep asked watchman to watch $TREE; later searches ask it once its crawl ends"
if within_10s watched; then
    echo "index asked the daemon to watch the tree" >&2
    crawled
elif grep -Eq '^note: watchman (get-sockname )?did not answer within [0-9]+ ms$' "$WORK/index-err" ||
    grep -Fxq -- "$INDEX_ASKED" "$WORK/index-err"; then
    echo "index gave up on watchman within its bounds: a search asks for the watch" >&2
else
    fail "the daemon does not watch $TREE, and index said nothing of a wait that ran out"
fi

search() {
    (cd "$TREE" && "$BIN" --index "$WORK/idx" -l needle </dev/null 2>"$WORK/err" | LC_ALL=C sort) >"$WORK/ours"
    (cd "$TREE" && rg -l needle </dev/null | LC_ALL=C sort) >"$WORK/theirs"
    cmp -s "$WORK/ours" "$WORK/theirs" || fail "fmgrep -l needle differs from rg"
    sed 's/^/  stderr: /' "$WORK/err" >&2
}
# Whether every warning of the last search is one of the lines given.
only() {
    local line want
    while IFS= read -r line; do
        [[ $line == warning:* ]] || continue
        for want in "$@"; do
            [[ $line == "$want" ]] && continue 2
        done
        return 1
    done <"$WORK/err"
}
cannot() { echo "warning: watchman cannot say what changed ($1); checked every file"; }
LATE="$(cannot "watchman did not answer within 100 ms")"
ASKED="$(cannot "fmgrep asked watchman to watch $TREE; later searches ask it once its crawl ends")"
CRAWLING="$(cannot "watchman is still crawling $TREE")"
ONCE="warning: checked every file once (no record of an earlier search); later searches ask watchman"

# The first token. A search that takes none says why, as one that came
# too early or too late: it asked for the watch, the crawl goes on, or
# the daemon passed its 100 ms. If the tree is then watched, the next
# search comes once the daemon has read it. If not, the next search asks
# for the watch again: this one gave up before it sent watch-project, or
# the daemon has not added the watch yet.
for try in 1 2 3 4 5; do
    search
    only "$ONCE" && grep -Fxq -- "$ONCE" "$WORK/err" && break
    only "$LATE" "$ASKED" "$CRAWLING" && grep -q '^warning: ' "$WORK/err" ||
        fail "a search before the first token said something else"
    [[ $try != 5 ]] || fail "5 searches took no token"
    if watched; then
        crawled
    fi
done
watched || fail "a search took the first token, but the daemon does not watch $TREE"
echo needle >>"$TREE/d001/f1001.c"
# The token question. A search late past its 100 ms checks every file,
# and says so; the next asks again.
for try in 1 2 3 4 5; do
    search
    ! grep -q '^warning: ' "$WORK/err" && break
    only "$LATE" || fail "the second search did not ask watchman"
    [[ $try != 5 ]] || fail "5 searches were late"
done
echo "fmgrep started watchman, which watches the tree; a later search asked it" >&2

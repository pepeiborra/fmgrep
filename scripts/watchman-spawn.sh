#!/usr/bin/env bash
#
# scripts/watchman-spawn.sh: with no watchman daemon running and
# WATCHMAN_SOCK unset, `fmgrep index` of a tree of 50,000 files or more
# runs the watchman client, which starts the daemon, and asks the daemon to
# watch the tree. Once the daemon has read the tree, the first search takes
# a token and checks every file; the next search asks watchman. Both are
# compared with rg.
#
# This starts, uses and stops the machine's own daemon, as fmgrep does on a
# user's machine, so it runs only where CI=true: a disposable machine.
# Needs watchman, python3 and rg. Exits 1 on a failed check, 2 on a usage
# error or a daemon already running.
#
# Usage: scripts/watchman-spawn.sh FMGREP

set -euo pipefail

[[ $# == 1 ]] || { sed -n 's/^# \{0,1\}//; 15p' "${BASH_SOURCE[0]}" >&2; exit 2; }
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

"$BIN" index --output "$WORK/idx" "$TREE" >&2
sock="$(running)" || fail "fmgrep index started no daemon"
echo "a daemon runs: $sock" >&2
watchman --no-spawn --no-pretty watch-list |
    python3 -I -c 'import json, sys; sys.exit(sys.argv[1] not in json.load(sys.stdin)["roots"])' "$TREE" ||
    fail "the daemon does not watch $TREE"
# Returns once the daemon has read the whole tree.
watchman --no-spawn --no-pretty -j <<<"[\"query\", \"$TREE\", {\"expression\": [\"false\"], \"sync_timeout\": 600000}]" >/dev/null

search() {
    (cd "$TREE" && "$BIN" --index "$WORK/idx" -l needle </dev/null 2>"$WORK/err" | LC_ALL=C sort) >"$WORK/ours"
    (cd "$TREE" && rg -l needle </dev/null | LC_ALL=C sort) >"$WORK/theirs"
    cmp -s "$WORK/ours" "$WORK/theirs" || fail "fmgrep -l needle differs from rg"
    sed 's/^/  stderr: /' "$WORK/err" >&2
}
search
grep -q '^warning: checked every file once (no record of an earlier search)' "$WORK/err" ||
    fail "the first search took no token"
echo needle >>"$TREE/d001/f1001.c"
search
! grep -q '^warning: ' "$WORK/err" || fail "the second search did not ask watchman"
echo "fmgrep started watchman, which watches the tree; the second search asked it" >&2

#!/usr/bin/env bash
#
# scripts/watchman-race.sh: changes made while watchman reads a tree must
# not make fmgrep give wrong answers. Starts a private watchman daemon (its
# own socket, state, log and pid files under a temporary directory; never
# the user's daemon), generates a tree of FILES small files (200,000 by
# default) and indexes it with WATCHMAN_SOCK set to that daemon. Four
# searches are compared with rg, three times each (a check). Then watchman
# reads the whole tree twice: after a restart of the daemon, and after its
# watch is removed, when the first search asks for a new one. During each
# read the script deletes, edits and adds 1,800 files and three loops
# search the tree; a check follows once both are done. A last round of
# changes, after a token, has its check too.
#
# Some changes must fall within the read after the restart: the script
# fails when none does (raise FILES). Needs python3 and rg. Ends with
# `pass N / fail M`; exits 1 on a wrong answer, 2 on a usage error.
#
# Usage: scripts/watchman-race.sh FMGREP WATCHMAN [FILES]

set -euo pipefail

[[ $# == 2 || $# == 3 ]] || { sed -n 's/^# \{0,1\}//; 19p' "${BASH_SOURCE[0]}" >&2; exit 2; }
BIN="$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")"
WATCHMAN="$(cd "$(dirname "$2")" && pwd -P)/$(basename "$2")"
FILES="${3:-200000}"
command -v rg >/dev/null || { echo "rg is not on PATH" >&2; exit 2; }
command -v python3 >/dev/null || { echo "python3 is not on PATH" >&2; exit 2; }

WORK="$(cd "$(mktemp -d)" && pwd -P)"
TREE="$WORK/tree"
IDX="$WORK/idx"
SOCK="$WORK/w/sock"
DAEMON=""
stop() {
    if [[ -n "$DAEMON" ]]; then
        kill "$DAEMON" 2>/dev/null || true
        wait "$DAEMON" 2>/dev/null || true
    fi
    DAEMON=""
}
trap 'stop; rm -rf "$WORK"' EXIT

# min_acceptable_nice_value: watchman refuses to start at a raised nice
# value, which CI runners and background shells can have. root_files []:
# watch the tree itself, whatever lies above it.
mkdir -p "$WORK/w"
echo '{"min_acceptable_nice_value": 19, "root_files": []}' >"$WORK/w/config.json"
start() {
    rm -f "$SOCK"
    WATCHMAN_CONFIG_FILE="$WORK/w/config.json" "$WATCHMAN" --foreground --sockname="$SOCK" \
        --statefile="$WORK/w/state" --logfile="$WORK/w/log" --pidfile="$WORK/w/pid" \
        </dev/null >>"$WORK/w/out" 2>&1 &
    DAEMON=$!
    for _ in $(seq 1 200); do
        [[ -S $SOCK ]] && return 0
        sleep 0.05
    done
    echo "watchman did not start:" >&2
    cat "$WORK/w/out" "$WORK/w/log" >&2 || true
    exit 2
}
# One command to the private daemon, through its socket only.
call() {
    python3 -I - "$SOCK" "$1" <<'EOF'
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall((sys.argv[2] + "\n").encode())
for line in s.makefile():
    reply = json.loads(line)
    if not reply.get("unilateral") and "log" not in reply:
        print(json.dumps(reply))
        sys.exit(1 if "error" in reply else 0)
sys.exit(1)
EOF
}
# Returns once the daemon has read the whole tree.
crawled() {
    call "[\"query\", \"$TREE\", {\"expression\": [\"false\"], \"sync_timeout\": 600000}]" >/dev/null
}
now() { python3 -I -c 'import time; print("%.6f" % time.time())'; }

echo "generating $FILES files" >&2
python3 -I - "$TREE" "$FILES" <<'EOF'
import os, sys
root, n = sys.argv[1], int(sys.argv[2])
for i in range(n):
    d = os.path.join(root, "d%03d" % (i // 1000), "e%d" % (i // 100 % 10))
    if i % 100 == 0:
        os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "f%d.c" % i), "w") as f:
        f.write("common id%d\n%s" % (i, "needle\n" if i % 7 == 0 else ""))
EOF

start
SECONDS=0
WATCHMAN_SOCK="$SOCK" "$BIN" index --output "$IDX" "$TREE" >&2
echo "indexed in ${SECONDS} s" >&2
call "[\"watch-project\", \"$TREE\"]" >/dev/null
crawled

# Deletes, edits and adds 1,800 files, in batches of 30 with 20 ms between
# them; the round names the added files. Prints the time of each change.
change() {
    python3 -I - "$TREE" "$FILES" "$1" <<'EOF'
import os, random, sys, time
root, n, round_ = sys.argv[1], int(sys.argv[2]), sys.argv[3]
rand = random.Random(round_)
for k, i in enumerate(rand.sample(range(n), 1800)):
    p = os.path.join(root, "d%03d" % (i // 1000), "e%d" % (i // 100 % 10), "f%d.c" % i)
    if k % 3 == 0:
        try:
            os.remove(p)
        except FileNotFoundError:
            pass
    elif k % 3 == 1:
        with open(p, "a") as f:
            f.write("needle edited %s\n" % round_)
    else:
        with open(os.path.join(os.path.dirname(p), "new-%s-%d.c" % (round_, i)), "w") as f:
            f.write("common added\nneedle\n")
    print("%.6f" % time.time(), flush=True)
    if k % 30 == 29:
        time.sleep(0.02)
EOF
}

# Each answer against rg's, sorted.
PASS=0 FAIL=0
compare() {
    local flags=$1 term=$2 ours theirs
    ours="$(cd "$TREE" && WATCHMAN_SOCK="$SOCK" "$BIN" --index "$IDX" $flags -- "$term" </dev/null 2>"$WORK/err" | LC_ALL=C sort)" || true
    theirs="$(cd "$TREE" && rg $flags -- "$term" </dev/null | LC_ALL=C sort)" || true
    if [[ "$ours" == "$theirs" ]]; then
        PASS=$((PASS + 1))
    else
        FAIL=$((FAIL + 1))
        echo "FAIL: fmgrep $flags $term differs from rg (< rg, > fmgrep):" >&2
        diff <(echo "$theirs") <(echo "$ours") | head -20 >&2 || true
        grep -v '^warning: excluding ' "$WORK/err" | sed 's/^/  stderr: /' >&2 || true
    fi
}
checks() {
    for _ in 1 2 3; do
        compare -l needle
        compare -c needle
        compare -l added
        compare --count-matches common
    done
}
# Searches until the changes are done and the read is over; loop $1 keeps
# its stderr in race.err.$1.
searching() {
    local n=0
    while kill -0 "$changer" 2>/dev/null || kill -0 "$reader" 2>/dev/null; do
        (cd "$TREE" && WATCHMAN_SOCK="$SOCK" "$BIN" --index "$IDX" -c needle </dev/null >/dev/null 2>>"$WORK/race.err.$1") || true
        n=$((n + 1))
    done
    echo "$n" >>"$WORK/searches"
}

# Round $2 of changes while watchman reads the tree, with three loops of
# searches, then the comparisons; $1 says why watchman reads it. Prints how
# many changes fell within the read.
race() {
    local why=$1 round=$2 t0 end searchers=()
    : >"$WORK/searches"
    rm -f "$WORK"/race.err.*
    t0=$(now)
    (
        # Once a search has asked for the watch, when there is none.
        until call '["watch-list"]' | grep -qF "\"$TREE\""; do sleep 0.01; done
        crawled || { echo "watchman did not read the tree" >&2; exit 1; }
        now >"$WORK/crawled"
    ) &
    reader=$!
    change "$round" >"$WORK/changes" &
    changer=$!
    for i in 1 2 3; do
        searching "$i" &
        searchers+=($!)
    done
    wait "$changer"
    wait "$reader"
    wait "${searchers[@]}"
    end=$(cat "$WORK/crawled")
    DURING=$(awk -v end="$end" '$1 < end' "$WORK/changes" | wc -l | tr -d ' ')
    echo "$why: watchman read the tree in $(awk -v a="$t0" -v b="$end" 'BEGIN {printf "%.1f", b - a}') s;" \
        "$DURING of $(wc -l <"$WORK/changes" | tr -d ' ') changes were made meanwhile;" \
        "$(awk '{n += $1} END {print n}' "$WORK/searches") searches ran during the changes" >&2
    cat "$WORK"/race.err.* | grep '^warning: ' | grep -v '^warning: excluding ' | sed -E 's/[0-9]+ ms/N ms/g' |
        sort | uniq -c | sort -rn >&2 || true
    checks
}

# Searches before any change, which leave a record of a token.
checks
# A restart: the daemon reads the whole tree again, from its saved state.
stop
start
race "after a restart" r1
[[ $DURING -gt 0 ]] || { echo "no change was made while watchman read the tree: raise FILES" >&2; exit 1; }
# No watch: the first search asks for one, and watchman reads the tree.
call "[\"watch-del\", \"$TREE\"]" >/dev/null
race "after the watch was removed" r2
# Changes after a token: no read.
change r3 >/dev/null
checks

echo "pass $PASS / fail $FAIL" >&2
[[ $FAIL == 0 ]]

#!/usr/bin/env bash
#
# scripts/fsmonitor.sh: have watchman watch a tree before fmgrep indexes it,
# as fmgrep index asks a running watchman for a token and a load of 50,000
# files or more asks it what changed since. The watch is watchman
# watch-project, which starts watchman if it is not running, waited on until
# watchman has crawled the tree; without watchman, or when the watch fails,
# a warning. Then give a git checkout git's file monitor, as a developer's
# would have, and print git status's wall time: git's builtin daemon where
# git has it (macOS, Windows, Linux from git 2.55), else watchman through
# git's fsmonitor-watchman hook, else a warning. Sets core.fsmonitor and
# core.untrackedCache unless the checkout already has them. See
# docs/benchmark.md#file-monitor.
#
# Usage: scripts/fsmonitor.sh DIR

set -euo pipefail

[[ $# == 1 ]] || { sed -n 's/^# \{0,1\}//; 15p' "${BASH_SOURCE[0]}" >&2; exit 2; }
DIR="$(cd "$1" && pwd -P)"

# fmgrep index waits at most 5 s for a watch it adds itself, so a big tree's
# crawl must be done before; the synced query returns once it is.
if ! command -v watchman >/dev/null; then
    echo "warning: watchman is not installed: fmgrep checks every file of $DIR on each load" >&2
elif ! OUT="$(watchman --no-pretty watch-project "$DIR")"; then
    echo "warning: watchman watch-project $DIR failed (${OUT:-no output}): fmgrep checks every file on each load" >&2
else
    WATCH="$(printf '%s' "$OUT" | sed -n 's/.*"watch": *"\([^"]*\)".*/\1/p')"
    WATCH="${WATCH:-$DIR}"
    SECONDS=0
    if watchman --no-pretty -j <<<"[\"query\", \"$WATCH\", {\"expression\": [\"false\"], \"sync_timeout\": 600000}]" >/dev/null; then
        echo "watchman watches $DIR (watch $WATCH, crawled in ${SECONDS} s)" >&2
    else
        echo "warning: watchman watches $DIR (watch $WATCH) but has not finished crawling it" >&2
    fi
fi
TOP="$(git -C "$DIR" rev-parse --show-toplevel 2>/dev/null)" || TOP=""
if [[ -z "$TOP" || "$(cd "$TOP" && pwd -P)" != "$DIR" ]]; then
    echo "warning: no file monitor for $DIR: not the top of a git checkout (nested checkouts, as in a repo-tool tree, are not covered)" >&2
    exit 0
fi
GIT_DIR="$(cd "$DIR" && cd "$(git rev-parse --git-dir)" && pwd -P)"

# The hook git ships among its templates, or the copy git init left.
hook_sample() {
    local s
    for s in "$GIT_DIR/hooks/fsmonitor-watchman.sample" \
        "$(git --exec-path)/../../share/git-core/templates/hooks/fsmonitor-watchman.sample"; do
        [[ -f "$s" ]] && { echo "$s"; return; }
    done
    return 1
}

MONITOR="$(git -C "$DIR" config core.fsmonitor || true)"
if [[ -z "$MONITOR" ]]; then
    if git version --build-options | grep -qx 'feature: fsmonitor--daemon'; then
        MONITOR=true
    elif watchman --version >/dev/null 2>&1 && SAMPLE="$(hook_sample)"; then
        MONITOR="$GIT_DIR/hooks/fsmonitor-watchman"
        mkdir -p "$GIT_DIR/hooks"
        cp "$SAMPLE" "$MONITOR"
        chmod 755 "$MONITOR"
    else
        echo "warning: no file monitor for $DIR: this git has no builtin daemon and watchman (or git's fsmonitor-watchman hook) is missing; git status scans the tree" >&2
    fi
    [[ -z "$MONITOR" ]] || git -C "$DIR" config core.fsmonitor "$MONITOR"
fi
[[ -n "$(git -C "$DIR" config core.untrackedCache || true)" ]] || git -C "$DIR" config core.untrackedCache true

# Wall seconds of one git status, which also refreshes the index's caches.
status_time() {
    local TIMEFORMAT=%R
    { time git -C "$DIR" status --porcelain >/dev/null 2>&1; } 2>&1
}
# The first starts the monitor and fills the caches; the second is what a developer's git status sees.
FIRST="$(status_time)"
SECOND="$(status_time)"
echo "git status in $DIR: ${FIRST} s, then ${SECOND} s (core.fsmonitor=${MONITOR:-unset})" >&2

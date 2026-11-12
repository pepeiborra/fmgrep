#!/usr/bin/env bash
#
# scripts/fsmonitor.sh: have watchman watch a tree before fmgrep searches it,
# as a load of 50,000 files or more asks watchman what changed (the first
# load of an index takes the token). The watch is watchman watch-project,
# which starts watchman if it is not running, waited on until watchman has
# crawled the tree; without watchman, or when the watch fails, a warning.
# See docs/benchmark.md#file-monitor.
#
# Usage: scripts/fsmonitor.sh DIR

set -euo pipefail

[[ $# == 1 ]] || { sed -n 's/^# \{0,1\}//; 10p' "${BASH_SOURCE[0]}" >&2; exit 2; }
DIR="$(cd "$1" && pwd -P)"

# fmgrep index and a search add a missing watch but do not wait for its
# crawl, and a search checks every file until the crawl ends; so the watch
# and a big tree's crawl are done here, before the first search. The
# synced query returns once it is.
if ! command -v watchman >/dev/null; then
    echo "warning: watchman is not installed: fmgrep checks every file of $DIR on each load" >&2
elif ! OUT="$(watchman --no-pretty watch-project "$DIR")"; then
    echo "warning: watchman watch-project $DIR failed (${OUT:-no output}): fmgrep checks every file on each load" >&2
elif ! WATCH="$(printf '%s' "$OUT" | sed -nE 's/.*"watch": *"(([^"\\]|\\.)*)".*/\1/p')" || [[ -z $WATCH ]]; then
    echo "warning: watchman watch-project $DIR named no watch ($OUT): fmgrep checks every file on each load" >&2
else
    # WATCH is the root as watchman's JSON spells it, escapes kept, so it
    # goes back into the query as it is.
    SECONDS=0
    if watchman --no-pretty -j <<<"[\"query\", \"$WATCH\", {\"expression\": [\"false\"], \"sync_timeout\": 600000}]" >/dev/null; then
        echo "watchman watches $DIR (watch $WATCH, crawled in ${SECONDS} s)" >&2
    else
        echo "warning: watchman watches $DIR (watch $WATCH) but has not finished crawling it" >&2
    fi
fi

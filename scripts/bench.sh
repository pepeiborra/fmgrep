#!/usr/bin/env bash
#
# scripts/bench.sh: benchmark fmgrep against ripgrep on a pinned corpus.
#
# Without --repo-path, shallow-clones the commit the corpus's query set is
# pinned to into BENCH_DIR/<corpus> (default target/bench), reusing a clean
# clone. Builds release, runs the harness over its default rows, and prints
# the markdown results (default BENCH_DIR/results-<corpus>.md). Exits
# non-zero when the rows disagree. The corpus is only read.
#
# Usage: scripts/bench.sh [--corpus ghc|linux|chromium] [--repo-path DIR]
#                         [--results FILE] [--bench-dir DIR] [-- HARNESS_ARGS...]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORPUS=ghc
REPO_PATH=""
RESULTS=""
BENCH_DIR="$ROOT/target/bench"
PASSTHROUGH=()

usage() { sed -n 's/^# \{0,1\}//; 11,12p' "${BASH_SOURCE[0]}"; }
die() { echo "error: $2" >&2; exit "$1"; }

while [[ $# -gt 0 ]]; do
    case "$1" in
    --corpus | --repo-path | --results | --bench-dir)
        [[ $# -ge 2 ]] || die 2 "$1 needs a value"
        case "$1" in
        --corpus) CORPUS="$2" ;;
        --repo-path) REPO_PATH="$2" ;;
        --results) RESULTS="$2" ;;
        --bench-dir) BENCH_DIR="$2" ;;
        esac
        shift 2
        ;;
    --) shift; PASSTHROUGH=("$@"); break ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; die 2 "unknown argument $1" ;;
    esac
done
case "$CORPUS" in
ghc | linux | chromium) ;;
*) die 2 "--corpus must be ghc, linux or chromium, not $CORPUS" ;;
esac

for tool in rg git cargo; do
    command -v "$tool" >/dev/null || die 2 "$tool is not on PATH"
done
( cd "$ROOT" && cargo build --release --locked -p fmgrep -p bench ) >&2
BENCH="${CARGO_TARGET_DIR:-$ROOT/target}/release/bench"

QUERIES="$ROOT/crates/bench/queries/$CORPUS.json"
URL="$(sed -n 's/.*"repo_url": *"\([^"]*\)".*/\1/p' "$QUERIES")"
COMMIT="$(sed -n 's/.*"commit": *"\([^"]*\)".*/\1/p' "$QUERIES")"

mkdir -p "$BENCH_DIR"
BENCH_DIR="$(cd "$BENCH_DIR" && pwd)"

# Fetch the pinned commit into the git checkout $1 and check it out.
fetch_pin() {
    git -C "$1" fetch -q --depth 1 --no-tags "$URL" "$COMMIT"
    git -C "$1" -c advice.detachedHead=false checkout -q --detach FETCH_HEAD
}

if [[ -n "$REPO_PATH" ]]; then
    [[ -d "$REPO_PATH" ]] || die 2 "--repo-path $REPO_PATH is not a directory"
    DIR="$(cd "$REPO_PATH" && pwd)"
    HEAD="$(git -C "$DIR" rev-parse HEAD 2>/dev/null || true)"
    if [[ "$HEAD" != "$COMMIT" ]]; then
        echo "warning: $DIR is at ${HEAD:-no git revision}, not the pinned $COMMIT; results are not comparable with the pinned set" >&2
    fi
else
    DIR="$BENCH_DIR/$CORPUS"
    if [[ ! -e "$DIR" ]]; then
        echo "cloning $URL at $COMMIT into $DIR" >&2
        TMP="$DIR.partial"
        rm -rf "$TMP"
        git init -q "$TMP"
        fetch_pin "$TMP"
        mv "$TMP" "$DIR"
    else
        TOP="$(git -C "$DIR" rev-parse --show-toplevel 2>/dev/null || true)"
        [[ -n "$TOP" && "$(cd "$TOP" && pwd -P)" == "$(cd "$DIR" && pwd -P)" ]] ||
            die 1 "$DIR is not a git checkout; remove it (rm -rf $DIR) to clone again"
        [[ -z "$(git -C "$DIR" status --porcelain -uall)" ]] ||
            die 1 "$DIR has local changes; remove it (rm -rf $DIR) to clone again"
        if [[ "$(git -C "$DIR" rev-parse HEAD)" == "$COMMIT" ]]; then
            echo "reusing $DIR at $COMMIT" >&2
        else
            echo "checking out $COMMIT in $DIR" >&2
            fetch_pin "$DIR"
        fi
    fi
fi

RESULTS="${RESULTS:-$BENCH_DIR/results-$CORPUS.md}"
mkdir -p "$(dirname "$RESULTS")"
rm -f "$RESULTS"
VAR="FMGREP_CORPUS_$(printf '%s' "$CORPUS" | tr '[:lower:]' '[:upper:]')"
set +e
env "$VAR=$DIR" "$BENCH" --corpus "$CORPUS" --trace "$BENCH_DIR/trace-$CORPUS.jsonl" \
    ${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"} >"$RESULTS"
STATUS=$?
set -e
if [[ -s "$RESULTS" ]]; then
    echo
    cat "$RESULTS"
fi
exit "$STATUS"

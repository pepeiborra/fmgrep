#!/usr/bin/env bash
#
# scripts/rg-bool-oracle.sh: ripgrep ground truth for `--expr-json` AND / OR
# pairs from the GHC query set, on one built index.
#
# AND a b: `-l` equals `rg -l a | xargs rg -l b`; `-c` / `--count-matches`
# equal `rg -e a -e b` over those files. OR a b: `-l`, `-c` and
# `--count-matches` equal `rg -e a -e b`. Either way the served
# `docs_matched` is the member count, and `total_matches` rg's
# `--count-matches` sum whenever `counts_exact`. rg reads the manifest's file
# list with -a, never the raw tree; outputs compare sorted. `--and` / `--or`
# over the leaves as terms must print and serve what `--expr-json` does, and
# `--expr` of the served label must list what `--expr-json -l` lists.
#
# Needs rg and jq, so it is not part of scripts/check.sh.
#
# Usage: scripts/rg-bool-oracle.sh <fmgrep-binary> <index-dir> [corpus-dir]
#        (corpus root: the parent of a .fmgrep index dir, else the
#        manifest's, else $FMGREP_CORPUS_GHC; `fmgrep index CORPUS --output
#        DIR` builds an index outside a read-only corpus)

set -euo pipefail

BIN="$1"
IDX="$2"
DOTDIR_ROOT=""
[[ "$(basename "$IDX")" == .fmgrep ]] && DOTDIR_ROOT="$(dirname "$IDX")"
ROOT="${3:-${DOTDIR_ROOT:-$(jq -r '.corpus_root // empty' "$IDX/manifest.json")}}"
ROOT="${ROOT:-${FMGREP_CORPUS_GHC:?set FMGREP_CORPUS_GHC or pass the corpus root}}"
WORK="$(mktemp -d)"
SERVER=""
trap '[[ -n "$SERVER" ]] && kill "$SERVER" 2>/dev/null; rm -rf "$WORK"' EXIT

jq -r '.files[]' "$IDX/manifest.json" | tr '\n' '\0' > "$WORK/all0"
echo "index=$IDX store_docs=$(jq -r .store_docs "$IDX/manifest.json") files=$(jq '.files | length' "$IDX/manifest.json")"

# rg over the NUL-separated list in $1; any rg error fails the run.
rg_over() {
    local list="$1"
    shift
    [[ -s "$list" ]] || return 0
    ( cd "$ROOT" && xargs -0 rg --no-ignore --no-require-git -a -H "$@" -- < "$list" 2> "$WORK/rgerr" ) || true
    if [[ -s "$WORK/rgerr" ]]; then
        cat "$WORK/rgerr" >&2
        exit 2
    fi
}

# rg's pattern for a leaf: `t:` literals escaped, `r:` regexes verbatim.
pattern() {
    case "$1" in
    t:*) printf '%s' "${1#t:}" | sed 's/[][\.*^$+?(){}|/]/\\&/g' ;;
    r:*) printf '%s' "${1#r:}" ;;
    esac
}

leaf_json() {
    case "$1" in
    t:*) jq -cn --arg v "${1#t:}" '{term: $v}' ;;
    r:*) jq -cn --arg v "${1#r:}" '{regex: $v}' ;;
    esac
}

"$BIN" serve "$IDX" > "$WORK/addr" &
SERVER=$!
until [[ -s "$WORK/addr" ]]; do
    kill -0 "$SERVER" 2>/dev/null || { echo "serve exited"; exit 2; }
    sleep 0.1
done
ADDR="$(head -1 "$WORK/addr")"

PASS=0
TOTAL=0

# One pair: op (and|or), then two leaves.
pair() {
    local op="$1" a="$2" b="$3" pa pb json fail=""
    pa="$(pattern "$a")"
    pb="$(pattern "$b")"
    json="$(jq -cn --arg op "$op" --argjson a "$(leaf_json "$a")" --argjson b "$(leaf_json "$b")" '{($op): [$a, $b]}')"
    TOTAL=$((TOTAL + 1))
    if [[ "$op" == and ]]; then
        rg_over "$WORK/all0" -l -e "$pa" | tr '\n' '\0' > "$WORK/a0"
        rg_over "$WORK/a0" -l -e "$pb" | LC_ALL=C sort > "$WORK/members"
    else
        rg_over "$WORK/all0" -l -e "$pa" -e "$pb" | LC_ALL=C sort > "$WORK/members"
    fi
    tr '\n' '\0' < "$WORK/members" > "$WORK/members0"
    "$BIN" --index "$IDX" -l --expr-json "$json" 2> /dev/null | LC_ALL=C sort > "$WORK/ours-l"
    cmp -s "$WORK/members" "$WORK/ours-l" || fail+=" -l"
    local flag
    for flag in -c --count-matches; do
        rg_over "$WORK/members0" "$flag" -e "$pa" -e "$pb" | LC_ALL=C sort > "$WORK/theirs$flag"
        "$BIN" --index "$IDX" "$flag" --expr-json "$json" 2> /dev/null | LC_ALL=C sort > "$WORK/ours$flag"
        cmp -s "$WORK/theirs$flag" "$WORK/ours$flag" || fail+=" $flag"
    done
    local docs sum served
    docs="$(wc -l < "$WORK/members" | tr -d ' ')"
    sum="$(awk -F: '{ s += $NF } END { print s + 0 }' "$WORK/theirs--count-matches")"
    served="$("$BIN" --server "$ADDR" --json --expr-json "$json")"
    [[ "$(jq '.queries[0].docs_matched' <<< "$served")" == "$docs" ]] || fail+=" docs_matched"
    local exact total
    exact="$(jq '.queries[0] | if has("counts_exact") then .counts_exact else true end' <<< "$served")"
    total="$(jq '.queries[0].total_matches' <<< "$served")"
    if [[ "$exact" == true && "$total" != "$sum" ]]; then
        fail+=" total_matches"
    fi
    local terms=() leaf
    for leaf in "$a" "$b"; do
        case "$leaf" in t:*) terms+=("${leaf#t:}") ;; r:*) terms+=("/${leaf#r:}/") ;; esac
    done
    for flag in -l -c --count-matches; do
        "$BIN" --index "$IDX" "$flag" "--$op" -- "${terms[@]}" 2> /dev/null | LC_ALL=C sort > "$WORK/flags$flag"
        cmp -s "$WORK/ours$flag" "$WORK/flags$flag" || fail+=" flags$flag"
    done
    local untimed='del(.index_time_us, .queries[].latency_us)'
    [[ "$("$BIN" --server "$ADDR" --json "--$op" -- "${terms[@]}" | jq -c "$untimed")" == "$(jq -c "$untimed" <<< "$served")" ]] || fail+=" flags-server"
    local label name
    label="$(jq -r '.queries[0].term' <<< "$served")"
    name="$(tr '[:lower:]' '[:upper:]' <<< "$op")"
    "$BIN" --index "$IDX" -l --expr "$label" 2> /dev/null | LC_ALL=C sort > "$WORK/text-l"
    cmp -s "$WORK/ours-l" "$WORK/text-l" || fail+=" text"
    if [[ -z "$fail" ]]; then
        PASS=$((PASS + 1))
        echo "ok   $name [$label] docs=$docs rg_matches=$sum served_total=$total exact=$exact text=ok"
    else
        echo "FAIL $name [$label]:$fail (docs=$docs rg_matches=$sum served=$served)"
        for f in -l -c --count-matches; do
            [[ "$f" == -l ]] && diff "$WORK/members" "$WORK/ours-l" | head -5
            [[ "$f" != -l ]] && diff "$WORK/theirs$f" "$WORK/ours$f" | head -5
        done
    fi
}

pair and t:HscEnv t:DynFlags
pair and t:StgTSO 't:StgClosure *'
pair and 't:StgClosure *' 't:Capability *cap'
pair and t:pprPanic t:assertPpr
pair and 't:instance Outputable' 't:import GHC.Types.Name'
pair and t:mkTyConApp 'r:pprPanic "[^"\n]*"'
pair and 'r:\{-# INLINE \w+ #-\}' 'r:import GHC\.(Core|Stg|Cmm)\.[\w.]+'
pair and t:StgToCmm 'r:import GHC\.(Core|Stg|Cmm)\.[\w.]+'
pair and 't:foreign import ccall' 'r:foreign import ccall (unsafe|safe)'
pair and t:allocateMightFail 't:instance Outputable'
pair or t:HscEnv t:DynFlags
pair or t:zonkTcType t:tcLookupGlobal
pair or t:debugBelch t:ACQUIRE_LOCK
pair or t:StgRhsClosure t:StgToCmm
pair or t:CmmGraph t:StgTSO
pair or t:pprPanic 'r:pprPanic "[^"\n]*"'
pair or 'r:stg_\w+_info' 't:StgClosure *'
pair or t:unsafePerformIO t:HasCallStack
pair or t:throwGhcException t:pprPanic
pair or 'r:foreign import ccall (unsafe|safe)' 'r:\{-# INLINE \w+ #-\}'

"$BIN" --server "$ADDR" --shutdown
wait "$SERVER" || true
SERVER=""
echo "pass $PASS/$TOTAL"
[[ "$PASS" == "$TOTAL" ]]

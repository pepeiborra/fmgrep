#!/usr/bin/env bash
#
# scripts/rg-oracle.sh: byte-for-byte diff of fmgrep's output against rg.
#
# Stages a fixture tree, indexes it into its .fmgrep, then compares our stdout
# (searching from a subdirectory, so the index is found by walking up) and
# exit status with rg's for every output flag combination. Sibling names are
# chosen so rg's per-directory DFS (`--sort path`) and our sorted relative
# paths agree (`a.txt` vs `a/b.txt` would not).
#
# Without --store-docs every case runs twice: on the indexed tree, backdated
# so discovery prunes by directory stamp, then on a stale one (files edited,
# deleted, renamed and added after index), where each search must print
# exactly three warnings: the deleted file, the one moved with its directory,
# and the one turned binary.
#
# --serve repeats every case through `--server` (lines `srv`): same stdout and
# exit status, empty stderr.
#
# --corpus DIR indexes DIR instead and checks AND / OR pairs of GHC terms:
# -l, -c and --count-matches against rg over the manifest's files, the
# served counts, --and / --or and --expr of the served label; then -g sets:
# our -l of every line, local and served, against rg's walk with the same
# globs over the manifest's files. Needs jq.
#
# Needs rg and builds release, so it is not part of scripts/check.sh. Ends with
# `pass N / fail M` (plus the served count); exits 1 if any case
# differs, 2 on a usage error or an index whose manifest contradicts the flag.
#
# Usage: scripts/rg-oracle.sh [--store-docs] [--serve]
#        scripts/rg-oracle.sh --corpus DIR [--store-docs]

set -euo pipefail
# Flags hold globs, word-split unquoted.
set -f

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v rg >/dev/null || { echo "rg is not on PATH"; exit 2; }

STORE_DOCS=false
INDEX_FLAGS=()
SERVE=false
CORPUS_DIR=""
usage() {
    sed -n 's/^# \{0,1\}//; 28,29p' "${BASH_SOURCE[0]}" >&2
    exit 2
}
while [[ $# -gt 0 ]]; do
    case "$1" in
    --store-docs) STORE_DOCS=true; INDEX_FLAGS=(--store-docs) ;;
    --serve) SERVE=true ;;
    --corpus) [[ $# -ge 2 ]] || usage; CORPUS_DIR="$2"; shift ;;
    *) usage ;;
    esac
    shift
done
if [[ -n "$CORPUS_DIR" && "$SERVE" == true ]]; then usage; fi
if [[ -n "$CORPUS_DIR" ]]; then
    command -v jq >/dev/null || { echo "jq is not on PATH"; exit 2; }
    CORPUS="$(cd "$CORPUS_DIR" && pwd)" || exit 2
fi

cargo build --release --locked -p fmgrep >&2
BIN="$ROOT/target/release/fmgrep"

WORK="$(mktemp -d)"
SERVER=""
ADDR=""
stop_server() {
    if [[ -n "$SERVER" ]]; then
        "$BIN" --server "$ADDR" --shutdown > /dev/null 2>&1 || kill "$SERVER" 2> /dev/null || true
        wait "$SERVER" 2> /dev/null || true
    fi
    SERVER=""
    ADDR=""
}
trap 'stop_server; rm -rf "$WORK"' EXIT
# `serve $1` in the background (`$2` in errors): pid into `$3`, address into `$4`.
spawn_serve() {
    local ready="$WORK/ready.$3"
    : > "$ready"
    "$BIN" serve "$1" > "$ready" 2>> "$WORK/serve.err" &
    printf -v "$3" %s "$!"
    for _ in $(seq 300); do
        [[ -s "$ready" ]] && break
        sleep 0.1
    done
    printf -v "$4" %s "$(head -n1 "$ready")"
    [[ -n "${!4}" ]] || { echo "$2 did not become ready"; cat "$WORK/serve.err"; exit 2; }
}

RG_COMMON=(--no-ignore --no-require-git -a --sort path)
# --iglob: rg adds them after every -g, so a case's own globs cannot undo them.
RG_SKIP=(--iglob '!k.dat')
TERM=alpha
# A case's PATHs: ours from sub/, rg's from the root; WANT overrides WARNINGS.
PATHS=""
RG_PATHS=.
WANT=""
FAIL=0
PASS=""
WARNINGS=0
OK=0
FAILED=0
SERVED=0
RC=0

# The case just run, again through the `--serve` server: no stderr unless the
# local search refused, and its exit status, RC.
served() {
    [[ -n "$ADDR" ]] || return 0
    local name="$1" rc=0
    shift
    ( cd "$CORPUS/sub" && "$BIN" --server "$ADDR" "$@" ) > "$WORK/srv" 2> "$WORK/srv.err" || rc=$?
    if [[ "$rc" != "$RC" || ("$RC" != 2 && -s "$WORK/srv.err") ]]; then
        echo "FAIL srv $PASS$name  (exit $rc, want exit $RC and no stderr)"
        cat "$WORK/srv.err"
        FAIL=1
        FAILED=$((FAILED + 1))
    elif cmp -s "$WORK/ours" "$WORK/srv"; then
        echo "srv $PASS$name"
        SERVED=$((SERVED + 1))
    else
        echo "FAIL srv $PASS$name  (differs from the local search)"
        diff -u "$WORK/ours" "$WORK/srv" || true
        FAIL=1
        FAILED=$((FAILED + 1))
    fi
}

# One case: name, our flags, rg's flags (both space-separated), term.
check() {
    local name="$1" mine="$2" theirs="$3" term="${4:-$TERM}" rgrc
    RC=0
    # shellcheck disable=SC2086
    ( cd "$HERE" && "$BIN" $mine -- "$term" $PATHS ) > "$WORK/ours" 2> "$WORK/err" || RC=$?
    set +e
    # rg echoes the directory it was given; our paths are corpus-relative.
    # shellcheck disable=SC2086
    ( cd "$CORPUS" && rg "${RG_COMMON[@]}" $theirs "${RG_SKIP[@]}" -- "$term" $RG_PATHS < /dev/null ) | sed 's|\./||g' > "$WORK/theirs"
    rgrc="${PIPESTATUS[0]}"
    set -e
    if [[ "$RC" != "$rgrc" ]]; then
        echo "FAIL $PASS$name  (exit $RC, rg exits $rgrc)"
        cat "$WORK/err"
        FAIL=1
        FAILED=$((FAILED + 1))
    elif [[ "$(grep -c '^warning:' "$WORK/err")" != "${WANT:-$WARNINGS}" ]]; then
        echo "FAIL $PASS$name  (want ${WANT:-$WARNINGS} warning line(s))"
        cat "$WORK/err"
        FAIL=1
        FAILED=$((FAILED + 1))
    elif diff -u "$WORK/theirs" "$WORK/ours" > "$WORK/diff"; then
        echo "ok   $PASS$name"
        OK=$((OK + 1))
    else
        echo "FAIL $PASS$name  (ours: $mine $PATHS | rg: $theirs $RG_PATHS)"
        cat "$WORK/diff"
        FAIL=1
        FAILED=$((FAILED + 1))
    fi
    # shellcheck disable=SC2086
    served "$name" $mine -- "$term" $PATHS
}

# A filter case in lines and -l: name, our flags, rg's flags, our PATHs, rg's
# PATHs (default ., none when empty), and `tb` when it searches tb.txt, whose
# read warns in the stale pass.
filtered() {
    PATHS="${4:-}"
    RG_PATHS="${5-.}"
    [[ "${6:-}" == tb ]] || WANT=$((WARNINGS > 0 ? WARNINGS - 1 : 0))
    check "$1 [lines]" "$2" "--heading -n $3"
    check "$1 [-l]" "$2 -l" "-l $3"
    PATHS=""
    RG_PATHS=.
    WANT=""
}

# Expressions (--expr-json, then the same tree as --expr text). Tokens:
# `t:lit` / `r:regex` leaves, `and{` / `or{` ... `}` nodes. A leaf's files are
# `rg -l` of it; nodes intersect or unite them. The truth is rg -e leaf1 -e
# leaf2 ... over those files.
TOK=()
POS=0
JSON=""
TEXT=""
RGE=()
MFLAGS=""
OURS=()

json_str() {
    local v="${1//\\/\\\\}"
    printf '"%s"' "${v//\"/\\\"}"
}

# A /regex/ leaf: unescaped '/' becomes '\/'; '\x' pairs are kept.
re_text() {
    local s="$1" o=""
    while [[ -n "$s" ]]; do
        case "$s" in
        \\?*) o+="${s:0:2}"; s="${s:2}" ;;
        /*) o+='\/'; s="${s:1}" ;;
        *) o+="${s:0:1}"; s="${s:1}" ;;
        esac
    done
    printf '/%s/' "$o"
}

# Parse the node at TOK[POS] into JSON, TEXT and RGE; write its sorted files to $1.
node() {
    local out="$1" tok="${TOK[$POS]}"
    POS=$((POS + 1))
    case "$tok" in
    'and{' | 'or{')
        local op="${tok%\{}" first=1 sub="$1.sub" word
        word="$(tr '[:lower:]' '[:upper:]' <<< "$op")"
        JSON+="{\"$op\":["
        TEXT+="("
        while [[ "${TOK[$POS]}" != "}" ]]; do
            [[ "$first" == 1 ]] || { JSON+=","; TEXT+=" $word "; }
            node "$sub"
            if [[ "$first" == 1 ]]; then
                cp "$sub" "$out"
            elif [[ "$op" == and ]]; then
                LC_ALL=C comm -12 "$out" "$sub" > "$out.tmp" && mv "$out.tmp" "$out"
            else
                LC_ALL=C sort -u "$out" "$sub" > "$out.tmp" && mv "$out.tmp" "$out"
            fi
            first=0
        done
        POS=$((POS + 1))
        JSON+="]}"
        TEXT+=")"
        ;;
    t:* | r:*)
        local val="${tok#?:}" key=regex pat
        pat="$val"
        if [[ "$tok" == t:* ]]; then
            key=term
            pat="$(printf '%s' "$val" | sed 's/[][\.*^$+?(){}|/]/\\&/g')"
        fi
        JSON+="{\"$key\":$(json_str "$val")}"
        if [[ "$key" == term ]]; then TEXT+="$(json_str "$val")"; else TEXT+="$(re_text "$val")"; fi
        RGE+=(-e "$pat")
        if [[ -n "$CORPUS_DIR" ]]; then
            rg_list "$WORK/all0" -l -e "$pat" | LC_ALL=C sort > "$out"
        else
            set +e
            # shellcheck disable=SC2086
            ( cd "$CORPUS" && rg "${RG_COMMON[@]}" "${RG_SKIP[@]}" $MFLAGS -l -e "$pat" . ) | sed 's|^\./||' | LC_ALL=C sort > "$out"
            set -e
        fi
        ;;
    *) echo "bad expression token $tok" >&2; exit 2 ;;
    esac
}

# Parse the tokens into JSON, TEXT and RGE; write their sorted files to $WORK/members.
parse() {
    TOK=("$@")
    POS=0
    JSON=""
    TEXT=""
    RGE=()
    node "$WORK/members"
}

# One expression case: name, our flags, rg's flags, then the tokens. Runs
# --expr-json, then --expr as its own `expr-text-*` check; OURS instead if set.
check_expr() {
    local name="$1" mine="$2" theirs="$3" w form
    shift 3
    MFLAGS=""
    for w in $theirs; do
        case "$w" in -i | -U | --multiline-dotall) MFLAGS+=" $w" ;; esac
    done
    parse "$@"
    : > "$WORK/theirs"
    local rgrc=1
    if [[ -s "$WORK/members" ]]; then
        local files=()
        while IFS= read -r w; do files+=("$w"); done < "$WORK/members"
        set +e
        # shellcheck disable=SC2086
        ( cd "$CORPUS" && rg "${RG_COMMON[@]}" "${RG_SKIP[@]}" -H $theirs "${RGE[@]}" -- "${files[@]}" ) > "$WORK/theirs"
        rgrc=$?
        set -e
    fi
    local forms=(json text) argv
    [[ ${#OURS[@]} -gt 0 ]] && forms=(flags)
    for form in "${forms[@]}"; do
        case "$form" in
        json) argv=(--expr-json "$JSON") ;;
        text) name="${name/expr-/expr-text-}"; argv=(--expr "$TEXT") ;;
        flags) argv=("${OURS[@]}") ;;
        esac
        RC=0
        # shellcheck disable=SC2086
        ( cd "$HERE" && "$BIN" $mine "${argv[@]}" ) > "$WORK/ours" 2> "$WORK/err" || RC=$?
        if [[ "$RC" != "$rgrc" ]]; then
            echo "FAIL $PASS$name  (exit $RC, rg exits $rgrc)"
            cat "$WORK/err"
            FAIL=1
            FAILED=$((FAILED + 1))
        elif [[ "$(grep -c '^warning:' "$WORK/err")" != "$WARNINGS" ]]; then
            echo "FAIL $PASS$name  (want $WARNINGS warning line(s))"
            cat "$WORK/err"
            FAIL=1
            FAILED=$((FAILED + 1))
        elif diff -u "$WORK/theirs" "$WORK/ours" > "$WORK/diff"; then
            echo "ok   $PASS$name"
            OK=$((OK + 1))
        else
            echo "FAIL $PASS$name  (ours: $mine ${argv[*]} | rg: $theirs ${RGE[*]})"
            cat "$WORK/diff"
            FAIL=1
            FAILED=$((FAILED + 1))
        fi
        # shellcheck disable=SC2086
        served "$name" $mine "${argv[@]}"
    done
    OURS=()
}

# OURS: `--$1` over the leaves as terms, `r:x` spelled /x/, or x if our flags $2 hold --regex.
terms() {
    local op="$1" mine="$2" tok
    shift 2
    OURS=("--$op" --)
    for tok in "$@"; do
        case "$tok" in
        t:*) OURS+=("${tok#t:}") ;;
        r:*) if [[ " $mine " == *" --regex "* ]]; then OURS+=("${tok#r:}"); else OURS+=("/${tok#r:}/"); fi ;;
        esac
    done
}

# `--and` / `--or` over the leaves; the truth is check_expr's for the same node.
check_flags() {
    local name="$1" mine="$2" theirs="$3" op="$4"
    shift 4
    terms "$op" "$mine" "$@"
    check_expr "$name" "$mine" "$theirs" "$op{" "$@" '}'
}

flag_cases() {
    local label="$1" m="$2"
    shift 2
    check_flags "flags-lines    [$label]" "$m"                   "--heading -n"      "$@"
    check_flags "flags-no-head  [$label]" "$m --no-heading"      "--no-heading -n"   "$@"
    check_flags "flags-count    [$label]" "$m -c"                "-c"                "$@"
    check_flags "flags-countm   [$label]" "$m --count-matches"   "--count-matches"   "$@"
    check_flags "flags-files    [$label]" "$m -l"                "-l"                "$@"
    check_flags "flags-only     [$label]" "$m -o"                "--heading -n -o"   "$@"
    check_flags "flags-context  [$label]" "$m -C 1"              "--heading -n -C 1" "$@"
}

expr_cases() {
    local label="$1"
    shift
    check_expr "expr-lines     [$label]" ""                "--heading -n"      "$@"
    check_expr "expr-no-head   [$label]" "--no-heading"    "--no-heading -n"   "$@"
    check_expr "expr-count     [$label]" "-c"              "-c"                "$@"
    check_expr "expr-countm    [$label]" "--count-matches" "--count-matches"   "$@"
    check_expr "expr-files     [$label]" "-l"              "-l"                "$@"
    check_expr "expr-only      [$label]" "-o"              "--heading -n -o"   "$@"
    check_expr "expr-context   [$label]" "-C 1"            "--heading -n -C 1" "$@"
}

# Exits 2 unless the manifest in index dir $1 records STORE_DOCS.
manifest_ok() {
    grep -q "\"store_docs\": *$STORE_DOCS[,}]" "$1/manifest.json" ||
        { echo "the index manifest does not record store_docs=$STORE_DOCS" >&2; exit 2; }
}

# rg over the NUL-separated file list $1 from the corpus root; any rg error exits 2.
rg_list() {
    local list="$1"
    shift
    [[ -s "$list" ]] || return 0
    ( cd "$CORPUS" && xargs -0 rg --no-ignore --no-require-git -a -H "$@" -- < "$list" 2> "$WORK/rgerr" ) || true
    [[ ! -s "$WORK/rgerr" ]] || { cat "$WORK/rgerr" >&2; exit 2; }
}

# Our sorted output for "$@" into $1; exit 1 (no match) is no error.
ours() {
    local out="$1"
    shift
    { "$BIN" --index "$IDX" "$@" 2> /dev/null || [[ $? == 1 ]]; } | LC_ALL=C sort > "$out"
}

# One --corpus pair: and|or, then two leaves.
pair() {
    local op="$1" fail="" untimed='del(.index_time_us, .queries[].latency_us)' name flag docs sum served rc=0 exact total label
    name="$(tr '[:lower:]' '[:upper:]' <<< "$op")"
    MFLAGS=""
    parse "$op{" "$2" "$3" '}'
    terms "$op" "" "$2" "$3"
    tr '\n' '\0' < "$WORK/members" > "$WORK/members0"
    cp "$WORK/members" "$WORK/theirs-l"
    for flag in -l -c --count-matches; do
        if [[ "$flag" != -l ]]; then
            rg_list "$WORK/members0" "$flag" "${RGE[@]}" | LC_ALL=C sort > "$WORK/theirs$flag"
        fi
        ours "$WORK/ours$flag" "$flag" --expr-json "$JSON"
        ours "$WORK/flags$flag" "$flag" "${OURS[@]}"
        cmp -s "$WORK/theirs$flag" "$WORK/ours$flag" || fail+=" $flag"
        cmp -s "$WORK/ours$flag" "$WORK/flags$flag" || fail+=" flags$flag"
    done
    docs="$(wc -l < "$WORK/members" | tr -d ' ')"
    sum="$(awk -F: '{ s += $NF } END { print s + 0 }' "$WORK/theirs--count-matches")"
    served="$("$BIN" --server "$ADDR" --json --expr-json "$JSON")" || rc=$?
    [[ "$rc" == "$((docs == 0))" ]] || fail+=" exit=$rc"
    [[ "$(jq '.queries[0].docs_matched' <<< "$served")" == "$docs" ]] || fail+=" docs_matched"
    exact="$(jq '.queries[0] | if has("counts_exact") then .counts_exact else true end' <<< "$served")"
    total="$(jq '.queries[0].total_matches' <<< "$served")"
    [[ "$exact" != true || "$total" == "$sum" ]] || fail+=" total_matches"
    [[ "$("$BIN" --server "$ADDR" --json "${OURS[@]}" | jq -c "$untimed")" == "$(jq -c "$untimed" <<< "$served")" ]] || fail+=" flags-server"
    label="$(jq -r '.queries[0].term' <<< "$served")"
    ours "$WORK/text-l" -l --expr "$label"
    cmp -s "$WORK/ours-l" "$WORK/text-l" || fail+=" text"
    OURS=()
    if [[ -z "$fail" ]]; then
        echo "ok   $name [$label] docs=$docs rg_matches=$sum served_total=$total exact=$exact text=ok"
        OK=$((OK + 1))
    else
        echo "FAIL $name [$label]:$fail (docs=$docs rg_matches=$sum served=$served)"
        for flag in -l -c --count-matches; do diff "$WORK/theirs$flag" "$WORK/ours$flag" | head -5 || true; done
        FAIL=1
        FAILED=$((FAILED + 1))
    fi
}

# One --corpus glob set: -g per argument, after GLOB_FLAGS.
globbed() {
    local g args=() rc=0 src=0 rgrc=0 fail=""
    for g in "$@"; do args+=(-g "$g"); done
    args=(${GLOB_FLAGS[@]+"${GLOB_FLAGS[@]}"} ${args[@]+"${args[@]}"})
    "$BIN" --index "$IDX" -l "${args[@]}" --regex '^' > "$WORK/raw" 2> /dev/null || rc=$?
    LC_ALL=C sort "$WORK/raw" > "$WORK/ours-g"
    "$BIN" --server "$ADDR" -l "${args[@]}" --regex '^' > "$WORK/raw" 2> /dev/null || src=$?
    LC_ALL=C sort "$WORK/raw" > "$WORK/srv-g"
    # No PATH: rg exits 2 only then when the globs exclude every file.
    ( cd "$CORPUS" && rg --no-ignore --no-require-git -a -l "${args[@]}" '^' < /dev/null ) > "$WORK/raw" 2> /dev/null || rgrc=$?
    sed 's|^\./||' "$WORK/raw" | LC_ALL=C sort | LC_ALL=C comm -12 - "$WORK/files" > "$WORK/rg-g"
    [[ "$rc" == "$rgrc" ]] || fail+=" exit=$rc/$rgrc"
    [[ "$src" == "$rc" ]] || fail+=" served-exit=$src"
    cmp -s "$WORK/ours-g" "$WORK/rg-g" || fail+=" files"
    cmp -s "$WORK/ours-g" "$WORK/srv-g" || fail+=" served"
    if [[ -z "$fail" ]]; then
        echo "ok   glob [${args[*]}] files=$(wc -l < "$WORK/ours-g" | tr -d ' ') exit=$rc"
        OK=$((OK + 1))
    else
        echo "FAIL glob [${args[*]}]:$fail"
        diff "$WORK/rg-g" "$WORK/ours-g" | head -5 || true
        FAIL=1
        FAILED=$((FAILED + 1))
    fi
}

if [[ -n "$CORPUS_DIR" ]]; then
    IDX="$WORK/index"
    "$BIN" index ${INDEX_FLAGS[@]+"${INDEX_FLAGS[@]}"} --output "$IDX" "$CORPUS" >&2
    manifest_ok "$IDX"
    jq -r '.files[]' "$IDX/manifest.json" | tr '\n' '\0' > "$WORK/all0"
    echo "corpus=$CORPUS store_docs=$STORE_DOCS files=$(jq '.files | length' "$IDX/manifest.json")"
    spawn_serve "$IDX" serve SERVER ADDR

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
    # common AND rare
    pair and t:import t:zonkTcType
    pair and t:zonkTcType t:import
    pair and t:the t:tcLookupGlobal
    pair and 'r:-- \|' t:simplifyTop
    pair and t:import 'r:zonkTc\w+'
    pair and t:where t:simplifyTop
    pair and t:module t:tcLookupGlobal

    LC_ALL=C sort <(tr '\0' '\n' < "$WORK/all0") > "$WORK/files"
    GLOB_FLAGS=()
    globbed '*.hs'
    globbed '!*.hs'
    globbed 'compiler/**'
    globbed '!testsuite/**'
    globbed '/compiler/GHC/*.hs'
    globbed '**/GHC/Core/*.hs'
    globbed '*.{c,h}'
    globbed '[A-Z]*.hs'
    globbed 'rts/'
    globbed '!testsuite/'
    globbed '*.hs' '!*Test*'
    globbed '!*' '*.hs'
    globbed '!**/tests/**'
    globbed 'docs/**/*.rst'
    globbed '*.cabal'
    globbed '!*.hs' '!*.c'
    globbed 'Makefile'
    globbed '/README*'
    globbed 'compiler/GHC/Tc/**' '!**/Solver/**'
    globbed '**/Utils/*'
    globbed '#comment' '*.y'
    globbed '*.[ch]'
    globbed 'libraries/base/**' '!libraries/base/src/GHC/**'
    globbed '!/compiler'
    GLOB_FLAGS=(--glob-case-insensitive)
    globbed '*.HS'
    GLOB_FLAGS=(--iglob '*.HS')
    globbed '!*Test*'

    echo "pass $OK / fail $FAILED"
    exit "$FAIL"
fi

CORPUS="$WORK/corpus"
mkdir -p "$CORPUS/sub"
printf 'alpha one\nbeta\nalpha two\ndelta\nepsilon\nzeta\nalpha and alpha\neta\n' > "$CORPUS/a.txt"
printf 'no hits here\n' > "$CORPUS/b.txt"
printf 'first\nalpha in sub\nthird\n' > "$CORPUS/sub/c.txt"
# No trailing newline: rg reports a zero-width match on the last line.
printf 'head\nalpha last' > "$CORPUS/d.txt"
# Mixed case plus the Unicode fold hazards: `-i` on `k` also matches U+212A
# KELVIN SIGN, on `s` also U+017F LATIN SMALL LETTER LONG S.
printf 'Alpha up\nALPHA shout\nalpha down\n' > "$CORPUS/h.txt"
printf 'printk here\nPRINTK there\nprint\xe2\x84\xaa kelvin\ncla\xc5\xbfs longs\n' > "$CORPUS/g.txt"
# Mixed case ACROSS a terminator: the -i x -U cross product.
printf 'Alpha One\nBETA\nalpha one\nbeta\n' > "$CORPUS/i.txt"
# Leading UTF-8 BOM: rg strips it before searching. Three shapes: a BOM
# alone (an empty file to rg), BOM + one line, BOM + CRLF lines.
printf '\xef\xbb\xbf' > "$CORPUS/e1.txt"
printf '\xef\xbb\xbfalpha bom\nsecond line\n' > "$CORPUS/e2.txt"
printf '\xef\xbb\xbfalpha crlf\r\nbeta\r\n' > "$CORPUS/e3.txt"
# Consecutive matching lines: under -U rg prints one block per RUN of them.
printf 'alpha 1\nalpha 2\nzz\nalpha 3\n' > "$CORPUS/f.txt"
# For the stale pass: a skipped binary alone in its directory (replaced by
# text, found once its directory is listed), a directory to rename, a text
# file to turn binary, and a tree to copy in with its mtimes.
mkdir -p "$CORPUS/kb" "$CORPUS/mw" "$WORK/tree/t/deep"
printf 'alpha\0binary\n' > "$CORPUS/kb/k.dat"
printf 'alpha moved\nbeta\n' > "$CORPUS/mw/m.txt"
printf 'alpha text\n' > "$CORPUS/tb.txt"
printf 'alpha deep tree\n' > "$WORK/tree/t/deep/x.txt"
printf 'beta\nalpha y\n' > "$WORK/tree/t/y.txt"
find "$CORPUS" "$WORK/tree" -exec touch -h -t 202001010000 {} +

( cd "$CORPUS" && "$BIN" index ${INDEX_FLAGS[@]+"${INDEX_FLAGS[@]}"} ) >&2
manifest_ok "$CORPUS/.fmgrep"
# Our searches run below the corpus root and find the index by walking up.
HERE="$CORPUS/sub"

start_server() {
    spawn_serve "$CORPUS" serve SERVER ADDR
}
[[ "$SERVE" == true ]] && start_server

cases() {

check default          ""                "--heading -n"
check no-heading       "--no-heading"    "--no-heading -n"
check no-line-number   "-N"              "--heading -N"
check count-lines      "-c"              "-c"
check count-matches    "--count-matches" "--count-matches"
check files-with-match "-l"              "-l"
check only-matching    "-o"              "--heading -n -o"
check after-1          "-A 1"            "--heading -n -A 1"
check before-1         "-B 1"            "--heading -n -B 1"
check context-2        "-C 2"            "--heading -n -C 2"
check color-always     "--color always"  "--heading -n --color always"
check color-no-heading "--no-heading --color always" "--no-heading -n --color always"
check color-count      "-c --color always"          "-c --color always"
check context-0        "-C 0"                       "--heading -n -C 0"
check context-no-head  "--no-heading -C 1"          "--no-heading -n -C 1"
check context-no-num   "--no-heading -N -C 1"       "--no-heading -N -C 1"
check only-no-heading  "-o --no-heading"            "--no-heading -n -o"
check only-context     "-o -C 1"                    "--heading -n -o -C 1"
check regex-lines      "--regex"                    "--heading -n"                'alpha \w+'
check regex-only       "--regex -o"                 "--heading -n -o"             'alpha \w+'

# -i: our bare terms are literals, so the fixture terms carry no metachar and
# `rg -i` is the same question. The last two pin the Unicode fold classes.
for term in alpha ALPHA Alpha printk PRINTK class; do
    check "ci-lines       [$term]" "-i"                 "--heading -n -i"    "$term"
    check "ci-only        [$term]" "-i -o"              "--heading -n -i -o" "$term"
    check "ci-count       [$term]" "-i -c"              "-c -i"              "$term"
    check "ci-countm      [$term]" "-i --count-matches" "--count-matches -i" "$term"
    check "ci-files       [$term]" "-i -l"              "-l -i"              "$term"
done
check ci-regex-lines   "--regex -i"                 "--heading -n -i"             'alpha \w+'
check ci-regex-only    "--regex -i -o"              "--heading -n -i -o"          'ALPHA \w+'

# -i x -U: the multiline test runs on the FOLDED pattern, so a term reaches
# rg's multiline core under -i exactly where it does without it.
for pat in 'alpha one\nbeta' 'ALPHA ONE\nBETA' '\nBeta' 'alpha\z' 'PRINTK' '(?s)alpha.*\z'; do
    check "ci-multi-lines [$pat]" "--regex -i -U"                 "--heading -n -i -U"      "$pat"
    check "ci-multi-only  [$pat]" "--regex -i -U -o"              "--heading -n -i -U -o"   "$pat"
    check "ci-multi-count [$pat]" "--regex -i -U -c"              "-i -U -c"                "$pat"
    check "ci-multi-countm[$pat]" "--regex -i -U --count-matches" "-i -U --count-matches"   "$pat"
    check "ci-multi-files [$pat]" "--regex -i -U -l"              "-i -U -l"                "$pat"
done
# A bare term holding a terminator: -U lifts the refusal, -i folds it.
check ci-multi-bare    "-i -U"                      "-i -U -F --heading -n"       'ALPHA ONE
beta'

# A leading BOM never shows in the text, the columns or the counts.
for pat in '^alpha' 'alpha' '^' '$'; do
    check "bom-lines      [$pat]" "--regex"                 "--heading -n"     "$pat"
    check "bom-only       [$pat]" "--regex -o"              "--heading -n -o"  "$pat"
    check "bom-count      [$pat]" "--regex -c"              "-c"               "$pat"
    check "bom-countm     [$pat]" "--regex --count-matches" "--count-matches"  "$pat"
    check "bom-files      [$pat]" "--regex -l"              "-l"               "$pat"
done

# Zero-width matches, including the unterminated last line of d.txt.
for pat in '$' 'a*$' '^' 'x*' 'x$'; do
    check "eol-lines      [$pat]" "--regex"                 "--heading -n"     "$pat"
    check "eol-only       [$pat]" "--regex -o"              "--heading -n -o"  "$pat"
    check "eol-count      [$pat]" "--regex -c"              "-c"               "$pat"
    check "eol-countm     [$pat]" "--regex --count-matches" "--count-matches"  "$pat"
    check "eol-files      [$pat]" "--regex -l"              "-l"               "$pat"
    check "eol-context    [$pat]" "--regex -C 1"            "--heading -n -C 1" "$pat"
done

# -U/--multiline. rg only searches whole documents when the pattern can hold
# a terminator (a \n in a literal or class, or any anchor); every other term
# keeps the line scan even under -U, so both paths are checked here.
for pat in 'alpha one\nbeta' '\nbeta' 'head\nalpha last' 'alpha last\z' 'alpha[^x]*\nzz' 'alpha 1\n' 'a\n?' '(?s)$' '(?s)alpha.*\z'; do
    check "multi-lines    [$pat]" "--regex -U"                 "--heading -n -U"       "$pat"
    check "multi-only     [$pat]" "--regex -U -o"              "--heading -n -U -o"    "$pat"
    check "multi-count    [$pat]" "--regex -U -c"              "-U -c"                 "$pat"
    check "multi-countm   [$pat]" "--regex -U --count-matches" "-U --count-matches"    "$pat"
    check "multi-files    [$pat]" "--regex -U -l"              "-U -l"                 "$pat"
    check "multi-context  [$pat]" "--regex -U -C 1"            "--heading -n -U -C 1"  "$pat"
    check "multi-color    [$pat]" "--regex -U --color always"  "--heading -n -U --color always" "$pat"
    check "multi-flat     [$pat]" "--regex -U --no-heading -N" "--no-heading -N -U"    "$pat"
done

# The same terms under -U that rg keeps on its line scan, plus the anchored
# ones, which take the multiline path and merge adjacent matched lines.
for pat in 'alpha' '^alpha' 'alpha$' 'x*' '$'; do
    check "multi-line-core[$pat]" "--regex -U"                 "--heading -n -U"       "$pat"
    check "multi-lc-only  [$pat]" "--regex -U -o"              "--heading -n -U -o"    "$pat"
    check "multi-lc-count [$pat]" "--regex -U -c"              "-U -c"                 "$pat"
    check "multi-lc-countm[$pat]" "--regex -U --count-matches" "-U --count-matches"    "$pat"
    check "multi-lc-ctx   [$pat]" "--regex -U -C 1"            "--heading -n -U -C 1"  "$pat"
done

# Anchorless / short-anchor patterns: the automaton candidate route.
for pat in '[a-z]+ha' '(alp|bet)a' 'al[a-z]{3}'; do
    check "auto-lines     [$pat]" "--regex"                 "--heading -n"     "$pat"
    check "auto-only      [$pat]" "--regex -o"              "--heading -n -o"  "$pat"
    check "auto-count     [$pat]" "--regex -c"              "-c"               "$pat"
    check "auto-countm    [$pat]" "--regex --count-matches" "--count-matches"  "$pat"
    check "auto-files     [$pat]" "--regex -l"              "-l"               "$pat"
done
# The same route under -U, where the plan follows the multiline flags.
check auto-multi       "--regex -U"    "--heading -n -U"   '[a-z]+ 1\n[a-z]+ 2'
check auto-multi-only  "--regex -U -o" "--heading -n -U -o" '[a-z]+ 1\n[a-z]+ 2'

# --multiline-dotall: `.` spans the terminator only with it.
for pat in 'one.beta' 'alpha.*zz'; do
    check "dotall-lines   [$pat]" "--regex -U --multiline-dotall" "--heading -n -U --multiline-dotall" "$pat"
    check "dotall-only    [$pat]" "--regex -U --multiline-dotall -o" "--heading -n -U --multiline-dotall -o" "$pat"
    check "dotall-off     [$pat]" "--regex -U"                    "--heading -n -U"    "$pat"
done

# Expressions: AND, OR, nesting, regex leaves, an empty intersection.
expr_cases "alpha AND beta"                'and{' t:alpha t:beta '}'
expr_cases "alpha OR printk"               'or{' t:alpha t:printk '}'
expr_cases "alpha AND /\w+ one/"           'and{' t:alpha 'r:\w+ one' '}'
expr_cases "(alpha AND beta) OR head"      'or{' 'and{' t:alpha t:beta '}' t:head '}'
expr_cases "alpha AND (beta OR /^z+$/)"    'and{' t:alpha 'or{' t:beta 'r:^z+$' '}' '}'
expr_cases "zeta AND first"                'and{' t:zeta t:first '}'
expr_cases "bom AND /crlf$/ OR sub"        'or{' 'and{' t:bom 'r:crlf$' '}' t:sub '}'
check_expr "expr-ci-lines  [ALPHA AND one]"      "-i"    "--heading -n -i" 'and{' t:ALPHA t:one '}'
check_expr "expr-ci-files  [ALPHA AND one]"      "-i -l" "-l -i"           'and{' t:ALPHA t:one '}'
check_expr "expr-ci-count  [printk OR /KELVIN/]" "-i -c" "-c -i"           'or{' t:printk r:KELVIN '}'
check_expr "expr-multi     [/alpha one\nbeta/ OR zz]" "-U" "--heading -n -U" 'or{' 'r:alpha one\nbeta' t:zz '}'
check_expr "expr-multi-files [/alpha 1\n/ AND 3]"     "-U -l" "-U -l"      'and{' 'r:alpha 1\n' t:3 '}'

# --and / --or: the same truth through the term flags.
flag_cases "--and alpha beta"               ""        and t:alpha t:beta
flag_cases "--or alpha printk"              ""        or t:alpha t:printk
flag_cases "--and alpha /\w+ one/"          ""        and t:alpha 'r:\w+ one'
flag_cases "--or alpha beta head"           ""        or t:alpha t:beta t:head
flag_cases "--and zeta first"               ""        and t:zeta t:first
flag_cases "--regex --and 'alpha \w+' beta" "--regex" and 'r:alpha \w+' r:beta
check_flags "flags-ci-lines [--and ALPHA one]"          "-i"    "--heading -n -i" and t:ALPHA t:one
check_flags "flags-ci-files [--and ALPHA one]"          "-i -l" "-l -i"           and t:ALPHA t:one
check_flags "flags-multi    [--or /alpha one\nbeta/ zz]" "-U"    "--heading -n -U" or 'r:alpha one\nbeta' t:zz

# -g, --iglob and PATHs; globs are relative to the root, PATHs to sub/.
filtered "glob-txt      " "-g *.txt"                  "-g *.txt"                  "" . tb
filtered "glob-not-sub  " "-g !sub"                   "-g !sub"                   "" . tb
filtered "glob-sub-tree " "-g sub/**"                 "-g sub/**"
filtered "glob-anchored " "-g /*.txt"                 "-g /*.txt"                 "" . tb
filtered "glob-last-wins" "-g !*.txt -g a.txt"        "-g !*.txt -g a.txt"
filtered "glob-none     " "-g a.txt -g !*.txt"        "-g a.txt -g !*.txt"        "" ""
filtered "iglob         " "--iglob H.TXT"             "--iglob H.TXT"
filtered "glob-ci       " "--glob-case-insensitive -g SUB/**" "--glob-case-insensitive -g SUB/**"
filtered "iglob-last    " "--iglob *.TXT -g !*.txt"   "--iglob *.TXT -g !*.txt"   "" . tb
filtered "glob-dir-only " "-g !mw/"                   "-g !mw/"                   "" . tb
filtered "path-dot      " ""                          ""                          "."                 sub
filtered "path-files    " ""                          ""                          "../a.txt ../h.txt" "a.txt h.txt"
filtered "path-up       " ""                          ""                          ".."                .   tb
filtered "path-glob-dir " "-g !sub"                   "-g !sub"                   "."                 sub
filtered "path-file-glob" "-g !*.txt"                 "-g !*.txt"                 "../a.txt ../b.txt" "a.txt b.txt"
}

# Under --serve, wait until the server's watch agrees with a local search.
await_watch() {
    [[ -n "$ADDR" ]] || return 0
    local deadline=$((SECONDS + 10))
    until [[ "$( cd "$HERE" && "$BIN" --server "$ADDR" -c -- alpha)" == "$( cd "$HERE" && "$BIN" -c -- alpha 2> /dev/null)" ]]; do
        if ((SECONDS >= deadline)); then
            echo "note: the watch missed the edits in 10 s; restarting serve"
            stop_server
            start_server
            return 0
        fi
        sleep 0.1
    done
}

cases
if [[ "$STORE_DOCS" == false ]]; then
    PASS="[stale] "
    WARNINGS=3
    printf 'alpha new\nbeta\nalpha two alpha\nalpha\nzeta\n' > "$CORPUS/a.txt"
    printf 'no hits here\nbut alpha now\n' > "$CORPUS/b.txt"
    rm -f "$CORPUS/d.txt"
    # Added files: at the root, in a new directory, one binary (skipped;
    # rg runs with -a, so it is excluded there), and one deleted again.
    printf 'alpha added\nbeta\nalpha and alpha\n' > "$CORPUS/ab.txt"
    mkdir -p "$CORPUS/new/deep"
    printf 'first\nalpha deep\n' > "$CORPUS/new/deep/n.txt"
    printf 'alpha\0binary\n' > "$CORPUS/bin.dat"
    printf 'alpha gone\n' > "$CORPUS/gone.txt"
    # tb.txt turned binary: excluded, warned.
    printf 'alpha new sub\n' > "$CORPUS/sub/new.txt"
    cp -Rp "$WORK/tree/t" "$CORPUS/sub/t"
    printf 'alpha now text\n' > "$CORPUS/kb/k.tmp" && mv -f "$CORPUS/kb/k.tmp" "$CORPUS/kb/k.dat"
    printf 'alpha\0now binary\n' > "$CORPUS/tb.txt"
    mv "$CORPUS/mw" "$CORPUS/mv2"
    RG_SKIP=(--iglob '!bin.dat' --iglob '!tb.txt')
    await_watch
    rm -f "$CORPUS/gone.txt"
    await_watch
    cases
fi

if [[ "$SERVE" == true ]]; then
    echo "pass $OK / fail $FAILED / served $SERVED"
else
    echo "pass $OK / fail $FAILED"
fi
exit "$FAIL"

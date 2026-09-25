#!/usr/bin/env bash
#
# scripts/rg-oracle.sh: byte-for-byte diff of fmgrep's human output against rg.
#
# Stages a fixture tree, indexes it into its .fmgrep, then compares our stdout
# (searching from a subdirectory, so the index is found by walking up) with
# rg's for every output flag combination. Sibling names are chosen so rg's
# per-directory DFS (`--sort path`) and our global sort of relative paths
# agree; a file and a sibling directory sharing a prefix (`a.txt` vs
# `a/b.txt`) would order differently and is out of scope here.
#
# Without --store-docs every case runs twice: on the indexed tree, then on a
# stale one (files edited and deleted after index), where each search must
# print exactly one warning, for the deleted file. With --store-docs the index
# holds the documents, so there is no stale pass.
#
# Needs `rg` on PATH and builds the release binary, so it is not part of
# scripts/check.sh. Ends with a `pass N / fail M` line; exits 1 if any case
# differs, 2 on a usage error or an index whose manifest contradicts the flag.
#
# Usage: scripts/rg-oracle.sh [--store-docs]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v rg >/dev/null || { echo "rg is not on PATH"; exit 2; }

STORE_DOCS=false
INDEX_FLAGS=()
for arg in "$@"; do
    case "$arg" in
    --store-docs) STORE_DOCS=true; INDEX_FLAGS=(--store-docs) ;;
    *) echo "usage: scripts/rg-oracle.sh [--store-docs]" >&2; exit 2 ;;
    esac
done

cargo build --release -p fmgrep >&2
BIN="$ROOT/target/release/fmgrep"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
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

( cd "$CORPUS" && "$BIN" index ${INDEX_FLAGS[@]+"${INDEX_FLAGS[@]}"} ) >&2
if ! grep -q "\"store_docs\": *$STORE_DOCS[,}]" "$CORPUS/.fmgrep/manifest.json"; then
    echo "the index manifest does not record store_docs=$STORE_DOCS" >&2
    exit 2
fi
# Our searches run below the corpus root and find the index by walking up.
HERE="$CORPUS/sub"

RG_COMMON=(--no-ignore --no-require-git -a --sort path)
TERM=alpha
FAIL=0
PASS=""
WARNINGS=0
OK=0
FAILED=0

# One case: name, our flags, rg's flags (both space-separated), term.
check() {
    local name="$1" mine="$2" theirs="$3" term="${4:-$TERM}"
    # shellcheck disable=SC2086
    ( cd "$HERE" && "$BIN" $mine -- "$term" ) > "$WORK/ours" 2> "$WORK/err"
    set +e
    # rg echoes the directory it was given; our paths are corpus-relative.
    # shellcheck disable=SC2086
    ( cd "$CORPUS" && rg "${RG_COMMON[@]}" $theirs -- "$term" . ) | sed 's|\./||g' > "$WORK/theirs"
    set -e
    if [[ "$(grep -c '^warning:' "$WORK/err")" != "$WARNINGS" ]]; then
        echo "FAIL $PASS$name  (want $WARNINGS warning line(s))"
        cat "$WORK/err"
        FAIL=1
        FAILED=$((FAILED + 1))
    elif diff -u "$WORK/theirs" "$WORK/ours" > "$WORK/diff"; then
        echo "ok   $PASS$name"
        OK=$((OK + 1))
    else
        echo "FAIL $PASS$name  (ours: $mine | rg: $theirs)"
        cat "$WORK/diff"
        FAIL=1
        FAILED=$((FAILED + 1))
    fi
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
        set +e
        # shellcheck disable=SC2086
        ( cd "$CORPUS" && rg "${RG_COMMON[@]}" $MFLAGS -l -e "$pat" . ) | sed 's|^\./||' | LC_ALL=C sort > "$out"
        set -e
        ;;
    *) echo "bad expression token $tok" >&2; exit 2 ;;
    esac
}

# One expression case: name, our flags, rg's flags, then the tokens. Runs
# --expr-json, then --expr as its own `expr-text-*` check; OURS instead if set.
check_expr() {
    local name="$1" mine="$2" theirs="$3" w form
    shift 3
    TOK=("$@")
    POS=0
    JSON=""
    TEXT=""
    RGE=()
    MFLAGS=""
    for w in $theirs; do
        case "$w" in -i | -U | --multiline-dotall) MFLAGS+=" $w" ;; esac
    done
    node "$WORK/members"
    : > "$WORK/theirs"
    if [[ -s "$WORK/members" ]]; then
        local files=()
        while IFS= read -r w; do files+=("$w"); done < "$WORK/members"
        set +e
        # shellcheck disable=SC2086
        ( cd "$CORPUS" && rg "${RG_COMMON[@]}" -H $theirs "${RGE[@]}" -- "${files[@]}" ) > "$WORK/theirs"
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
        # shellcheck disable=SC2086
        ( cd "$HERE" && "$BIN" $mine "${argv[@]}" ) > "$WORK/ours" 2> "$WORK/err"
        if [[ "$(grep -c '^warning:' "$WORK/err")" != "$WARNINGS" ]]; then
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
    done
    OURS=()
}

# `--and` / `--or` over the leaves as terms: `r:x` spelled /x/, or x under
# --regex. The truth is check_expr's for the same node.
check_flags() {
    local name="$1" mine="$2" theirs="$3" op="$4" tok
    shift 4
    OURS=("--$op" --)
    for tok in "$@"; do
        case "$tok" in
        t:*) OURS+=("${tok#t:}") ;;
        r:*) if [[ " $mine " == *" --regex "* ]]; then OURS+=("${tok#r:}"); else OURS+=("/${tok#r:}/"); fi ;;
        esac
    done
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
}

cases
if [[ "$STORE_DOCS" == false ]]; then
    PASS="[stale] "
    WARNINGS=1
    printf 'alpha new\nbeta\nalpha two alpha\nalpha\nzeta\n' > "$CORPUS/a.txt"
    printf 'no hits here\nbut alpha now\n' > "$CORPUS/b.txt"
    rm -f "$CORPUS/d.txt"
    cases
fi

echo "pass $OK / fail $FAILED"
exit "$FAIL"

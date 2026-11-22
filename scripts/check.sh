#!/usr/bin/env bash
# The CI gate, run locally: formatting, lints, tests.
#
#   scripts/check.sh         test the crates this branch changes, and the
#                            crates that depend on them
#   scripts/check.sh --all   test every crate (what CI runs)
#
# Format and lints always cover the whole workspace; they take seconds
# once built. Tests take minutes, so a local run skips the crates that
# cannot have changed. The changes are those since the base, committed or
# not, plus untracked files. The base is the nearest commit below HEAD among
# the merge base with origin/main, the tips of the local pr/* branches and
# the refs under refs/jobs/. In a stack of pull requests that is the one
# below this branch. A ref at HEAD itself is not below it.
#
# cargo-test-changed (a cargo subcommand that tests the changed crates and
# their dependents) would do the selection, but not the rest: files outside
# the crates select bench, whose tests read them; bench needs the fmgrep
# binary built; a workspace-wide file tests everything. It is also one more
# tool to install. So the selection is done here, with git and cargo tree.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

all=0
case "${1:-}" in
  "") ;;
  --all) all=1 ;;
  *) echo "usage: scripts/check.sh [--all]" >&2; exit 2 ;;
esac

# Prints "name dir" for each workspace member.
members() {
  cargo tree --workspace --depth 0 --prefix none --locked |
    sed -n 's/^\([^ ]*\) v[^ ]* (\(.*\))$/\1 \2/p' |
    while read -r name path; do echo "$name ${path##*/}"; done
}

# Prints why every crate is tested, or else the packages to test, as "-p a -p b".
select_packages() {
  local base changed path dir name found sel="" line deps
  if ! git rev-parse --verify -q origin/main >/dev/null; then
    echo "all crates: no origin/main to compare with"; return
  fi
  if ! base=$(git merge-base HEAD origin/main 2>/dev/null); then
    echo "all crates: no merge base with origin/main"; return
  fi
  # In a stack, a nearer base: the closest proper ancestor among the pr/*
  # branches and the jobs. Ties keep the merge base.
  local label=origin/main head sha ref n best
  head=$(git rev-parse HEAD)
  best=$(git rev-list --count "$base..HEAD")
  while read -r sha ref; do
    [ -n "$sha" ] && [ "$sha" != "$head" ] || continue
    git merge-base --is-ancestor "$sha" HEAD 2>/dev/null || continue
    n=$(git rev-list --count "$sha..HEAD")
    if [ "$n" -lt "$best" ]; then best=$n; base=$sha; label=$ref; fi
  done <<EOF
$(git for-each-ref --format='%(objectname) %(refname:short)' refs/heads/pr/ refs/jobs/)
EOF
  [ "$label" = origin/main ] || echo "check: base is $label, not origin/main" >&2
  # --no-renames lists the path a file left as well as the one it reached;
  # quotePath=false keeps non-ASCII names readable by the patterns below.
  changed=$( { git -c core.quotePath=false diff --name-only --no-renames "$base"
              git -c core.quotePath=false ls-files --others --exclude-standard; } | sort -u)
  if [ -z "$changed" ]; then
    echo "all crates: no change since $label"; return
  fi
  local mem; mem=$(members)
  local docs=0
  while IFS= read -r path; do
    case "$path" in
      Cargo.toml|Cargo.lock|*/Cargo.lock|rust-toolchain*|.cargo/*|rustfmt.toml|.rustfmt.toml|clippy.toml|.github/*|scripts/check.sh)
        echo "all crates: $path changed"; return ;;
      \"*)
        echo "all crates: cannot read the path $path"; return ;;
      crates/*/*)
        dir=${path#crates/}; dir=${dir%%/*}
        name=$(echo "$mem" | awk -v d="$dir" '$2 == d { print $1 }')
        if [ -z "$name" ]; then
          echo "all crates: $path is in no workspace member"; return
        fi
        # The member and every crate that depends on it, by any edge.
        deps=$(cargo tree --workspace --locked -i "$name" -e normal,dev,build --prefix none |
          sed -n 's/^\([^ ]*\) v[^ ]* (\/.*)\( (\*)\)\{0,1\}$/\1/p') || deps=""
        case " $(echo $deps) " in
          *" $name "*) ;;
          *) echo "all crates: cannot read the dependents of $name"; return ;;
        esac
        sel="$sel $deps" ;;
      *) docs=1 ;;
    esac
  done <<EOF
$changed
EOF
  # Files outside the crates (docs, README) are read by bench's tests.
  if [ "$docs" = 1 ]; then sel="$sel bench"; fi
  sel=$(echo "$sel" | tr ' ' '\n' | sed '/^$/d' | sort -u | tr '\n' ' ')
  echo "$sel"
}

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings

if [ "$all" = 1 ]; then
  echo "check: testing all crates (--all)"
  cargo test --workspace --locked
  exit 0
fi

sel=$(select_packages)
case "$sel" in
  "all crates"*)
    echo "check: testing $sel"
    cargo test --workspace --locked ;;
  *)
    echo "check: testing $sel(changed crates and their dependents; --all for every crate)"
    args=()
    for p in $sel; do args+=(-p "$p"); done
    # bench's tests run the fmgrep binary, which `cargo test -p bench`
    # alone does not build.
    cargo build --locked -p fmgrep
    cargo test --locked "${args[@]}" ;;
esac

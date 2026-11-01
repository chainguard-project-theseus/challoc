#!/bin/sh
# tests/ctest-cc-race.sh [N]: N (default 16) concurrent
# `sh scripts/ctest-cc.sh --quantum` calls on a cold quantum cache, as the
# parallel C-program tests make them. Every call must exit 0 and print the
# same quantum, and no per-process cache file may be left beside the
# cache. Needs the release library (cargo build --release); honours
# CHALLOC_LIB and CARGO_TARGET_DIR like ctest-cc.sh. QUANTUM is unset so
# that every call probes the library.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
LIB=${CHALLOC_LIB:-${CARGO_TARGET_DIR:-$ROOT/target}/release/libchalloc.so}
[ -f "$LIB" ] || { echo "ctest-cc-race: $LIB missing: cargo build --release first" >&2; exit 2; }
n=${1:-16}
cache=$(dirname "$LIB")/ctest-quantum
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
rm -f "$cache"
i=0
while [ "$i" -lt "$n" ]; do
  i=$((i + 1))
  (
    rc=0
    unset QUANTUM; sh "$ROOT/scripts/ctest-cc.sh" --quantum >"$out/$i" 2>"$out/$i.err" || rc=$?
    echo "$rc" >"$out/$i.rc"
  ) &
done
wait
fail=0
want=$(cat "$out/1")
i=0
while [ "$i" -lt "$n" ]; do
  i=$((i + 1))
  rc=$(cat "$out/$i.rc")
  got=$(cat "$out/$i")
  if [ "$rc" != 0 ] || [ "$got" != "$want" ] || [ -s "$out/$i.err" ]; then
    echo "ctest-cc-race: call $i: exit $rc, quantum '$got' (call 1: '$want'), stderr: $(cat "$out/$i.err")"
    fail=1
  fi
done
case $want in 8|16) ;; *) echo "ctest-cc-race: quantum '$want' is neither 8 nor 16"; fail=1 ;; esac
[ "$(cat "$cache")" = "$want" ] || { echo "ctest-cc-race: the cache holds '$(cat "$cache")', not '$want'"; fail=1; }
for f in "$cache".*; do
  [ -e "$f" ] || continue
  echo "ctest-cc-race: $f left behind"
  fail=1
done
[ "$fail" = 0 ] && echo "ctest-cc-race: $n concurrent calls agree on quantum $want"
exit "$fail"

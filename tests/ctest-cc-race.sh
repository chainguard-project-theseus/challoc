#!/bin/sh
# tests/ctest-cc-race.sh [N [ROUNDS]]: ROUNDS (default 3) rounds of N
# (default 64) concurrent `sh scripts/ctest-cc.sh --quantum` calls on a
# cold quantum cache, as the parallel C-program tests make them. Every call
# must exit 0 and print the same quantum, no per-process cache file may be
# left beside the cache, and no probe directory may be left in TMPDIR.
# Needs the release library (cargo build --release); honours CHALLOC_LIB
# and CARGO_TARGET_DIR like ctest-cc.sh. QUANTUM is unset so that every
# call probes the library.
#
# The race this guards against needs the probe's temporary directory on
# another filesystem than the cache (a cross-filesystem mv is a copy and an
# O_EXCL create, not a rename), so the callers get a private TMPDIR on a
# filesystem other than the library's: /dev/shm, else /tmp. With neither
# available the test prints SKIP and passes. The race is a matter of
# timing: against the unfixed script a round of 64 callers fails about two
# times in three, so three rounds catch it almost always.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
LIB=${CHALLOC_LIB:-${CARGO_TARGET_DIR:-$ROOT/target}/release/libchalloc.so}
[ -f "$LIB" ] || { echo "ctest-cc-race: $LIB missing: cargo build --release first" >&2; exit 2; }
n=${1:-64}
rounds=${2:-3}
libdir=$(dirname "$LIB")
cache=$libdir/ctest-quantum
tmpbase=
for d in /dev/shm /tmp; do
  [ -d "$d" ] && [ -w "$d" ] || continue
  [ "$(stat -c %d "$d")" != "$(stat -c %d "$libdir")" ] || continue
  tmpbase=$d
  break
done
if [ -z "$tmpbase" ]; then
  echo "SKIP ctest-cc-race: no writable /dev/shm or /tmp on another filesystem than $libdir"
  exit 0
fi
calltmp=$(mktemp -d "$tmpbase/ctest-cc-race.XXXXXX")
out=$(mktemp -d)
trap 'rm -rf "$out" "$calltmp"' EXIT

# round R: one round of N callers; returns 1 on any failure.
round() {
  rm -f "$cache" "$out"/*
  i=0
  while [ "$i" -lt "$n" ]; do
    i=$((i + 1))
    (
      rc=0
      unset QUANTUM
      TMPDIR=$calltmp sh "$ROOT/scripts/ctest-cc.sh" --quantum >"$out/$i" 2>"$out/$i.err" || rc=$?
      echo "$rc" >"$out/$i.rc"
    ) &
  done
  wait
  bad=0
  want=$(cat "$out/1")
  i=0
  while [ "$i" -lt "$n" ]; do
    i=$((i + 1))
    rc=$(cat "$out/$i.rc")
    got=$(cat "$out/$i")
    if [ "$rc" != 0 ] || [ "$got" != "$want" ] || [ -s "$out/$i.err" ]; then
      echo "ctest-cc-race: round $1, call $i: exit $rc, quantum '$got' (call 1: '$want'), stderr: $(cat "$out/$i.err")"
      bad=1
    fi
  done
  case $want in 8|16) ;; *) echo "ctest-cc-race: round $1: quantum '$want' is neither 8 nor 16"; bad=1 ;; esac
  [ "$(cat "$cache" 2>/dev/null)" = "$want" ] || { echo "ctest-cc-race: round $1: the cache does not hold '$want'"; bad=1; }
  for f in "$cache".*; do
    [ -e "$f" ] || continue
    echo "ctest-cc-race: round $1: $f left behind"
    bad=1
  done
  for f in "$calltmp"/* "$calltmp"/.[!.]*; do
    [ -e "$f" ] || continue
    echo "ctest-cc-race: round $1: probe directory $f left behind"
    rm -rf "$f"
    bad=1
  done
  return "$bad"
}

fail=0
r=0
while [ "$r" -lt "$rounds" ]; do
  r=$((r + 1))
  round "$r" || fail=1
done
[ "$fail" = 0 ] && echo "ctest-cc-race: $rounds rounds of $n concurrent calls agree on quantum $want (probes in $tmpbase)"
exit "$fail"

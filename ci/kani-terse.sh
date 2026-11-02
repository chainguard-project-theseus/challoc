#!/bin/sh
# ci/kani-terse.sh: every Kani harness but those KANI_SKIP names, in
# terse format four at a time (the kani check, presubmit).
#
# KANI_SKIP is a space-separated list of full harness paths that run in
# a check of their own instead (kani-fmt, kani.yaml), so that the slowest
# harness does not set every Kani pull request's time to green. Each
# name must be a harness `cargo kani list` reports: a renamed or deleted
# one fails here until kani.yaml follows. kani-ignoring still runs every
# harness, these included, after each landing.
#
#   QUANTUM=16 KANI_SKIP=kani_proofs::misc::formatter_write_fmt sh ci/kani-terse.sh
#
# Exit 0: every harness run verified; 1: one failed, or a KANI_SKIP name
# is not a harness.
set -eu
set -- -Z unstable-options -Z stubbing -Z mem-predicates --ignore-global-asm \
  --no-default-features --features "quantum${QUANTUM:?}"
out=target/kani-terse
mkdir -p "$out"
cargo kani "$@" list -Z stubbing -Z mem-predicates --format json >/dev/null
# Every harness name in kani-list.json, read as ci/kani-regular.sh reads it.
sed -n 's/^ *"\([A-Za-z0-9_:]*\)",\{0,1\}$/\1/p' kani-list.json | sort -u >"$out/harnesses.txt"
total=$(sed -n '/"totals"/,/}/s/^ *"\(standard\|contract\)-harnesses": *\([0-9]*\).*/\2/p' kani-list.json | awk '{ s += $1 } END { print s + 0 }')
if [ "$(wc -l <"$out/harnesses.txt")" -ne "$total" ]; then
  echo "kani-list.json lists $total harnesses, read $(wc -l <"$out/harnesses.txt")" >&2
  exit 1
fi
: >"$out/skip.txt"
for h in ${KANI_SKIP:-}; do
  grep -qxF "$h" "$out/harnesses.txt" || { echo "KANI_SKIP: $h is not a harness" >&2; exit 1; }
  echo "$h" >>"$out/skip.txt"
done
if [ -s "$out/skip.txt" ]; then
  grep -vxF -f "$out/skip.txt" "$out/harnesses.txt" >"$out/run.txt" || true
else
  cp "$out/harnesses.txt" "$out/run.txt"
fi
[ -s "$out/run.txt" ] || { echo "no harness left to run" >&2; exit 1; }
echo "$(wc -l <"$out/run.txt") of $total harnesses at quantum $QUANTUM; skipped: ${KANI_SKIP:-none}"
while read -r h; do set -- "$@" --harness "$h"; done <"$out/run.txt"
exec cargo kani "$@" --exact --output-format terse -j 4

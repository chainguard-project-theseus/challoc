#!/bin/sh
# ci/kani-regular-sharded.sh QUANTUM SHARDS: `make kani-regular` at
# QUANTUM split into SHARDS shards by ci/kani-weights.txt, all of them at
# once (the Makefile's kani-regular-sharded, which says why). Run it from
# the repository root.
#
# Each shard runs in its own copy of the tree under
# target/kani-shards/qQUANTUM/, since the kit's kani.sh keeps its state in
# ./target/kani and `cargo kani list` writes ./kani-list.json. Each
# shard's output is printed in shard order, once it and the shards before
# it have finished. It passes only when every shard started and passed
# and the shards' harnesses add up to the total their kani.sh listed, so
# a split that drops a harness cannot pass. It fails when a shard fails
# (kani.sh fails a shard with no harnesses, a harness that does not
# verify and a `warning: ignoring` line), when SHARDS is not a whole
# number from 1 to 999, and when a copy of the tree fails. A passing
# shard's copy is removed; a failing one's is kept, with its log.
# ci/kani-regular-sharded-test.sh checks it with a stand-in make.
set -eu

[ "$#" -eq 2 ] || { echo "usage: kani-regular-sharded.sh QUANTUM SHARDS" >&2; exit 2; }
quantum=$1
shards=$2
make=${MAKE:-make}

case "$shards" in
  '' | *[!0-9]* | 0* | ????*)
    echo "kani-regular-sharded: SHARDS is a whole number from 1 to 999, not '$shards'" >&2
    exit 2 ;;
esac

root=target/kani-shards/q$quantum
rm -rf "$root"
mkdir -p "$root"

echo "kani-regular-sharded: quantum $quantum, $shards shards at once"
pids=
started=0
rc=0
s=1
while [ "$s" -le "$shards" ]; do
  mkdir -p "$root/$s"
  if ! tar cf - --exclude=./target --exclude=./.git . | tar xf - -C "$root/$s"; then
    echo "kani-regular-sharded: copying the tree for shard $s failed" >&2
    rc=1
    break
  fi
  (
    cd "$root/$s"
    "$make" --no-print-directory kani-regular QUANTUM="$quantum" \
      KANI_SHARD="$s" KANI_SHARDS="$shards" KANI_WEIGHTS=ci/kani-weights.txt
  ) > "$root/$s.log" 2>&1 &
  pids="$pids $!"
  started=$((started + 1))
  s=$((s + 1))
done

ran=0
total=
s=0
for p in $pids; do
  s=$((s + 1))
  if wait "$p"; then r=passed; else r=FAILED; rc=1; fi
  echo "=== shard $s of $shards, quantum $quantum: $r"
  cat "$root/$s.log"
  # kani.sh: "kani: shard S of N: K of T harnesses".
  line=$(grep -E '^kani: shard [0-9]+ of [0-9]+: [0-9]+ of [0-9]+ harnesses$' "$root/$s.log" | head -n 1 || true)
  if [ -z "$line" ]; then
    echo "kani-regular-sharded: shard $s printed no 'kani: shard' line" >&2
    rc=1
  else
    k=$(echo "$line" | awk '{ print $6 }')
    t=$(echo "$line" | awk '{ print $8 }')
    ran=$((ran + k))
    if [ -z "$total" ]; then
      total=$t
    elif [ "$t" -ne "$total" ]; then
      echo "kani-regular-sharded: shard $s listed $t harnesses, an earlier shard $total" >&2
      rc=1
    fi
  fi
  [ "$r" = FAILED ] || rm -rf "${root:?}/$s"
done

# The pass is stated as a positive, so a test that errors (an operand out
# of the shell's range) fails the run rather than falling through.
if [ "$rc" -eq 0 ] && [ "$started" -eq "$shards" ] && [ -n "$total" ] && [ "$ran" -eq "$total" ]; then
  echo "kani-regular-sharded: quantum $quantum, $ran of $total harnesses in $shards shards"
  exit 0
fi
echo "kani-regular-sharded: started $started of $shards shards, ran $ran harnesses of the ${total:-?} listed" >&2
exit 1

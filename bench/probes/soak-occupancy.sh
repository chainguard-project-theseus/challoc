#!/bin/sh
# Runs inside the dev container: the Valkey soak plus a sidecar that dumps
# MEMORY MALLOC-STATS (per-class slab occupancy) every MS_INTERVAL seconds.
set -u
cd /src
export DURATION=${DURATION:-600} SETTLE=${SETTLE:-60} INTERVAL=${INTERVAL:-10}
export RESULT_DIR=${RESULT_DIR:-/src/target/valkey/soak-results}; export ALLOCS=${ALLOCS:-"challoc jemalloc"}
export OUT=/src/target/valkey/soak
MS_INTERVAL=${MS_INTERVAL:-20}
PORT=6399
CLI=/src/target/valkey/src-9.1.2/src/valkey-cli
mkdir -p "$RESULT_DIR"
sh bench/valkey-soak.sh > "$RESULT_DIR/soak.log" 2>&1 &
SOAK=$!
T0=$(date +%s)
while kill -0 $SOAK 2>/dev/null; do
  sleep "$MS_INTERVAL"
  alloc=$($CLI -p $PORT info memory 2>/dev/null | tr -d '\r' | awk -F: '$1=="mem_allocator"{print $2}')
  [ -n "$alloc" ] || continue
  case $alloc in *challoc*) a=challoc;; *) a=jemalloc;; esac
  d="$RESULT_DIR/mstats-$a"; mkdir -p "$d"
  t=$(( $(date +%s) - T0 ))
  { echo "# t=$t"; $CLI -p $PORT info memory | tr -d '\r' | grep -E '^(used_memory|used_memory_rss|allocator_allocated|allocator_active|allocator_resident|allocator_frag_ratio|active_defrag_hits):'; $CLI -p $PORT memory malloc-stats; } > "$d/$(printf %05d $t).txt" 2>&1
done
wait $SOAK
echo "soak exit $?"
tail -60 "$RESULT_DIR/soak.log"

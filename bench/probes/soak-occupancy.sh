#!/bin/sh
# Runs inside the dev container: the Valkey soak with the per-class occupancy
# sidecar. The sidecar now lives in bench/valkey-soak.sh behind OCCUPANCY=1
# (MEMORY MALLOC-STATS every MS_INTERVAL seconds into
# RESULT_DIR/<date>-<arch>-valkey-soak-<alloc>-mstats/); this wrapper keeps
# the old entry point and defaults (600 s, results under target/).
# `make soak-quick` is the Makefile entry for the same thing against the
# cached jemalloc soak.
set -u
cd /src
export DURATION=${DURATION:-600} SETTLE=${SETTLE:-60} INTERVAL=${INTERVAL:-10}
export RESULT_DIR=${RESULT_DIR:-/src/target/valkey/soak-results} ALLOCS=${ALLOCS:-"challoc jemalloc"}
export OUT=/src/target/valkey/soak OCCUPANCY=1 MS_INTERVAL=${MS_INTERVAL:-20}
mkdir -p "$RESULT_DIR"
sh bench/valkey-soak.sh > "$RESULT_DIR/soak.log" 2>&1
echo "soak exit $?"
tail -60 "$RESULT_DIR/soak.log"

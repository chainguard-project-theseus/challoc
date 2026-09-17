#!/bin/sh
# Cached baselines for the two benchmark tiers (bench/README.md): the
# protocol rows under the stock allocators, measured once per machine with
# bench/ab.sh and stored under target/baselines/<arch>-<bench-build-hash>.md.
# `make baseline` inside the dev container, or `challoc-bench run arm64 'make
# baseline'` on a VM. The hash covers the benchmark program versions, the
# kernel and the CPU count, so the file is only ever reused on the machine
# that produced it (target/ is a per-machine volume anyway).
#
# bench/quick.sh and bench/ab.sh read it with BASELINES=cached and verify it
# with one jemalloc sanity round each time (3% tolerance); acceptance runs do
# not use it. Rerun this after bench/build.sh changes, a kernel update, or a
# stale verdict.
#
#   ROUNDS=5  BENCHES=<ab.sh default>  ALLOCS="jemalloc glibc mimalloc"
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
. "$ROOT/bench/lib.sh"
ensure_bench_programs
mkdir -p "$BASELINE_DIR"
RESULT=$(baseline_file)
echo "== baseline for $ARCH, bench build $(bench_build_hash) -> $RESULT"
RESULT=$RESULT VARIANTS=${ALLOCS:-"jemalloc glibc mimalloc"} ROUNDS=${ROUNDS:-5} BASELINES= TITLE="cached baseline" \
  sh "$ROOT/bench/ab.sh"
echo "== baseline written; quick.sh/ab.sh use it with BASELINES=cached"

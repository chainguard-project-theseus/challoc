#!/usr/bin/env bash
# quick-perf.sh: the PERF=1 half of bench/quick.sh for the GCE VMs, where
# perf lives on the host and the rows run in the dev container. Run on the
# VM from ~/challoc (bench/gce/challoc-bench quick-perf <vm> does this after
# `make quick QUICK_BUILD_ONLY=1`, which leaves the old and new libraries in
# target/quick/{old,new}/release/):
#
#   bash bench/gce/quick-perf.sh arm64|amd64 [ROWS="larson1 valkey1"] [VARIANTS="old new jemalloc"]
#                                            [EVENTS=cycles,instructions,L1-dcache-load-misses,dTLB-load-misses]
#                                            [OUTDIR=~/logs/quick-perf]
#
# For every (row, variant) the container starts detached with `sleep 1; exec
# env LD_PRELOAD=<lib> <row command>`, `sudo perf stat -x, --inherit -p` is
# attached to the container's init pid before the exec (the recipe of
# bench/results/2026-09-17-gce-review-counters.md), the container is waited
# for, perf is stopped by killing its sleep child, and the counters are
# divided by the operation count the program printed. The output is the same
# per-operation table quick.sh prints with PERF=1 in a container that has perf.
set -euo pipefail
T=${1:?arm64|amd64}; shift || true
for kv in "$@"; do case $kv in *=*) export "$kv";; *) echo "quick-perf: bad argument $kv" >&2; exit 2;; esac; done
ROWS=${ROWS:-"larson1 valkey1"}
VARIANTS=${VARIANTS:-"old new jemalloc"}
EVENTS=${EVENTS:-cycles,instructions,L1-dcache-load-misses,dTLB-load-misses}
OUTDIR=${OUTDIR:-$HOME/logs/quick-perf}
IMAGE=challoc-dev:$T
REPO=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$OUTDIR"

drun() { docker run --rm -v "$REPO":/src -w /src -v "challoc-target-$T":/src/target "$IMAGE" "$@"; }
# Row command lines and the probes come from bench/lib.sh inside the container.
in_lib() { drun sh -c "ROOT=/src; . /src/bench/lib.sh; $1"; }
lib_of() {
  case $1 in
    old|new) echo "/src/target/quick/$1/release/libchalloc.so";;
    jemalloc) echo /usr/lib/libjemalloc.so.2;;
    glibc) echo "";;
    mimalloc) echo /src/target/bench-bin/libmimalloc.so;;
    *) echo "quick-perf: unknown variant $1" >&2; exit 2;;
  esac
}

in_lib "ensure_probe valkey_model; ensure_probe churn" >/dev/null
for v in $VARIANTS; do
  case $v in old|new) in_lib "test -f $(lib_of "$v")" || { echo "quick-perf: no $(lib_of "$v") in the target volume; run 'make quick QUICK_BUILD_ONLY=1' first" >&2; exit 1; };; esac
done

echo "== quick-perf on $(hostname) ($(uname -m)), load $(cut -d' ' -f1-3 /proc/loadavg), events $EVENTS, rows: $ROWS, variants: $VARIANTS"
echo
echo "| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |"
echo "|---|---|---:|---:|---:|---:|---:|"
for b in $ROWS; do
  cmd=$(in_lib "bench_args $b")
  secs=$(in_lib "secs_of $b")
  for v in $VARIANTS; do
    tag=$b.$v
    cid=$(docker run -d -v "$REPO":/src -w /src -v "challoc-target-$T":/src/target "$IMAGE" \
          sh -c "sleep 1; exec env LD_PRELOAD=$(lib_of "$v") $cmd")
    pid=
    for _ in $(seq 1 50); do pid=$(docker inspect -f '{{.State.Pid}}' "$cid" 2>/dev/null || true); [ -n "$pid" ] && [ "$pid" != 0 ] && break; sleep 0.1; done
    [ -n "$pid" ] && [ "$pid" != 0 ] || { echo "quick-perf: container for $tag did not start" >&2; docker rm -f "$cid" >/dev/null 2>&1 || true; continue; }
    # perf must be root to attach to the container's process; stopped by
    # killing its sleep child (SIGINT is swallowed without a terminal).
    # perf reports the killed sleep on stderr ("sleep: Terminated"); drop that line only.
    sudo perf stat -x, -e "$EVENTS" --inherit -p "$pid" -o "$OUTDIR/$tag.csv" -- sleep 1000000 2> >(grep -v '^sleep: Terminated' >&2) &
    pp=$!
    docker wait "$cid" >/dev/null
    sudo pkill -f '^sleep 1000000$' 2>/dev/null || true; wait $pp || true
    docker logs "$cid" > "$OUTDIR/$tag.out" 2>&1
    docker rm -f "$cid" >/dev/null 2>&1 || true
    # The same extraction as bench/lib.sh ops_of, on the host side.
    ops=$(awk -v b="$b" -v secs="$secs" '
      b ~ /^larson/ && /Throughput/ {printf "%.0f", $3*secs}
      b ~ /^xmalloc/ && /free\/sec/ {sub(",", "", $4); printf "%.0f", $4*1e6*5}
      b ~ /^valkey/ && /^ops / {for (i=2;i<=NF;i++) {split($i, p, "="); if (p[1]=="total") print p[2]}}' "$OUTDIR/$tag.out")
    awk -F, -v ops="${ops:--}" -v secs="$secs" -v row="$b" -v var="$v" '
      /^#/ || NF < 3 {next}
      {c[$3]=$1}
      END {
        mops = (ops!="-" && secs!="-" && secs>0) ? sprintf("%.2f", ops/secs/1e6) : "-"
        if (ops=="-" || ops==0) { printf "| %s | %s | %s | - | - | - | - |\n", row, var, mops; exit }
        printf "| %s | %s | %s | %.1f | %.1f | %.3f | %.3f |\n", row, var, mops, c["cycles"]/ops, c["instructions"]/ops, c["L1-dcache-load-misses"]/ops, c["dTLB-load-misses"]/ops
      }' "$OUTDIR/$tag.csv"
  done
done
echo
echo "Per-operation counters, one run each: sudo perf stat -x, -e $EVENTS --inherit -p <container init pid> (attached before the exec), divided by the program's own operation count. Raw files under $OUTDIR. Compare variants within a row; attribution, not a gate."

#!/usr/bin/env bash
# perf-record.sh: symbol shares of one bench row under one library on a GCE
# VM, the `perf record` companion of quick-perf.sh (bench/gce/README.md: perf
# lives on the VM host, the row runs in the dev container). quick-perf.sh
# gives the per-operation counters; this script says which functions the
# cycles went to, which is what CLAUDE.md's attribution rule asks for before
# a fast-path change (the change names the mechanism it attacks). Run on the
# VM from ~/challoc after `make quick QUICK_BUILD_ONLY=1`:
#
#   bash bench/gce/perf-record.sh arm64|amd64 ROW VARIANT [FREQ=4000] [TOP=30]
#        [MATCH='sort|median3|partition|pivot'] [OUTDIR=~/logs/perf-record]
#
# ROW is a bench/lib.sh row (glibc-simple, python1-malloc, larson1, ...);
# VARIANT is old or new (target/quick/{old,new}/release/libchalloc.so),
# jemalloc, mimalloc, tcmalloc or glibc, optionally with a MALLOC_CONF after
# a colon (new:tcache_compact_shift:0). The container starts detached with
# `sleep 1; exec env LD_PRELOAD=<lib> <row command>`, `sudo perf record -F
# FREQ -p <container init pid>` is attached before the exec and stopped when
# the container exits. Symbols are resolved with `perf report --symfs` over a
# tree filled by `docker cp` from the finished container: every DSO with
# samples (the paths `perf script -F dso` prints) is copied to its container path,
# so /src/target/... (the target volume) and the image's own libraries
# (libc, libpython) resolve alike. Prints the DSO table, the top TOP symbols
# and the summed share of the libchalloc symbols matching MATCH (default:
# Heap::flush's slot sort and its helpers), all as percentages of every
# sample of the process (user + kernel). Raw files: OUTDIR/ROW.VARIANT.*.
set -euo pipefail
T=${1:?arm64|amd64}; ROW=${2:?row}; V=${3:?variant}; shift 3
for kv in "$@"; do case $kv in *=*) export "$kv";; *) echo "perf-record: bad argument $kv" >&2; exit 2;; esac; done
FREQ=${FREQ:-4000}
TOP=${TOP:-30}
MATCH=${MATCH:-'sort|median3|partition|pivot'}
OUTDIR=${OUTDIR:-$HOME/logs/perf-record}
IMAGE=challoc-dev:$T
REPO=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$OUTDIR"
tag=$ROW.${V//:/_}
SYMFS=$OUTDIR/$tag.symfs
DATA=$OUTDIR/$tag.perf.data

drun() { docker run --rm -v "$REPO":/src -w /src -v "challoc-target-$T":/src/target "$IMAGE" "$@"; }
in_lib() { drun sh -c "ROOT=/src; . /src/bench/lib.sh; $1"; }
lib_of() {
  case ${1%%:*} in
    old|new) echo "/src/target/quick/${1%%:*}/release/libchalloc.so";;
    jemalloc) echo /usr/lib/libjemalloc.so.2;;
    glibc) echo "";;
    mimalloc) echo /src/target/bench-bin/libmimalloc.so;;
    tcmalloc) echo /usr/lib/libtcmalloc_minimal.so.4;;
    *) echo "perf-record: unknown variant $1" >&2; exit 2;;
  esac
}
conf_of() { case $1 in *:*) echo "${1#*:}";; esac; }

in_lib "ensure_probe valkey_model; ensure_probe churn" >/dev/null
case ${V%%:*} in old|new) in_lib "test -f $(lib_of "$V")" || { echo "perf-record: no $(lib_of "$V") in the target volume; run 'make quick QUICK_BUILD_ONLY=1' first" >&2; exit 1; };; esac
cmd=$(in_lib "bench_args $ROW")

cid=$(docker run -d -v "$REPO":/src -w /src -v "challoc-target-$T":/src/target "$IMAGE" \
      sh -c "sleep 1; exec env MALLOC_CONF=$(conf_of "$V") LD_PRELOAD=$(lib_of "$V") $cmd")
pid=
for _ in $(seq 1 50); do pid=$(docker inspect -f '{{.State.Pid}}' "$cid" 2>/dev/null || true); [ -n "$pid" ] && [ "$pid" != 0 ] && break; sleep 0.1; done
[ -n "$pid" ] && [ "$pid" != 0 ] || { echo "perf-record: container for $tag did not start" >&2; docker rm -f "$cid" >/dev/null 2>&1 || true; exit 1; }
# perf must be root to attach to the container's process; it is stopped by
# killing its sleep child (SIGINT is swallowed without a terminal). No
# build-id cache: the DSO paths are the container's and do not exist here.
sudo rm -f "$DATA"
sudo perf record -F "$FREQ" -N -p "$pid" -o "$DATA" -- sleep 1000000 2> >(grep -v '^sleep: Terminated\|^\[ perf record' >&2) &
pp=$!
docker wait "$cid" >/dev/null
sudo pkill -f '^sleep 1000000$' 2>/dev/null || true; wait $pp || true
docker logs "$cid" > "$OUTDIR/$tag.out" 2>&1

# Fill the symfs tree from the finished container: every DSO with samples at
# its container path (volumes are reachable through docker cp).
sudo rm -rf "$SYMFS"; mkdir -p "$SYMFS"
sudo perf script -i "$DATA" -F ip,dso 2>/dev/null | sed -n 's/.*(\(\/[^)]*\)).*/\1/p' | sort -u > "$OUTDIR/$tag.dsos"
while read -r dso; do
  mkdir -p "$SYMFS$(dirname "$dso")"
  docker cp -L "$cid:$dso" "$SYMFS$dso" 2>/dev/null || true
done < "$OUTDIR/$tag.dsos"
docker rm -f "$cid" >/dev/null 2>&1 || true

report() { sudo perf report -i "$DATA" --symfs "$SYMFS" --stdio -g none --percent-limit 0 "$@" 2>/dev/null; }
echo "== perf record on $(hostname) ($(uname -m)), row $ROW, variant $V, -F $FREQ, load $(cut -d' ' -f1-3 /proc/loadavg)"
report --sort dso -F overhead,dso | grep -E '^#? *(Samples|Event count|Total Lost)|^ +[0-9.]+%' | awk 'NR <= 20'
echo
echo "top $TOP symbols (share of all samples):"
report --sort dso,sym -F overhead,dso,sym > "$OUTDIR/$tag.syms"
grep -E '^ +[0-9.]+%' "$OUTDIR/$tag.syms" | sort -rn | awk -v n="$TOP" 'NR <= n'
echo
lib=$(basename "$(lib_of "$V")" 2>/dev/null || true); lib=${lib:-'(none)'}
grep -E '^ +[0-9.]+%' "$OUTDIR/$tag.syms" | awk -v lib="$lib" -v m="$MATCH" '
  $2 == lib {libsum += $1}
  $2 == lib && $0 ~ m {sum += $1; n++}
  END {printf "%s: %.2f%% of all samples; symbols matching /%s/ in it: %.2f%% (%d symbols)\n", lib, libsum, m, sum, n}'
echo "raw: $DATA, $OUTDIR/$tag.syms (perf report --sort dso,sym), $OUTDIR/$tag.out (the program's output)"

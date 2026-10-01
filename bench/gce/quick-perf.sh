#!/usr/bin/env bash
# quick-perf.sh: the PERF=1 half of bench/quick.sh for the GCE VMs, where
# perf lives on the host and the rows run in the dev container. Run on the
# VM from ~/challoc (bench/gce/challoc-bench quick-perf <vm> does this after
# `make quick-legacy QUICK_BUILD_ONLY=1`, which leaves the old and new libraries in
# target/quick/{old,new}/release/):
#
#   bash bench/gce/quick-perf.sh arm64|amd64 [ROWS="larson1 valkey1"] [VARIANTS="old new jemalloc"]
#                                            (a variant may carry a MALLOC_CONF: "new:double_free_scan:true")
#                                            [EVENTS=cycles,instructions,L1-dcache-load-misses,dTLB-load-misses]
#                                            (any further event gets its own per-operation column)
#                                            [OUTDIR=~/logs/quick-perf]
#                                            [CHALLOC_THP_COUNTERS=1] (plan Phase 2.5 task 17: the deltas of
#                                            /proc/vmstat thp_* and hugepages-<size>kB/stats/* around each
#                                            container, read on the host, as a second table)
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
CHALLOC_THP_COUNTERS=${CHALLOC_THP_COUNTERS:-0}
# The THP counter snapshot of bench/lib.sh, on the host side (the same files;
# sysfs and /proc/vmstat are the host's from either side).
thp_snapshot() {
  { grep '^thp_' /proc/vmstat 2>/dev/null
    for d in /sys/kernel/mm/transparent_hugepage/hugepages-*kB; do
      [ -d "$d/stats" ] || continue
      sz=${d##*/hugepages-}
      for f in "$d"/stats/*; do [ -r "$f" ] && echo "$sz/${f##*/} $(cat "$f")"; done
    done
  } > "$1" 2>/dev/null
}
thp_delta() { awk 'NR==FNR {b[$1]=$2; next} ($1 in b) && $2 != b[$1] {printf "%s=%d ", $1, $2 - b[$1]; n++} END {if (!n) printf "none"; print ""}' "$1" "$2"; }
thp_state() {
  local t=/sys/kernel/mm/transparent_hugepage en df sizes= d v
  en=$(sed -n 's/.*\[\(.*\)\].*/\1/p' $t/enabled 2>/dev/null); df=$(sed -n 's/.*\[\(.*\)\].*/\1/p' $t/defrag 2>/dev/null)
  for d in $t/hugepages-*kB; do [ -f "$d/enabled" ] || continue; v=$(sed -n 's/.*\[\(.*\)\].*/\1/p' "$d/enabled"); [ "$v" = never ] || sizes="$sizes ${d##*/hugepages-}=$v"; done
  echo "enabled=${en:--} defrag=${df:--}${sizes:+ per-size:$sizes}"
}
# The docker run line (REPO at /src, the target volume, IMAGE): bench/gce/drun.sh.
. "$(dirname "$0")/drun.sh"
mkdir -p "$OUTDIR"

# Row command lines and the probes come from bench/lib.sh inside the container.
in_lib() { drun sh -c "ROOT=/src; . /src/bench/lib.sh; $1"; }
# A variant is old|new|jemalloc|glibc|mimalloc|tcmalloc, or LABEL@PATH for
# any library already in the target volume (PATH as the container sees it,
# say slab16@/src/target/ab/slab16/release/libchalloc.so after `make ab`
# built it); either form may carry a MALLOC_CONF after a colon.
lib_of() {
  local n=${1%%:*}
  case $n in
    *@*) echo "${n#*@}";;
    old|new) echo "/src/target/quick/$n/release/libchalloc.so";;
    jemalloc) echo /usr/lib/libjemalloc.so.2;;
    glibc) echo "";;
    mimalloc) echo /src/target/bench-bin/libmimalloc.so;;
    tcmalloc) echo /usr/lib/libtcmalloc_minimal.so.4;;
    *) echo "quick-perf: unknown variant $1" >&2; exit 2;;
  esac
}
label_of() { local n=${1%%:*}; n=${n%%@*}; case $1 in *:*) echo "$n:${1#*:}";; *) echo "$n";; esac; }

in_lib "ensure_probe valkey_model; ensure_probe churn" >/dev/null
conf_of() { case $1 in *:*) echo "${1#*:}";; esac; }
for v in $VARIANTS; do
  case ${v%%:*} in old|new|*@*) in_lib "test -f $(lib_of "$v")" || { echo "quick-perf: no $(lib_of "$v") in the target volume; run 'make quick-legacy QUICK_BUILD_ONLY=1' (or the make ab that builds it) first" >&2; exit 1; };; esac
done

echo "== quick-perf on $(hostname) ($(uname -m)), load $(cut -d' ' -f1-3 /proc/loadavg), events $EVENTS, rows: $ROWS, variants: $VARIANTS"
echo
# Events beyond the four standard columns (say `arith.div_active` on Intel,
# a raw `r09b0`, or `dp_spec` on Arm) get one per-operation column each,
# in the order given; names with a comma inside (`cpu/event=..,umask=../`)
# cannot be used here, use the raw `rXXXX` form instead.
# (`|| true`: with only the four standard events the grep matches nothing,
# and under `set -e -o pipefail` that used to end the script right after the
# header, silently.)
EXTRA=$(echo "$EVENTS" | tr , '\n' | { grep -vxE 'cycles|instructions|L1-dcache-load-misses|dTLB-load-misses' || true; } | tr '\n' ' ')
hdr="| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |"; sep="|---|---|---:|---:|---:|---:|---:|"
for e in $EXTRA; do hdr="$hdr $e/op |"; sep="$sep---:|"; done
echo "$hdr"
echo "$sep"
for b in $ROWS; do
  cmd=$(in_lib "bench_args $b")
  secs=$(in_lib "secs_of $b")
  for v in $VARIANTS; do
    # The raw files are named by the label: a LABEL@PATH variant's path
    # would put slashes into the file name.
    tag=$b.$(label_of "$v")
    cid=$(docker run -d "${DRUN_ARGS[@]}" "$IMAGE" \
          sh -c "sleep 1; exec env MALLOC_CONF=$(conf_of "$v") LD_PRELOAD=$(lib_of "$v") $cmd")
    [ "$CHALLOC_THP_COUNTERS" = 1 ] && thp_snapshot "$OUTDIR/$tag.thp0"
    pid=
    for _ in $(seq 1 50); do pid=$(docker inspect -f '{{.State.Pid}}' "$cid" 2>/dev/null || true); [ -n "$pid" ] && [ "$pid" != 0 ] && break; sleep 0.1; done
    [ -n "$pid" ] && [ "$pid" != 0 ] || { echo "quick-perf: container for $tag did not start" >&2; docker rm -f "$cid" >/dev/null 2>&1 || true; continue; }
    # perf must be root to attach to the container's process; stopped by
    # killing its sleep child (SIGINT is swallowed without a terminal).
    # perf reports the killed sleep on stderr ("sleep: Terminated"); drop that line only.
    sudo perf stat -x, -e "$EVENTS" --inherit -p "$pid" -o "$OUTDIR/$tag.csv" -- sleep 1000000 2> >(grep -v '^sleep: Terminated' >&2) &
    pp=$!
    docker wait "$cid" >/dev/null
    if [ "$CHALLOC_THP_COUNTERS" = 1 ]; then thp_snapshot "$OUTDIR/$tag.thp1"; thp_delta "$OUTDIR/$tag.thp0" "$OUTDIR/$tag.thp1" > "$OUTDIR/$tag.thp"; fi
    sudo pkill -f '^sleep 1000000$' 2>/dev/null || true; wait $pp || true
    docker logs "$cid" > "$OUTDIR/$tag.out" 2>&1
    docker rm -f "$cid" >/dev/null 2>&1 || true
    # The same extraction as bench/lib.sh ops_of, on the host side.
    # glibc-simple is 16 passes of 2M malloc+free pairs (no output to read).
    case $b in glibc-simple) ops=64000000;; *)
    ops=$(awk -v b="$b" -v secs="$secs" '
      b ~ /^larson/ && /Throughput/ {printf "%.0f", $3*secs}
      b ~ /^xmalloc/ && /free\/sec/ {sub(",", "", $4); printf "%.0f", $4*1e6*5}
      b ~ /^valkey/ && /^ops / {for (i=2;i<=NF;i++) {split($i, p, "="); if (p[1]=="total") print p[2]}}' "$OUTDIR/$tag.out");; esac
    awk -F, -v ops="${ops:--}" -v secs="$secs" -v row="$b" -v var="$(label_of "$v")" -v extra="$EXTRA" '
      /^#/ || NF < 3 {next}
      {c[$3]=$1}
      END {
        mops = (ops!="-" && secs!="-" && secs>0) ? sprintf("%.2f", ops/secs/1e6) : "-"
        n = split(extra, ex, " ")
        # A wall-time row without an operation count (python1, cfrac, ...) gets
        # whole-process totals instead: G cycles, G instructions, M misses.
        if (ops=="-" || ops==0) {
          printf "| %s | %s | - | %.2f G | %.2f G | %.1f M | %.1f M |", row, var, c["cycles"]/1e9, c["instructions"]/1e9, c["L1-dcache-load-misses"]/1e6, c["dTLB-load-misses"]/1e6
          for (i = 1; i <= n; i++) printf " %.1f M |", c[ex[i]]/1e6
          printf "\n"; exit
        }
        printf "| %s | %s | %s | %.1f | %.1f | %.3f | %.3f |", row, var, mops, c["cycles"]/ops, c["instructions"]/ops, c["L1-dcache-load-misses"]/ops, c["dTLB-load-misses"]/ops
        for (i = 1; i <= n; i++) printf " %.2f |", c[ex[i]]/ops
        printf "\n"
      }' "$OUTDIR/$tag.csv"
  done
done
echo
echo "Per-operation counters (whole-process totals for a row without an operation count), one run each: sudo perf stat -x, -e $EVENTS --inherit -p <container init pid> (attached before the exec), divided by the program's own operation count. Raw files under $OUTDIR. Compare variants within a row; attribution, not a gate."
if [ "$CHALLOC_THP_COUNTERS" = 1 ]; then
  echo
  echo "THP counters around each run (deltas of /proc/vmstat thp_* and hugepages-<size>kB/stats/* read on the host; host-global, so a quiet host is assumed; sysfs: $(thp_state)):"
  echo
  echo "| row | variant | deltas |"
  echo "|---|---|---|"
  for b in $ROWS; do for v in $VARIANTS; do [ -f "$OUTDIR/$b.$v.thp" ] && echo "| $b | $v | $(cat "$OUTDIR/$b.$v.thp") |"; done; done
fi

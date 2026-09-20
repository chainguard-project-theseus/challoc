#!/bin/sh
# bench/gce/native-rows.sh MODE: a challoc-bench-independent row runner for
# a host without Docker, make or cc (Phase 2.5 task 14, the Chainguard OS
# cells and the metal probe): the same rows, the same protocol as bench/ab.sh (one
# discarded warm-up, ROUNDS interleaved timed rounds, sorted scores, median
# RSS from bench/rt), run either natively on the Chainguard OS host (MODE
# native: ROOT=$HOME/native holds the binaries copied out of the container,
# the host's own glibc 2.44 underneath) or inside the podman container (MODE
# container: ROOT=/src). Shows whether host-native and in-container numbers
# agree (Phase 2.5 task 14). Native mode wants ~/native/target/bench-bin with
# rt, larson, valkey_model, libchalloc.so, libjemalloc.so.2, libmimalloc.so
# and libtcmalloc_minimal.so.4 copied out of the container, and the
# repository's bench/lib.sh at ~/challoc/bench/lib.sh. The THP section at
# the end samples AnonHugePages and the thp_* vmstat counters per allocator;
# ONLY_THP=1 runs just that; the `challoc-thp` allocator is challoc with
# MALLOC_CONF=thp:always.
#   ROUNDS=3  ROWS="larson1 larson16 valkey1 valkey1-free"  ALLOCS="challoc glibc jemalloc mimalloc tcmalloc"
#   THP_ROWS="valkey1 larson16"
set -u
MODE=${1:-native}
ROUNDS=${ROUNDS:-3}
ROWS=${ROWS:-"larson1 larson16 valkey1 valkey1-free"}
ALLOCS=${ALLOCS:-"challoc glibc jemalloc mimalloc tcmalloc"}
case $MODE in
  native)
    ROOT=$HOME/native; LIB=$HOME/challoc/bench/lib.sh
    CHALLOC=$ROOT/target/bench-bin/libchalloc.so ;;
  container)
    ROOT=/src; LIB=/src/bench/lib.sh
    CHALLOC=/src/target/release/libchalloc.so ;;
  *) echo "rows.sh native|container" >&2; exit 2 ;;
esac
. "$LIB"
if [ "$MODE" = native ]; then
  JEMALLOC_LIB=$BIN/libjemalloc.so.2
  TCMALLOC_LIB=$BIN/libtcmalloc_minimal.so.4
  # The probes were compiled in the container; there is no cc here.
  ensure_probe() { [ -x "$BIN/$1" ] || die "no $BIN/$1 (copy it out of the container)"; }
fi
OUT=$ROOT/rows-out; rm -rf "$OUT"; mkdir -p "$OUT"
lib_of() { case $1 in challoc|challoc-thp) echo "$CHALLOC";; *) stock_lib "$1";; esac; }
# conf_of ALLOC: MALLOC_CONF for the variant (challoc-thp: challoc's own thp:always key).
conf_of() { case $1 in challoc-thp) echo "thp:always";; *) echo "";; esac; }
echo "# rows ($MODE) on $(hostname): $(uname -m) $(uname -r), $PROCS CPUs, glibc $(/usr/lib/libc.so.6 2>/dev/null | head -1 | sed 's/.*version //;s/\.$//'), $(date -u +%FT%TZ)"
echo "- rows: $ROWS; allocators: $ALLOCS; $ROUNDS timed rounds after one warm-up, interleaved"
echo "- load at start: $(loadavg)"
for a in $ALLOCS; do
  l=$(lib_of "$a")
  if [ -n "$l" ]; then
    [ -f "$l" ] || { echo "- $a: MISSING $l"; continue; }
    m=$(LD_PRELOAD="$l" cat /proc/self/maps 2>&1 | grep -c "$(basename "$l")")
    echo "- $a: $l mapped=$m"
  else
    echo "- $a: no LD_PRELOAD (the host's libc malloc)"
  fi
done
if [ -n "${ONLY_THP:-}" ]; then ROUNDS=-1; fi
r=0
while [ "$r" -le "$ROUNDS" ]; do
  for b in $ROWS; do for a in $ALLOCS; do
    l=$(lib_of "$a") || continue
    [ -z "$l" ] || [ -f "$l" ] || continue
    run_row "$b" "$l" "$(conf_of "$a")" "$b.$a"
  done; done
  r=$((r + 1))
done
if [ -z "${ONLY_THP:-}" ]; then
echo
echo "## Sorted scores ($MODE)"
echo
echo "| program | variant | sorted scores | median | maxrss MiB |"
echo "|---|---|---|---:|---:|"
for b in $ROWS; do for a in $ALLOCS; do
  f=$OUT/$b.$a.scores; [ -f "$f" ] || continue
  s=$(sorted_scores "$f" 1); echo "| $b | $a | $s | $(median_list "$s") | $(kib_to_mib "$(median_rss "$f" 1)") |"
done; done
echo
echo "## challoc against each (positive = challoc worse), $MODE"
echo
printf '| program |'; for a in $ALLOCS; do [ "$a" = challoc ] || printf ' vs %s |' "$a"; done; echo
printf '%s' '|---|'; for a in $ALLOCS; do [ "$a" = challoc ] || printf '%s' '---:|'; done; echo
for b in $ROWS; do
  c=$(median_list "$(sorted_scores "$OUT/$b.challoc.scores" 1)")
  printf '| %s |' "$b"
  for a in $ALLOCS; do
    [ "$a" = challoc ] && continue
    f=$OUT/$b.$a.scores
    if [ -f "$f" ]; then printf ' %s |' "$(worse_pct "$b" "$c" "$(median_list "$(sorted_scores "$f" 1)")")"; else printf ' - |'; fi
  done
  echo
done
echo "- load at end: $(loadavg)"
fi

# ---- THP under `always` (the coordinator's request, 2026-09-19): one run of
# valkey1 and larson16 per allocator with AnonHugePages and Rss sampled from
# /proc/<pid>/smaps_rollup every 200 ms (the peak kept) and the deltas of
# the thp_* counters of /proc/vmstat around the run (the host's counters:
# runs are sequential, nothing else runs on the VM).
echo
echo "## THP under always ($MODE): peak AnonHugePages per allocator, /proc/vmstat deltas"
echo
echo "- thp enabled: $(cat /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null | tr -d '\n'); defrag: $(cat /sys/kernel/mm/transparent_hugepage/defrag 2>/dev/null | tr -d '\n'); khugepaged/max_ptes_none: $(cat /sys/kernel/mm/transparent_hugepage/khugepaged/max_ptes_none 2>/dev/null); hugepages-2048kB: $(cat /sys/kernel/mm/transparent_hugepage/hugepages-2048kB/enabled 2>/dev/null | tr -d '\n')"
echo
echo "| program | variant | score | peak Rss MiB | peak AnonHugePages MiB | thp_fault_alloc | thp_fault_fallback | thp_split_page | thp_deferred_split_page |"
echo "|---|---|---:|---:|---:|---:|---:|---:|---:|"
vm() { awk -v k="$1" '$1 == k { print $2 }' /proc/vmstat; }
THP_ROWS=${THP_ROWS:-"valkey1 larson16"}
for b in $THP_ROWS; do for a in $ALLOCS; do
  l=$(lib_of "$a") || continue
  [ -z "$l" ] || [ -f "$l" ] || continue
  set -- $(bench_args "$b")
  fa0=$(vm thp_fault_alloc); ff0=$(vm thp_fault_fallback); sp0=$(vm thp_split_page); ds0=$(vm thp_deferred_split_page)
  log=$OUT/thp.$b.$a.out
  cf=$(conf_of "$a")
  if [ -n "$l" ] && [ -n "$cf" ]; then env MALLOC_CONF="$cf" LD_PRELOAD="$l" "$@" > "$log" 2>&1 &
  elif [ -n "$l" ]; then env LD_PRELOAD="$l" "$@" > "$log" 2>&1 &
  else "$@" > "$log" 2>&1 & fi
  pid=$!; maxa=0; maxr=0
  while kill -0 "$pid" 2>/dev/null; do
    x=$(awk '/^AnonHugePages/ { a = $2 } /^Rss:/ { r = $2 } END { print a + 0, r + 0 }' "/proc/$pid/smaps_rollup" 2>/dev/null)
    set -- $x; [ "${1:-0}" -gt "$maxa" ] && maxa=$1; [ "${2:-0}" -gt "$maxr" ] && maxr=$2
    sleep 0.2
  done
  wait "$pid"
  case $b in
    larson*)  s=$(awk '/Throughput/ {printf "%.1f", $3/1e6}' "$log");;
    valkey*)  s=$(awk '/^ops / {for (i=2;i<=NF;i++) {split($i, p, "="); if (p[1]=="write_mops") print p[2]}}' "$log");;
    *)        s=-;;
  esac
  echo "| $b | $a | ${s:--} | $(kib_to_mib "$maxr") | $(kib_to_mib "$maxa") | $(( $(vm thp_fault_alloc) - fa0 )) | $(( $(vm thp_fault_fallback) - ff0 )) | $(( $(vm thp_split_page) - sp0 )) | $(( $(vm thp_deferred_split_page) - ds0 )) |"
done; done

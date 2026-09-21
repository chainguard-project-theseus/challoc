#!/bin/bash
# Perf sidecar for the Valkey soak (docs/plan.md Phase 1.5 task 4d), run as
# root on the GCE VM host beside a detached `make valkey-soak`:
#
#   setsid nohup sudo bash bench/probes/valkey-perf-sidecar.sh ~/logs/4d [D0] [S0] &
#
# For each valkey-server the soak starts (ALLOCS, default challoc then jemalloc), it
# attaches `perf stat -I 1000` (task-clock plus five PMU events, no
# multiplexing on Neoverse V2) for the server's whole life, records 60 s of
# cycles at t=100 (write phase) and t=D0 (delete phase; default 400, use 700
# for DURATION=1200), and snapshots INFO commandstats / INFO stats / the
# server's per-thread and the generators' CPU ticks around each window and
# at t=S0 (settle; default 605, use 1205). Output under $1:
# stat-<alloc>.csv, rec-<phase>-<alloc>.data, snap-<alloc>-<tag>.txt.
# t0 is the server's process start time, so a late attach still aligns with
# the soak's T0. Read the records with `perf report` while the pid is alive
# (the container's mount namespace resolves symbols) or afterwards with
# `--symfs` over copies of the container's binaries; method and reading in
# bench/results/2026-09-18-aarch64-gce-valkey-throughput.md.
set -u
L=${1:?usage: valkey-perf-sidecar.sh OUTDIR [D0] [S0]}; D0=${2:-400}; S0=${3:-605}
# The servers the soak starts, in order (bench/valkey-soak.sh ALLOCS; a
# same-run comparison of two contrib patches passes "challoc-old challoc jemalloc").
ALLOCS=${ALLOCS:-"challoc jemalloc"}
EV=task-clock,cycles,instructions,L1-dcache-load-misses,dTLB-load-misses,branch-misses
CLI=/src/target/valkey/src-9.1.2/src/valkey-cli
last=
for alloc in $ALLOCS; do
  while :; do
    pid=$(pgrep -x valkey-server | head -1)
    if [ -n "$pid" ] && [ "$pid" != "$last" ] && tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -q 6399; then
      sleep 1; kill -0 "$pid" 2>/dev/null && break
    fi
    sleep 1
  done
  last=$pid
  # t0 = process start (btime + starttime ticks / CLK_TCK), so a late attach still aligns with the soak's T0
  t0=$(( $(awk '/^btime/ {print $2}' /proc/stat) + $(awk '{print $22}' "/proc/$pid/stat") / 100 ))
  cid=$(docker ps -q | head -1)
  # $cli is a command line (docker exec CID CLI -p PORT): word-split on purpose where it is used.
  cli="docker exec $cid $CLI -p 6399"
  echo "== $alloc pid=$pid t0=$t0 cid=$cid $($cli info memory | grep mem_allocator)"
  ls "/proc/$pid/task" | wc -l
  for t in "/proc/$pid/task/"*; do echo "  tid $(basename "$t") $(cat "$t/comm")"; done
  perf stat -I 1000 -x, -e "$EV" -p "$pid" -o "$L/stat-$alloc.csv" &
  STATPID=$!
  snapshot() {
    { echo "ts $(date +%s) t=$(( $(date +%s) - t0 ))"
      echo "-- threads (comm utime stime, ticks)"
      for t in "/proc/$pid/task/"*; do echo "$(cat "$t/comm") $(awk '{print $14, $15}' "$t/stat")"; done
      echo "-- generators"
      for g in $(pgrep -f "valkey-benchmark|memtier_benchmark"); do
        echo "$(cat "/proc/$g/comm") pid=$g threads=$(ls "/proc/$g/task" | wc -l) $(awk '{print $14, $15}' "/proc/$g/stat")"
      done
      echo "-- info stats"
      $cli info stats | tr -d '\r' | grep -E '^(total_commands_processed|instantaneous_ops_per_sec|total_net_input_bytes|total_net_output_bytes|total_connections_received|rejected_connections|evicted_keys|expired_keys):'
      echo "-- info commandstats"
      $cli info commandstats | tr -d '\r'
      echo "-- info memory (allocator lines)"
      $cli info memory | tr -d '\r' | grep -E '^(used_memory|used_memory_rss|allocator_allocated|allocator_active|allocator_resident|allocator_frag_ratio|active_defrag_running|mem_allocator):'
      echo "-- clients"
      $cli info clients | tr -d '\r' | grep -E '^(connected_clients|blocked_clients|io_threads_active):'
    } > "$L/snap-$alloc-$1.txt" 2>&1
    echo "snapshot $alloc $1 at t=$(( $(date +%s) - t0 ))"
  }
  sleep_until() { while [ $(( $(date +%s) - t0 )) -lt "$1" ]; do sleep 1; done; }
  sleep_until 30; snapshot early
  sleep_until 100; snapshot write-a
  perf record -e cycles -F 2999 -p "$pid" -o "$L/rec-write-$alloc.data" -- sleep 60 2>&1 | tail -2
  snapshot write-b
  sleep_until "$D0"; snapshot delete-a
  perf record -e cycles -F 2999 -p "$pid" -o "$L/rec-delete-$alloc.data" -- sleep 60 2>&1 | tail -2
  snapshot delete-b
  sleep_until "$S0"; snapshot settle
  while kill -0 "$pid" 2>/dev/null; do sleep 2; done
  wait "$STATPID"
  echo "== $alloc done at $(date +%s)"
done
echo "== sidecar done"

#!/bin/bash
# Defrag sidecar for the Valkey soak, run
# on the VM host beside a detached `make valkey-soak`, no root needed (the
# server is root inside the container, so liveness is `/proc/<pid>`, not
# `kill -0`, which EPERMs and had the first attempt wait forever):
#
#   (bash bench/probes/valkey-defrag-sidecar.sh ~/logs/4f > ~/logs/4f/sidecar.log 2>&1 &)
#
# For each valkey-server the soak starts (challoc first, then jemalloc) it
# switches the server to `loglevel verbose`, so defrag.c's own decision
# lines land in the soak's server.log ("Starting active defrag, frag=X%,
# frag_bytes=N, cpu=P%" when a cycle begins, "Active defrag done in Nms,
# reallocated=N, frag=X%, frag_bytes=N" when it ends, and the CPU changes
# in between), then samples INFO memory / stats / keyspace once a second
# for the server's life into $1/defrag-<alloc>.csv: t (seconds since the
# process started, the soak's own clock), used_memory, allocator_allocated,
# allocator_active, allocator_resident, allocator_frag_ratio,
# allocator_frag_bytes, active_defrag_running, active_defrag_hits,
# active_defrag_misses, active_defrag_key_hits, active_defrag_key_misses,
# total_active_defrag_time, total_commands_processed, keys. The soak's own
# sampler at INTERVAL=1 gives the same fields except frag_bytes and the
# key counts; this one exists for the log switch and the two extra columns.
# A third argument is a `CONFIG SET` applied at attach, for diagnostics only
# (e.g. `"active-defrag-ignore-bytes 83886080"` under challoc to see what a
# third cycle does; never a product setting).
# server.log itself is in the target volume
# (/src/target/valkey/soak/<alloc>/server.log): copy it out with `make run`
# after the soak.
set -u
L=${1:?usage: valkey-defrag-sidecar.sh OUTDIR [ALLOCS] [CONFIG-SET]}; ALLOCS=${2:-"challoc jemalloc"}; SETCONF=${3:-}
mkdir -p "$L"
CLI=/src/target/valkey/src-9.1.2/src/valkey-cli
last=
for alloc in $ALLOCS; do
  while :; do
    pid=$(pgrep -x valkey-server | head -1)
    if [ -n "$pid" ] && [ "$pid" != "$last" ] && tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -q 6399; then
      sleep 1; [ -d "/proc/$pid" ] && break
    fi
    sleep 1
  done
  last=$pid
  t0=$(( $(awk '/^btime/ {print $2}' /proc/stat) + $(awk '{print $22}' "/proc/$pid/stat") / 100 ))
  cid=$(docker ps -q | head -1)
  # $cli is a command line (docker exec CID CLI -p PORT) and $SETCONF a
  # "key value" pair: both are word-split on purpose where they are used.
  cli="docker exec $cid $CLI -p 6399"
  echo "== $alloc pid=$pid t0=$t0 cid=$cid $($cli info memory | tr -d '\r' | grep mem_allocator)"
  echo "loglevel verbose: $($cli config set loglevel verbose)"
  # shellcheck disable=SC2086
  [ -z "$SETCONF" ] || echo "config set $SETCONF: $($cli config set $SETCONF)"
  echo "active-defrag: $($cli config get 'active-defrag-*' | tr '\r\n' '  ')"
  csv=$L/defrag-$alloc.csv
  echo "t,used_memory,allocator_allocated,allocator_active,allocator_resident,allocator_frag_ratio,allocator_frag_bytes,active_defrag_running,active_defrag_hits,active_defrag_misses,active_defrag_key_hits,active_defrag_key_misses,total_active_defrag_time,total_commands_processed,keys" > "$csv"
  while [ -d "/proc/$pid" ]; do
    t=$(( $(date +%s) - t0 ))
    { $cli info memory; $cli info stats; $cli info keyspace; } 2>/dev/null | tr -d '\r' | awk -F'[:,=]' -v t="$t" '
      /^db0:/ { v["keys"]=$3; next }
      /^[a-z_]+:/ { v[$1]=$2 }
      END {
        n=split("used_memory allocator_allocated allocator_active allocator_resident allocator_frag_ratio allocator_frag_bytes active_defrag_running active_defrag_hits active_defrag_misses active_defrag_key_hits active_defrag_key_misses total_active_defrag_time total_commands_processed keys", k, " ")
        printf "%s", t
        for (i=1;i<=n;i++) printf ",%s", v[k[i]]
        print ""
      }' >> "$csv"
    sleep 1
  done
  echo "== $alloc server gone at t=$(( $(date +%s) - t0 ))"
done

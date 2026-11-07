#!/bin/sh
# Valkey (and Redis) soak: the `valkey-soak` and `redis-soak` driver rows of
# bench/cb (bench/rows.toml, cb mode below), or by hand inside the dev
# container: `make run CMD='sh bench/drivers/valkey-soak.sh'` (needs `make
# valkey STAGE=build`).
#
# SERVER=valkey (default) or redis picks the server (bench/drivers/kv.sh:
# the pinned tags, the bundled-jemalloc, libc and runpath builds). Any
# allocator kv.sh knows can be soaked: `jemalloc` is the server's bundled
# jemalloc 5.3.0 (the stock build), `challoc` the server's runpath build
# with its challoc integration (contrib/valkey, contrib/redis;
# CHALLOC_LINK=native, the default) or preloaded on the libc build
# (CHALLOC_LINK=preload), and glibc, jemalloc53,
# jemalloc54, tcmalloc and mimalloc are preloaded on the libc build. Active
# defrag (DEFRAG) runs only on the builds that have it (bundled jemalloc,
# challoc native); the libc build runs with it off, and the summary says
# so per allocator.
#
# cb mode (CB_ALLOC set; bench/README.md, "Drivers"): one allocator per
# invocation, the variant cb hands over, and the metrics as one JSON line
# to RESULT_JSONL. A variant whose label contains "preload" (e.g.
# `challoc-preload=head`) sets CHALLOC_LINK=preload. A challoc variant's
# library (CB_LIB, a quantum8 build) is swapped in under the fixed runpath
# VALKEY_RUNPATH (default /opt/challoc/lib): one Valkey build, linked with
# -Wl,-rpath,/opt/challoc/lib -Wl,--enable-new-dtags (a RUNPATH), serves
# every variant, and /opt/challoc/lib/libchalloc.so.0 is pointed at the
# variant's file before the server starts. CB_CONF is the variant's conf,
# handed to whichever allocator it names (kv.sh's kv_conf_env). The client binaries (valkey-cli, valkey-benchmark) come from
# the jemalloc build, so they never load the library under test. Sources
# are cloned from GitHub at VALKEY_TAG when target/valkey has no tree.
#
# For each allocator in ALLOCS a valkey-server is started from the matching
# Valkey build (challoc: the patched tree from tests/consumers/valkey-build.sh;
# jemalloc: an unpatched clone of the same tag built with its bundled jemalloc
# 5.3.0, created here on first use) and driven through two load phases of
# DURATION/2 seconds each, then left idle for SETTLE seconds:
#
#   write-heavy   valkey-benchmark `-t set -d SIZE` over KEYS random keys,
#                 then memtier_benchmark SET-only with values of MT_SIZES bytes
#                 over KEYS random keys, repeated until the phase ends
#   delete-heavy  valkey-benchmark `del key:__rand_int__`, then
#                 memtier_benchmark DEL:SET at DEL_RATIO:1, repeated likewise
#   settle        no load; the decay window and active defrag run alone
#
# While the server runs, `INFO memory` and `INFO stats` are sampled every
# INTERVAL seconds into a CSV with used_memory, used_memory_rss,
# mem_fragmentation_ratio, allocator_frag_ratio, allocator_rss_ratio,
# active_defrag_hits and the p99 latency and throughput reported by the load
# generator run that finished most recently. Every generator run also lands
# in a per-run CSV. A markdown summary compares challoc with jemalloc on
# throughput, p99, peak RSS (the server's VmHWM) and post-settle RSS, the
# four acceptance measures.
#
# The valkey-benchmark and valkey-cli binaries come from the challoc tree for
# every allocator (same client binary, only the server changes);
# memtier_benchmark is Wolfi's package.
#
#   ALLOCS="challoc jemalloc"  servers to soak, in order; challoc-old adds the
#                              server of CHALLOC_OLD_SRC (default target/valkey/
#                              src-<tag>-oldpatch, a tree built with a previous
#                              contrib patch) on the same library, with its own
#                              "vs jemalloc" column
#   CHALLOC_CONF=              MALLOC_CONF for the challoc server alone (a
#                              MALLOC_CONF in the environment would reach the
#                              jemalloc server too)
#   DURATION=1200              load seconds per allocator (write + delete)
#   SETTLE=60                  idle seconds after the delete phase
#   REFILL=0                   seconds of a second write phase after the
#                              settle (0: none), which reuses what the settle
#                              purged: its throughput and p99 are the
#                              refault cost (refill_* metrics)
#   INTERVAL=10                sampling period in seconds
#   CHUNK=30                   memtier --test-time per run
#   VB_REQS=20000000           valkey-benchmark -n per run
#   KEYS=500000                keyspace of every generator
#   SIZE=512                   valkey-benchmark -d
#   MT_SIZES=16-1024           memtier --data-size-range
#   DEL_RATIO=4                memtier DEL:SET ratio in the delete phase
#   KEY_PATTERN=stock          stock: memtier's default fixed-seed random key stream
#                              (the acceptance protocol; it replays the same keys every
#                              run and pins ~2200 values per class through the delete
#                              phase);
#                              uniform: `--randomize` (time-seeded) with random key
#                              patterns (`--key-pattern=R:R`, `--command-key-pattern=R`)
#                              in both phases, so no key is structurally pinned
#   VB_CLIENTS=50 MT_THREADS=4 MT_CLIENTS=10 PIPELINE=16
#   DEFRAG=yes                 --activedefrag on the server
#   IO_THREADS=                --io-threads N on the server
#                              (with 4, the frees of one thread's allocations land in
#                              other threads' caches); empty = Valkey's default (1),
#                              the acceptance protocol. Recorded in the summary header
#                              and the results file name gets no suffix: pass RESULT_DIR.
#   PORT=6399
#   OUT=target/valkey/soak     scratch and raw logs
#   RESULT_DIR=target/results  <date>-<arch>-<server>-soak.md and
#                              <date>-<arch>-<server>-soak-<alloc>.csv land here
#   OCCUPANCY=0                1: a sidecar dumps INFO memory's allocator lines and
#                              MEMORY MALLOC-STATS (per-class slab occupancy) every
#                              MS_INTERVAL (20) seconds into
#                              RESULT_DIR/<date>-<arch>-<server>-soak-<alloc>-mstats/<t>.txt
#   BASELINES=cached           do not soak jemalloc; take its column from the cached
#                              soak under target/baselines/valkey-soak-jemalloc-<arch>.*
#                              (written by SAVE_BASELINE=1, `make soak-baseline`). The
#                              summary says so, with the cache's date and DURATION.
#                              `make soak-quick` uses this; acceptance never does.
#   SAVE_BASELINE=0            1: after soaking jemalloc, store its samples CSV, runs
#                              CSV, VmHWM, dbsize, version and a .meta line under
#                              target/baselines/ for BASELINES=cached
#
# Per-phase cgroup peak: beside the server's VmHWM
# the summary carries the cgroup's memory.peak for the write, delete and
# settle phases, reset at each boundary through bench/probes/cgpeak.c (the
# 6.12 reset is scoped to the writer's file description, so a sidecar holds
# it; bench/drivers/lib.sh cgroup_peak_*). The peak is the whole container's
# (server, valkey-benchmark, memtier and this script). The write needs a
# writable cgroup tree, `make valkey-soak CGROUP_PEAK=1`, and a 6.12+
# kernel; otherwise the rows read "-" and the header says why.
set -eu
cd "$(dirname "$0")/../.."
ROOT=$(pwd)
. "$ROOT/bench/drivers/lib.sh"
ALLOCS=${ALLOCS:-"challoc jemalloc"}
DURATION=${DURATION:-1200}
SETTLE=${SETTLE:-60}
INTERVAL=${INTERVAL:-10}
CHUNK=${CHUNK:-30}
VB_REQS=${VB_REQS:-20000000}
KEYS=${KEYS:-500000}
SIZE=${SIZE:-512}
MT_SIZES=${MT_SIZES:-16-1024}
DEL_RATIO=${DEL_RATIO:-4}
KEY_PATTERN=${KEY_PATTERN:-stock}
case $KEY_PATTERN in
  stock)   MT_RANDOM=;;
  uniform) MT_RANDOM=--randomize;;
  *) echo "valkey-soak: KEY_PATTERN must be stock or uniform" >&2; exit 1;;
esac
VB_CLIENTS=${VB_CLIENTS:-50}
MT_THREADS=${MT_THREADS:-4}
MT_CLIENTS=${MT_CLIENTS:-10}
PIPELINE=${PIPELINE:-16}
DEFRAG=${DEFRAG:-yes}
IO_THREADS=${IO_THREADS:-}
PORT=${PORT:-6399}
OUT=${OUT:-$ROOT/target/${SERVER:-valkey}/soak}
RESULT_DIR=${RESULT_DIR:-$ROOT/target/results}
OCCUPANCY=${OCCUPANCY:-0}
MS_INTERVAL=${MS_INTERVAL:-20}
BASELINES=${BASELINES:-}
SAVE_BASELINE=${SAVE_BASELINE:-0}
SERVER=${SERVER:-valkey}
KV_SERVER=$SERVER
. "$ROOT/bench/drivers/kv.sh"
CHALLOC_LINK=${CHALLOC_LINK:-native}
case ${CB_VARIANT:-} in *preload*) CHALLOC_LINK=preload;; esac
SRC=$KV_SRC
# By hand on Valkey the clients are the challoc tree's (valkey-build.sh);
# under cb, and on Redis, the bundled-jemalloc build's, so a client never
# loads the library under test.
CLI=$SRC/src/valkey-cli
VB=$SRC/src/valkey-benchmark
[ "$SERVER" = valkey ] || { CLI=$KV_CLI; VB=$KV_BENCH; }
# ARCH, DATE and BASELINE_DIR come from bench/drivers/lib.sh. COMMIT overrides git's
# answer (a tree synced to a VM from a git worktree has no usable .git).
COMMIT=${COMMIT:-$(commit_short)}
RESULT=$RESULT_DIR/$DATE-$ARCH-$SERVER-soak.md
BASE=$BASELINE_DIR/$SERVER-soak-jemalloc-$ARCH
CB_MODE=0
if [ -n "${CB_ALLOC:-}" ]; then
  CB_MODE=1
  case $CB_ALLOC in
    challoc) [ -f "${CB_LIB:-}" ] || { echo "valkey-soak: CB_LIB='${CB_LIB:-}' is not a file" >&2; exit 1; }
             CHALLOC_CONF=${CB_CONF:-};;
    jemalloc|jemalloc53|jemalloc54|glibc|tcmalloc|mimalloc) STOCK_CONF=${CB_CONF:-};;
    *) echo "valkey-soak: allocator $CB_ALLOC is not one kv.sh knows" >&2; exit 1;;
  esac
  ALLOCS=$CB_ALLOC
  OUT=${CB_OUT:?}/soak
  RESULT_DIR=$CB_OUT
  RESULT=$RESULT_DIR/$SERVER-soak.md
  BASELINES=
  SAVE_BASELINE=0
  CLI=$KV_CLI
  VB=$KV_BENCH
fi

step() { printf '\n== %s\n' "$*"; }
now() { date +%s; }

# The allocators in the summary: the soaked ones plus, with BASELINES=cached,
# jemalloc from the cache.
SUMMARY_ALLOCS=$ALLOCS
if [ "$BASELINES" = cached ]; then
  [ -f "$BASE.csv" ] && [ -f "$BASE.runs.csv" ] && [ -f "$BASE.meta" ] \
    || { echo "valkey-soak: BASELINES=cached but no $BASE.{csv,runs.csv,meta}; run 'make soak-baseline' on this machine first" >&2; exit 1; }
  case " $ALLOCS " in *" jemalloc "*) echo "valkey-soak: BASELINES=cached and jemalloc in ALLOCS: drop one" >&2; exit 1;; esac
  SUMMARY_ALLOCS="$ALLOCS jemalloc"
fi

if [ $CB_MODE = 0 ] && [ "$SERVER" = valkey ]; then
  [ -x "$CLI" ] && [ -x "$VB" ] && [ -x "$SRC/src/valkey-server" ] \
    || { echo "valkey-soak: $SRC is not built; run 'make valkey STAGE=build' first" >&2; exit 1; }
fi
command -v memtier_benchmark >/dev/null || { echo "valkey-soak: memtier_benchmark not installed" >&2; exit 1; }

take_bench_lock   # one protocol run at a time (bench/drivers/lib.sh)
rm -rf "$OUT"
mkdir -p "$OUT" "$RESULT_DIR"

# The servers come from bench/drivers/kv.sh (kv_resolve: the bundled-jemalloc
# build, the libc build every preloaded allocator shares, Valkey's runpath
# challoc build), each built once per target volume.
# server_for ALLOC: sets SRV PRELOAD EXPECT DEFRAG_OK BUILD for ALLOC.
#
# challoc-old: a second challoc server from CHALLOC_OLD_SRC (default the
# tree tests/consumers/valkey-build.sh built with the previous contrib patch, kept as
# target/valkey/src-<tag>-oldpatch), for a same-run comparison of two
# versions of the Valkey-side patch against one library and one jemalloc
# column. It runs the same library as the
# challoc server (both binaries carry the rpath of the challoc build).
CHALLOC_OLD_SRC=${CHALLOC_OLD_SRC:-$SRC-oldpatch}
server_for() {
  case $1 in
    challoc-old) [ -x "$CHALLOC_OLD_SRC/src/valkey-server" ] || { echo "valkey-soak: no $CHALLOC_OLD_SRC/src/valkey-server (CHALLOC_OLD_SRC)" >&2; exit 1; }
              SRV=$CHALLOC_OLD_SRC/src/valkey-server; PRELOAD=; EXPECT=jemalloc-5.3.0-challoc; DEFRAG_OK=yes; BUILD=byhand;;
    challoc)  if [ $CB_MODE = 0 ] && [ "$SERVER" = valkey ] && [ "$CHALLOC_LINK" = native ]; then
                # By hand: the valkey-build.sh tree, linked against its own build.
                SRV=$SRC/src/valkey-server; PRELOAD=; EXPECT=jemalloc-5.3.0-challoc; DEFRAG_OK=yes; BUILD=byhand
              else
                kv_resolve challoc "$CHALLOC_LINK"
                SRV=$KV_SRV; PRELOAD=$KV_PRELOAD; EXPECT=$KV_EXPECT; DEFRAG_OK=$KV_DEFRAG; BUILD=$KV_BUILD
              fi;;
    *)        kv_resolve "$1"
              SRV=$KV_SRV; PRELOAD=$KV_PRELOAD; EXPECT=$KV_EXPECT; DEFRAG_OK=$KV_DEFRAG; BUILD=$KV_BUILD;;
  esac
}

SERVER_PID=
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null
  [ -n "${SAMPLER_PID:-}" ] && kill "$SAMPLER_PID" 2>/dev/null
  [ -n "${OCC_PID:-}" ] && kill "$OCC_PID" 2>/dev/null
  :
}
BENCH_CLEANUP=cleanup   # run by the bench lock's EXIT/INT/TERM trap (bench/drivers/lib.sh take_bench_lock)

# info_field SECTION FIELD: one INFO value, CRLF stripped.
info_field() { "$CLI" -p "$PORT" info "$1" | tr -d '\r' | awk -F: -v k="$2" '$1==k {print $2}'; }

# The sampler runs beside the load generators and appends one CSV row per
# INTERVAL. The main loop tells it the current phase through $D/phase and the
# most recent generator result through $D/last (rps p99_ms).
sample_once() { # DIR START_TS CSV: append one row, fail if the server is gone
  d=$1; start=$2; csv=$3
  ts=$(now)
  mem=$("$CLI" -p "$PORT" info memory 2>/dev/null | tr -d '\r') || return 1
  st=$("$CLI" -p "$PORT" info stats 2>/dev/null | tr -d '\r') || return 1
  last=$(cat "$d/last" 2>/dev/null || echo ",")
  printf '%s,%s,%s,' "$ts" "$((ts - start))" "$(cat "$d/phase")" >> "$csv"
  printf '%s\n%s\n' "$mem" "$st" | awk -F: -v last="$last" '
    { v[$1]=$2 }
    END {
      n=split("used_memory used_memory_rss used_memory_peak allocator_allocated allocator_active allocator_resident mem_fragmentation_ratio allocator_frag_ratio allocator_rss_ratio active_defrag_running active_defrag_hits active_defrag_misses active_defrag_key_hits", k, " ")
      for (i=1;i<=n;i++) printf "%s,", v[k[i]]
      print last
    }' >> "$csv"
}
sampler() { # DIR START_TS CSV
  echo "ts,elapsed_s,phase,used_memory,used_memory_rss,used_memory_peak,allocator_allocated,allocator_active,allocator_resident,mem_fragmentation_ratio,allocator_frag_ratio,allocator_rss_ratio,active_defrag_running,active_defrag_hits,active_defrag_misses,active_defrag_key_hits,last_rps,last_p99_ms" > "$3"
  while [ ! -e "$1/stop" ]; do
    sample_once "$1" "$2" "$3" || break
    sleep "$INTERVAL"
  done
}
# The occupancy sidecar (OCCUPANCY=1):
# every MS_INTERVAL seconds one file with the phase, the allocator lines of
# INFO memory and the full MEMORY MALLOC-STATS print, so per-class curslabs /
# curregs can be read along the soak.
occupancy_sampler() { # DIR START_TS OUTDIR
  mkdir -p "$3"
  while [ ! -e "$1/stop" ]; do
    sleep "$MS_INTERVAL"
    [ -e "$1/stop" ] && break
    t=$(( $(now) - $2 ))
    { echo "# t=$t phase=$(cat "$1/phase")"
      "$CLI" -p "$PORT" info memory | tr -d '\r' | grep -E '^(used_memory|used_memory_rss|allocator_allocated|allocator_active|allocator_resident|allocator_frag_ratio|active_defrag_hits):'
      "$CLI" -p "$PORT" memory malloc-stats
    } > "$3/$(printf %05d "$t").txt" 2>&1 || break
  done
}

# Generator runs. Each appends "phase,tool,test,start_s,end_s,seconds,rps,p99_ms"
# to $RUNS and rewrites $D/last for the sampler.
record_run() { # phase tool test start end rps p99
  echo "$1,$2,$3,$4,$5,$(($5 - $4)),$6,$7" >> "$RUNS"
  echo "$6,$7" > "$D/last"
}
vb_run() { # phase test args...
  phase=$1; test=$2; shift 2
  s=$(($(now) - T0))
  out=$("$VB" -p "$PORT" -c "$VB_CLIENTS" -P "$PIPELINE" -r "$KEYS" -n "$VB_REQS" --csv "$@" 2>>"$D/generators.log" | tail -1)
  e=$(($(now) - T0))
  echo "$phase valkey-benchmark $test: $out" >> "$D/generators.log"
  # "test","rps","avg","min","p50","p95","p99","max"
  rps=$(echo "$out" | awk -F'","' '{printf "%.0f", $2}')
  p99=$(echo "$out" | awk -F'","' '{print $7}')
  record_run "$phase" valkey-benchmark "$test" "$s" "$e" "$rps" "$p99"
}
mt_run() { # phase test args...
  phase=$1; test=$2; shift 2
  s=$(($(now) - T0))
  # $MT_RANDOM is empty (stock) or --randomize (KEY_PATTERN=uniform); unquoted on purpose.
  out=$(memtier_benchmark -p "$PORT" --threads="$MT_THREADS" --clients="$MT_CLIENTS" --pipeline="$PIPELINE" \
        --test-time="$CHUNK" --key-prefix=key: --key-maximum="$KEYS" --data-size-range="$MT_SIZES" \
        --hide-histogram --print-percentiles=50,99,99.9 $MT_RANDOM "$@" 2>&1)
  e=$(($(now) - T0))
  echo "$phase memtier $test:" >> "$D/generators.log"
  echo "$out" | sed -n '/^ALL STATS/,$p' >> "$D/generators.log"
  # Totals  Ops/sec  Hits/sec  Misses/sec  Avg  p50  p99  p99.9  KB/sec
  # shellcheck disable=SC2046 # the two fields, one word each
  set -- $(echo "$out" | awk '$1=="Totals" {printf "%.0f %s", $2, $7}')
  record_run "$phase" memtier "$test" "$s" "$e" "${1:-0}" "${2:-}"
}

write_phase() {
  vb_run write set -t set -d "$SIZE"
  mt_run write set --ratio=1:0 --key-pattern=R:R
}
delete_phase() {
  vb_run delete del del key:__rand_int__
  mt_run delete "del:set=$DEL_RATIO:1" \
    --command="DEL __key__" --command-key-pattern=R --command-ratio="$DEL_RATIO" \
    --command="SET __key__ __data__" --command-key-pattern=R --command-ratio=1
}

soak() { # alloc
  alloc=$1
  D=$OUT/$alloc; mkdir -p "$D"
  RUNS=$D/runs.csv
  echo "phase,tool,test,start_s,end_s,seconds,rps,p99_ms" > "$RUNS"
  server_for "$alloc"
  server=$SRV
  # Active defrag only where the build has it (the libc build refuses
  # --activedefrag yes at startup).
  defrag=$DEFRAG; [ "$DEFRAG_OK" = yes ] || defrag=no
  step "$alloc: $server${PRELOAD:+ (LD_PRELOAD=$PRELOAD)}"
  "$server" --version | tee "$D/version.txt"
  # $IO_THREADS unquoted on purpose: empty means no flag. CHALLOC_CONF
  # reaches the challoc server only, a cb variant's conf (STOCK_CONF) the
  # stock allocator it names, each in the variable that allocator reads
  # (kv_conf_env), and CHALLOC_LIB the challoc server only: a directory
  # holding a libchalloc.so.0 (a quantum8 A/B build of a `.tmp-<v>` source
  # copy) that is LD_PRELOADed under its soname, so the binary's DT_NEEDED
  # libchalloc.so.0 resolves to it whether the link left an RPATH or a
  # RUNPATH and the same valkey-server runs on a src/ variant; the mapped
  # library and its sha256 are checked and recorded below.
  conf_env=${PRELOAD:+LD_PRELOAD=$PRELOAD}
  case $alloc in challoc) conf=${CHALLOC_CONF:-};; *) conf=${STOCK_CONF:-};; esac
  conf_word=$(kv_conf_env "$alloc" "$conf") || exit 1
  conf_env="$conf_env${conf_word:+ $conf_word}"
  case $alloc in challoc)
    [ -n "${CHALLOC_LIB:-}" ] && { [ -f "$CHALLOC_LIB/libchalloc.so.0" ] || { echo "valkey-soak: CHALLOC_LIB=$CHALLOC_LIB has no libchalloc.so.0" >&2; exit 1; }; conf_env="$conf_env LD_PRELOAD=$CHALLOC_LIB/libchalloc.so.0"; };;
  esac
  # shellcheck disable=SC2086 # conf_env is VAR=value words for env
  env $conf_env "$server" --port "$PORT" --save "" --appendonly no --daemonize no \
    --logfile "$D/server.log" --activedefrag "$defrag" ${IO_THREADS:+--io-threads $IO_THREADS} >/dev/null 2>&1 9>&- &
  SERVER_PID=$!
  i=0
  until [ "$("$CLI" -p "$PORT" ping 2>/dev/null)" = PONG ]; do
    i=$((i+1)); [ $i -gt 100 ] && { echo "valkey-soak: $server did not start; see $D/server.log" >&2; exit 1; }
    sleep 0.1
  done
  got=$(info_field memory mem_allocator)
  [ "$got" = "$EXPECT" ] \
    || { echo "valkey-soak: $alloc server reports mem_allocator=$got, expected $EXPECT" >&2; exit 1; }
  # Which allocator library the server mapped, and its hash, for the header.
  mapped=$(awk '$6 ~ /libchalloc\.so|libjemalloc|libtcmalloc|libmimalloc/ {print $6}' "/proc/$SERVER_PID/maps" 2>/dev/null | sort -u | head -1)
  # The kv.sh builds: exactly the library kv_resolve named (none for the
  # bundled jemalloc and glibc).
  [ "$BUILD" = byhand ] || kv_check_binding "$SERVER_PID" "$alloc"
  case $alloc in
    challoc)
      [ -n "$mapped" ] || { echo "valkey-soak: the challoc server maps no libchalloc.so" >&2; exit 1; }
      if [ -n "${CHALLOC_LIB:-}" ]; then
        case $mapped in "$CHALLOC_LIB"/*) ;; *) echo "valkey-soak: CHALLOC_LIB=$CHALLOC_LIB but the server mapped $mapped" >&2; exit 1;; esac
      fi
      if [ $CB_MODE = 1 ] && [ "$(readlink -f "$mapped")" != "$(readlink -f "$CB_LIB")" ]; then
        echo "valkey-soak: the server mapped $mapped, not the variant's $CB_LIB" >&2; exit 1
      fi
      echo "$mapped $(so_hash "$mapped")" > "$D/lib.txt";;
    *) if [ -n "$mapped" ]; then echo "$mapped $(so_hash "$mapped")" > "$D/lib.txt"
       elif [ "$BUILD" = libc ]; then echo "glibc malloc (libc build, no preload)" > "$D/lib.txt"
       else echo "bundled (static)" > "$D/lib.txt"; fi;;
  esac
  echo "$BUILD" > "$D/build.txt"
  echo "allocator library: $(cat "$D/lib.txt")"
  "$CLI" -p "$PORT" config get activedefrag | tail -1 > "$D/activedefrag.txt"
  "$CLI" -p "$PORT" config get jemalloc-bg-thread | tail -1 > "$D/bg-thread.txt"
  "$CLI" -p "$PORT" config get io-threads | tail -1 > "$D/io-threads.txt"
  echo "mem_allocator=$got activedefrag=$(cat "$D/activedefrag.txt") jemalloc-bg-thread=$(cat "$D/bg-thread.txt") io-threads=$(cat "$D/io-threads.txt") pid=$SERVER_PID"

  T0=$(now)
  echo write > "$D/phase"; rm -f "$D/last" "$D/stop"
  sampler "$D" "$T0" "$D/samples.csv" 9>&- &
  SAMPLER_PID=$!
  cgroup_peak_start "$D" || true
  OCC_PID=
  if [ "$OCCUPANCY" = 1 ]; then
    occupancy_sampler "$D" "$T0" "$RESULT_DIR/$DATE-$ARCH-$SERVER-soak-$alloc-mstats" 9>&- &
    OCC_PID=$!
    echo "occupancy sampler: MEMORY MALLOC-STATS every ${MS_INTERVAL}s into $RESULT_DIR/$DATE-$ARCH-$SERVER-soak-$alloc-mstats/"
  fi

  half=$((DURATION / 2))
  echo "-- write phase, ${half}s"
  while [ $(($(now) - T0)) -lt $half ]; do write_phase; printf .; done; echo
  echo "$(($(now) - T0))" > "$D/write_end"
  echo delete > "$D/phase"; rm -f "$D/last"
  cgroup_peak_phase "$D" delete
  echo "-- delete phase, ${half}s"
  while [ $(($(now) - T0)) -lt "$DURATION" ]; do delete_phase; printf .; done; echo
  echo "$(($(now) - T0))" > "$D/delete_end"
  echo settle > "$D/phase"; rm -f "$D/last"
  cgroup_peak_phase "$D" settle
  echo "-- settle, ${SETTLE}s"
  sleep "$SETTLE"
  # Stop the periodic sampler, then take the post-settle sample explicitly so
  # the summary's "after settle" row exists even when SETTLE < INTERVAL.
  touch "$D/stop"; wait "$SAMPLER_PID" 2>/dev/null || true; SAMPLER_PID=
  [ -z "$OCC_PID" ] || { wait "$OCC_PID" 2>/dev/null || true; OCC_PID=; }
  cgroup_peak_stop
  sample_once "$D" "$T0" "$D/samples.csv"
  if [ "${REFILL:-0}" -gt 0 ]; then
    echo refill > "$D/phase"; rm -f "$D/last"
    echo "-- refill phase, ${REFILL}s"
    r0=$(now)
    while [ $(($(now) - r0)) -lt "$REFILL" ]; do
      vb_run refill set -t set -d "$SIZE"
      mt_run refill set --ratio=1:0 --key-pattern=R:R
      printf .
    done; echo
    rss=$(grep '^VmRSS:' "/proc/$SERVER_PID/status" | awk '{print int($2/1024)}')
    echo "$rss" > "$D/refill_rss"
  fi

  # Final state: kernel RSS figures for the server, the full INFO memory and
  # the allocator's own stats print, then a clean shutdown.
  grep -E '^(VmHWM|VmRSS):' "/proc/$SERVER_PID/status" | tr -s ' ' | tee "$D/vm.txt"
  "$CLI" -p "$PORT" info memory | tr -d '\r' > "$D/info-memory.txt"
  "$CLI" -p "$PORT" info stats | tr -d '\r' > "$D/info-stats.txt"
  "$CLI" -p "$PORT" memory malloc-stats > "$D/malloc-stats.txt" 2>&1 || true
  "$CLI" -p "$PORT" dbsize | tr -d '\r' > "$D/dbsize.txt"
  "$CLI" -p "$PORT" shutdown nosave >/dev/null 2>&1 || true
  wait "$SERVER_PID" 2>/dev/null || true; SERVER_PID=
  cp "$D/samples.csv" "$RESULT_DIR/$DATE-$ARCH-$SERVER-soak-$alloc.csv"
  echo "$alloc: $(wc -l < "$RUNS") generator runs, $(wc -l < "$D/samples.csv") samples; cgroup memory.peak MiB write $(cgroup_peak_mib "$D" write) delete $(cgroup_peak_mib "$D" delete) settle $(cgroup_peak_mib "$D" settle)"
}

# ---- summary helpers -------------------------------------------------------

# phase_stat ALLOC PHASE TOOL COL: mean (COL=rps) or median (COL=p99) over runs
phase_stat() {
  awk -F, -v ph="$2" -v tool="$3" -v col="$4" '
    NR>1 && $1==ph && $2==tool { if (col=="rps") { s+=$7; n++ } else { v[++n]=$8 } }
    END {
      if (!n) { print "-"; exit }
      if (col=="rps") { printf "%.0f", s/n; exit }
      for (i=2;i<=n;i++) { t=v[i]; for (j=i-1;j>=1&&v[j]>t;j--) v[j+1]=v[j]; v[j+1]=t }
      print v[int((n+1)/2)]
    }' "$OUT/$1/runs.csv"
}
# sample_stat ALLOC WHAT: peak_rss (max used_memory_rss), end_<phase> (last used_memory_rss in phase),
# frag_end (last allocator_frag_ratio), defrag_hits (last active_defrag_hits); bytes are printed in MiB
sample_stat() {
  awk -F, -v what="$2" '
    NR>1 {
      if ($5+0 > peak) peak=$5+0
      last_rss[$3]=$5; frag=$11; hits=$14
    }
    END {
      if (what=="peak_rss") printf "%.1f", peak/1048576
      else if (what ~ /^end_/) { p=substr(what,5); if (p in last_rss) printf "%.1f", last_rss[p]/1048576; else print "-" }
      else if (what=="frag_end") print frag
      else if (what=="defrag_hits") print hits
    }' "$OUT/$1/samples.csv"
}
vmhwm() { awk '$1=="VmHWM:" {printf "%.1f", $2/1024}' "$OUT/$1/vm.txt"; }

# The jemalloc server is built before any soak runs: a build inside the
# container leaves hundreds of MiB of page cache charged to the cgroup, and
# the per-phase cgroup peak would carry it for whichever soak came after
# the build (jemalloc's write-phase peak 311 MiB above challoc's at equal
# VmHWM).
# Every build (the clients' too) is made here, before the first soak.
{ [ $CB_MODE = 1 ] || [ "$SERVER" != valkey ]; } && kv_ensure_jemalloc
for alloc in $ALLOCS; do server_for "$alloc"; done
for alloc in $ALLOCS; do soak "$alloc"; done

# SAVE_BASELINE=1: keep the jemalloc half for later BASELINES=cached runs.
if [ "$SAVE_BASELINE" = 1 ]; then
  case " $ALLOCS " in *" jemalloc "*) ;; *) echo "valkey-soak: SAVE_BASELINE=1 but jemalloc not in ALLOCS" >&2; exit 1;; esac
  mkdir -p "$BASELINE_DIR"
  cp "$OUT/jemalloc/samples.csv" "$BASE.csv"
  cp "$OUT/jemalloc/runs.csv" "$BASE.runs.csv"
  cp "$OUT/jemalloc/vm.txt" "$BASE.vm.txt"
  cp "$OUT/jemalloc/dbsize.txt" "$BASE.dbsize.txt"
  cp "$OUT/jemalloc/version.txt" "$BASE.version.txt"
  for ph in write delete settle; do for sfx in "" .file; do rm -f "$BASE.cgpeak.$ph$sfx"; [ -f "$OUT/jemalloc/cgpeak.$ph$sfx" ] && cp "$OUT/jemalloc/cgpeak.$ph$sfx" "$BASE.cgpeak.$ph$sfx"; done; done
  echo "date=$(date -u +%FT%TZ) commit=$COMMIT duration=$DURATION settle=$SETTLE interval=$INTERVAL kernel=$(uname -r) nproc=$(nproc) host=${HOST_CPU:-unknown}" > "$BASE.meta"
  echo "== jemalloc soak cached as $BASE.{csv,runs.csv,vm.txt,dbsize.txt,version.txt,meta}"
fi
# BASELINES=cached: materialise the cached jemalloc half where the summary
# helpers expect a soaked allocator's files.
CACHE_NOTE=
if [ "$BASELINES" = cached ]; then
  mkdir -p "$OUT/jemalloc"
  cp "$BASE.csv" "$OUT/jemalloc/samples.csv"
  cp "$BASE.runs.csv" "$OUT/jemalloc/runs.csv"
  cp "$BASE.vm.txt" "$OUT/jemalloc/vm.txt"
  cp "$BASE.dbsize.txt" "$OUT/jemalloc/dbsize.txt"
  cp "$BASE.version.txt" "$OUT/jemalloc/version.txt"
  for ph in write delete settle; do for sfx in "" .file; do [ -f "$BASE.cgpeak.$ph$sfx" ] && cp "$BASE.cgpeak.$ph$sfx" "$OUT/jemalloc/cgpeak.$ph$sfx"; done; done
  meta=$(cat "$BASE.meta")
  CACHE_NOTE="the jemalloc column is CACHED, not from this run: $BASE.csv, recorded $(echo "$meta" | tr ' ' '\n' | sed -n 's/^date=//p') at commit $(echo "$meta" | tr ' ' '\n' | sed -n 's/^commit=//p') with DURATION=$(echo "$meta" | tr ' ' '\n' | sed -n 's/^duration=//p') SETTLE=$(echo "$meta" | tr ' ' '\n' | sed -n 's/^settle=//p') (this run: DURATION=$DURATION SETTLE=$SETTLE)"
  case $meta in *"duration=$DURATION "*) ;; *) CACHE_NOTE="$CACHE_NOTE; the phase lengths differ, so the end-of-phase RSS rows compare different points of the drain and only the throughput, p99 and peak rows are like for like";; esac
fi

# RESULT_JSONL (cb mode, or any caller): one line per allocator with the
# summary's figures as numbers (missing ones omitted).
if [ -n "${RESULT_JSONL:-}" ]; then
  for a in $ALLOCS; do
    {
      printf '{"schema":"challoc.bench.driver/v1","driver":"%s-soak","alloc":"%s","metrics":{' "$SERVER" "$a"
      sep=
      for kv in \
        "write_vb_rps=$(phase_stat "$a" write valkey-benchmark rps)" "write_mt_rps=$(phase_stat "$a" write memtier rps)" \
        "delete_vb_rps=$(phase_stat "$a" delete valkey-benchmark rps)" "delete_mt_rps=$(phase_stat "$a" delete memtier rps)" \
        "write_vb_p99_ms=$(phase_stat "$a" write valkey-benchmark p99)" "write_mt_p99_ms=$(phase_stat "$a" write memtier p99)" \
        "delete_vb_p99_ms=$(phase_stat "$a" delete valkey-benchmark p99)" "delete_mt_p99_ms=$(phase_stat "$a" delete memtier p99)" \
        "vmhwm_mib=$(vmhwm "$a")" "rss_peak_mib=$(sample_stat "$a" peak_rss)" \
        "rss_write_mib=$(sample_stat "$a" end_write)" "rss_delete_mib=$(sample_stat "$a" end_delete)" "rss_settle_mib=$(sample_stat "$a" end_settle)" \
        "frag_settle=$(sample_stat "$a" frag_end)" "defrag_hits=$(sample_stat "$a" defrag_hits)" "keys=$(cat "$OUT/$a/dbsize.txt" 2>/dev/null)" \
        "cgpeak_write_mib=$(cgroup_peak_mib "$OUT/$a" write)" "cgpeak_delete_mib=$(cgroup_peak_mib "$OUT/$a" delete)" "cgpeak_settle_mib=$(cgroup_peak_mib "$OUT/$a" settle)" \
        "refill_vb_rps=$(phase_stat "$a" refill valkey-benchmark rps)" "refill_mt_rps=$(phase_stat "$a" refill memtier rps)" \
        "refill_vb_p99_ms=$(phase_stat "$a" refill valkey-benchmark p99)" "refill_mt_p99_ms=$(phase_stat "$a" refill memtier p99)" \
        "rss_refill_mib=$(cat "$OUT/$a/refill_rss" 2>/dev/null)"; do
        k=${kv%%=*}; v=${kv#*=}
        case $v in ''|-|*[!0-9.eE+-]*) continue;; esac
        printf '%s"%s":%s' "$sep" "$k" "$v"; sep=,
      done
      printf '}}\n'
    } >> "$RESULT_JSONL"
  done
fi

step "summary -> $RESULT"
cmp=0
case " $SUMMARY_ALLOCS " in *" challoc "*) case " $SUMMARY_ALLOCS " in *" jemalloc "*) cmp=1;; esac;; esac
ALLOCS=$SUMMARY_ALLOCS
{
  echo "# challoc $SERVER soak, $DATE, $ARCH"
  echo
  echo "- commit: $COMMIT"
  echo "- host: ${HOST_CPU:-unknown CPU}, $(nproc) CPUs visible to the container"
  echo "- kernel: $(uname -r), container: Wolfi"
  for alloc in $ALLOCS; do echo "- $alloc server: $(cat "$OUT/$alloc/version.txt") (build: $(cat "$OUT/$alloc/build.txt" 2>/dev/null || echo cached); activedefrag $(cat "$OUT/$alloc/activedefrag.txt" 2>/dev/null || echo -))"; done
  echo "- clients: $(basename "$VB") from $(dirname "$(dirname "$VB")"), $(memtier_benchmark --version 2>&1 | head -1)"
  [ -n "${CHALLOC_CONF:-}" ] && echo "- challoc server MALLOC_CONF: $CHALLOC_CONF (CHALLOC_CONF; the jemalloc server runs stock)"
  [ -n "${STOCK_CONF:-}" ] && echo "- $ALLOCS server conf: $(kv_conf_env "$ALLOCS" "$STOCK_CONF")"
  for alloc in $ALLOCS; do [ -f "$OUT/$alloc/lib.txt" ] && echo "- $alloc allocator library (path, sha256 prefix): $(cat "$OUT/$alloc/lib.txt")${CHALLOC_LIB:+$([ "$alloc" = challoc ] && echo " (CHALLOC_LIB=$CHALLOC_LIB, an A/B build preloaded under the soname)")}"; done
  # shellcheck disable=SC2086 # the first word of ALLOCS, whatever whitespace separates them
  echo "- workload: write phase $((DURATION / 2)) s (valkey-benchmark -t set -d $SIZE -r $KEYS -n $VB_REQS -c $VB_CLIENTS -P $PIPELINE, then memtier SET-only $MT_SIZES B over $KEYS keys, $MT_THREADS x $MT_CLIENTS clients, -P $PIPELINE, $CHUNK s), delete phase $((DURATION / 2)) s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET $DEL_RATIO:1), settle $SETTLE s idle; activedefrag $DEFRAG; io-threads $(cat "$OUT/$(echo $ALLOCS | cut -d' ' -f1)/io-threads.txt" 2>/dev/null || echo "${IO_THREADS:-default}"); INFO sampled every $INTERVAL s; memtier key pattern: $KEY_PATTERN$([ "$KEY_PATTERN" = uniform ] && echo " (--randomize, time-seeded random keys in both phases; NOT the acceptance protocol)" || echo " (memtier's fixed-seed key stream, the acceptance protocol)")"
  echo "- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase"
  echo "- $(cgroup_peak_note)"
  if [ "$CG_PEAK_STATE" = yes ]; then
    for a in $ALLOCS; do
      [ -f "$OUT/$a/cgpeak.write.file" ] && echo "  - $a: cgroup page cache (memory.stat file) at the end of the write / delete / settle phases: $(cgroup_peak_file_mib "$OUT/$a" write) / $(cgroup_peak_file_mib "$OUT/$a" delete) / $(cgroup_peak_file_mib "$OUT/$a" settle) MiB"
    done
  fi
  [ -z "$CACHE_NOTE" ] || echo "- CACHED BASELINE: $CACHE_NOTE. Acceptance uses the same-run \`make valkey-soak\`."
  [ "$OCCUPANCY" = 1 ] && echo "- occupancy: MEMORY MALLOC-STATS every $MS_INTERVAL s under $RESULT_DIR/$DATE-$ARCH-$SERVER-soak-<alloc>-mstats/"
  echo
  echo "## Summary"
  echo
  hdr="| measure |"; sep="|---|"
  for a in $ALLOCS; do hdr="$hdr $a |"; sep="$sep---:|"; done
  [ $cmp = 1 ] && { hdr="$hdr challoc vs jemalloc |"; sep="$sep---:|"; }
  cmp_old=0
  case " $ALLOCS " in *" challoc-old "*) [ $cmp = 1 ] && { cmp_old=1; hdr="$hdr challoc-old vs jemalloc |"; sep="$sep---:|"; };; esac
  echo "$hdr"; echo "$sep"
  row() { # label higher_is_better (1, 0, or 2 for no comparison) values...
    label=$1; hib=$2; shift 2
    r="| $label |"; c=; j=; o=; i=0
    for a in $ALLOCS; do
      i=$((i+1)); v=$(eval echo \$$i)
      r="$r $v |"
      [ "$a" = challoc ] && c=$v; [ "$a" = jemalloc ] && j=$v; [ "$a" = challoc-old ] && o=$v
    done
    if [ $cmp = 1 ]; then
      case $hib in
        1) r="$r $(pct_of "$j" "$c") |";;
        0) r="$r $(pct_of "$c" "$j") |";;
        *) r="$r - |";;
      esac
    fi
    if [ $cmp_old = 1 ]; then
      case $hib in
        1) r="$r $(pct_of "$j" "$o") |";;
        0) r="$r $(pct_of "$o" "$j") |";;
        *) r="$r - |";;
      esac
    fi
    echo "$r"
  }
  for ph in write delete; do
    for tool in valkey-benchmark memtier; do
      set --; for a in $ALLOCS; do set -- "$@" "$(phase_stat "$a" $ph $tool rps)"; done
      row "$ph: $tool req/s" 1 "$@"
      set --; for a in $ALLOCS; do set -- "$@" "$(phase_stat "$a" $ph $tool p99)"; done
      row "$ph: $tool p99 ms" 0 "$@"
    done
  done
  set --; for a in $ALLOCS; do set -- "$@" "$(vmhwm "$a")"; done;               row "peak RSS MiB (VmHWM)" 0 "$@"
  for ph in write delete settle; do
    set --; for a in $ALLOCS; do set -- "$@" "$(cgroup_peak_mib "$OUT/$a" $ph)"; done; row "cgroup memory.peak MiB, $ph phase" 0 "$@"
  done
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" peak_rss)"; done;  row "peak sampled used_memory_rss MiB" 0 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" end_write)"; done; row "RSS MiB at end of write phase" 0 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" end_delete)"; done; row "RSS MiB at end of delete phase" 0 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" end_settle)"; done; row "RSS MiB after settle" 0 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" frag_end)"; done;  row "allocator_frag_ratio after settle" 0 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" defrag_hits)"; done; row "active_defrag_hits" 1 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(cat "$OUT/$a/dbsize.txt")"; done;   row "keys after settle" 2 "$@"
  echo
  echo "Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance: throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.$([ -n "$CACHE_NOTE" ] && echo " The jemalloc figures here are cached ($(echo "$meta" | tr ' ' '\n' | sed -n 's/^date=//p')), so this table is a signal, not an acceptance result.")"
  echo
  echo "## Samples"
  echo
  for a in $ALLOCS; do
    if [ -n "$CACHE_NOTE" ] && [ "$a" = jemalloc ]; then echo "- jemalloc: \`$BASE.csv\` (cached, $(($(wc -l < "$BASE.csv") - 1)) rows)"
    else echo "- $a: \`$RESULT_DIR/$DATE-$ARCH-$SERVER-soak-$a.csv\` ($(($(wc -l < "$OUT/$a/samples.csv") - 1)) rows)"; fi
  done
  echo
  echo "## Generator runs"
  echo
  for a in $ALLOCS; do
    echo "### $a"; echo; echo '```'; cat "$OUT/$a/runs.csv"; echo '```'; echo
  done
} > "$RESULT"
echo "== wrote $RESULT"
sed -n '/^## Summary/,/^Positive/p' "$RESULT"

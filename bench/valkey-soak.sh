#!/bin/sh
# Valkey soak (docs/plan.md, Phase 1 task 8), run inside the dev container:
# `make valkey-soak` or `make run CMD='sh bench/valkey-soak.sh'`.
#
# For each allocator in ALLOCS a valkey-server is started from the matching
# Valkey build (challoc: the patched tree from contrib/valkey/build.sh;
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
# four Phase 1 acceptance measures.
#
# The valkey-benchmark and valkey-cli binaries come from the challoc tree for
# every allocator (same client binary, only the server changes);
# memtier_benchmark is Wolfi's package.
#
#   ALLOCS="challoc jemalloc"  servers to soak, in order
#   CHALLOC_CONF=              MALLOC_CONF for the challoc server alone (plan
#                              Phase 2.5 task 8: percpu_arena:percpu soaks the
#                              per-CPU mode against the same jemalloc, which a
#                              MALLOC_CONF in the environment would reach too)
#   DURATION=1200              load seconds per allocator (write + delete)
#   SETTLE=60                  idle seconds after the delete phase
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
#                              phase, see bench/results/2026-09-17-aarch64-gce-compacting-cache-soak3.md);
#                              uniform: `--randomize` (time-seeded) with random key
#                              patterns (`--key-pattern=R:R`, `--command-key-pattern=R`)
#                              in both phases, so no key is structurally pinned
#   VB_CLIENTS=50 MT_THREADS=4 MT_CLIENTS=10 PIPELINE=16
#   DEFRAG=yes                 --activedefrag on the server
#   IO_THREADS=                --io-threads N on the server (Phase 3 prerequisite (d):
#                              with 4, the frees of one thread's allocations land in
#                              other threads' caches); empty = Valkey's default (1),
#                              the acceptance protocol. Recorded in the summary header
#                              and the results file name gets no suffix: pass RESULT_DIR.
#   PORT=6399
#   OUT=target/valkey/soak     scratch and raw logs
#   RESULT_DIR=bench/results   <date>-<arch>-valkey-soak.md and
#                              <date>-<arch>-valkey-soak-<alloc>.csv land here
#   OCCUPANCY=0                1: a sidecar dumps INFO memory's allocator lines and
#                              MEMORY MALLOC-STATS (per-class slab occupancy) every
#                              MS_INTERVAL (20) seconds into
#                              RESULT_DIR/<date>-<arch>-valkey-soak-<alloc>-mstats/<t>.txt
#   BASELINES=cached           do not soak jemalloc; take its column from the cached
#                              soak under target/baselines/valkey-soak-jemalloc-<arch>.*
#                              (written by SAVE_BASELINE=1, `make soak-baseline`). The
#                              summary says so, with the cache's date and DURATION.
#                              `make soak-quick` uses this; acceptance never does.
#   SAVE_BASELINE=0            1: after soaking jemalloc, store its samples CSV, runs
#                              CSV, VmHWM, dbsize, version and a .meta line under
#                              target/baselines/ for BASELINES=cached
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
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
OUT=${OUT:-$ROOT/target/valkey/soak}
RESULT_DIR=${RESULT_DIR:-$ROOT/bench/results}
OCCUPANCY=${OCCUPANCY:-0}
MS_INTERVAL=${MS_INTERVAL:-20}
BASELINES=${BASELINES:-}
SAVE_BASELINE=${SAVE_BASELINE:-0}
BASELINE_DIR=$ROOT/target/baselines
VALKEY_TAG=${VALKEY_TAG:-9.1.2}
WORK=$ROOT/target/valkey
SRC=$WORK/src-$VALKEY_TAG
SRC_JE=$WORK/src-$VALKEY_TAG-jemalloc
CLI=$SRC/src/valkey-cli
VB=$SRC/src/valkey-benchmark
ARCH=$(uname -m)
DATE=$(date -u +%F)
# COMMIT overrides git's answer (a tree synced to a VM from a git worktree has no usable .git).
COMMIT=${COMMIT:-$(git -c safe.directory='*' rev-parse --short HEAD 2>/dev/null || echo unknown)}
RESULT=$RESULT_DIR/$DATE-$ARCH-valkey-soak.md
BASE=$BASELINE_DIR/valkey-soak-jemalloc-$ARCH

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

[ -x "$CLI" ] && [ -x "$VB" ] && [ -x "$SRC/src/valkey-server" ] \
  || { echo "valkey-soak: $SRC is not built; run 'make valkey STAGE=build' first" >&2; exit 1; }
command -v memtier_benchmark >/dev/null || { echo "valkey-soak: memtier_benchmark not installed" >&2; exit 1; }

# One protocol run at a time (see bench/run.sh).
exec 9>"$ROOT/target/bench.lock"
flock -n 9 || { echo "another bench run holds target/bench.lock; wait for it or check docker ps" >&2; exit 1; }
rm -rf "$OUT"
mkdir -p "$OUT" "$RESULT_DIR"

# The jemalloc baseline: the same tag, unpatched, with Valkey's bundled
# jemalloc 5.3.0. Cloned from the local checkout (the patch is uncommitted
# there, so the clone is pristine) and built once.
ensure_jemalloc_server() {
  [ -x "$SRC_JE/src/valkey-server" ] && return
  # To stderr: server_for runs this inside $(...), and a line on stdout
  # would become part of the server path (the 64 KiB-page soak of Phase
  # 2.5 task 4d hit this on its first jemalloc build).
  step "building the jemalloc baseline in $SRC_JE" >&2
  [ -d "$SRC_JE/.git" ] || git -c safe.directory='*' clone -q "$SRC" "$SRC_JE"
  ( cd "$SRC_JE" && make -j"$(nproc)" MALLOC=jemalloc BUILD_TLS=yes all ) \
    > "$WORK/logs/build-jemalloc-$ARCH.log" 2>&1 \
    || { tail -30 "$WORK/logs/build-jemalloc-$ARCH.log" >&2; exit 1; }
}

server_for() {
  case $1 in
    challoc)  echo "$SRC/src/valkey-server";;
    jemalloc) ensure_jemalloc_server; echo "$SRC_JE/src/valkey-server";;
    *) echo "valkey-soak: unknown allocator $1" >&2; exit 1;;
  esac
}
expected_allocator() {
  case $1 in challoc) echo jemalloc-5.3.0-challoc;; jemalloc) echo jemalloc-5.3.0;; esac
}

SERVER_PID=
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null
  [ -n "${SAMPLER_PID:-}" ] && kill "$SAMPLER_PID" 2>/dev/null
  [ -n "${OCC_PID:-}" ] && kill "$OCC_PID" 2>/dev/null
  :
}
trap cleanup EXIT INT TERM

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
# The occupancy sidecar (OCCUPANCY=1, formerly bench/probes/soak-occupancy.sh):
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
  # `|| exit 1`: a failure inside the $(...) subshell (the jemalloc baseline
  # build) does not stop the script by itself under set -e.
  server=$(server_for "$alloc") || exit 1
  step "$alloc: $server"
  "$server" --version | tee "$D/version.txt"
  # $IO_THREADS unquoted on purpose: empty means no flag. CHALLOC_CONF
  # reaches the challoc server only.
  conf_env=
  case $alloc in challoc) [ -n "${CHALLOC_CONF:-}" ] && conf_env="MALLOC_CONF=$CHALLOC_CONF";; esac
  env $conf_env "$server" --port "$PORT" --save "" --appendonly no --daemonize no \
    --logfile "$D/server.log" --activedefrag "$DEFRAG" ${IO_THREADS:+--io-threads $IO_THREADS} >/dev/null 2>&1 &
  SERVER_PID=$!
  i=0
  until [ "$("$CLI" -p "$PORT" ping 2>/dev/null)" = PONG ]; do
    i=$((i+1)); [ $i -gt 100 ] && { echo "valkey-soak: $server did not start; see $D/server.log" >&2; exit 1; }
    sleep 0.1
  done
  got=$(info_field memory mem_allocator)
  [ "$got" = "$(expected_allocator "$alloc")" ] \
    || { echo "valkey-soak: $alloc server reports mem_allocator=$got" >&2; exit 1; }
  "$CLI" -p "$PORT" config get activedefrag | tail -1 > "$D/activedefrag.txt"
  "$CLI" -p "$PORT" config get jemalloc-bg-thread | tail -1 > "$D/bg-thread.txt"
  "$CLI" -p "$PORT" config get io-threads | tail -1 > "$D/io-threads.txt"
  echo "mem_allocator=$got activedefrag=$(cat "$D/activedefrag.txt") jemalloc-bg-thread=$(cat "$D/bg-thread.txt") io-threads=$(cat "$D/io-threads.txt") pid=$SERVER_PID"

  T0=$(now)
  echo write > "$D/phase"; rm -f "$D/last" "$D/stop"
  sampler "$D" "$T0" "$D/samples.csv" &
  SAMPLER_PID=$!
  OCC_PID=
  if [ "$OCCUPANCY" = 1 ]; then
    occupancy_sampler "$D" "$T0" "$RESULT_DIR/$DATE-$ARCH-valkey-soak-$alloc-mstats" &
    OCC_PID=$!
    echo "occupancy sampler: MEMORY MALLOC-STATS every ${MS_INTERVAL}s into $RESULT_DIR/$DATE-$ARCH-valkey-soak-$alloc-mstats/"
  fi

  half=$((DURATION / 2))
  echo "-- write phase, ${half}s"
  while [ $(($(now) - T0)) -lt $half ]; do write_phase; printf .; done; echo
  echo "$(($(now) - T0))" > "$D/write_end"
  echo delete > "$D/phase"; rm -f "$D/last"
  echo "-- delete phase, ${half}s"
  while [ $(($(now) - T0)) -lt $DURATION ]; do delete_phase; printf .; done; echo
  echo "$(($(now) - T0))" > "$D/delete_end"
  echo settle > "$D/phase"; rm -f "$D/last"
  echo "-- settle, ${SETTLE}s"
  sleep "$SETTLE"
  # Stop the periodic sampler, then take the post-settle sample explicitly so
  # the summary's "after settle" row exists even when SETTLE < INTERVAL.
  touch "$D/stop"; wait "$SAMPLER_PID" 2>/dev/null || true; SAMPLER_PID=
  [ -z "$OCC_PID" ] || { wait "$OCC_PID" 2>/dev/null || true; OCC_PID=; }
  sample_once "$D" "$T0" "$D/samples.csv"

  # Final state: kernel RSS figures for the server, the full INFO memory and
  # the allocator's own stats print, then a clean shutdown.
  grep -E '^(VmHWM|VmRSS):' "/proc/$SERVER_PID/status" | tr -s ' ' | tee "$D/vm.txt"
  "$CLI" -p "$PORT" info memory | tr -d '\r' > "$D/info-memory.txt"
  "$CLI" -p "$PORT" info stats | tr -d '\r' > "$D/info-stats.txt"
  "$CLI" -p "$PORT" memory malloc-stats > "$D/malloc-stats.txt" 2>&1 || true
  "$CLI" -p "$PORT" dbsize | tr -d '\r' > "$D/dbsize.txt"
  "$CLI" -p "$PORT" shutdown nosave >/dev/null 2>&1 || true
  wait "$SERVER_PID" 2>/dev/null || true; SERVER_PID=
  cp "$D/samples.csv" "$RESULT_DIR/$DATE-$ARCH-valkey-soak-$alloc.csv"
  echo "$alloc: $(wc -l < "$RUNS") generator runs, $(wc -l < "$D/samples.csv") samples"
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
# pct A B: (A/B-1)*100 signed, "-" if either is missing
pct() { awk -v a="$1" -v b="$2" 'BEGIN {if(a=="-"||b=="-"||a==""||b==""||b==0)print "-";else printf "%+.1f%%", (a/b-1)*100}'; }

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
  meta=$(cat "$BASE.meta")
  CACHE_NOTE="the jemalloc column is CACHED, not from this run: $BASE.csv, recorded $(echo "$meta" | tr ' ' '\n' | sed -n 's/^date=//p') at commit $(echo "$meta" | tr ' ' '\n' | sed -n 's/^commit=//p') with DURATION=$(echo "$meta" | tr ' ' '\n' | sed -n 's/^duration=//p') SETTLE=$(echo "$meta" | tr ' ' '\n' | sed -n 's/^settle=//p') (this run: DURATION=$DURATION SETTLE=$SETTLE)"
  case $meta in *"duration=$DURATION "*) ;; *) CACHE_NOTE="$CACHE_NOTE; the phase lengths differ, so the end-of-phase RSS rows compare different points of the drain and only the throughput, p99 and peak rows are like for like";; esac
fi

step "summary -> $RESULT"
cmp=0
case " $SUMMARY_ALLOCS " in *" challoc "*) case " $SUMMARY_ALLOCS " in *" jemalloc "*) cmp=1;; esac;; esac
ALLOCS=$SUMMARY_ALLOCS
{
  echo "# challoc Valkey soak, $DATE, $ARCH"
  echo
  echo "- commit: $COMMIT"
  echo "- host: ${HOST_CPU:-unknown CPU}, $(nproc) CPUs visible to the container"
  echo "- kernel: $(uname -r), container: Wolfi"
  for alloc in $ALLOCS; do echo "- $alloc server: $(cat "$OUT/$alloc/version.txt")"; done
  echo "- clients: valkey-benchmark from the challoc tree, $(memtier_benchmark --version 2>&1 | head -1)"
  [ -n "${CHALLOC_CONF:-}" ] && echo "- challoc server MALLOC_CONF: $CHALLOC_CONF (CHALLOC_CONF; the jemalloc server runs stock)"
  echo "- workload: write phase $((DURATION / 2)) s (valkey-benchmark -t set -d $SIZE -r $KEYS -n $VB_REQS -c $VB_CLIENTS -P $PIPELINE, then memtier SET-only $MT_SIZES B over $KEYS keys, $MT_THREADS x $MT_CLIENTS clients, -P $PIPELINE, $CHUNK s), delete phase $((DURATION / 2)) s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET $DEL_RATIO:1), settle $SETTLE s idle; activedefrag $DEFRAG; io-threads $(cat "$OUT/$(echo $ALLOCS | cut -d' ' -f1)/io-threads.txt" 2>/dev/null || echo "${IO_THREADS:-default}"); INFO sampled every $INTERVAL s; memtier key pattern: $KEY_PATTERN$([ "$KEY_PATTERN" = uniform ] && echo " (--randomize, time-seeded random keys in both phases; NOT the acceptance protocol)" || echo " (memtier's fixed-seed key stream, the acceptance protocol)")"
  echo "- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase"
  [ -z "$CACHE_NOTE" ] || echo "- CACHED BASELINE: $CACHE_NOTE. Acceptance uses the same-run \`make valkey-soak\`."
  [ "$OCCUPANCY" = 1 ] && echo "- occupancy: MEMORY MALLOC-STATS every $MS_INTERVAL s under $RESULT_DIR/$DATE-$ARCH-valkey-soak-<alloc>-mstats/"
  echo
  echo "## Summary"
  echo
  hdr="| measure |"; sep="|---|"
  for a in $ALLOCS; do hdr="$hdr $a |"; sep="$sep---:|"; done
  [ $cmp = 1 ] && { hdr="$hdr challoc vs jemalloc |"; sep="$sep---:|"; }
  echo "$hdr"; echo "$sep"
  row() { # label higher_is_better (1, 0, or 2 for no comparison) values...
    label=$1; hib=$2; shift 2
    r="| $label |"; c=; j=; i=0
    for a in $ALLOCS; do
      i=$((i+1)); v=$(eval echo \$$i)
      r="$r $v |"
      [ "$a" = challoc ] && c=$v; [ "$a" = jemalloc ] && j=$v
    done
    if [ $cmp = 1 ]; then
      case $hib in
        1) r="$r $(pct "$j" "$c") |";;
        0) r="$r $(pct "$c" "$j") |";;
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
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" peak_rss)"; done;  row "peak sampled used_memory_rss MiB" 0 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" end_write)"; done; row "RSS MiB at end of write phase" 0 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" end_delete)"; done; row "RSS MiB at end of delete phase" 0 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" end_settle)"; done; row "RSS MiB after settle" 0 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" frag_end)"; done;  row "allocator_frag_ratio after settle" 0 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(sample_stat "$a" defrag_hits)"; done; row "active_defrag_hits" 1 "$@"
  set --; for a in $ALLOCS; do set -- "$@" "$(cat "$OUT/$a/dbsize.txt")"; done;   row "keys after settle" 2 "$@"
  echo
  echo "Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.$([ -n "$CACHE_NOTE" ] && echo " The jemalloc figures here are cached ($(echo "$meta" | tr ' ' '\n' | sed -n 's/^date=//p')), so this table is a signal, not an acceptance result.")"
  echo
  echo "## Samples"
  echo
  for a in $ALLOCS; do
    if [ -n "$CACHE_NOTE" ] && [ "$a" = jemalloc ]; then echo "- jemalloc: \`$BASE.csv\` (cached, $(($(wc -l < "$BASE.csv") - 1)) rows)"
    else echo "- $a: \`$RESULT_DIR/$DATE-$ARCH-valkey-soak-$a.csv\` ($(($(wc -l < "$OUT/$a/samples.csv") - 1)) rows)"; fi
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

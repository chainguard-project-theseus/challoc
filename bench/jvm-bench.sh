#!/bin/sh
# JVM benchmark rows (docs/plan.md Phase 3 task 5 (a)): Wolfi's openjdk-21
# from the image, measured under six allocator variants via LD_PRELOAD (and
# one glibc tunable), in Ruby's protocol shape (bench/ruby-bench.sh). Runs
# inside the dev container: `make jvm-bench`, or on a VM
# `bench/gce/challoc-bench make arm64 jvm-bench HOST_KIND=...`.
#
# Rows:
#   startup      `java -version`, one process per round under bench/rt: wall
#                time (s) and max RSS (KiB); ROUNDS timed rounds after one
#                discarded warm-up, allocators interleaved, sorted lists.
#   server       bench/jvm/HttpBench.java on com.sun.net.httpserver with a
#                fixed THREADS-thread executor, -Xms/-Xmx HEAP and
#                -XX:+AlwaysPreTouch (the Java heap is constant and resident
#                from the start under every allocator, so RSS differs by native
#                memory only), served for DURATION seconds per allocator, once,
#                to two `ab` processes (Wolfi's apache2-utils; wrk and oha are
#                not in Wolfi, checked 2026-09-20) of CONNECTIONS/2 keep-alive
#                connections each, one on /json (a ~300-byte body built per
#                request) and one on /blob (64 KiB copied out of a per-request
#                ByteBuffer.allocateDirect, one malloc(65536) per request in
#                the JVM, freed in bursts when the Cleaner runs after a GC),
#                after a WARMUP-second discarded warm-up load. Sampled every
#                INTERVAL seconds from /proc/<pid>/smaps_rollup (Rss, Pss,
#                Anonymous) and /proc/<pid>/status (VmRSS, VmHWM): idle RSS
#                (before the load), peak and final sampled Rss, VmHWM; req/s
#                and p99 (ms) per endpoint from ab; GC pauses from -Xlog:gc;
#                the final /proc/<pid>/smaps Rss by mapping name ([heap],
#                challoc's named regions, unnamed anonymous, file-backed) and
#                the allocator's own stats print at exit (MALLOC_CONF=
#                stats_print:true, MIMALLOC_SHOW_STATS=1) go to OUT for
#                attribution.
#
# Allocators (ALLOCS): challoc (REV, default the working tree, built into
# target/jvm/lib), glibc (no preload), glibc-arena2 (no preload,
# MALLOC_ARENA_MAX=2: the fleet's usual mitigation for glibc's per-thread
# arenas), jemalloc (Wolfi's /usr/lib/libjemalloc.so.2), mimalloc
# (bench/build.sh's libmimalloc.so), tcmalloc (Wolfi's tcmalloc-minimal).
# Every variant's /proc/self/maps is checked once before the rounds: exactly
# the expected library is mapped into the JVM.
#
#   ROUNDS=5       timed startup rounds after one discarded warm-up
#   DURATION=300   measured server seconds per allocator (the plan's 5 minutes)
#   WARMUP=15      discarded warm-up load seconds before the measured window
#   INTERVAL=5     RSS sample period (s)
#   CONNECTIONS=64 total ab connections (half per endpoint)
#   THREADS=200    executor threads     HEAP=512m   -Xms and -Xmx
#   PORT=18080     the server's port on 127.0.0.1
#   ROWS="startup server"
#   RESULT=path    markdown output (default bench/results/<date>-<arch>-jvm.md)
#   OUT=dir        raw logs, samples and score files (default target/jvm/bench-out)
#   REV=tree       challoc rev (any git rev; "tree" = the working tree)
#   HOST_CPU, HOST_KIND   recorded in the header (the Makefile passes HOST_CPU)
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
. "$ROOT/bench/lib.sh"
ALLOCS=${ALLOCS:-"challoc glibc glibc-arena2 jemalloc mimalloc tcmalloc"}
ROUNDS=${ROUNDS:-5}
ROWS=${ROWS:-"startup server"}
DURATION=${DURATION:-300}
WARMUP=${WARMUP:-15}
INTERVAL=${INTERVAL:-5}
CONNECTIONS=${CONNECTIONS:-64}
THREADS=${THREADS:-200}
HEAP=${HEAP:-512m}
PORT=${PORT:-18080}
REV=${REV:-tree}
WORK=$ROOT/target/jvm
OUT=${OUT:-$WORK/bench-out}
RESULT=${RESULT:-$ROOT/bench/results/$DATE-$ARCH-jvm.md}
# -Dsun.net.httpserver.nodelay=true sets TCP_NODELAY on the server's sockets
# (off by default in the JDK server): without it the small /json response
# (headers and body in two writes) waits for the client's delayed ACK and
# every request takes 40 ms, measuring the ACK timer instead of the
# allocator (775 req/s at p99 44 ms against 76k req/s on /blob, Mac
# container, 2026-09-20).
JAVA_OPTS=${JAVA_OPTS:-"-Xms$HEAP -Xmx$HEAP -XX:+AlwaysPreTouch -Dsun.net.httpserver.nodelay=true"}
IDLE_SECS=${IDLE_SECS:-10}
T_START=$(date +%s)
LOAD0=$(loadavg)

take_bench_lock
ensure_bench_programs
for a in $ALLOCS; do case $a in challoc|glibc-arena2) ;; *) is_stock "$a" || die "unknown allocator '$a'";; esac; done
for row in $ROWS; do case $row in startup|server) ;; *) die "unknown row '$row' (startup|server)";; esac; done
rm -rf "$OUT"; mkdir -p "$OUT" "$(dirname "$RESULT")"

# ---- the JDK and the generator ------------------------------------------------
# Wolfi's openjdk-NN installs under /usr/lib/jvm/java-NN-openjdk and does not
# put java on PATH; take the newest one unless JAVA_HOME says otherwise.
if [ -n "${JAVA_HOME:-}" ]; then JAVA=$JAVA_HOME/bin/java; JAVAC=$JAVA_HOME/bin/javac
else
  JAVA=$(ls -d /usr/lib/jvm/java-*-openjdk/bin/java 2>/dev/null | sort -V | tail -1); JAVAC=${JAVA%/java}/javac
fi
[ -x "${JAVA:-}" ] || die "no JDK under /usr/lib/jvm (rebuild the image from Dockerfile.dev: openjdk-21)"
[ -x "$JAVAC" ] || die "no javac beside $JAVA (openjdk-21 is the JDK, not the -jre package)"
JAVA_VERSION=$("$JAVA" -version 2>&1)
JDK_PKG=$(apk list --installed 2>/dev/null | awk '/^openjdk-[0-9]+-[0-9]/ {print $1}' | head -1)
AB=$(command -v ab || true)
[ -n "$AB" ] || die "no ab (apache2-utils) in this container; rebuild the image from Dockerfile.dev"
AB_VERSION=$("$AB" -V 2>&1 | head -1)
# The server class, compiled once (again when the source is newer).
CLASSES=$WORK/classes
if [ ! -f "$CLASSES/HttpBench.class" ] || [ "$ROOT/bench/jvm/HttpBench.java" -nt "$CLASSES/HttpBench.class" ]; then
  mkdir -p "$CLASSES"
  "$JAVAC" -d "$CLASSES" "$ROOT/bench/jvm/HttpBench.java" || die "javac failed on bench/jvm/HttpBench.java"
fi

# ---- the challoc library --------------------------------------------------------
CHALLOC_LIB=
case " $ALLOCS " in *" challoc "*)
  rev=$(resolve_rev "$REV")
  printf '== building challoc (%s) into target/jvm/lib ... ' "$([ "$rev" = tree ] && echo "working tree, HEAD $(commit_short)$(tree_dirty && echo ' + uncommitted src changes')" || echo "$rev" | cut -c1-12)"
  build_lib "$rev" "$WORK/lib"; CHALLOC_LIB=$WORK/lib/release/libchalloc.so
  echo "done, $(so_hash "$CHALLOC_LIB")";;
esac
lib_of() { case $1 in challoc) echo "$CHALLOC_LIB";; glibc-arena2) echo "";; *) stock_lib "$1";; esac; }
# env_of ALLOC [server]: the extra environment of a variant (LD_PRELOAD and
# tunables). The server row adds the stats prints at exit (they only act at
# exit and are ignored by the other allocators); the startup row must not
# pay for formatting a stats report inside its 100 ms.
# The server row also preloads bench/probes/statsdump.so after the
# allocator: an atexit hook calling malloc_stats_print where the allocator
# exports it (challoc, jemalloc), since challoc's stats_print key is inert.
env_of() {
  lib=$(lib_of "$1")
  if [ "${2:-}" = server ]; then
    printf 'LD_PRELOAD=%s MALLOC_CONF=stats_print:true MIMALLOC_SHOW_STATS=1' "${lib:+$lib:}$STATSDUMP"
  else
    printf '%s' "${lib:+LD_PRELOAD=$lib}"
  fi
  case $1 in glibc-arena2) printf ' MALLOC_ARENA_MAX=2';; esac
  :
}
STATSDUMP=$BIN/statsdump.so
[ "$STATSDUMP" -nt "$ROOT/bench/probes/statsdump.c" ] || cc -O2 -Wall -Wextra -Werror -shared -fPIC -o "$STATSDUMP" "$ROOT/bench/probes/statsdump.c" -ldl || die "statsdump.c did not compile"

# ---- binding check ----------------------------------------------------------------
echo "== java: $(echo "$JAVA_VERSION" | head -1) ($JAVA, Wolfi ${JDK_PKG:-openjdk}); generator: $AB_VERSION"
for a in $ALLOCS; do
  mapped=$(env $(env_of "$a") "$JAVA" -cp "$CLASSES" HttpBench maps | tr '\n' ' ' | sed 's/ $//')
  lib=$(lib_of "$a"); want=; [ -n "$lib" ] && want=$(readlink -f "$lib")
  [ "$mapped" = "$want" ] || die "$a: java maps '$mapped', expected '${want:-nothing}'"
  echo "   $a: maps ${mapped:-no allocator library (glibc malloc)}$(case $a in glibc-arena2) echo ', MALLOC_ARENA_MAX=2';; esac)"
done

# ---- startup row -------------------------------------------------------------------
# run_startup ALLOC: appends "wall_s maxrss_kb exit" to $OUT/startup.ALLOC.scores.
run_startup() {
  a=$1; tag=startup.$a
  [ -f "$OUT/$tag.scores" ] || : > "$OUT/$tag.scores"
  n=$(wc -l < "$OUT/$tag.scores")
  log=$OUT/$tag.$n.out; rt=$OUT/$tag.$n.rt
  RT_OUT=$rt "$BIN/rt" env $(env_of "$a") "$JAVA" -version > "$log" 2>&1 || true
  if [ -s "$rt" ] && [ "$(wc -l < "$rt")" = 1 ]; then
    read -r el _us _sy rss ex < "$rt"
  else
    el=-; rss=-; ex=1; echo "  $tag run $n: rt wrote $(wc -l < "$rt" 2>/dev/null || echo 0) lines, expected 1; see $log" >&2
  fi
  [ "$ex" = 0 ] || { el=-; echo "  $tag run $n failed (exit $ex), see $log" >&2; }
  echo "$el $rss $ex" >> "$OUT/$tag.scores"
}
has_row() { case " $ROWS " in *" $1 "*) return 0;; *) return 1;; esac; }
if has_row startup; then
  echo "== startup: $ROUNDS rounds + warm-up x $(echo $ALLOCS | wc -w) allocators ($ALLOCS); load $LOAD0"
  r=0
  while [ $r -le "$ROUNDS" ]; do
    for a in $ALLOCS; do run_startup "$a"; done
    r=$((r+1))
  done
  printf '   startup rounds done, load %s, %s\n' "$(loadavg)" "$(date -u +%T)"
  for a in $ALLOCS; do
    n=$(wc -l < "$OUT/startup.$a.scores")
    [ "$n" -eq $((ROUNDS+1)) ] || die "$OUT/startup.$a.scores has $n lines, expected $((ROUNDS+1)): concurrent run?"
  done
fi

# ---- server row ---------------------------------------------------------------------
now() { date +%s; }
# sample PID DIR CSV T0: one line "elapsed,phase,rss_kb,pss_kb,anon_kb,vmrss_kb,vmhwm_kb"; 1 when the process is gone.
sample() {
  [ -r "/proc/$1/smaps_rollup" ] || return 1
  awk -v t="$(( $(now) - $4 ))" -v ph="$(cat "$2/phase")" '
    FILENAME ~ /smaps_rollup/ { if ($1=="Rss:") r=$2; if ($1=="Pss:") p=$2; if ($1=="Anonymous:") an=$2 }
    FILENAME ~ /status/ { if ($1=="VmRSS:") vr=$2; if ($1=="VmHWM:") hw=$2 }
    END { printf "%d,%s,%d,%d,%d,%d,%d\n", t, ph, r, p, an, vr, hw }' "/proc/$1/smaps_rollup" "/proc/$1/status" >> "$3" 2>/dev/null || return 1
}
sampler() { # PID DIR CSV T0
  echo "elapsed_s,phase,rss_kb,pss_kb,anon_kb,vmrss_kb,vmhwm_kb" > "$3"
  while [ ! -e "$2/stop" ]; do sample "$1" "$2" "$3" "$4" || break; sleep "$INTERVAL"; done
}
# smaps_by_name PID: "kb name" lines, Rss summed per mapping name: [heap],
# [stack], the named anonymous regions verbatim ([anon:challoc:classes] ...),
# "anon" for unnamed anonymous mappings, "file" for file-backed ones and
# "other" for the remaining bracket names ([vdso], [vvar]), descending.
smaps_by_name() {
  awk '
    /^[0-9a-f]+-[0-9a-f]+ / { name = (NF >= 6) ? $6 : "anon"; if (name ~ /^\//) name = "file"; else if (name ~ /^\[/ && name !~ /^\[(anon:|heap\]|stack\])/) name = "other" }
    $1 == "Rss:" { rss[name] += $2 }
    END { for (n in rss) printf "%d %s\n", rss[n], n }' "/proc/$1/smaps" 2>/dev/null | sort -rn
}
# cpu_ticks PID: utime+stime of the process (all threads) in clock ticks.
cpu_ticks() { awk '{print $14 + $15}' "/proc/$1/stat" 2>/dev/null || echo 0; }
CLK_TCK=$(getconf CLK_TCK 2>/dev/null || echo 100)
# ab_run ALLOC PATH CONNS SECS LOG: one ab on http://127.0.0.1:PORT/PATH.
# -k keep-alive, -r keep going on socket errors, -l accept varying body
# lengths (the JSON body's length varies per request); -n after -t (ab sizes
# its per-request array from -n, and -t alone caps it at 50000 requests).
# Under bench/rt so the generator's own CPU time is on record ($LOG.rt: a
# user+sys near the wall time means ab, not the server, was the limit).
ab_run() { RT_OUT=$5.rt "$BIN/rt" "$AB" -q -k -r -l -c "$3" -t "$4" -n 50000000 "http://127.0.0.1:$PORT/$2" > "$5" 2>&1 || echo "ab exit $? ($1 $2)" >> "$5"; }
ab_kv() { # ab_kv LOG KEY: Requests per second | 99% | Failed requests | Non-2xx responses | Complete requests | Keep-Alive requests
  case $2 in
    rps)   awk '/^Requests per second:/ {printf "%.0f", $4}' "$1";;
    p99)   awk '$1 == "99%" {print $2}' "$1";;
    fail)  awk '/^Failed requests:/ {print $3}' "$1";;
    non2xx) awk '/^Non-2xx responses:/ {print $3}' "$1";;
    done)  awk '/^Complete requests:/ {print $3}' "$1";;
    ka)    awk '/^Keep-Alive requests:/ {print $3}' "$1";;
  esac
}
# fetch PATH: one GET, the body on stdout (the image has python3, no wget or curl).
fetch() { python3 -c 'import sys,urllib.request; sys.stdout.write(urllib.request.urlopen(sys.argv[1], timeout=10).read().decode(errors="replace"))' "http://127.0.0.1:$PORT/$1" 2>/dev/null; }
# run_server ALLOC: one DURATION-second server row; everything under $OUT/server.ALLOC.*
run_server() {
  a=$1; d=$OUT/server.$a; mkdir -p "$d"
  echo idle > "$d/phase"; rm -f "$d/stop"
  # The environment for the JVM: the allocator, the stats prints at exit,
  # GC log to a file. stderr carries the stats print at exit.
  env $(env_of "$a" server) "$JAVA" $JAVA_OPTS "-Xlog:gc:file=$d/gc.log" -cp "$CLASSES" HttpBench serve "$PORT" "$THREADS" > "$d/stdout" 2> "$d/stderr" &
  pid=$!
  i=0; until grep -q '^READY' "$d/stdout" 2>/dev/null; do
    i=$((i+1)); if [ $i -gt 300 ] || ! kill -0 "$pid" 2>/dev/null; then echo "  server ($a) did not start; stderr:" >&2; tail -5 "$d/stderr" >&2; echo "$a start-failed" >> "$OUT/server.failed"; wait "$pid" 2>/dev/null || true; return 0; fi
    sleep 0.1
  done
  t0=$(now)
  [ "$(fetch json | head -c 1)" = "{" ] || echo "  warning: /json did not answer as expected ($a)" >&2
  sampler "$pid" "$d" "$d/samples.csv" "$t0" &
  spid=$!
  sleep "$IDLE_SECS"
  sample "$pid" "$d" "$d/samples.csv" "$t0" || true   # the idle figure is the last idle sample
  cp "$d/samples.csv" "$d/idle.csv"
  echo "  $a: server pid $pid up, idle Rss $(tail -1 "$d/samples.csv" | cut -d, -f3) KiB; warm-up ${WARMUP}s, then ${DURATION}s of load ($(date -u +%T))"
  half=$((CONNECTIONS / 2))
  echo warmup > "$d/phase"
  ab_run "$a" json "$half" "$WARMUP" "$d/warmup-json.ab" & w1=$!
  ab_run "$a" blob "$half" "$WARMUP" "$d/warmup-blob.ab" & w2=$!
  wait $w1 $w2
  echo load > "$d/phase"
  c0=$(cpu_ticks "$pid"); w0=$(now)
  ab_run "$a" json "$half" "$DURATION" "$d/json.ab" & l1=$!
  ab_run "$a" blob "$half" "$DURATION" "$d/blob.ab" & l2=$!
  wait $l1 $l2
  c1=$(cpu_ticks "$pid"); w1=$(now)
  # The server's CPU seconds over the measured window and the wall seconds
  # it spanned; CPU per request is what an allocator costs even when the
  # generator is the throughput limit.
  awk -v c0="$c0" -v c1="$c1" -v w0="$w0" -v w1="$w1" -v tck="$CLK_TCK" 'BEGIN {printf "%.2f %d\n", (c1-c0)/tck, w1-w0}' > "$d/cpu.txt"
  echo final > "$d/phase"
  sleep 2
  # The final state before the shutdown: one more sample, the smaps
  # breakdown by mapping name, VmHWM.
  touch "$d/stop"; wait "$spid" 2>/dev/null || true
  sample "$pid" "$d" "$d/samples.csv" "$t0" || true
  smaps_by_name "$pid" > "$d/smaps-by-name.txt"
  grep -E '^(VmHWM|VmRSS|Threads):' "/proc/$pid/status" | tr -s ' ' > "$d/vm.txt" 2>/dev/null || true
  fetch quit > /dev/null || true
  i=0; while [ -d "/proc/$pid" ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done
  [ -d "/proc/$pid" ] && { echo "  $a: server did not exit on /quit, killing" >&2; kill "$pid" 2>/dev/null || true; }
  if wait "$pid" 2>/dev/null; then ex=0; else ex=$?; fi
  echo "$ex" > "$d/exit"
  echo "     json $(ab_kv "$d/json.ab" rps) req/s p99 $(ab_kv "$d/json.ab" p99) ms, blob $(ab_kv "$d/blob.ab" rps) req/s p99 $(ab_kv "$d/blob.ab" p99) ms (keep-alive $(ab_kv "$d/json.ab" ka) + $(ab_kv "$d/blob.ab" ka) of $(ab_kv "$d/json.ab" done) + $(ab_kv "$d/blob.ab" done)); VmHWM $(awk '$1=="VmHWM:" {print $2}' "$d/vm.txt") KiB, exit $ex"
}
if has_row server; then
  echo "== server: $DURATION s per allocator after $WARMUP s warm-up, $THREADS threads, $JAVA_OPTS, ab -c $((CONNECTIONS/2)) x2 (/json, /blob); load $(loadavg)"
  for a in $ALLOCS; do run_server "$a"; done
  printf '   server rows done, load %s, %s\n' "$(loadavg)" "$(date -u +%T)"
fi

# ---- report ---------------------------------------------------------------------------
list_of() { sorted_scores "$OUT/$1.$2.scores" 1; }
rss_of() { median_rss "$OUT/$1.$2.scores" 1; }
med_of() { median_list "$(list_of "$1" "$2")"; }
# server figures, all "-" when the row did not run
srv_csv() { echo "$OUT/server.$1/samples.csv"; }
srv_idle_kb() { awk -F, 'NR>1 && $2=="idle" {v=$3} END {print v ? v : "-"}' "$(srv_csv "$1")" 2>/dev/null || echo -; }
srv_peak_kb() { awk -F, 'NR>1 && $3>m {m=$3} END {print m ? m : "-"}' "$(srv_csv "$1")" 2>/dev/null || echo -; }
srv_final_kb() { awk -F, 'NR>1 {v=$3} END {print v ? v : "-"}' "$(srv_csv "$1")" 2>/dev/null || echo -; }
srv_hwm_kb() { awk '$1=="VmHWM:" {print $2}' "$OUT/server.$1/vm.txt" 2>/dev/null | grep . || echo -; }
srv_ab() { [ -f "$OUT/server.$1/$2.ab" ] && ab_kv "$OUT/server.$1/$2.ab" "$3" | grep . || echo "${4:--}"; }
srv_cpu_s() { awk '{print $1}' "$OUT/server.$1/cpu.txt" 2>/dev/null | grep . || echo -; }
# srv_cpu_us ALLOC: server CPU microseconds per request over the measured window.
srv_cpu_us() {
  awk -v c="$(srv_cpu_s "$1")" -v j="$(srv_ab "$1" json done)" -v b="$(srv_ab "$1" blob done)" 'BEGIN {if (c=="-"||j=="-"||b=="-"||j+b==0) print "-"; else printf "%.1f", c*1e6/(j+b)}'
}
# srv_gen_cpu ALLOC: the two generators' user+sys over their wall time, in
# percent of one CPU each ("json%/blob%"); near 100 means ab was the limit.
srv_gen_cpu() {
  for e in json blob; do awk '{if ($1 > 0) printf "%.0f", ($2+$3)/$1*100; else print "-"}' "$OUT/server.$1/$e.ab.rt" 2>/dev/null || printf '%s' -; printf '/'; done | sed 's,/$,,'
}
srv_gcs() { f=$OUT/server.$1/gc.log; if [ -f "$f" ]; then awk '/Pause/ {n++} END {print n+0}' "$f"; else echo -; fi; }
srv_exit() { cat "$OUT/server.$1/exit" 2>/dev/null || echo -; }
srv_name_kb() { awk -v n="$2" '$2==n {print $1}' "$OUT/server.$1/smaps-by-name.txt" 2>/dev/null | grep . || echo 0; }
srv_named() { awk '$2 ~ /^\[anon:/ {n=$2; sub(/^\[anon:/, "", n); sub(/\]$/, "", n); printf "%s=%.1f ", n, $1/1024}' "$OUT/server.$1/smaps-by-name.txt" 2>/dev/null | sed 's/ $//' | grep . || echo "none (kernel without anonymous VMA names, or no allocator names its mappings)"; }
# pct_lower A B / pct_higher A B: challoc (A) against B, positive = challoc worse
pct_lower() { pct_of "$1" "$2"; }
pct_higher() { pct_of "$2" "$1"; }
describe() {
  case $1 in
    challoc)  echo "LD_PRELOAD=$CHALLOC_LIB ($([ "$REV" = tree ] && echo "working tree, HEAD $(commit_short)$(tree_dirty && echo ' + uncommitted src changes')" || echo "$REV"), sha256 $(so_hash "$CHALLOC_LIB"))";;
    glibc)    echo "no LD_PRELOAD ($(stock_version glibc) malloc, default arenas: up to 8 x CPUs)";;
    glibc-arena2) echo "no LD_PRELOAD, MALLOC_ARENA_MAX=2 ($(stock_version glibc) malloc; the fleet's usual mitigation for the per-thread arenas)";;
    jemalloc) echo "LD_PRELOAD=$JEMALLOC_LIB (Wolfi $(stock_version jemalloc))";;
    mimalloc) echo "LD_PRELOAD=$BIN/libmimalloc.so ($(stock_version mimalloc), bench/build.sh)";;
    tcmalloc) echo "LD_PRELOAD=$TCMALLOC_LIB (Wolfi $(stock_version tcmalloc), gperftools without profiler hooks)";;
  esac
}
has() { case " $ALLOCS " in *" $1 "*) return 0;; *) return 1;; esac; }
{
  echo "# JVM benchmark rows, $DATE, $ARCH"
  echo
  echo "- commit: $(commit_short)$(tree_dirty && echo ' (uncommitted changes in src/)')"
  echo "- date: $STAMP"
  echo "- host: ${HOST_CPU:-unknown CPU}, $PROCS CPUs visible to the container${HOST_KIND:+; $HOST_KIND}"
  echo "- kernel: $(uname -r), container: Wolfi"
  echo "- java: $JAVA (Wolfi ${JDK_PKG:-openjdk}); \`java -version\`:"
  echo "$JAVA_VERSION" | sed 's/^/      /'
  echo "- generator: $AB_VERSION (Wolfi apache2-utils $(apk_version apache2-utils)); wrk and oha are not in Wolfi"
  echo "- load average at start: $LOAD0; at end: $(loadavg); $(( $(date +%s) - T_START )) s total"
  echo "- method: startup = \`java -version\`, $ROUNDS timed rounds after one discarded warm-up, allocators interleaved per round, one process per run under bench/rt, every run's wall time listed sorted, wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB). server = bench/jvm/HttpBench.java (com.sun.net.httpserver, a fixed $THREADS-thread executor, \`$JAVA_OPTS\`, so the Java heap is constant and resident from the start and RSS differs by native memory only) served once per allocator for $DURATION s to two \`ab -k -c $((CONNECTIONS/2))\` processes (one on /json, a ~300-byte body built per request; one on /blob, 64 KiB copied out of a per-request ByteBuffer.allocateDirect, i.e. one malloc(65536) per request in the JVM, freed in bursts when the Cleaner runs after a GC) after $WARMUP s of discarded warm-up load; RSS sampled every $INTERVAL s from /proc/<pid>/smaps_rollup (Rss) and /proc/<pid>/status (VmHWM): idle = the last sample before the load (${IDLE_SECS} s after the server answered), peak = the largest sample, final = the last sample after the load; req/s and p99 (ms) are ab's per endpoint over the measured window; GCs = pauses in -Xlog:gc over the whole process. Percentages are challoc against the other allocator: positive means challoc is slower, larger or serves fewer requests."
  echo "- allocators:"
  for a in $ALLOCS; do echo "  - $a: $(describe "$a")"; done
  if has_row startup; then
    echo
    echo "## Sorted scores (\`java -version\` wall seconds)"
    echo
    echo "| row | allocator | sorted wall s | maxrss KiB |"
    echo "|---|---|---|---:|"
    for a in $ALLOCS; do
      flag=; [ "$(any_failed "$OUT/startup.$a.scores" 1)" = 0 ] || flag=" (FAILED)"
      echo "| startup | $a | $(list_of startup "$a")$flag | $(rss_of startup "$a") |"
    done
    echo
    echo "## Medians (startup)"
    echo
    hdr="| row |"; sep="|---|"
    for a in $ALLOCS; do hdr="$hdr $a s | $a RSS MiB |"; sep="$sep---:|---:|"; done
    if has challoc; then for a in $ALLOCS; do [ "$a" = challoc ] || { hdr="$hdr challoc vs $a time | RSS |"; sep="$sep---:|---:|"; }; done; fi
    echo "$hdr"; echo "$sep"
    line="| startup |"; deltas=
    for a in $ALLOCS; do line="$line $(med_of startup "$a") | $(kib_to_mib "$(rss_of startup "$a")") |"; done
    if has challoc; then
      cm=$(med_of startup challoc); cr=$(rss_of startup challoc)
      for a in $ALLOCS; do [ "$a" = challoc ] || deltas="$deltas $(pct_of "$cm" "$(med_of startup "$a")") | $(pct_of "$cr" "$(rss_of startup "$a")") |"; done
    fi
    echo "$line$deltas"
  fi
  if has_row server; then
    echo
    echo "## HTTP server, $DURATION s per allocator ($THREADS threads, \`$JAVA_OPTS\`, ab -k -c $((CONNECTIONS/2)) on /json and on /blob)"
    echo
    echo "| allocator | idle Rss MiB | peak Rss MiB | final Rss MiB | VmHWM MiB | json req/s | json p99 ms | blob req/s | blob p99 ms | server CPU s | CPU us/req | requests (json + blob) | keep-alive | failed | non-2xx | GCs | ab CPU % | exit |"
    echo "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    for a in $ALLOCS; do
      echo "| $a | $(kib_to_mib "$(srv_idle_kb "$a")") | $(kib_to_mib "$(srv_peak_kb "$a")") | $(kib_to_mib "$(srv_final_kb "$a")") | $(kib_to_mib "$(srv_hwm_kb "$a")") | $(srv_ab "$a" json rps) | $(srv_ab "$a" json p99) | $(srv_ab "$a" blob rps) | $(srv_ab "$a" blob p99) | $(srv_cpu_s "$a") | $(srv_cpu_us "$a") | $(srv_ab "$a" json done) + $(srv_ab "$a" blob done) | $(srv_ab "$a" json ka) + $(srv_ab "$a" blob ka) | $(srv_ab "$a" json fail) + $(srv_ab "$a" blob fail) | $(srv_ab "$a" json non2xx 0) + $(srv_ab "$a" blob non2xx 0) | $(srv_gcs "$a") | $(srv_gen_cpu "$a") | $(srv_exit "$a") |"
    done
    if has challoc; then
      echo
      echo "### challoc against each allocator (server row; positive = challoc worse)"
      echo
      hdr="| metric |"; sep="|---|"
      for a in $ALLOCS; do [ "$a" = challoc ] || { hdr="$hdr vs $a |"; sep="$sep---:|"; }; done
      echo "$hdr"; echo "$sep"
      for m in "idle Rss:srv_idle_kb:lower" "peak Rss:srv_peak_kb:lower" "final Rss:srv_final_kb:lower" "VmHWM:srv_hwm_kb:lower" "json req/s:json rps:higher" "json p99:json p99:lower" "blob req/s:blob rps:higher" "blob p99:blob p99:lower" "server CPU us/req:srv_cpu_us:lower"; do
        name=${m%%:*}; rest=${m#*:}; fn=${rest%%:*}; dir=${rest#*:}
        val() { case $fn in srv_*) $fn "$1";; *) srv_ab "$1" ${fn%% *} ${fn#* };; esac; }
        cv=$(val challoc); line="| $name |"
        for a in $ALLOCS; do [ "$a" = challoc ] || line="$line $(pct_$dir "$cv" "$(val "$a")") |"; done
        echo "$line"
      done
    fi
    echo
    echo "### Final Rss by mapping (MiB, /proc/<pid>/smaps at the last sample; \"anon\" is unnamed anonymous memory: the Java heap, metaspace, code cache, thread stacks and every allocator's unnamed arenas; challoc names its regions)"
    echo
    echo "| allocator | [heap] | anon | file-backed | named anonymous regions |"
    echo "|---|---:|---:|---:|---|"
    for a in $ALLOCS; do
      echo "| $a | $(kib_to_mib "$(srv_name_kb "$a" '[heap]')") | $(kib_to_mib "$(srv_name_kb "$a" anon)") | $(kib_to_mib "$(srv_name_kb "$a" file)") | $(srv_named "$a") |"
    done
    echo
    echo "\"server CPU s\" is the server process's user+sys over the measured window (/proc/<pid>/stat), \"CPU us/req\" that divided by the requests both generators completed: the allocator's cost per request even where the generator is the throughput limit. \"ab CPU %\" is each generator's user+sys over its wall time (json/blob; near 100 means ab, not the server, set the rate). p99 is ab's integer-millisecond percentile."
    echo
    echo "Raw: $OUT/server.<allocator>/{samples.csv,json.ab,blob.ab,gc.log,smaps-by-name.txt,cpu.txt,stderr} (stderr carries the allocator's own stats at exit: malloc_stats_print for challoc and jemalloc through bench/probes/statsdump.so, mimalloc's MIMALLOC_SHOW_STATS report)."
    [ -f "$OUT/server.failed" ] && { echo; echo "FAILED server rows: $(tr '\n' ' ' < "$OUT/server.failed")"; }
  fi
} > "$RESULT"
echo "== wrote $RESULT"
sed -n '/^## Medians/,$p' "$RESULT"

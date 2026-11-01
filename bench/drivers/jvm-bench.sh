#!/bin/sh
# JVM benchmark rows: Wolfi's openjdk-21
# from the image, measured under seven allocator variants via LD_PRELOAD (and
# two glibc tunings). The jvm-server and jvm-soak rows of bench/cb run it
# (cb mode below); by hand, inside the dev container: `make run CMD='sh
# bench/drivers/jvm-bench.sh'` (ROWS=soak for the soak).
#
# Rows:
#   startup      `java -version`, one process per round under bench/rt: wall
#                time (s) and max RSS (KiB); ROUNDS timed rounds after one
#                discarded warm-up, allocators interleaved, sorted lists.
#   server       bench/drivers/jvm/HttpBench.java on com.sun.net.httpserver with a
#                fixed THREADS-thread executor, -Xms/-Xmx HEAP and
#                -XX:+AlwaysPreTouch (the Java heap is constant and resident
#                from the start under every allocator, so RSS differs by native
#                memory only), served for DURATION seconds per allocator, once,
#                to two `ab` processes (Wolfi's apache2-utils; wrk and oha are
#                not in Wolfi) of CONNECTIONS/2 keep-alive
#                connections each, one on /json (a ~300-byte body built per
#                request) and one on /blob (64 KiB copied out of a per-request
#                ByteBuffer.allocateDirect, one malloc(65536) per request in
#                the JVM, freed in bursts when the Cleaner runs after a GC),
#                after a WARMUP-second discarded warm-up load. Sampled every
#                INTERVAL seconds from /proc/<pid>/smaps_rollup (Rss, Pss,
#                Anonymous) and /proc/<pid>/status (VmRSS, VmHWM, Threads): idle
#                RSS (before the load), peak and final sampled Rss, VmHWM; req/s
#                and p99 (ms) per endpoint from ab; GC pauses from -Xlog:gc;
#                the final /proc/<pid>/smaps Rss by mapping name ([heap],
#                challoc's named regions, unnamed anonymous, file-backed) and
#                the allocator's own stats print at exit (MALLOC_CONF=
#                stats_print:true, MIMALLOC_SHOW_STATS=1) go to OUT for
#                attribution. The JVM runs with -XX:NativeMemoryTracking=NMT
#                (summary; NMT=off disables) and `jcmd <pid> VM.native_memory
#                summary` is taken at the peak-RSS sample (whenever a sample
#                sets a new peak, at most once per NMT_GAP seconds; the last
#                one taken is the peak snapshot) and at the end: NMT's total
#                committed, the malloc total and the Java Heap, Class, Thread,
#                Code, GC, Compiler, Internal, Other categories' committed,
#                and RSS minus NMT committed at the same instant, the
#                "allocator overhead" column (the JVM asked for NMT committed
#                bytes; everything above that is the allocator's own: caching,
#                fragmentation, retained pages, metadata). NMT itself costs a
#                16-byte header per malloc plus its tables ("tracking
#                overhead" in its report), the same bytes for every allocator;
#                the header does move a malloc(65536) to 65552 bytes, across a
#                power-of-two class boundary in every size-class allocator, so
#                the NMT'd server row's peaks are compared with an NMT-less
#                server run.
#   soak         the shape that can separate the allocators (a 5-minute run
#                with one buffer size never lets glibc's arenas fragment): HttpBench in `soak` mode,
#                a ThreadPoolExecutor of SOAK_CORE core / SOAK_MAX maximum
#                threads retired after SOAK_KEEPALIVE s idle, /blob's body drawn
#                per request log-uniform between SOAK_MIN and SOAK_MAXB bytes
#                (direct ByteBuffers of mixed sizes), the two generators
#                alternating SOAK_WINDOW-second windows at SOAK_HIGH total
#                connections and at SOAK_LOW (threads are created and retired
#                all run long), SOAK_DURATION seconds per allocator, RSS sampled
#                every INTERVAL s into a CSV per allocator (copied beside the
#                results file) and summarised as peak, final and the slope over
#                the last SOAK_TAIL seconds (MiB per minute, least squares:
#                growth after warm-up is the fragmentation signal); req/s, p99
#                and the server's CPU per request per window from ab; NMT at
#                the end; the same smaps and stats prints as the server row.
#                ROWS=soak runs alone and writes RESULT (default
#                target/results/<date>-<arch>-jvm-soak.md) with the series as
#                <RESULT without .md>-<allocator>.csv.
#
# Allocators (ALLOCS): challoc (REV, default the working tree, built into
# target/jvm/lib), glibc (no preload), glibc-arena2 (no preload,
# MALLOC_ARENA_MAX=2: the fleet's usual mitigation for glibc's per-thread
# arenas), glibc-trim (no preload, the JVM's own -XX:TrimNativeHeapInterval=
# TRIM_MS: a periodic malloc_trim(0) from the JVM's service thread, what a
# well-tuned Java image ships today; JDK 21.0.12 accepts the flag, see the
# check at the start), jemalloc (Wolfi's /usr/lib/libjemalloc.so.2), mimalloc
# (bench/build.sh's libmimalloc.so), tcmalloc (Wolfi's tcmalloc-minimal).
# Every variant's /proc/self/maps is checked once before the rounds: exactly
# the expected library is mapped into the JVM. The soak's default set is
# SOAK_ALLOCS (challoc, glibc, glibc-arena2, glibc-trim, jemalloc).
# challoc-<label> is the same challoc library under a configuration:
# CHALLOC_VARIANTS lists "<label>=<conf>" pairs (jemalloc's key:value,key:value
# grammar) and the variant runs with CHALLOC_CONF=<conf> in its environment
# (read after MALLOC_CONF, so it wins over the harness's stats_print), e.g.
# CHALLOC_VARIANTS="hd2000=huge_dirty_decay_ms:2000" ALLOCS="challoc
# challoc-hd2000 glibc-trim" (the huge-decay rows). The report
# compares plain challoc against every other variant, its own included.
# challoc-trim is challoc under the JVM's TrimNativeHeapInterval (the
# glibc-trim flags): the JVM calls challoc's malloc_trim(0) every TRIM_MS,
# a periodic full purge regardless of age, the economics glibc-trim runs on.
#
#   ROUNDS=5       timed startup rounds after one discarded warm-up
#   DURATION=300   measured server seconds per allocator (the plan's 5 minutes)
#   WARMUP=15      discarded warm-up load seconds before the measured window
#   INTERVAL=5     RSS sample period (s)
#   CONNECTIONS=64 total ab connections (half per endpoint)
#   THREADS=200    executor threads     HEAP=512m   -Xms and -Xmx
#   PORT=18080     the server's port on 127.0.0.1
#   NMT=summary    -XX:NativeMemoryTracking level (off disables the snapshots)
#   NMT_GAP=30     least seconds between two peak snapshots
#   TRIM_MS=5000   glibc-trim's -XX:TrimNativeHeapInterval
#   ROWS="startup server"   or ROWS=soak (alone)
#   SOAK_DURATION=1200 SOAK_WINDOW=60 SOAK_HIGH=256 SOAK_LOW=8 SOAK_CORE=16
#   SOAK_MAX=400 SOAK_KEEPALIVE=2 SOAK_MIN=256 SOAK_MAXB=1048576 SOAK_TAIL=600
#   RESULT=path    markdown output (default target/results/<date>-<arch>-jvm[-soak].md)
#   OUT=dir        raw logs, samples and score files (default target/jvm/bench-out,
#                  target/jvm/soak-out for ROWS=soak: each run starts with rm -rf OUT)
#   REV=tree       challoc rev (any git rev; "tree" = the working tree)
#   CHALLOC_VARIANTS="<label>=<conf> ..."   the configurations behind the challoc-<label> variants
#   HOST_CPU, HOST_KIND   recorded in the header (the Makefile passes HOST_CPU)
#
# cb mode (CB_ALLOC set; the `jvm-*` driver rows of bench/cb, bench/README.md
# "Drivers"): one allocator per invocation (challoc or a stock allocator),
# the variant's library from CB_LIB instead of a build of REV, CB_CONF as
# challoc's CHALLOC_CONF, and the figures of the rows run as one JSON line
# to RESULT_JSONL. ROWS picks the rows as above.
set -eu
cd "$(dirname "$0")/../.."
ROOT=$(pwd)
. "$ROOT/bench/drivers/lib.sh"
ROWS=${ROWS:-"startup server"}
CB_MODE=0
[ -z "${CB_ALLOC:-}" ] || { CB_MODE=1; ALLOCS=$CB_ALLOC; OUT=${CB_OUT:?}/out; RESULT=$CB_OUT/jvm.md; }
case " $ROWS " in *" soak "*) [ "$ROWS" = soak ] || die "ROWS=soak runs alone (its report has its own shape)"; SOAK=1;; *) SOAK=0;; esac
if [ $SOAK = 1 ]; then ALLOCS=${ALLOCS:-${SOAK_ALLOCS:-"challoc glibc glibc-arena2 glibc-trim jemalloc"}}
else ALLOCS=${ALLOCS:-"challoc glibc glibc-arena2 glibc-trim jemalloc mimalloc tcmalloc"}; fi
ROUNDS=${ROUNDS:-5}
DURATION=${DURATION:-300}
WARMUP=${WARMUP:-15}
INTERVAL=${INTERVAL:-5}
CONNECTIONS=${CONNECTIONS:-64}
THREADS=${THREADS:-200}
HEAP=${HEAP:-512m}
PORT=${PORT:-18080}
NMT=${NMT:-summary}
NMT_GAP=${NMT_GAP:-30}
TRIM_MS=${TRIM_MS:-5000}
SOAK_DURATION=${SOAK_DURATION:-1200}
SOAK_WINDOW=${SOAK_WINDOW:-60}
SOAK_HIGH=${SOAK_HIGH:-256}
SOAK_LOW=${SOAK_LOW:-8}
SOAK_CORE=${SOAK_CORE:-16}
SOAK_MAX=${SOAK_MAX:-400}
SOAK_KEEPALIVE=${SOAK_KEEPALIVE:-2}
SOAK_MIN=${SOAK_MIN:-256}
SOAK_MAXB=${SOAK_MAXB:-1048576}
SOAK_TAIL=${SOAK_TAIL:-600}
REV=${REV:-tree}
CHALLOC_VARIANTS=${CHALLOC_VARIANTS:-}
WORK=$ROOT/target/jvm
# conf_of ALLOC: the CHALLOC_CONF of a challoc-<label> variant from
# CHALLOC_VARIANTS; empty for plain challoc, dies on a label with no pair.
conf_of() {
  case $1 in challoc-trim) return 0;; challoc-*) ;; *) return 0;; esac
  label=${1#challoc-}
  for pair in $CHALLOC_VARIANTS; do
    [ "${pair%%=*}" = "$label" ] && { printf '%s' "${pair#*=}"; return 0; }
  done
  die "allocator '$1' has no CHALLOC_VARIANTS entry '$label=<conf>'"
}
is_challoc() { case $1 in challoc|challoc-*) return 0;; *) return 1;; esac; }
# The soak's raw output lives apart from the other rows' (each run starts
# with rm -rf OUT, and a server row after a soak must not erase the soak's
# stats prints and NMT snapshots before they are read).
if [ $SOAK = 1 ]; then OUT=${OUT:-$WORK/soak-out}; else OUT=${OUT:-$WORK/bench-out}; fi
if [ $SOAK = 1 ]; then RESULT=${RESULT:-$ROOT/target/results/$DATE-$ARCH-jvm-soak.md}
else RESULT=${RESULT:-$ROOT/target/results/$DATE-$ARCH-jvm.md}; fi
# -Dsun.net.httpserver.nodelay=true sets TCP_NODELAY on the server's sockets
# (off by default in the JDK server): without it the small /json response
# (headers and body in two writes) waits for the client's delayed ACK and
# every request takes 40 ms, measuring the ACK timer instead of the
# allocator (775 req/s at p99 44 ms against 76k req/s on /blob).
JAVA_OPTS=${JAVA_OPTS:-"-Xms$HEAP -Xmx$HEAP -XX:+AlwaysPreTouch -Dsun.net.httpserver.nodelay=true"}
IDLE_SECS=${IDLE_SECS:-10}
T_START=$(date +%s)
LOAD0=$(loadavg)

take_bench_lock
ensure_bench_programs
for a in $ALLOCS; do case $a in challoc|glibc-arena2|glibc-trim|challoc-trim) ;; challoc-*) conf_of "$a" > /dev/null;; *) is_stock "$a" || die "unknown allocator '$a'";; esac; done
for row in $ROWS; do case $row in startup|server|soak) ;; *) die "unknown row '$row' (startup|server|soak)";; esac; done
rm -rf "$OUT"; mkdir -p "$OUT" "$(dirname "$RESULT")"

# ---- the JDK and the generator ------------------------------------------------
# Wolfi's openjdk-NN installs under /usr/lib/jvm/java-NN-openjdk and does not
# put java on PATH; take the newest one unless JAVA_HOME says otherwise.
if [ -n "${JAVA_HOME:-}" ]; then JAVA=$JAVA_HOME/bin/java; JAVAC=$JAVA_HOME/bin/javac
else
  # shellcheck disable=SC2012 # JDK directory names are plain; sort -V picks the newest
  JAVA=$(ls -d /usr/lib/jvm/java-*-openjdk/bin/java 2>/dev/null | sort -V | tail -1); JAVAC=${JAVA%/java}/javac
fi
[ -x "${JAVA:-}" ] || die "no JDK under /usr/lib/jvm (rebuild the image from Dockerfile.dev: openjdk-21)"
[ -x "$JAVAC" ] || die "no javac beside $JAVA (openjdk-21 is the JDK, not the -jre package)"
JCMD=${JAVA%/java}/jcmd
if [ "$NMT" != off ] && [ ! -x "$JCMD" ]; then echo "== no jcmd beside $JAVA: NMT snapshots off" >&2; NMT=off; fi
JAVA_VERSION=$("$JAVA" -version 2>&1)
JDK_PKG=$(apk list --installed 2>/dev/null | awk '/^openjdk-[0-9]+-[0-9]/ {print $1}' | head -1)
AB=$(command -v ab || true)
[ -n "$AB" ] || die "no ab (apache2-utils) in this container; rebuild the image from Dockerfile.dev"
AB_VERSION=$("$AB" -V 2>&1 | head -1)
# The server class, compiled once (again when the source is newer).
CLASSES=$WORK/classes
if [ ! -f "$CLASSES/HttpBench.class" ] || [ "$ROOT/bench/drivers/jvm/HttpBench.java" -nt "$CLASSES/HttpBench.class" ]; then
  mkdir -p "$CLASSES"
  "$JAVAC" -d "$CLASSES" "$ROOT/bench/drivers/jvm/HttpBench.java" || die "javac failed on bench/drivers/jvm/HttpBench.java"
fi
# glibc-trim: the JDK must accept the flag (JDK-8293114, in 21.0.2+; the
# Unlock flag is harmless where it is already a product flag). If it does
# not, the variant is refused rather than silently measuring plain glibc.
TRIM_OPTS="-XX:+UnlockExperimentalVMOptions -XX:TrimNativeHeapInterval=$TRIM_MS"
case " $ALLOCS " in *" glibc-trim "*|*" challoc-trim "*)
  # shellcheck disable=SC2086 # TRIM_OPTS is a list of JVM flags
  "$JAVA" $TRIM_OPTS -version >/dev/null 2>&1 || die "this JDK rejects '$TRIM_OPTS'; drop glibc-trim from ALLOCS (a jcmd System.trim_native_heap sidecar would be the fallback)";;
esac

# ---- the challoc library --------------------------------------------------------
CHALLOC_LIB=
if [ $CB_MODE = 1 ]; then
  [ "$CB_ALLOC" != challoc ] || { [ -f "${CB_LIB:-}" ] || die "CB_LIB='${CB_LIB:-}' is not a file"; CHALLOC_LIB=$CB_LIB; }
else
case " $ALLOCS " in *" challoc "*|*" challoc-"*)
  rev=$(resolve_rev "$REV")
  printf '== building challoc (%s) into target/jvm/lib ... ' "$(rev_label "$rev")"
  build_lib "$rev" "$WORK/lib"; CHALLOC_LIB=$WORK/lib/release/libchalloc.so
  echo "done, $(so_hash "$CHALLOC_LIB")";;
esac
fi
lib_of() { case $1 in glibc-arena2|glibc-trim) echo "";; challoc-*) echo "$CHALLOC_LIB";; *) default_lib_of "$1";; esac; }
# env_of ALLOC [server]: the extra environment of a variant (LD_PRELOAD and
# tunables). The server rows add the stats prints at exit (they only act at
# exit and are ignored by the other allocators); the startup row must not
# pay for formatting a stats report inside its 100 ms.
# The server rows also preload bench/probes/statsdump.so after the
# allocator: an atexit hook calling malloc_stats_print where the allocator
# exports it (challoc, jemalloc), since challoc's stats_print key is inert.
env_of() {
  lib=$(lib_of "$1")
  if [ "${2:-}" = server ]; then
    printf 'LD_PRELOAD=%s MALLOC_CONF=stats_print:true MIMALLOC_SHOW_STATS=1' "${lib:+$lib:}$STATSDUMP"
  else
    printf '%s' "${lib:+LD_PRELOAD=$lib}"
  fi
  case $1 in glibc-arena2) printf ' MALLOC_ARENA_MAX=2';; challoc-trim) ;; challoc-*) printf ' CHALLOC_CONF=%s' "$(conf_of "$1")";; esac
  [ $CB_MODE = 0 ] || [ "$1" != challoc ] || [ -z "${CB_CONF:-}" ] || printf ' CHALLOC_CONF=%s' "$CB_CONF"
  :
}
# note_of ALLOC: what distinguishes a variant, for the maps line.
note_of() { case $1 in glibc-arena2) echo ', MALLOC_ARENA_MAX=2';; glibc-trim|challoc-trim) echo ", $TRIM_OPTS";; challoc-*) echo ", CHALLOC_CONF=$(conf_of "$1")";; esac; :; }
# opts_of ALLOC: the variant's extra JVM flags (glibc-trim's periodic trim).
opts_of() { case $1 in glibc-trim|challoc-trim) printf '%s' "$TRIM_OPTS";; esac; :; }
# nmt_opts: the JVM's Native Memory Tracking flag for the server rows.
nmt_opts() { [ "$NMT" = off ] || printf '%s' "-XX:NativeMemoryTracking=$NMT"; :; }
STATSDUMP=$BIN/statsdump.so
[ "$STATSDUMP" -nt "$ROOT/bench/probes/statsdump.c" ] || cc -O2 -Wall -Wextra -Werror -shared -fPIC -o "$STATSDUMP" "$ROOT/bench/probes/statsdump.c" -ldl || die "statsdump.c did not compile"

# ---- binding check ----------------------------------------------------------------
echo "== java: $(echo "$JAVA_VERSION" | head -1) ($JAVA, Wolfi ${JDK_PKG:-openjdk}); generator: $AB_VERSION; NMT: $NMT"
for a in $ALLOCS; do
  # shellcheck disable=SC2046 # env_of and opts_of print word lists
  mapped=$(env $(env_of "$a") "$JAVA" $(opts_of "$a") -cp "$CLASSES" HttpBench maps | tr '\n' ' ' | sed 's/ $//')
  maps_expect "$a" java "$(lib_of "$a")" "$mapped" "$(note_of "$a")"
done

# ---- startup row -------------------------------------------------------------------
# run_startup ALLOC: appends "wall_s maxrss_kb exit" to $OUT/startup.ALLOC.scores.
run_startup() {
  a=$1; tag=startup.$a
  [ -f "$OUT/$tag.scores" ] || : > "$OUT/$tag.scores"
  n=$(wc -l < "$OUT/$tag.scores")
  log=$OUT/$tag.$n.out; rt=$OUT/$tag.$n.rt
  # shellcheck disable=SC2046 # env_of and opts_of print word lists
  RT_OUT=$rt "$BIN/rt" env $(env_of "$a") "$JAVA" $(opts_of "$a") -version > "$log" 2>&1 || true
  record_rt "$tag" "$rt" "$log"
}
has_row() { case " $ROWS " in *" $1 "*) return 0;; *) return 1;; esac; }
if has_row startup; then
  echo "== startup: $ROUNDS rounds + warm-up x $(echo "$ALLOCS" | wc -w) allocators ($ALLOCS); load $LOAD0"
  r=0
  while [ $r -le "$ROUNDS" ]; do
    for a in $ALLOCS; do run_startup "$a"; done
    r=$((r+1))
  done
  printf '   startup rounds done, load %s, %s\n' "$(loadavg)" "$(date -u +%T)"
  check_score_lines startup "$ALLOCS" $((ROUNDS+1))
fi

# ---- server and soak rows: shared machinery ---------------------------------------------
now() { date +%s; }
# sample_line PID DIR T0: one line "elapsed,phase,rss_kb,pss_kb,anon_kb,vmrss_kb,vmhwm_kb,threads" on stdout; 1 when the process is gone.
sample_line() {
  [ -r "/proc/$1/smaps_rollup" ] || return 1
  awk -v t="$(( $(now) - $3 ))" -v ph="$(cat "$2/phase")" '
    FILENAME ~ /smaps_rollup/ { if ($1=="Rss:") r=$2; if ($1=="Pss:") p=$2; if ($1=="Anonymous:") an=$2 }
    FILENAME ~ /status/ { if ($1=="VmRSS:") vr=$2; if ($1=="VmHWM:") hw=$2; if ($1=="Threads:") th=$2 }
    END { printf "%d,%s,%d,%d,%d,%d,%d,%d\n", t, ph, r, p, an, vr, hw, th }' "/proc/$1/smaps_rollup" "/proc/$1/status" 2>/dev/null || return 1
}
# sample PID DIR CSV T0: append one line; 1 when the process is gone.
sample() { l=$(sample_line "$1" "$2" "$4") || return 1; echo "$l" >> "$3"; }
rss_of_line() { echo "$1" | cut -d, -f3; }
# nmt_take PID FILE RSS: `jcmd PID VM.native_memory summary` into FILE (KB
# units), the process's Rss at that instant into FILE.rss. jcmd is a JVM of
# its own started from this shell (no LD_PRELOAD: the allocator under test
# is not in it); the attach listener it wakes in the server is one more
# thread, the same for every variant.
nmt_take() {
  [ "$NMT" = off ] && return 0
  "$JCMD" "$1" VM.native_memory summary scale=KB > "$2" 2>&1 || echo "jcmd failed (exit $?)" >> "$2"
  echo "$3" > "$2.rss"
}
# sampler PID DIR CSV T0: every INTERVAL s until DIR/stop; a new peak Rss
# asks for an NMT snapshot (DIR/nmt-peak.txt), taken at the first sample at
# least NMT_GAP s after the previous one (the request waits, it is not
# dropped: an allocator whose Rss plateaus inside the first NMT_GAP s must
# still get its snapshot on the plateau, not the idle one), so the snapshot
# on record is the one at or just after the highest Rss the run reached.
sampler() {
  echo "elapsed_s,phase,rss_kb,pss_kb,anon_kb,vmrss_kb,vmhwm_kb,threads" > "$3"
  peak=0; lastnmt=0; pending=0
  while [ ! -e "$2/stop" ]; do
    l=$(sample_line "$1" "$2" "$4") || break
    echo "$l" >> "$3"
    r=$(rss_of_line "$l"); t=$(echo "$l" | cut -d, -f1)
    [ "$r" -gt "$peak" ] && { peak=$r; pending=1; }
    if [ $pending = 1 ] && [ $((t - lastnmt)) -ge "$NMT_GAP" ]; then nmt_take "$1" "$2/nmt-peak.txt" "$r"; lastnmt=$t; pending=0; echo "$t" > "$2/nmt-peak.at"; fi
    sleep "$INTERVAL"
  done
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
ab_kv() { # ab_kv LOG KEY: Requests per second | 99% | Failed requests | Non-2xx responses | Complete requests | Keep-Alive requests | Transfer rate
  case $2 in
    rps)   awk '/^Requests per second:/ {printf "%.0f", $4}' "$1";;
    p99)   awk '$1 == "99%" {print $2}' "$1";;
    fail)  awk '/^Failed requests:/ {print $3}' "$1";;
    non2xx) awk '/^Non-2xx responses:/ {print $3}' "$1";;
    done)  awk '/^Complete requests:/ {print $3}' "$1";;
    ka)    awk '/^Keep-Alive requests:/ {print $3}' "$1";;
    mbps)  awk '/^Transfer rate:/ {printf "%.1f", $3/1024}' "$1";;
  esac
}
# fetch PATH: one GET, the body on stdout (the image has python3, no wget or curl).
fetch() { python3 -c 'import sys,urllib.request; sys.stdout.write(urllib.request.urlopen(sys.argv[1], timeout=10).read().decode(errors="replace"))' "http://127.0.0.1:$PORT/$1" 2>/dev/null; }
# start_jvm ALLOC DIR MODE ARGS...: the server JVM under the variant, its pid
# in DIR/pid, 1 when it did not come up (recorded in $OUT/<row>.failed).
start_jvm() {
  a=$1; d=$2; mode=$3; shift 3
  # shellcheck disable=SC2046,SC2086 # env_of, opts_of, nmt_opts and JAVA_OPTS are word lists
  env $(env_of "$a" server) "$JAVA" $JAVA_OPTS $(opts_of "$a") $(nmt_opts) "-Xlog:gc:file=$d/gc.log" -cp "$CLASSES" HttpBench "$mode" "$PORT" "$@" > "$d/stdout" 2> "$d/stderr" 9>&- &
  pid=$!; echo "$pid" > "$d/pid"
  BENCH_PIDS=$pid   # the bench lock's trap kills it if the harness dies here
  i=0; until grep -q '^READY' "$d/stdout" 2>/dev/null; do
    i=$((i+1)); if [ $i -gt 300 ] || ! kill -0 "$pid" 2>/dev/null; then echo "  server ($a) did not start; stderr:" >&2; tail -5 "$d/stderr" >&2; echo "$a start-failed" >> "$OUT/$mode.failed"; wait "$pid" 2>/dev/null || true; return 1; fi
    sleep 0.1
  done
  [ "$(fetch json | head -c 1)" = "{" ] || echo "  warning: /json did not answer as expected ($a)" >&2
}
# finish_jvm ALLOC DIR PID SPID T0: the final sample, the NMT snapshot, the
# smaps breakdown, /quit, the exit code.
finish_jvm() {
  a=$1; d=$2; pid=$3; spid=$4; t0=$5
  touch "$d/stop"; wait "$spid" 2>/dev/null || true
  l=$(sample_line "$pid" "$d" "$t0") && echo "$l" >> "$d/samples.csv" || l=",,0"
  nmt_take "$pid" "$d/nmt-final.txt" "$(rss_of_line "$l")"
  smaps_by_name "$pid" > "$d/smaps-by-name.txt"
  grep -E '^(VmHWM|VmRSS|Threads):' "/proc/$pid/status" | tr -s ' ' > "$d/vm.txt" 2>/dev/null || true
  fetch stats > "$d/stats.txt" 2>/dev/null || true
  fetch quit > /dev/null || true
  i=0; while [ -d "/proc/$pid" ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done
  [ -d "/proc/$pid" ] && { echo "  $a: server did not exit on /quit, killing" >&2; kill "$pid" 2>/dev/null || true; }
  if wait "$pid" 2>/dev/null; then ex=0; else ex=$?; fi
  BENCH_PIDS=
  echo "$ex" > "$d/exit"
}

# ---- server row ---------------------------------------------------------------------
# run_server ALLOC: one DURATION-second server row; everything under $OUT/server.ALLOC.*
run_server() {
  a=$1; d=$OUT/server.$a; mkdir -p "$d"
  echo idle > "$d/phase"; rm -f "$d/stop"
  start_jvm "$a" "$d" serve "$THREADS" || return 0
  pid=$(cat "$d/pid"); t0=$(now)
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
  finish_jvm "$a" "$d" "$pid" "$spid" "$t0"
  # shellcheck disable=SC1010 # `done` is an argument (the ab field name)
  echo "     json $(ab_kv "$d/json.ab" rps) req/s p99 $(ab_kv "$d/json.ab" p99) ms, blob $(ab_kv "$d/blob.ab" rps) req/s p99 $(ab_kv "$d/blob.ab" p99) ms (keep-alive $(ab_kv "$d/json.ab" ka) + $(ab_kv "$d/blob.ab" ka) of $(ab_kv "$d/json.ab" done) + $(ab_kv "$d/blob.ab" done)); VmHWM $(awk '$1=="VmHWM:" {print $2}' "$d/vm.txt") KiB, NMT committed $(nmt_kb "$d/nmt-peak.txt" total) KiB at peak, exit $(cat "$d/exit")"
}
# nmt_kb FILE WHAT: a figure from a jcmd VM.native_memory summary (KB
# units): total (committed), malloc, mmap (committed), overhead (NMT's own
# tracking overhead), or a category's committed by its name ("Java Heap",
# Class, Thread, Code, GC, Compiler, Internal, Other, Symbol ...). "-" when
# the file is missing or the category was not reported.
nmt_kb() {
  [ -f "$1" ] || { echo -; return; }
  awk -v what="$2" '
    /^Total:/ { if (what=="total" && match($0, /committed=[0-9]+/)) { print substr($0, RSTART+10, RLENGTH-10); found=1 } }
    /^ *malloc:/ { if (what=="malloc" && match($0, /malloc: *[0-9]+/)) { v=substr($0, RSTART, RLENGTH); sub(/malloc: */, "", v); print v; found=1 } }
    /^ *mmap:/ && !incat { if (what=="mmap" && match($0, /committed=[0-9]+/)) { print substr($0, RSTART+10, RLENGTH-10); found=1 } }
    /^- / { incat=1; name=$0; sub(/^- +/, "", name); sub(/ +\(reserved=.*/, "", name); if (name==what && match($0, /committed=[0-9]+/)) { print substr($0, RSTART+10, RLENGTH-10); found=1 } }
    /tracking overhead=/ { if (what=="overhead" && match($0, /overhead=[0-9]+/)) { print substr($0, RSTART+9, RLENGTH-9); found=1 } }
    END { if (!found) print "-" }' "$1"
}
nmt_rss_kb() { cat "$1.rss" 2>/dev/null | grep . || echo -; }
# gap_kb RSS_KB NMT_KB: RSS minus NMT committed, "-" when either is missing.
gap_kb() { awk -v r="$1" -v n="$2" 'BEGIN {if (r=="-"||n=="-"||r==""||n=="") print "-"; else print r-n}'; }
if has_row server; then
  echo "== server: $DURATION s per allocator after $WARMUP s warm-up, $THREADS threads, $JAVA_OPTS $(nmt_opts), ab -c $((CONNECTIONS/2)) x2 (/json, /blob); load $(loadavg)"
  for a in $ALLOCS; do run_server "$a"; done
  printf '   server rows done, load %s, %s\n' "$(loadavg)" "$(date -u +%T)"
fi

# ---- soak row --------------------------------------------------------------------------
# run_soak ALLOC: SOAK_DURATION seconds of alternating windows; everything
# under $OUT/soak.ALLOC.*; windows.csv has one line per window.
run_soak() {
  a=$1; d=$OUT/soak.$a; mkdir -p "$d"
  echo idle > "$d/phase"; rm -f "$d/stop"
  start_jvm "$a" "$d" soak "$SOAK_CORE" "$SOAK_MAX" "$SOAK_KEEPALIVE" "$SOAK_MIN" "$SOAK_MAXB" || return 0
  pid=$(cat "$d/pid"); t0=$(now)
  sampler "$pid" "$d" "$d/samples.csv" "$t0" &
  spid=$!
  sleep "$IDLE_SECS"
  sample "$pid" "$d" "$d/samples.csv" "$t0" || true
  echo "window,level,connections,start_s,end_s,json_rps,json_p99_ms,blob_rps,blob_p99_ms,blob_mib_s,requests,server_cpu_s,cpu_us_req,rss_kb_end,threads_end,pool,largest,ab_cpu_json_pct,ab_cpu_blob_pct" > "$d/windows.csv"
  nwin=$((SOAK_DURATION / SOAK_WINDOW))
  echo "  $a: server pid $pid up, idle Rss $(tail -1 "$d/samples.csv" | cut -d, -f3) KiB; $nwin windows of ${SOAK_WINDOW}s alternating $SOAK_HIGH / $SOAK_LOW connections ($(date -u +%T))"
  i=0
  while [ $i -lt $nwin ]; do
    if [ $((i % 2)) = 0 ]; then level=high; conns=$SOAK_HIGH; else level=low; conns=$SOAK_LOW; fi
    half=$((conns / 2)); [ $half -ge 1 ] || half=1
    echo "w$i:$level" > "$d/phase"
    c0=$(cpu_ticks "$pid"); w0=$(now)
    ab_run "$a" json "$half" "$SOAK_WINDOW" "$d/w$i-json.ab" & l1=$!
    ab_run "$a" blob "$half" "$SOAK_WINDOW" "$d/w$i-blob.ab" & l2=$!
    wait $l1 $l2
    c1=$(cpu_ticks "$pid"); w1=$(now)
    st=$(fetch stats 2>/dev/null || echo "pool=- largest=-")
    l=$(sample_line "$pid" "$d" "$t0") && echo "$l" >> "$d/samples.csv" || l=",,-,,,,,-"
    # shellcheck disable=SC1010 # `done` is an argument (the ab field name)
    awk -v i="$i" -v lv="$level" -v c="$conns" -v s0=$((w0 - t0)) -v s1=$((w1 - t0)) \
        -v jr="$(ab_kv "$d/w$i-json.ab" rps)" -v jp="$(ab_kv "$d/w$i-json.ab" p99)" -v br="$(ab_kv "$d/w$i-blob.ab" rps)" -v bp="$(ab_kv "$d/w$i-blob.ab" p99)" -v bm="$(ab_kv "$d/w$i-blob.ab" mbps)" \
        -v jd="$(ab_kv "$d/w$i-json.ab" done)" -v bd="$(ab_kv "$d/w$i-blob.ab" done)" -v c0="$c0" -v c1="$c1" -v tck="$CLK_TCK" \
        -v rss="$(echo "$l" | cut -d, -f3)" -v th="$(echo "$l" | cut -d, -f8)" -v pool="$(echo "$st" | sed -n 's/.*pool=\([^ ]*\).*/\1/p')" -v largest="$(echo "$st" | sed -n 's/.*largest=\([^ ]*\).*/\1/p')" \
        -v aj="$(awk '{if ($1>0) printf "%.0f", ($2+$3)/$1*100; else print "-"}' "$d/w$i-json.ab.rt" 2>/dev/null)" -v ab="$(awk '{if ($1>0) printf "%.0f", ($2+$3)/$1*100; else print "-"}' "$d/w$i-blob.ab.rt" 2>/dev/null)" \
        'BEGIN { cpu=(c1-c0)/tck; req=jd+bd; printf "%d,%s,%d,%d,%d,%s,%s,%s,%s,%s,%d,%.2f,%s,%s,%s,%s,%s,%s,%s\n", i, lv, c, s0, s1, jr, jp, br, bp, bm, req, cpu, (req>0 ? sprintf("%.1f", cpu*1e6/req) : "-"), rss, th, pool, largest, aj, ab }' >> "$d/windows.csv"
    printf '     w%-2d %-4s c=%-3d json %6s req/s p99 %3s ms, blob %6s req/s p99 %3s ms; Rss %s KiB, threads %s, pool %s\n' "$i" "$level" "$conns" "$(ab_kv "$d/w$i-json.ab" rps)" "$(ab_kv "$d/w$i-json.ab" p99)" "$(ab_kv "$d/w$i-blob.ab" rps)" "$(ab_kv "$d/w$i-blob.ab" p99)" "$(echo "$l" | cut -d, -f3)" "$(echo "$l" | cut -d, -f8)" "$st"
    i=$((i+1))
  done
  echo final > "$d/phase"
  sleep 2
  finish_jvm "$a" "$d" "$pid" "$spid" "$t0"
  echo "     $a done: peak Rss $(srv_peak_kb soak "$a") KiB, final $(srv_final_kb soak "$a") KiB, slope $(soak_slope "$a") MiB/min over the last ${SOAK_TAIL}s, NMT committed $(nmt_kb "$d/nmt-final.txt" total) KiB at the end, exit $(cat "$d/exit")"
}
# soak_slope ALLOC: least-squares slope of Rss over elapsed time across the
# samples of the last SOAK_TAIL seconds, MiB per minute (positive = growing).
soak_slope() {
  f=$OUT/soak.$1/samples.csv; [ -f "$f" ] || { echo -; return; }
  awk -F, -v tail="$SOAK_TAIL" 'NR>1 && $3>0 {t[NR]=$1; r[NR]=$3; last=$1}
    END { n=0; for (k in t) if (t[k] >= last - tail) { n++; sx+=t[k]; sy+=r[k]; sxx+=t[k]*t[k]; sxy+=t[k]*r[k] }
          if (n < 3 || n*sxx - sx*sx == 0) { print "-"; exit }
          b = (n*sxy - sx*sy) / (n*sxx - sx*sx); printf "%+.2f", b*60/1024 }' "$f"
}
# soak_tail_mean ALLOC: the mean Rss (KiB) over the last SOAK_TAIL seconds.
soak_tail_mean() { awk -F, -v tail="$SOAK_TAIL" 'NR>1 && $3>0 {t[NR]=$1; r[NR]=$3; last=$1} END {for (k in t) if (t[k] >= last-tail) {n++; s+=r[k]}; if (n) printf "%d", s/n; else print "-"}' "$OUT/soak.$1/samples.csv" 2>/dev/null || echo -; }
# soak_win ALLOC LEVEL FIELD FN: mean|max|min of a windows.csv column over the windows of LEVEL.
soak_win() {
  awk -F, -v lv="$2" -v col="$3" -v fn="$4" 'NR==1 {for (i=1;i<=NF;i++) if ($i==col) c=i; next}
    $2==lv && c && $c != "-" && $c != "" { n++; s+=$c; if (n==1 || $c>mx) mx=$c; if (n==1 || $c<mn) mn=$c }
    END { if (!n) print "-"; else if (fn=="mean") printf "%.0f", s/n; else if (fn=="max") print mx; else print mn }' "$OUT/soak.$1/windows.csv" 2>/dev/null || echo -
}
soak_win1() { awk -F, -v lv="$2" -v col="$3" 'NR==1 {for (i=1;i<=NF;i++) if ($i==col) c=i; next} $2==lv && c {n++; s+=$c} END {if (n) printf "%.1f", s/n; else print "-"}' "$OUT/soak.$1/windows.csv" 2>/dev/null || echo -; }
# server figures for a row (server|soak), all "-" when the row did not run
srv_csv() { echo "$OUT/$1.$2/samples.csv"; }
srv_idle_kb() { awk -F, 'NR>1 && $2=="idle" {v=$3} END {print v ? v : "-"}' "$(srv_csv "$1" "$2")" 2>/dev/null || echo -; }
srv_peak_kb() { awk -F, 'NR>1 && $3>m {m=$3} END {print m ? m : "-"}' "$(srv_csv "$1" "$2")" 2>/dev/null || echo -; }
srv_final_kb() { awk -F, 'NR>1 && $3>0 {v=$3} END {print v ? v : "-"}' "$(srv_csv "$1" "$2")" 2>/dev/null || echo -; }
srv_hwm_kb() { awk '$1=="VmHWM:" {print $2}' "$OUT/$1.$2/vm.txt" 2>/dev/null | grep . || echo -; }
srv_max_threads() { awk -F, 'NR>1 && $8>m {m=$8} END {print m ? m : "-"}' "$(srv_csv "$1" "$2")" 2>/dev/null || echo -; }
srv_largest_pool() { sed -n 's/.*largest=\([0-9]*\).*/\1/p' "$OUT/$1.$2/stats.txt" 2>/dev/null | grep . || echo -; }
srv_gcs() { f=$OUT/$1.$2/gc.log; if [ -f "$f" ]; then awk '/Pause/ {n++} END {print n+0}' "$f"; else echo -; fi; }
srv_exit() { cat "$OUT/$1.$2/exit" 2>/dev/null || echo -; }
srv_name_kb() { awk -v n="$3" '$2==n {print $1}' "$OUT/$1.$2/smaps-by-name.txt" 2>/dev/null | grep . || echo 0; }
srv_named() { awk '$2 ~ /^\[anon:/ {n=$2; sub(/^\[anon:/, "", n); sub(/\]$/, "", n); printf "%s=%.1f ", n, $1/1024}' "$OUT/$1.$2/smaps-by-name.txt" 2>/dev/null | sed 's/ $//' | grep . || echo "none (kernel without anonymous VMA names, or no allocator names its mappings)"; }
srv_nmt() { nmt_kb "$OUT/$1.$2/nmt-$3.txt" "$4"; }
srv_nmt_rss() { nmt_rss_kb "$OUT/$1.$2/nmt-$3.txt"; }
srv_gap() { gap_kb "$(srv_nmt_rss "$1" "$2" "$3")" "$(srv_nmt "$1" "$2" "$3" total)"; }
if has_row soak; then
  echo "== soak: $SOAK_DURATION s per allocator, $((SOAK_DURATION / SOAK_WINDOW)) windows of $SOAK_WINDOW s alternating $SOAK_HIGH and $SOAK_LOW connections; pool $SOAK_CORE..$SOAK_MAX threads, keep-alive ${SOAK_KEEPALIVE}s; /blob log-uniform $SOAK_MIN..$SOAK_MAXB bytes; $JAVA_OPTS $(nmt_opts); load $(loadavg)"
  for a in $ALLOCS; do run_soak "$a"; done
  printf '   soak rows done, load %s, %s\n' "$(loadavg)" "$(date -u +%T)"
fi

# ---- report ---------------------------------------------------------------------------
list_of() { sorted_scores "$OUT/$1.$2.scores" 1; }
rss_of() { median_rss "$OUT/$1.$2.scores" 1; }
med_of() { median_list "$(list_of "$1" "$2")"; }
# server-row shorthands (row = server)
sv_ab() { [ -f "$OUT/server.$1/$2.ab" ] && ab_kv "$OUT/server.$1/$2.ab" "$3" | grep . || echo "${4:--}"; }
sv_cpu_s() { awk '{print $1}' "$OUT/server.$1/cpu.txt" 2>/dev/null | grep . || echo -; }
# sv_cpu_us ALLOC: server CPU microseconds per request over the measured window.
sv_cpu_us() {
  # shellcheck disable=SC1010 # `done` is an argument (the ab field name)
  awk -v c="$(sv_cpu_s "$1")" -v j="$(sv_ab "$1" json done)" -v b="$(sv_ab "$1" blob done)" 'BEGIN {if (c=="-"||j=="-"||b=="-"||j+b==0) print "-"; else printf "%.1f", c*1e6/(j+b)}'
}
# sv_gen_cpu ALLOC: the two generators' user+sys over their wall time, in
# percent of one CPU each ("json%/blob%"); near 100 means ab was the limit.
sv_gen_cpu() {
  for e in json blob; do awk '{if ($1 > 0) printf "%.0f", ($2+$3)/$1*100; else print "-"}' "$OUT/server.$1/$e.ab.rt" 2>/dev/null || printf '%s' -; printf '/'; done | sed 's,/$,,'
}
# pct_lower A B / pct_higher A B: challoc (A) against B, positive = challoc worse
pct_lower() { pct_of "$1" "$2"; }
pct_higher() { pct_of "$2" "$1"; }
describe() {
  case $1 in
    challoc)  describe_challoc "$CHALLOC_LIB" "$REV";;
    challoc-trim) echo "$(describe_challoc "$CHALLOC_LIB" "$REV") with \`$TRIM_OPTS\` (the JVM's service thread calls challoc's malloc_trim(0) every $TRIM_MS ms: every Empty slab and huge extent purged regardless of age, glibc-trim's economics on challoc)";;
    challoc-*) echo "$(describe_challoc "$CHALLOC_LIB" "$REV") with CHALLOC_CONF=$(conf_of "$1") (the same library as challoc under the key)";;
    glibc)    echo "no LD_PRELOAD ($(stock_version glibc) malloc, default arenas: up to 8 x CPUs)";;
    glibc-arena2) echo "no LD_PRELOAD, MALLOC_ARENA_MAX=2 ($(stock_version glibc) malloc; the fleet's usual mitigation for the per-thread arenas)";;
    glibc-trim) echo "no LD_PRELOAD, \`$TRIM_OPTS\` ($(stock_version glibc) malloc; the JVM's service thread calls malloc_trim(0) every $TRIM_MS ms: what a well-tuned Java image ships today)";;
    *)        describe_stock "$1";;
  esac
}
has() { case " $ALLOCS " in *" $1 "*) return 0;; *) return 1;; esac; }
NMT_CATS="Java Heap:Class:Thread:Code:GC:Compiler:Internal:Other:Symbol"
# nmt_table ROW WHEN: the NMT category table for a row at peak|final.
nmt_table() {
  echo "| allocator | Rss MiB at the sample | NMT committed MiB | allocator overhead MiB (Rss - NMT) | malloc'd MiB (all categories) | mmap committed MiB | Java Heap | Class | Thread | Code | GC | Compiler | Internal | Other | Symbol | NMT's own overhead MiB |"
  echo "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
  for a in $ALLOCS; do
    line="| $a | $(kib_to_mib "$(srv_nmt_rss "$1" "$a" "$2")") | $(kib_to_mib "$(srv_nmt "$1" "$a" "$2" total)") | $(kib_to_mib "$(srv_gap "$1" "$a" "$2")") | $(kib_to_mib "$(srv_nmt "$1" "$a" "$2" malloc)") | $(kib_to_mib "$(srv_nmt "$1" "$a" "$2" mmap)") |"
    IFS=:; for c in $NMT_CATS; do line="$line $(kib_to_mib "$(srv_nmt "$1" "$a" "$2" "$c")") |"; done; unset IFS
    echo "$line $(kib_to_mib "$(srv_nmt "$1" "$a" "$2" overhead)") |"
  done
}
nmt_note() {
  echo "NMT (\`-XX:NativeMemoryTracking=$NMT\`, \`jcmd <pid> VM.native_memory summary scale=KB\`) is the JVM's own account of the native memory it asked for: \"committed\" counts the bytes the JVM malloc'd (with NMT's 16-byte header each) plus the pages it committed by mmap (Java heap, metaspace, code cache, thread stacks and GC structures). Rss minus NMT committed at the same instant is what the allocator adds on its own account (caching, size-class rounding, fragmentation, retained pages, metadata) less what the JVM committed but never touched (NMT counts mmap commits, not resident pages, so the gap understates the allocator by the same untouched amount under every variant). \"malloc'd\" is NMT's malloc total across categories: \`Other\` holds the direct ByteBuffers (\`Unsafe.allocateMemory\`), \`Internal\` the JVM's miscellany, \`Thread\` the stacks (touched pages) and per-thread arenas, \`Class\` metaspace, \`Code\` the code cache, \`GC\` the card table and remembered sets, \`Compiler\` the JIT arenas. NMT's own overhead (its header per malloc plus its tables, the last column) is the same bytes for every allocator, so the comparisons between allocators hold; the header does push a malloc(65536) to 65552 bytes, across a power-of-two class boundary in every size-class allocator (see the reading). jcmd is a second JVM started from the harness shell (no LD_PRELOAD) and wakes the server's attach listener (one more thread, the same for every variant)."
}
{
  result_header "JVM benchmark rows$([ $SOAK = 1 ] && echo ': the mixed-size soak')"
  echo "- java: $JAVA (Wolfi ${JDK_PKG:-openjdk}); \`java -version\`:"
  echo "$JAVA_VERSION" | sed 's/^/      /'
  echo "- generator: $AB_VERSION (Wolfi apache2-utils $(apk_version apache2-utils)); wrk and oha are not in Wolfi"
  echo "- load average at start: $LOAD0; at end: $(loadavg); $(( $(date +%s) - T_START )) s total"
  if [ $SOAK = 1 ]; then
    echo "- method: bench/drivers/jvm/HttpBench.java in \`soak\` mode (com.sun.net.httpserver on a ThreadPoolExecutor of $SOAK_CORE core and $SOAK_MAX maximum threads, idle threads retired after $SOAK_KEEPALIVE s, a SynchronousQueue; \`$JAVA_OPTS $(nmt_opts)\`, so the Java heap is constant and resident from the start and RSS differs by native memory only), served once per allocator for $SOAK_DURATION s to two \`ab -k\` processes (one on /json, a ~300-byte body built per request; one on /blob, a body of a log-uniform size between $SOAK_MIN and $SOAK_MAXB bytes drawn per request and copied out of a per-request ByteBuffer.allocateDirect of that size, i.e. one malloc(n) per request in the JVM with n spread over every size class from 256 bytes to 1 MiB, freed when the buffer's Cleaner runs after a GC) in $((SOAK_DURATION / SOAK_WINDOW)) windows of $SOAK_WINDOW s alternating $SOAK_HIGH total connections (ab -c $((SOAK_HIGH/2)) x2) and $SOAK_LOW (ab -c $((SOAK_LOW/2)) x2), so the pool grows to hundreds of threads and shrinks to $SOAK_CORE every other minute all run long; RSS sampled every $INTERVAL s from /proc/<pid>/smaps_rollup (Rss) and /proc/<pid>/status (VmHWM, Threads) into a CSV per allocator (beside this file): idle = the last sample before the load (${IDLE_SECS} s after the server answered), peak = the largest sample, final = the last sample after the load, slope = the least-squares slope of Rss over the samples of the last $SOAK_TAIL s in MiB per minute (growth after warm-up is the fragmentation signal), tail mean = the mean Rss over the same samples; req/s and p99 (ms) are ab's per endpoint per window, summarised over the high and the low windows; CPU us/req is the server's user+sys over a window divided by the requests both generators completed in it; NMT (\`jcmd VM.native_memory summary\`) at the end. Percentages are challoc against the other allocator: positive means challoc is larger, slower or serves fewer requests."
  else
    echo "- method: startup = \`java -version\`, $ROUNDS timed rounds after one discarded warm-up, allocators interleaved per round, one process per run under bench/rt, every run's wall time listed sorted, wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB). server = bench/drivers/jvm/HttpBench.java (com.sun.net.httpserver, a fixed $THREADS-thread executor, \`$JAVA_OPTS $(nmt_opts)\`, so the Java heap is constant and resident from the start and RSS differs by native memory only) served once per allocator for $DURATION s to two \`ab -k -c $((CONNECTIONS/2))\` processes (one on /json, a ~300-byte body built per request; one on /blob, 64 KiB copied out of a per-request ByteBuffer.allocateDirect, i.e. one malloc(65536) per request in the JVM, freed in bursts when the Cleaner runs after a GC) after $WARMUP s of discarded warm-up load; RSS sampled every $INTERVAL s from /proc/<pid>/smaps_rollup (Rss) and /proc/<pid>/status (VmHWM): idle = the last sample before the load (${IDLE_SECS} s after the server answered), peak = the largest sample, final = the last sample after the load; NMT committed and the allocator overhead (Rss minus NMT committed at the same instant) from the \`jcmd VM.native_memory summary\` taken at the peak-Rss sample (a sample that sets a new peak asks for a snapshot, taken at the first sample at least $NMT_GAP s after the previous snapshot: the snapshot on record is the one at or just after the highest Rss the run reached) and at the end; req/s and p99 (ms) are ab's per endpoint over the measured window; GCs = pauses in -Xlog:gc over the whole process. Percentages are challoc against the other allocator: positive means challoc is slower, larger or serves fewer requests."
  fi
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
    echo "## HTTP server, $DURATION s per allocator ($THREADS threads, \`$JAVA_OPTS $(nmt_opts)\`, ab -k -c $((CONNECTIONS/2)) on /json and on /blob)"
    echo
    echo "| allocator | idle Rss MiB | peak Rss MiB | final Rss MiB | VmHWM MiB | NMT committed MiB (peak) | allocator overhead MiB (peak) | allocator overhead MiB (final) | json req/s | json p99 ms | blob req/s | blob p99 ms | server CPU s | CPU us/req | requests (json + blob) | keep-alive | failed | non-2xx | GCs | ab CPU % | exit |"
    echo "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    for a in $ALLOCS; do
      # shellcheck disable=SC1010 # `done` is an argument (the ab field name)
      echo "| $a | $(kib_to_mib "$(srv_idle_kb server "$a")") | $(kib_to_mib "$(srv_peak_kb server "$a")") | $(kib_to_mib "$(srv_final_kb server "$a")") | $(kib_to_mib "$(srv_hwm_kb server "$a")") | $(kib_to_mib "$(srv_nmt server "$a" peak total)") | $(kib_to_mib "$(srv_gap server "$a" peak)") | $(kib_to_mib "$(srv_gap server "$a" final)") | $(sv_ab "$a" json rps) | $(sv_ab "$a" json p99) | $(sv_ab "$a" blob rps) | $(sv_ab "$a" blob p99) | $(sv_cpu_s "$a") | $(sv_cpu_us "$a") | $(sv_ab "$a" json done) + $(sv_ab "$a" blob done) | $(sv_ab "$a" json ka) + $(sv_ab "$a" blob ka) | $(sv_ab "$a" json fail) + $(sv_ab "$a" blob fail) | $(sv_ab "$a" json non2xx 0) + $(sv_ab "$a" blob non2xx 0) | $(srv_gcs server "$a") | $(sv_gen_cpu "$a") | $(srv_exit server "$a") |"
    done
    if has challoc; then
      echo
      echo "### challoc against each allocator (server row; positive = challoc worse)"
      echo
      hdr="| metric |"; sep="|---|"
      for a in $ALLOCS; do [ "$a" = challoc ] || { hdr="$hdr vs $a |"; sep="$sep---:|"; }; done
      echo "$hdr"; echo "$sep"
      for m in "idle Rss:srv_idle_kb server:lower" "peak Rss:srv_peak_kb server:lower" "final Rss:srv_final_kb server:lower" "VmHWM:srv_hwm_kb server:lower" "allocator overhead (peak):srv_gap server @ peak:lower" "allocator overhead (final):srv_gap server @ final:lower" "json req/s:json rps:higher" "json p99:json p99:lower" "blob req/s:blob rps:higher" "blob p99:blob p99:lower" "server CPU us/req:sv_cpu_us:lower"; do
        name=${m%%:*}; rest=${m#*:}; fn=${rest%%:*}; dir=${rest#*:}
        # shellcheck disable=SC2086 # fn holds a function and its arguments, split on purpose
        val() { case $fn in srv_gap*) srv_gap server "$1" "${fn##* }";; srv_*) ${fn%% *} "${fn#* }" "$1";; sv_*) $fn "$1";; *) sv_ab "$1" ${fn%% *} ${fn#* };; esac; }
        cv=$(val challoc); line="| $name |"
        for a in $ALLOCS; do [ "$a" = challoc ] || line="$line $("pct_$dir" "$cv" "$(val "$a")") |"; done
        echo "$line"
      done
    fi
    if [ "$NMT" != off ]; then
      echo
      echo "### Native Memory Tracking at the peak-Rss sample (MiB; the JVM's own account of the native memory it asked for; \"allocator overhead\" = Rss - NMT committed at the same instant)"
      echo
      nmt_table server peak
      echo
      echo "### Native Memory Tracking at the end (MiB, the last sample)"
      echo
      nmt_table server final
      echo
      nmt_note
    fi
    echo
    echo "### Final Rss by mapping (MiB, /proc/<pid>/smaps at the last sample; \"anon\" is unnamed anonymous memory: the Java heap, metaspace, code cache, thread stacks and every allocator's unnamed arenas; challoc names its regions)"
    echo
    echo "| allocator | [heap] | anon | file-backed | named anonymous regions |"
    echo "|---|---:|---:|---:|---|"
    for a in $ALLOCS; do
      echo "| $a | $(kib_to_mib "$(srv_name_kb server "$a" '[heap]')") | $(kib_to_mib "$(srv_name_kb server "$a" anon)") | $(kib_to_mib "$(srv_name_kb server "$a" file)") | $(srv_named server "$a") |"
    done
    echo
    echo "\"server CPU s\" is the server process's user+sys over the measured window (/proc/<pid>/stat), \"CPU us/req\" that divided by the requests both generators completed: the allocator's cost per request even where the generator is the throughput limit. \"ab CPU %\" is each generator's user+sys over its wall time (json/blob; near 100 means ab, not the server, set the rate). p99 is ab's integer-millisecond percentile."
    echo
    echo "Raw: $OUT/server.<allocator>/{samples.csv,json.ab,blob.ab,gc.log,smaps-by-name.txt,cpu.txt,nmt-peak.txt,nmt-final.txt,stderr} (stderr carries the allocator's own stats at exit: malloc_stats_print for challoc and jemalloc through bench/probes/statsdump.so, mimalloc's MIMALLOC_SHOW_STATS report)."
    [ -f "$OUT/serve.failed" ] && { echo; echo "FAILED server rows: $(tr '\n' ' ' < "$OUT/serve.failed")"; }
  fi
  if has_row soak; then
    echo
    echo "## Mixed-size soak, $SOAK_DURATION s per allocator (pool $SOAK_CORE..$SOAK_MAX threads, keep-alive $SOAK_KEEPALIVE s; /blob log-uniform $SOAK_MIN..$SOAK_MAXB bytes; $SOAK_WINDOW s windows alternating $SOAK_HIGH and $SOAK_LOW connections)"
    echo
    echo "| allocator | idle Rss MiB | peak Rss MiB | final Rss MiB | tail mean Rss MiB (last $SOAK_TAIL s) | slope MiB/min (last $SOAK_TAIL s) | VmHWM MiB | NMT committed MiB (end) | allocator overhead MiB (end) | json req/s high | json p99 ms high (max) | blob req/s high | blob p99 ms high (max) | blob MiB/s high | json req/s low | blob req/s low | CPU us/req high | CPU us/req low | max threads | largest pool | GCs | exit |"
    echo "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    for a in $ALLOCS; do
      echo "| $a | $(kib_to_mib "$(srv_idle_kb soak "$a")") | $(kib_to_mib "$(srv_peak_kb soak "$a")") | $(kib_to_mib "$(srv_final_kb soak "$a")") | $(kib_to_mib "$(soak_tail_mean "$a")") | $(soak_slope "$a") | $(kib_to_mib "$(srv_hwm_kb soak "$a")") | $(kib_to_mib "$(srv_nmt soak "$a" final total)") | $(kib_to_mib "$(srv_gap soak "$a" final)") | $(soak_win "$a" high json_rps mean) | $(soak_win "$a" high json_p99_ms max) | $(soak_win "$a" high blob_rps mean) | $(soak_win "$a" high blob_p99_ms max) | $(soak_win "$a" high blob_mib_s mean) | $(soak_win "$a" low json_rps mean) | $(soak_win "$a" low blob_rps mean) | $(soak_win1 "$a" high cpu_us_req) | $(soak_win1 "$a" low cpu_us_req) | $(srv_max_threads soak "$a") | $(srv_largest_pool soak "$a") | $(srv_gcs soak "$a") | $(srv_exit soak "$a") |"
    done
    if has challoc; then
      echo
      echo "### challoc against each allocator (soak; positive = challoc worse; the slope row is the difference in MiB/min, challoc minus the other)"
      echo
      hdr="| metric |"; sep="|---|"
      for a in $ALLOCS; do [ "$a" = challoc ] || { hdr="$hdr vs $a |"; sep="$sep---:|"; }; done
      echo "$hdr"; echo "$sep"
      for m in "peak Rss:srv_peak_kb soak:lower" "final Rss:srv_final_kb soak:lower" "tail mean Rss:soak_tail_mean:lower" "VmHWM:srv_hwm_kb soak:lower" "allocator overhead (end):srv_gap soak @ final:lower" "json req/s high:soak_win @ high json_rps mean:higher" "json p99 high (max):soak_win @ high json_p99_ms max:lower" "blob req/s high:soak_win @ high blob_rps mean:higher" "blob p99 high (max):soak_win @ high blob_p99_ms max:lower" "json req/s low:soak_win @ low json_rps mean:higher" "blob req/s low:soak_win @ low blob_rps mean:higher" "CPU us/req high:soak_win1 @ high cpu_us_req:lower" "CPU us/req low:soak_win1 @ low cpu_us_req:lower"; do
        name=${m%%:*}; rest=${m#*:}; fn=${rest%%:*}; dir=${rest#*:}
        # shellcheck disable=SC2086 # $args is the window function's argument list
        val() { case $fn in srv_gap*) srv_gap soak "$1" final;; soak_win*) f=${fn%% *}; args=${fn#*@ }; $f "$1" $args;; soak_tail_mean) soak_tail_mean "$1";; *) ${fn%% *} "${fn#* }" "$1";; esac; }
        cv=$(val challoc); line="| $name |"
        for a in $ALLOCS; do [ "$a" = challoc ] || line="$line $("pct_$dir" "$cv" "$(val "$a")") |"; done
        echo "$line"
      done
      cs=$(soak_slope challoc); line="| slope MiB/min (difference) |"
      for a in $ALLOCS; do [ "$a" = challoc ] || line="$line $(awk -v a="$cs" -v b="$(soak_slope "$a")" 'BEGIN {if (a=="-"||b=="-") print "-"; else printf "%+.2f", a-b}') |"; done
      echo "$line"
    fi
    echo
    echo "### Rss at the end of each window (MiB; h = $SOAK_HIGH connections, l = $SOAK_LOW)"
    echo
    hdr="| allocator |"; sep="|---|"
    nwin=$((SOAK_DURATION / SOAK_WINDOW)); i=0
    while [ $i -lt $nwin ]; do hdr="$hdr w$i $([ $((i % 2)) = 0 ] && echo h || echo l) |"; sep="$sep---:|"; i=$((i+1)); done
    echo "$hdr"; echo "$sep"
    for a in $ALLOCS; do
      echo "| $a |$(awk -F, 'NR>1 {printf " %.1f |", $14/1024}' "$OUT/soak.$a/windows.csv" 2>/dev/null)"
    done
    echo
    echo "### Threads (/proc/<pid>/status) at the end of each window"
    echo
    echo "$hdr"; echo "$sep"
    for a in $ALLOCS; do
      echo "| $a |$(awk -F, 'NR>1 {printf " %s |", $15}' "$OUT/soak.$a/windows.csv" 2>/dev/null)"
    done
    echo
    echo "### req/s per window (json / blob)"
    echo
    echo "$hdr"; echo "$sep"
    for a in $ALLOCS; do
      echo "| $a |$(awk -F, 'NR>1 {printf " %s / %s |", $6, $8}' "$OUT/soak.$a/windows.csv" 2>/dev/null)"
    done
    echo
    echo "### p99 ms per window (json / blob)"
    echo
    echo "$hdr"; echo "$sep"
    for a in $ALLOCS; do
      echo "| $a |$(awk -F, 'NR>1 {printf " %s / %s |", $7, $9}' "$OUT/soak.$a/windows.csv" 2>/dev/null)"
    done
    if [ "$NMT" != off ]; then
      echo
      echo "### Native Memory Tracking at the end (MiB, the last sample; \"allocator overhead\" = Rss - NMT committed at the same instant)"
      echo
      nmt_table soak final
      echo
      nmt_note
    fi
    echo
    echo "### Final Rss by mapping (MiB, /proc/<pid>/smaps at the last sample; \"anon\" is unnamed anonymous memory: the Java heap, metaspace, code cache, thread stacks and every allocator's unnamed arenas; challoc names its regions)"
    echo
    echo "| allocator | [heap] | anon | file-backed | named anonymous regions |"
    echo "|---|---:|---:|---:|---|"
    for a in $ALLOCS; do
      echo "| $a | $(kib_to_mib "$(srv_name_kb soak "$a" '[heap]')") | $(kib_to_mib "$(srv_name_kb soak "$a" anon)") | $(kib_to_mib "$(srv_name_kb soak "$a" file)") | $(srv_named soak "$a") |"
    done
    echo
    echo "Series: $(for a in $ALLOCS; do printf '%s ' "$(basename "${RESULT%.md}-$a.csv")"; done)(elapsed_s,phase,rss_kb,pss_kb,anon_kb,vmrss_kb,vmhwm_kb,threads, one line per $INTERVAL s; phase is idle, w<i>:high|low or final) and $(for a in $ALLOCS; do printf '%s ' "$(basename "${RESULT%.md}-$a-windows.csv")"; done)(one line per window: ab's req/s and p99 per endpoint, the server's CPU per request, Rss, threads and the pool's size at the window's end). Raw: $OUT/soak.<allocator>/{samples.csv,windows.csv,w<i>-json.ab,w<i>-blob.ab,gc.log,smaps-by-name.txt,nmt-peak.txt,nmt-final.txt,stats.txt,stderr}."
    [ -f "$OUT/soak.failed" ] && { echo; echo "FAILED soak rows: $(tr '\n' ' ' < "$OUT/soak.failed")"; }
  fi
} > "$RESULT"
if has_row soak; then
  for a in $ALLOCS; do
    [ -f "$OUT/soak.$a/samples.csv" ] && cp "$OUT/soak.$a/samples.csv" "${RESULT%.md}-$a.csv"
    [ -f "$OUT/soak.$a/windows.csv" ] && cp "$OUT/soak.$a/windows.csv" "${RESULT%.md}-$a-windows.csv"
  done
fi
echo "== wrote $RESULT"
sed -n '/^## Medians/,$p;/^## Mixed-size soak/,/^### Rss at the end/p' "$RESULT" | head -60

# RESULT_JSONL (cb mode, or any caller): one line per allocator with the
# figures of the rows that ran (MiB, req/s, ms, s; missing ones omitted).
mib_of() { awk -v k="$1" 'BEGIN {if (k=="-"||k=="") print "-"; else printf "%.1f", k/1024}'; }
if [ -n "${RESULT_JSONL:-}" ]; then
  for a in $ALLOCS; do
    set --
    if has_row startup; then set -- "$@" "startup_s=$(med_of startup "$a")" "startup_rss_mib=$(mib_of "$(rss_of startup "$a")")"; fi
    if has_row server; then
      set -- "$@" "server_idle_mib=$(mib_of "$(srv_idle_kb server "$a")")" "server_peak_mib=$(mib_of "$(srv_peak_kb server "$a")")" \
        "server_final_mib=$(mib_of "$(srv_final_kb server "$a")")" "server_hwm_mib=$(mib_of "$(srv_hwm_kb server "$a")")" \
        "json_rps=$(sv_ab "$a" json rps)" "json_p99_ms=$(sv_ab "$a" json p99)" "blob_rps=$(sv_ab "$a" blob rps)" "blob_p99_ms=$(sv_ab "$a" blob p99)" \
        "server_cpu_us_req=$(sv_cpu_us "$a")"
    fi
    if has_row soak; then
      set -- "$@" "soak_idle_mib=$(mib_of "$(srv_idle_kb soak "$a")")" "soak_peak_mib=$(mib_of "$(srv_peak_kb soak "$a")")" \
        "soak_final_mib=$(mib_of "$(srv_final_kb soak "$a")")" "soak_tail_mib=$(mib_of "$(soak_tail_mean "$a")")" \
        "soak_hwm_mib=$(mib_of "$(srv_hwm_kb soak "$a")")" "soak_slope_mib_min=$(soak_slope "$a")" \
        "soak_json_rps_high=$(soak_win "$a" high json_rps mean)" "soak_blob_rps_high=$(soak_win "$a" high blob_rps mean)" \
        "soak_json_p99_ms_high=$(soak_win "$a" high json_p99_ms max)" "soak_cpu_us_req_high=$(soak_win1 "$a" high cpu_us_req)"
    fi
    {
      printf '{"schema":"challoc.bench.driver/v1","driver":"jvm-bench","alloc":"%s","rows":"%s","metrics":{' "$a" "$ROWS"
      sep=
      for kv in "$@"; do
        k=${kv%%=*}; v=${kv#*=}; v=${v#+}
        case $v in ''|-|*[!0-9.eE+-]*) continue;; esac
        printf '%s"%s":%s' "$sep" "$k" "$v"; sep=,
      done
      printf '}}\n'
    } >> "$RESULT_JSONL"
  done
fi

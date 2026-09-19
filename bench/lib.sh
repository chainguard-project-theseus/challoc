# bench/lib.sh: helpers shared by quick.sh, ab.sh and baseline.sh (the two
# benchmark tiers, bench/README.md). POSIX sh; source it after `ROOT` is set
# and the caller has `cd`ed to the repository root inside the dev container.
#
# What lives here: the protocol rows as command lines (the same programs and
# parameters bench/run.sh uses, with fixed thread counts so the rows are
# comparable across hosts), running one row under one library and scoring it,
# building a variant library from a git rev or the working tree into its own
# CARGO_TARGET_DIR, sorted-list/median arithmetic, the cached-baseline file
# name, and the bench lock every tier shares with run.sh.

BIN=$ROOT/target/bench-bin
PROCS=$(nproc)
ARCH=$(uname -m)
DATE=$(date -u +%F)
STAMP=$(date -u +%FT%TZ)
# The repository is bind-mounted and owned by another uid, so git needs
# safe.directory; the worktree case also needs the common .git mounted (the
# Makefile does that).
GIT="git -c safe.directory=*"
JEMALLOC_LIB=/usr/lib/libjemalloc.so.2
# Wolfi's tcmalloc-minimal (gperftools, no profiler hooks): the fourth stock
# allocator since Phase 2.5 task 1.
TCMALLOC_LIB=/usr/lib/libtcmalloc_minimal.so.4
# The Valkey model rows (bench/probes/valkey_model.c): keys and seconds
# (T/2 write, T/2 delete). quick.sh sets VALKEY_MODEL_SECS=5 before sourcing.
VALKEY_MODEL_N=${VALKEY_MODEL_N:-850000}
VALKEY_MODEL_SECS=${VALKEY_MODEL_SECS:-10}
# The redis row (run.sh's recipe): port and request count.
REDIS_PORT=${REDIS_PORT:-6379}
REDIS_N=${REDIS_N:-1000000}
# The ruby-churn row runs on the --without-jemalloc Ruby build (the same
# binary under every allocator, LD_PRELOAD only), installed by ruby-bench.sh.
RUBY_INST=$ROOT/target/ruby/install-glibc
# The python1 rows (Phase 2.5 task 1) run bench/probes/python/json_bench.py
# on the image's python3 (Wolfi python-3.13); python1 leaves PYTHONMALLOC
# unset (pymalloc for objects under 512 bytes), python1-malloc sets
# PYTHONMALLOC=malloc (every object through the allocator).
PYTHON=${PYTHON:-python3}
# PIN: empty/0 = off; 1 = pin the row to CPUs 0..threads-1; any other value is
# a CPU list for bench/pin.c. An experiment for the xmalloc-test spread, see
# bench/README.md; not part of any protocol.
PIN=${PIN:-}

die() { echo "${0##*/}: $*" >&2; exit 1; }
loadavg() { cut -d' ' -f1-3 /proc/loadavg; }
commit_short() { $GIT -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown; }
# tree_dirty: 0 if the working tree differs from HEAD in what the library is
# built from.
tree_dirty() { ! $GIT -C "$ROOT" diff --quiet HEAD -- src Cargo.toml Cargo.lock build.rs 2>/dev/null; }

# take_bench_lock: one protocol run per target volume (see bench/run.sh).
take_bench_lock() {
  exec 9>"$ROOT/target/bench.lock"
  flock -n 9 || die "another bench run holds target/bench.lock; wait for it or check docker ps"
}

# ensure_bench_programs: bench/build.sh is idempotent but slow to even check
# (cmake); skip it when the programs this tier needs are already there.
ensure_bench_programs() {
  for p in rt larson xmalloc-test alloc-test cfrac espresso mstress glibc-simple libmimalloc.so versions.txt; do
    [ -e "$BIN/$p" ] || { echo "== bench programs missing ($p): running bench/build.sh"; sh "$ROOT/bench/build.sh" >"$ROOT/target/bench-build.log" 2>&1 || { tail -20 "$ROOT/target/bench-build.log" >&2; die "bench/build.sh failed"; }; break; }
  done
}
# ensure_probe NAME: bench/probes/NAME.c (churn, valkey_model) or bench/NAME.c
# (pin) compiled on demand into $BIN/NAME; bench/build.sh builds them too.
ensure_probe() {
  src=$ROOT/bench/probes/$1.c; [ -f "$src" ] || src=$ROOT/bench/$1.c
  [ -f "$src" ] || die "no source for probe $1"
  mkdir -p "$BIN"
  [ "$BIN/$1" -nt "$src" ] || cc -O2 -Wall -Wextra -Werror -o "$BIN/$1" "$src" || die "$1.c did not compile"
}
ensure_churn() { ensure_probe churn; }
# ruby_available: 0 when the ruby-churn row can run. The --without-jemalloc
# build (make ruby STAGE=build ALLOC=glibc) is installed into its prefix on
# first use, as ruby-bench.sh does (the build directory's ruby has no stdlib
# path). Prints why not on stderr otherwise.
ruby_available() {
  [ -x "$RUBY_INST/bin/ruby" ] && return 0
  if [ -x "$ROOT/target/ruby/build-glibc/ruby" ]; then
    echo "== installing the glibc Ruby build into ${RUBY_INST#"$ROOT"/} for the ruby-churn row" >&2
    make -C "$ROOT/target/ruby/build-glibc" install > "$ROOT/target/ruby/install-glibc.log" 2>&1 && [ -x "$RUBY_INST/bin/ruby" ] && return 0
    echo "ruby-churn: make install of target/ruby/build-glibc failed, see target/ruby/install-glibc.log" >&2; return 1
  fi
  echo "ruby-churn: skipped, no Ruby build under target/ruby (run 'make ruby STAGE=build ALLOC=glibc' first; the VMs have it, the Mac usually not)" >&2
  return 1
}

# python_available: 0 when the python rows can run (python3 in the image).
python_available() {
  command -v "$PYTHON" >/dev/null 2>&1 && return 0
  echo "python1: skipped, no $PYTHON in this container (rebuild the image from Dockerfile.dev)" >&2; return 1
}

# ---- cgroup memory.peak per phase (plan Phase 2.5 task 17) ------------------
# The kernel's `memory.peak` is a high-water mark since the cgroup's birth;
# from 6.12 a write resets it, but only for the open file description that
# wrote (a later `cat` still shows the birth peak), so the reset and the
# reads go through one long-lived descriptor: the sidecar
# bench/probes/cgpeak.c, driven by the phase file the soak harness writes
# (DIR/phase names the phase, DIR/stop ends the run) and answering with
# DIR/cgpeak.state (`yes <path>` or `no <reason>`), DIR/cgpeak.<phase> (the
# phase's peak in bytes) and DIR/cgpeak.phase (the acknowledged phase). The
# write is probed, never assumed: Docker mounts the container's cgroup tree
# read-only and the nsdelegate boundary would refuse the write from inside
# the namespace anyway, so `make ... CGROUP_PEAK=1` starts the container
# with --cgroupns=host and /sys/fs/cgroup mounted read-write (the
# Makefile); on a kernel before 6.12 the write then fails with EINVAL. The
# reason is printed once and the callers fall back to VmHWM. The peak is the
# whole container's (server, load generators, harness), unlike VmHWM.
CG_PEAK_STATE=unprobed; CG_PEAK_REASON=; CG_PEAK_PID=
# cgroup_peak_start DIR: start the sidecar on DIR (DIR/phase should already
# name the first phase); waits for its verdict, prints it once. 0 when the
# reset is available.
cgroup_peak_start() {
  ensure_probe cgpeak
  rm -f "$1/cgpeak.state" "$1/cgpeak.phase"
  "$BIN/cgpeak" "$1" &
  CG_PEAK_PID=$!
  i=0; while [ ! -e "$1/cgpeak.state" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i+1)); done
  cg_state=$(cat "$1/cgpeak.state" 2>/dev/null || echo "no the cgpeak sidecar gave no verdict in 5 s")
  case $cg_state in
    yes*) CG_PEAK_STATE=yes; CG_PEAK_REASON=${cg_state#yes };;
    *)    CG_PEAK_STATE=no; CG_PEAK_REASON=${cg_state#no }; wait "$CG_PEAK_PID" 2>/dev/null || true; CG_PEAK_PID=;;
  esac
  if [ "$CG_PEAK_STATE" = yes ]; then echo "== cgroup memory.peak: $CG_PEAK_REASON accepts a reset (kernel $(uname -r)); per-phase peaks are the cgroup's" >&2
  else echo "== cgroup memory.peak: not reset per phase, VmHWM only: $CG_PEAK_REASON" >&2; fi
  [ "$CG_PEAK_STATE" = yes ]
}
# cgroup_peak_phase DIR NAME: the harness has written NAME to DIR/phase;
# wait (up to 3 s) until the sidecar has recorded the previous phase and
# reset, so the new phase's load starts after the reset. No-op without it.
cgroup_peak_phase() {
  [ "$CG_PEAK_STATE" = yes ] || return 0
  i=0; while [ "$(cat "$1/cgpeak.phase" 2>/dev/null)" != "$2" ] && [ $i -lt 30 ]; do sleep 0.1; i=$((i+1)); done
  [ "$(cat "$1/cgpeak.phase" 2>/dev/null)" = "$2" ] || echo "== cgroup memory.peak: the sidecar did not acknowledge phase $2 within 3 s" >&2
}
# cgroup_peak_stop DIR: the harness has touched DIR/stop; wait for the last
# phase's record.
cgroup_peak_stop() { [ -n "$CG_PEAK_PID" ] && { wait "$CG_PEAK_PID" 2>/dev/null || true; CG_PEAK_PID=; }; :; }
# cgroup_peak_mib DIR PHASE: the phase's peak in MiB, "-" without a record.
cgroup_peak_mib() { awk -v f="$1/cgpeak.$2" 'BEGIN {if ((getline b < f) > 0 && b+0 > 0) printf "%.1f", b/1048576; else print "-"}'; }
# cgroup_peak_file_mib DIR PHASE: the cgroup's page cache (memory.stat file)
# at the end of PHASE in MiB, "-" without a record; the peak counts it.
cgroup_peak_file_mib() { awk -v f="$1/cgpeak.$2.file" 'BEGIN {if ((getline b < f) > 0 && b+0 >= 0) printf "%.1f", b/1048576; else print "-"}'; }
# cgroup_peak_note: one line for a results file header.
cgroup_peak_note() {
  if [ "$CG_PEAK_STATE" = yes ]; then echo "cgroup memory.peak reset per phase through $CG_PEAK_REASON (kernel $(uname -r), bench/probes/cgpeak.c); the cgroup peak counts every process of the container and its page cache (memory.stat file, given per phase below), VmHWM the server alone"
  else echo "cgroup memory.peak not reset per phase, VmHWM only: $CG_PEAK_REASON"; fi
}

# ---- THP counters around a row (plan Phase 2.5 task 17) ---------------------
# CHALLOC_THP_COUNTERS=1 snapshots every thp_* line of /proc/vmstat and every
# per-size counter under /sys/kernel/mm/transparent_hugepage/hugepages-<size>kB/
# stats/ (6.12+: anon_fault_alloc, anon_fault_fallback, anon_fault_fallback_charge,
# swpout, swpout_fallback, split, split_failed, split_deferred, nr_anon, ...)
# before and after each run of run_row and writes the deltas beside the run's
# log ($OUT/TAG.<n>.thp, "name=delta" for every counter that moved; the
# per-size names are "<size>kB/<counter>"). ab.sh and quick.sh print them as
# a section after their tables; bench/gce/quick-perf.sh takes the same
# snapshots on the VM host around each container. The counters are
# host-global (there is no per-cgroup THP counter), so the deltas include
# anything else the host faulted in the meantime; on a quiet VM that is
# noise of a few pages. Readable from inside a container (sysfs and
# /proc/vmstat are the host's).
THP_SYSFS=/sys/kernel/mm/transparent_hugepage
CHALLOC_THP_COUNTERS=${CHALLOC_THP_COUNTERS:-0}
thp_snapshot() { # thp_snapshot FILE: "name value" lines
  { grep '^thp_' /proc/vmstat 2>/dev/null
    for d in "$THP_SYSFS"/hugepages-*kB; do
      [ -d "$d/stats" ] || continue
      sz=${d##*/hugepages-}
      for f in "$d"/stats/*; do [ -r "$f" ] && echo "$sz/${f##*/} $(cat "$f")"; done
    done
  } > "$1" 2>/dev/null
}
# thp_delta BEFORE AFTER: one line, "name=delta ..." for the counters that moved ("none" if none)
thp_delta() {
  awk 'NR==FNR {b[$1]=$2; next} ($1 in b) && $2 != b[$1] {printf "%s=%d ", $1, $2 - b[$1]; n++} END {if (!n) printf "none"; print ""}' "$1" "$2"
}
# thp_state: the sysfs table in one line (enabled, defrag, the per-size enabled values that are not never)
thp_state() {
  en=$(cat "$THP_SYSFS/enabled" 2>/dev/null | sed -n 's/.*\[\(.*\)\].*/\1/p'); df=$(cat "$THP_SYSFS/defrag" 2>/dev/null | sed -n 's/.*\[\(.*\)\].*/\1/p')
  sizes=; for d in "$THP_SYSFS"/hugepages-*kB; do [ -f "$d/enabled" ] || continue; v=$(sed -n 's/.*\[\(.*\)\].*/\1/p' "$d/enabled"); [ "$v" = never ] || sizes="$sizes ${d##*/hugepages-}=$v"; done
  echo "enabled=${en:--} defrag=${df:--}${sizes:+ per-size:$sizes}$([ -d "$THP_SYSFS/hugepages-64kB/stats" ] && echo " (per-size stats/ present)" || echo " (no per-size stats/ directory: /proc/vmstat thp_* only)")"
}
# thp_mean_deltas GLOB: the per-counter mean over the .thp files matching
# GLOB (the timed runs of one row and variant), "name=mean" sorted by name.
thp_mean_deltas() {
  cat $1 2>/dev/null | tr ' ' '\n' | awk -F= 'NF==2 {s[$1]+=$2; c[$1]++} END {for (k in s) printf "%s=%.1f\n", k, s[k]/c[k]}' | sort | tr '\n' ' ' | sed 's/ $//'
}

# ---- protocol rows ----------------------------------------------------------

stock_lib() { # stock_lib ALLOC: LD_PRELOAD path for jemalloc|glibc|mimalloc|tcmalloc, 1 otherwise
  case $1 in
    jemalloc) echo "$JEMALLOC_LIB";;
    mimalloc) echo "$BIN/libmimalloc.so";;
    tcmalloc) echo "$TCMALLOC_LIB";;
    glibc)    echo "";;
    *) return 1;;
  esac
}
is_stock() { case $1 in jemalloc|glibc|mimalloc|tcmalloc) return 0;; *) return 1;; esac; }
# apk_version PKG: the installed version of a Wolfi package ("5.3.1" for
# jemalloc-5.3.1-r3), empty when it is not installed.
apk_version() { apk list --installed 2>/dev/null | awk -v p="^$1-[0-9]" '$1 ~ p {print $1}' | sed "s/^$1-//;s/-r[0-9]*\$//" | head -1; }
# stock_version ALLOC: a short version string for a stock allocator.
stock_version() {
  case $1 in
    jemalloc) echo "jemalloc $(apk_version jemalloc)";;
    tcmalloc) echo "tcmalloc-minimal $(apk_version tcmalloc-minimal)";;
    mimalloc) echo "mimalloc $(awk '/^mimalloc /{print $2}' "$BIN/versions.txt" 2>/dev/null)";;
    glibc)    echo "glibc $(apk_version glibc-2.44 || true)";;
  esac
}

# bench_args BENCH: the command line of one row. larson/xmalloc rows are fixed
# duration (5 s) and scored by the throughput they print; the Valkey model
# rows run VALKEY_MODEL_SECS and are scored by their write-phase M ops/s;
# redis by redis-benchmark's k requests/s (run_row handles the server); the
# rest by wall time. ruby-churn is bench/ruby/gc_churn.rb on the glibc Ruby
# build and needs that build (ruby_available says why when it cannot run);
# python1 and python1-malloc are bench/probes/python/json_bench.py with
# PYTHONMALLOC unset and =malloc (wall time).
bench_args() {
  case $1 in
    larson1)      echo "$BIN/larson 5 8 1000 5000 100 4141 1";;
    larson10)     echo "$BIN/larson 5 8 1000 5000 100 4141 10";;
    larson16)     echo "$BIN/larson 5 8 1000 5000 100 4141 16";;
    larson64)     echo "$BIN/larson 5 8 1000 5000 100 4141 64";;   # threads > CPUs (plan Phase 2.5 task 8)
    larsonN)      echo "$BIN/larson 5 8 1000 5000 100 4141 $PROCS";;
    xmalloc)      echo "$BIN/xmalloc-test -w 10 -t 5 -s 64";;
    xmallocN)     echo "$BIN/xmalloc-test -w $PROCS -t 5 -s 64";;
    alloc-test1)  echo "$BIN/alloc-test 1";;
    alloc-testN)  n=$PROCS; [ "$n" -gt 16 ] && n=16; echo "$BIN/alloc-test $n";;
    cfrac)        echo "$BIN/cfrac 17545186520507317056371138836327483792789528";;
    espresso)     echo "$BIN/espresso $ROOT/target/bench-src/mimalloc-bench/bench/espresso/largest.espresso";;
    mstress)      echo "$BIN/mstress $PROCS 50 25";;
    glibc-simple) echo "$BIN/glibc-simple";;
    valkey1)      echo "$BIN/valkey_model -n $VALKEY_MODEL_N -t $VALKEY_MODEL_SECS -s";;
    valkey1-free) echo "$BIN/valkey_model -n $VALKEY_MODEL_N -t $VALKEY_MODEL_SECS -s -f free";;
    valkey1-usable) echo "$BIN/valkey_model -n $VALKEY_MODEL_N -t $VALKEY_MODEL_SECS -s -u";;
    # Write phase only (SET over a full heap), so a per-operation counter is
    # per SET: the instruction budget of plan Phase 2.5 task 4c.
    valkey1-write) echo "$BIN/valkey_model -n $VALKEY_MODEL_N -w $VALKEY_MODEL_SECS -d 0 -s";;
    redis)        echo "$BIN/redis-server";;
    ruby-churn)   ruby_available || return 1
                  echo "env LD_LIBRARY_PATH=$RUBY_INST/lib $RUBY_INST/bin/ruby --disable-gems $ROOT/bench/ruby/gc_churn.rb 10 10";;
    python1)      python_available || return 1
                  echo "$PYTHON $ROOT/bench/probes/python/json_bench.py";;
    python1-malloc) python_available || return 1
                  echo "env PYTHONMALLOC=malloc $PYTHON $ROOT/bench/probes/python/json_bench.py";;
    *) return 1;;
  esac
}
higher_is_better() { case $1 in larson*|xmalloc*|valkey*|redis) return 0;; *) return 1;; esac; }
unit_of() { case $1 in larson*|valkey*) echo "M ops/s";; xmalloc*) echo "M frees/s";; redis) echo "k req/s";; *) echo "s";; esac; }
# threads_of BENCH: how many CPUs the row keeps busy (for PIN=1).
threads_of() {
  case $1 in
    larson1|alloc-test1|cfrac|espresso|glibc-simple|valkey*|ruby-churn|python*) echo 1;;
    larson10|xmalloc) echo 10;;
    larson16) echo 16;;
    larson64) echo 64;;
    alloc-testN) n=$PROCS; [ "$n" -gt 16 ] && n=16; echo "$n";;
    redis) echo 2;;
    *) echo "$PROCS";;
  esac
}
# pin_cpus BENCH: the CPU list PIN selects for a row: PIN=1 gives CPUs
# 0..threads-1 (a single-threaded row gets CPU 1, away from CPU 0's
# interrupts, when the machine has one), anything else is taken verbatim.
pin_cpus() {
  case $PIN in
    1) k=$(threads_of "$1"); [ "$k" -gt "$PROCS" ] && k=$PROCS
       if [ "$k" -le 1 ]; then [ "$PROCS" -gt 1 ] && echo 1 || echo 0; else echo "0-$((k-1))"; fi;;
    *) echo "$PIN";;
  esac
}
# secs_of BENCH: the fixed duration of a throughput row ("-" for wall-time rows).
secs_of() { case $1 in larson*|xmalloc*) echo 5;; valkey*) echo "$VALKEY_MODEL_SECS";; *) echo -;; esac; }
# ops_of BENCH LOG: the operation count of a finished run, for per-operation
# counters ("-" when the program reports none).
ops_of() {
  case $1 in
    larson*)  awk -v secs="$(secs_of "$1")" '/Throughput/ {printf "%.0f", $3*secs}' "$2";;   # larson's "relative time" is 1e9/throughput, not the run time
    xmalloc*) awk '/free\/sec/ {sub(",", "", $4); printf "%.0f", $4*1e6*5}' "$2";;
    valkey*)  awk '/^ops / {for (i=2;i<=NF;i++) {split($i, p, "="); if (p[1]=="total") print p[2]}}' "$2";;
    redis)    echo $((2 * REDIS_N));;   # lpush and lrange, REDIS_N requests each
    glibc-simple) echo 64000000;;   # 16 do_benchmark passes (4 warm-up + 3 scenarios x 4 counts) of NUM_ITERS=2M malloc+free pairs
    *) echo "-";;
  esac
}

# run_row BENCH LIB CONF TAG: one run of BENCH with LD_PRELOAD=LIB and
# MALLOC_CONF=CONF (empty: unset). Appends "score maxrss_kb exit" to
# $OUT/TAG.scores; the program's output is $OUT/TAG.<n>.out. A failed run
# scores "-" so the sorted lists stay aligned.
run_row() {
  bench=$1; lib=$2; conf=$3; tag=$4
  [ -f "$OUT/$tag.scores" ] || : > "$OUT/$tag.scores"
  n=$(wc -l < "$OUT/$tag.scores")
  log=$OUT/$tag.$n.out; rt=$OUT/$tag.$n.rt
  bench_args "$bench" >/dev/null || die "unknown or unavailable bench $bench"
  case $bench in valkey*) ensure_probe valkey_model;; esac
  set -- $(bench_args "$bench")
  if [ -n "$PIN" ] && [ "$PIN" != 0 ]; then
    ensure_probe pin
    set -- "$BIN/pin" "$(pin_cpus "$bench")" "$@"
  fi
  [ "$CHALLOC_THP_COUNTERS" = 1 ] && thp_snapshot "$OUT/$tag.$n.thp0"
  if [ "$bench" = redis ]; then
    run_redis "$lib" "$conf" "$log" "$rt" "$@"
  elif [ -n "$conf" ]; then
    RT_OUT=$rt "$BIN/rt" env MALLOC_CONF="$conf" LD_PRELOAD="$lib" "$@" > "$log" 2>&1 || true
  else
    RT_OUT=$rt "$BIN/rt" env LD_PRELOAD="$lib" "$@" > "$log" 2>&1 || true
  fi
  if [ "$CHALLOC_THP_COUNTERS" = 1 ]; then
    thp_snapshot "$OUT/$tag.$n.thp1"; thp_delta "$OUT/$tag.$n.thp0" "$OUT/$tag.$n.thp1" > "$OUT/$tag.$n.thp"; rm -f "$OUT/$tag.$n.thp0" "$OUT/$tag.$n.thp1"
  fi
  read -r el _us _sy rss ex < "$rt"
  case $bench in
    larson*)  s=$(awk '/Throughput/ {printf "%.1f", $3/1e6}' "$log");;
    xmalloc*) s=$(awk '/free\/sec/ {sub(",", "", $4); print $4}' "$log");;
    valkey*)  s=$(awk '/^ops / {for (i=2;i<=NF;i++) {split($i, p, "="); if (p[1]=="write_mops") print p[2]}}' "$log");;
    redis)    s=$(awk '/requests per second/ {for(i=1;i<=NF;i++) if($(i+1)=="requests") v=$i} END {if (v) printf "%.1f", v/1e3}' "$log");;
    *)        s=$(awk -v e="$el" 'BEGIN {printf "%.3f", e}');;
  esac
  [ "$ex" = 0 ] || { s=-; echo "  $tag run $n failed (exit $ex), see $log" >&2; }
  echo "${s:--} $rss $ex" >> "$OUT/$tag.scores"
}

# run_redis LIB CONF LOG RT SERVER...: bench/run.sh's redis recipe. The
# server runs under the allocator (RSS is the server's, from its own rt
# line), the client is redis-benchmark -r 1000000 -n REDIS_N -P 16 lpush/lrange
# and its wall time and exit status make the row's rt line.
run_redis() {
  lib=$1; conf=$2; log=$3; rt=$4; shift 4
  srv_rt=$rt.server
  : > "$srv_rt"
  if [ -n "$conf" ]; then
    RT_OUT=$srv_rt "$BIN/rt" env MALLOC_CONF="$conf" LD_PRELOAD="$lib" "$@" --port "$REDIS_PORT" --save "" --appendonly no --daemonize no --logfile /dev/null >/dev/null 2>&1 &
  else
    RT_OUT=$srv_rt "$BIN/rt" env LD_PRELOAD="$lib" "$@" --port "$REDIS_PORT" --save "" --appendonly no --daemonize no --logfile /dev/null >/dev/null 2>&1 &
  fi
  i=0
  until [ "$("$BIN/redis-cli" -p "$REDIS_PORT" ping 2>/dev/null)" = PONG ]; do
    i=$((i+1)); [ $i -gt 100 ] && { echo "redis-server did not start (port $REDIS_PORT busy?)" >&2; echo "0 0 0 0 1" > "$rt"; return; }
    sleep 0.1
  done
  "$BIN/redis-cli" -p "$REDIS_PORT" flushall >/dev/null
  RT_OUT=$rt.client "$BIN/rt" "$BIN/redis-benchmark" -p "$REDIS_PORT" -r 1000000 -n "$REDIS_N" -q -P 16 lpush a 1 2 3 4 5 lrange a 1 5 2>&1 | tr '\r' '\n' > "$log" || true
  "$BIN/redis-cli" -p "$REDIS_PORT" shutdown nosave >/dev/null 2>&1 || true
  wait
  # elapsed/user/sys from the client, maxrss from the server, exit = client's
  # or 1 when the server's rt line is missing.
  awk 'FNR==NR {c=$0} FILENAME!=ARGV[1] {s=$0} END {
    split(c, a, " "); split(s, b, " ")
    if (!length(s)) b[4]="-"
    printf "%s %s %s %s %s\n", a[1], a[2], a[3], b[4], (length(s) ? a[5] : 1) }' "$rt.client" "$srv_rt" > "$rt"
}

# run_churn LIB CONF TAG SECS CLASS: the churn probe (bench/probes/churn.c,
# Phase 1 acceptance 3 (c)). Writes "deleted_slabs end_slabs ops_per_s exit
# end_occupancy_pct" to $OUT/TAG.churn. The occupancy is what decides
# compaction: it does not depend on the class's slab size, while the slab
# count does (469 -> under 230 in the plan is 64 KiB slabs, i.e. about 90%).
run_churn() {
  lib=$1; conf=$2; tag=$3; secs=$4; class=$5
  log=$OUT/$tag.churn.out
  if [ -n "$conf" ]; then
    env MALLOC_CONF="$conf" LD_PRELOAD="$lib" "$BIN/churn" "$class" 30000 "$secs" 0.56 > "$log" 2>&1; ex=$?
  else
    env LD_PRELOAD="$lib" "$BIN/churn" "$class" 30000 "$secs" 0.56 > "$log" 2>&1; ex=$?
  fi
  awk -v ex="$ex" '
    $2=="deleted" && !d {d=$4}
    $2=="end" {e=$4; occ=$10; sub(/%/, "", occ)}
    /^ops/ {o=$3; gsub(/[^0-9.]/, "", o)}
    END {printf "%s %s %s %s %s\n", d?d:"-", e?e:"-", o?o:"-", ex, occ?occ:"-"}' "$log" > "$OUT/$tag.churn"
}

# ---- arithmetic on score files ---------------------------------------------

# sorted_scores FILE SKIP: column 1 of FILE from line SKIP+1 on, numerically
# sorted, space-separated ("-" entries dropped).
sorted_scores() { awk -v s="$2" 'NR>s && $1!="-" {print $1}' "$1" | sort -n | tr '\n' ' ' | sed 's/ $//'; }
# median_rss FILE SKIP: median of column 2 (maxrss KiB) from line SKIP+1 on.
median_rss() { awk -v s="$2" 'NR>s {print $2}' "$1" | sort -n | awk '{a[NR]=$1} END {if (NR) print a[int((NR+1)/2)]; else print "-"}'; }
# median_list "v1 v2 ...": median of a sorted list.
median_list() { echo "$1" | tr ' ' '\n' | awk 'NF {a[++n]=$1} END {if (n) print a[int((n+1)/2)]; else print "-"}'; }
# any_failed FILE SKIP: 1 if any run from line SKIP+1 exited non-zero.
any_failed() { awk -v s="$2" 'NR>s && $3!=0 {f=1} END {print f+0}' "$1"; }
# worse_pct BENCH NEW REF: signed percentage, positive when NEW did worse
# than REF on BENCH (run.sh's orientation); "-" when either is missing.
worse_pct() {
  if higher_is_better "$1"; then a=$3; b=$2; else a=$2; b=$3; fi
  awk -v a="$a" -v b="$b" 'BEGIN {if (a=="-"||b=="-"||a==""||b==""||b==0) print "-"; else printf "%+.1f%%", (a/b-1)*100}'
}
# pct_of A B: (A/B-1)*100 signed.
pct_of() { awk -v a="$1" -v b="$2" 'BEGIN {if (a=="-"||b=="-"||a==""||b==""||b==0) print "-"; else printf "%+.1f%%", (a/b-1)*100}'; }
# lists_overlap "list1" "list2": 0 when the ranges [min,max] intersect.
lists_overlap() {
  awk -v x="$1" -v y="$2" 'BEGIN {
    nx=split(x, a, " "); ny=split(y, b, " ")
    if (!nx || !ny) exit 0
    exit !(a[1] <= b[ny] && b[1] <= a[nx]) }'
}
kib_to_mib() { awk -v k="$1" 'BEGIN {if (k=="-"||k=="") print "-"; else printf "%.1f", k/1024}'; }

# ---- variant libraries ------------------------------------------------------

# resolve_rev REV: full hash of a git rev, or "tree" for the working tree.
resolve_rev() {
  case $1 in tree|.|WORKTREE) echo tree;; *) $GIT -C "$ROOT" rev-parse --verify "$1^{commit}" 2>/dev/null || die "cannot resolve rev '$1' (in a worktree the Makefile must mount the common .git)";; esac
}
# build_lib REV TARGETDIR: build libchalloc.so from REV ("tree" = this
# working tree, otherwise `git archive REV` into TARGETDIR/src-tree) with
# CARGO_TARGET_DIR=TARGETDIR. Paths are remapped so identical sources give an
# identical .so and the callers can tell "no code change" from "stale build".
# Fails unless cargo succeeded and the .so exists. Prints nothing on success.
build_lib() {
  rev=$1; tdir=$2
  mkdir -p "$tdir"
  if [ "$rev" = tree ]; then
    srcdir=$ROOT
    # Restored or rsynced files may carry old mtimes and cargo would skip the
    # rebuild; touching the crate root forces it (~seconds).
    touch "$ROOT/src/lib.rs"
  else
    srcdir=$tdir/src-tree
    rm -rf "$srcdir"; mkdir -p "$srcdir"
    $GIT -C "$ROOT" archive "$rev" | tar -x -C "$srcdir" || die "git archive $rev failed"
    [ -f "$srcdir/Cargo.toml" ] || die "git archive $rev produced no tree"
    # git archive stamps every file with the commit time, which is older
    # than the previous build in this target dir: cargo would call the old
    # library fresh. Touch the tree so it always recompiles.
    find "$srcdir" -type f -exec touch {} +
  fi
  if ! (cd "$srcdir" && RUSTFLAGS="--remap-path-prefix=$srcdir=/challoc" CARGO_TARGET_DIR=$tdir cargo build --release > "$tdir/build.log" 2>&1); then
    grep -E '^(error|warning)' -A5 "$tdir/build.log" | head -40 >&2
    die "build of $rev into $tdir failed (see $tdir/build.log)"
  fi
  # "compiled" means cargo actually ran rustc on the crate this time; a
  # "Finished" without "Compiling challoc" is a stale reuse.
  grep -q 'Compiling challoc' "$tdir/build.log" || die "cargo reused a previous build in $tdir instead of compiling $rev (stale mtimes?); see $tdir/build.log"
  [ -f "$tdir/release/libchalloc.so" ] || die "build of $rev produced no $tdir/release/libchalloc.so"
}
so_hash() { sha256sum "$1" | cut -c1-16; }

# ---- cached baselines --------------------------------------------------------

BASELINE_DIR=$ROOT/target/baselines
# bench_build_hash: identifies the benchmark build this VM runs (program
# versions, kernel, CPU count); a cached baseline is only valid for the same.
bench_build_hash() { { cat "$BIN/versions.txt" 2>/dev/null; uname -r; nproc; } | sha256sum | cut -c1-12; }
baseline_file() { echo "$BASELINE_DIR/$ARCH-$(bench_build_hash).md"; }
# baseline_row FILE BENCH VARIANT: "sorted scores|maxrss" from a baseline (or
# any ab.sh results) file, empty if absent.
baseline_row() { awk -F'|' -v b="$2" -v v="$3" '{gsub(/^ +| +$/, "", $2); gsub(/^ +| +$/, "", $3); gsub(/^ +| +$/, "", $4); gsub(/^ +| +$/, "", $5)} $2==b && $3==v {print $4 "|" $5; exit}' "$1"; }
baseline_date() { awk -F': ' '/^- date: /{print $2; exit}' "$1"; }

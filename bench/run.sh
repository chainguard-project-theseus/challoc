#!/bin/sh
# Phase 0 benchmark protocol (docs/plan.md, "Benchmark protocol"), run inside
# the dev container after bench/build.sh: `make bench`.
#
# For every benchmark and allocator: one discarded warm-up, then RUNS timed
# runs through bench/rt (wall time, max RSS). The same binary is used for
# every allocator; only LD_PRELOAD changes. Medians land in
# bench/results/<date>-<arch>.md together with the raw runs and, for challoc,
# the larson sample profile from bench/profile.sh.
#
# larson and xmalloc-test run for a fixed number of seconds, so their score is
# the throughput they print (higher is better) rather than wall time; redis
# scores the requests/second that redis-benchmark reports for a fixed request
# count. Everything else scores wall time (lower is better). The "vs jemalloc"
# column is oriented so that a positive percentage always means challoc did
# worse.
#
#   RUNS=5                  timed runs per cell
#   ALLOCS="challoc jemalloc glibc mimalloc"
#   BENCHES="cfrac espresso larson mstress xmalloc-test alloc-test1 alloc-testN glibc-simple redis"
#   HOST_CPU="..."          recorded in the header (the container only sees an implementer id)
#   HOST_KIND="..."         what runs the kernel (default: "Docker Desktop VM" on a linuxkit
#                           kernel, "host kernel" otherwise; e.g. "GCE c4a-standard-16, Ubuntu 24.04")
#   RESULT=path             results file (default bench/results/<date>-<arch>.md)
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
BIN=$ROOT/target/bench-bin
SRC=$ROOT/target/bench-src
OUT=${OUT:-$ROOT/target/bench-out}
RUNS=${RUNS:-5}
PROCS=$(nproc)
ARCH=$(uname -m)
DATE=$(date -u +%F)
RESULT=${RESULT:-$ROOT/bench/results/$DATE-$ARCH.md}
ALLOCS=${ALLOCS:-"challoc jemalloc glibc mimalloc"}
BENCHES=${BENCHES:-"cfrac espresso larson mstress xmalloc-test alloc-test1 alloc-testN glibc-simple redis"}
CHALLOC=$ROOT/target/release/libchalloc.so
COMMIT=$(git -c safe.directory='*' rev-parse --short HEAD 2>/dev/null || echo unknown)
# mimalloc-bench uses -n 100000, which finishes in about 0.15 s here and is
# too short to compare at 10%; ten times that keeps the same request mix.
REDIS_N=${REDIS_N:-1000000}

[ -x "$BIN/rt" ] || { echo "run bench/build.sh first" >&2; exit 1; }
[ -f "$CHALLOC" ] || { echo "cargo build --release first" >&2; exit 1; }
# One protocol run at a time. A `make bench` container outlives the host
# `make` that started it when that process is killed, and a second run in the
# same target volume would then share the CPUs and append to the same raw
# files. The lock lives in the shared volume so it is visible across containers
# and is released by the kernel when the holder exits.
exec 9>"$ROOT/target/bench.lock"
flock -n 9 || { echo "another bench run holds target/bench.lock; wait for it or check docker ps" >&2; exit 1; }
rm -rf "$OUT"
mkdir -p "$OUT" "$(dirname "$RESULT")"

lib_for() {
  case $1 in
    challoc)  echo "$CHALLOC";;
    jemalloc) echo /usr/lib/libjemalloc.so.2;;
    mimalloc) echo "$BIN/libmimalloc.so";;
    glibc)    echo "";;
    *) echo "unknown allocator $1" >&2; exit 1;;
  esac
}

# run_one BENCH ALLOC RUN: append one rt line to $OUT/BENCH.ALLOC.rt
run_one() {
  bench=$1; alloc=$2; run=$3
  lib=$(lib_for "$alloc")
  log=$OUT/$bench.$alloc.$run.out
  case $bench in
    cfrac)        set -- "$BIN/cfrac" 17545186520507317056371138836327483792789528;;
    espresso)     set -- "$BIN/espresso" "$SRC/mimalloc-bench/bench/espresso/largest.espresso";;
    larson)       set -- "$BIN/larson" 5 8 1000 5000 100 4141 "$PROCS";;
    mstress)      set -- "$BIN/mstress" "$PROCS" 50 25;;
    xmalloc-test) set -- "$BIN/xmalloc-test" -w "$PROCS" -t 5 -s 64;;
    alloc-test1)  set -- "$BIN/alloc-test" 1;;
    alloc-testN)  n=$PROCS; [ "$n" -gt 16 ] && n=16; set -- "$BIN/alloc-test" "$n";;
    glibc-simple) set -- "$BIN/glibc-simple";;
    redis)
      # Server under the allocator; time is the client's wall time for the
      # fixed request count, RSS is the server's.
      RT_OUT=$OUT/redis-server.$alloc.rt "$BIN/rt" env LD_PRELOAD="$lib" \
        "$BIN/redis-server" --port 6379 --save "" --appendonly no --daemonize no \
        --logfile /dev/null >/dev/null 2>&1 &
      i=0
      until [ "$("$BIN/redis-cli" -p 6379 ping 2>/dev/null)" = PONG ]; do
        i=$((i+1)); [ $i -gt 100 ] && { echo "redis-server did not start" >&2; exit 1; }
        sleep 0.1
      done
      "$BIN/redis-cli" -p 6379 flushall >/dev/null
      RT_OUT=$OUT/redis.$alloc.rt "$BIN/rt" "$BIN/redis-benchmark" -p 6379 \
        -r 1000000 -n "$REDIS_N" -q -P 16 lpush a 1 2 3 4 5 lrange a 1 5 2>&1 | tr '\r' '\n' >"$log" || true
      "$BIN/redis-cli" -p 6379 shutdown nosave >/dev/null 2>&1 || true
      wait
      return;;
    *) echo "unknown benchmark $bench" >&2; exit 1;;
  esac
  RT_OUT=$OUT/$bench.$alloc.rt "$BIN/rt" env LD_PRELOAD="$lib" "$@" >"$log" 2>&1 \
    || { echo "  $bench under $alloc failed (exit $?), see $log" >&2; tail -5 "$log" >&2; }
}

echo "== $DATE $ARCH commit $COMMIT: $RUNS runs (+1 warm-up) x $(echo $BENCHES | wc -w) benches x $(echo $ALLOCS | wc -w) allocators"
for bench in $BENCHES; do
  for alloc in $ALLOCS; do
    printf '  %-13s %-9s' "$bench" "$alloc"
    r=0
    while [ $r -le "$RUNS" ]; do run_one "$bench" "$alloc" $r; printf .; r=$((r+1)); done
    echo
  done
done

# Every raw file must hold exactly the warm-up plus RUNS lines; anything else
# means a concurrent writer and the medians would be meaningless.
for bench in $BENCHES; do
  for a in $ALLOCS; do
    n=$(wc -l <"$OUT/$bench.$a.rt")
    [ "$n" -eq $((RUNS+1)) ] || { echo "$OUT/$bench.$a.rt has $n lines, expected $((RUNS+1)): concurrent run?" >&2; exit 1; }
  done
done

# score BENCH ALLOC: one score per run (warm-up first) into $OUT/BENCH.ALLOC.score
score() {
  case $1 in
    larson)       r=0; while [ $r -le "$RUNS" ]; do awk '/Throughput/ {printf "%.1f\n", $3/1e6}' "$OUT/$1.$2.$r.out"; r=$((r+1)); done;;
    xmalloc-test) r=0; while [ $r -le "$RUNS" ]; do awk '/free\/sec/ {sub(",", "", $4); print $4}' "$OUT/$1.$2.$r.out"; r=$((r+1)); done;;
    redis)        r=0; while [ $r -le "$RUNS" ]; do awk '/requests per second/ {for(i=1;i<=NF;i++) if($(i+1)=="requests") v=$i} END {printf "%.1f\n", v/1e3}' "$OUT/$1.$2.$r.out"; r=$((r+1)); done;;
    *)            awk '{print $1}' "$OUT/$1.$2.rt";;
  esac > "$OUT/$1.$2.score"
}
higher_is_better() { case $1 in larson|xmalloc-test|redis) return 0;; *) return 1;; esac; }
unit() { case $1 in larson) echo "M ops/s";; xmalloc-test) echo "M frees/s";; redis) echo "k req/s";; *) echo "s";; esac; }

# median FILE COL: median of column COL over lines 2..N (line 1 is the warm-up)
median() {
  awk -v c="$2" 'NR>1 {v[++n]=$c} END {if(!n){print "-";exit} asort_fallback(v,n); print v[int((n+1)/2)]}
    function asort_fallback(a,n,  i,j,t){for(i=2;i<=n;i++){t=a[i];for(j=i-1;j>=1&&a[j]>t;j--)a[j+1]=a[j];a[j+1]=t}}' "$1"
}
# any_failed FILE: 1 if any run exited non-zero
any_failed() { awk 'NR>1 && $5!=0 {f=1} END {print f+0}' "$1"; }

{
  echo "# challoc Phase 0 benchmarks, $DATE, $ARCH"
  echo
  echo "- commit: $COMMIT"
  echo "- host: ${HOST_CPU:-unknown CPU}, $PROCS CPUs visible to the container"
  echo "- kernel: $(uname -r) (${HOST_KIND:-$(case $(uname -r) in *linuxkit*) echo "Docker Desktop VM";; *) echo "host kernel";; esac)}), container: Wolfi"
  sed 's/^/- /' "$BIN/versions.txt"
  echo "- protocol: $RUNS runs after one discarded warm-up; median wall time (s) and max RSS (MiB) from wait4 rusage via bench/rt; identical binaries, only LD_PRELOAD changes"
  echo "- larson, mstress, xmalloc-test and alloc-testN use $PROCS threads"
  echo "- scores: wall time in seconds (lower is better) except larson (M ops/s), xmalloc-test (M frees/s) and redis (k requests/s from redis-benchmark -n $REDIS_N -P 16; RSS is redis-server's), which are higher-is-better"
  echo
  echo "## Medians"
  echo
  hdr="| benchmark | unit |"; sep="|---|---|"
  for a in $ALLOCS; do hdr="$hdr $a score | $a RSS MiB |"; sep="$sep---:|---:|"; done
  cmp=0
  case " $ALLOCS " in *" challoc "*) case " $ALLOCS " in *" jemalloc "*) cmp=1;; esac;; esac
  [ "$cmp" = 1 ] && { hdr="$hdr score vs jemalloc | RSS vs jemalloc |"; sep="$sep---:|---:|"; }
  echo "$hdr"; echo "$sep"
  for bench in $BENCHES; do
    row="| $bench | $(unit "$bench") |"
    ct=; cr=; jt=; jr=
    for a in $ALLOCS; do
      score "$bench" "$a"
      tf=$OUT/$bench.$a.rt
      rf=$tf; [ "$bench" = redis ] && rf=$OUT/redis-server.$a.rt
      t=$(median "$OUT/$bench.$a.score" 1); r=$(median "$rf" 4)
      rm=$(awk -v k="$r" 'BEGIN {if (k=="-") print "-"; else printf "%.1f", k/1024}')
      flag=""; [ "$(any_failed "$tf")" = 1 ] && flag=" (FAILED)"
      row="$row $t$flag | $rm |"
      [ "$a" = challoc ] && { ct=$t; cr=$r; }
      [ "$a" = jemalloc ] && { jt=$t; jr=$r; }
    done
    if [ "$cmp" = 1 ]; then
      if higher_is_better "$bench"; then x=$jt; y=$ct; else x=$ct; y=$jt; fi
      row="$row $(awk -v a="$x" -v b="$y" 'BEGIN {if(a=="-"||b=="-"||b==0)print "-";else printf "%+.1f%%", (a/b-1)*100}') |"
      row="$row $(awk -v a="$cr" -v b="$jr" 'BEGIN {if(a=="-"||b=="-"||b==0)print "-";else printf "%+.1f%%", (a/b-1)*100}') |"
    fi
    echo "$row"
  done
  echo
  echo "Positive percentages mean challoc did worse than jemalloc (slower, lower throughput, or more RSS)."
  echo
  echo "## Raw runs"
  echo
  echo "Columns: elapsed_s user_s sys_s maxrss_kb exit. First line of each block is the discarded warm-up. Throughput benches also list the per-run score."
  echo
  for bench in $BENCHES; do
    for a in $ALLOCS; do
      echo "### $bench / $a"; echo; echo '```'
      cat "$OUT/$bench.$a.rt"
      if higher_is_better "$bench"; then echo "score ($(unit "$bench")): $(tr '\n' ' ' <"$OUT/$bench.$a.score")"; fi
      if [ "$bench" = redis ]; then echo "server:"; cat "$OUT/redis-server.$a.rt"; fi
      echo '```'; echo
    done
  done
} > "$RESULT"

case " $ALLOCS " in *" challoc "*)
  case " $BENCHES " in *" larson "*)
    echo "== larson profile under challoc"
    sh "$ROOT/bench/profile.sh" "$OUT" >> "$RESULT";;
  esac;;
esac
echo "== wrote $RESULT"
sed -n '/^## Medians/,/^Positive/p' "$RESULT"

#!/bin/sh
# scripts/cgroup-tests.sh: the memory-limited container tests, run INSIDE one memory-limited dev container; the Makefile's
# `cgroup_run` starts one container per run (`--memory=L --memory-swap=L`,
# swap off) so each run has its own cgroup, its own `memory.events` and
# nothing else charged to it.
#
#   sh scripts/cgroup-tests.sh churn     ALLOC MODE MIB THRESHOLD_MIB
#   sh scripts/cgroup-tests.sh oscillate ALLOC SECONDS [LOW_MIB HIGH_MIB [HOLD_MS]]
#   sh scripts/cgroup-tests.sh lazy      ALLOC MODE [MIB]
#
#   ALLOC   challoc       target/release/libchalloc.so, defaults (policy on)
#           challoc-off   the same with MALLOC_CONF=cgroup:false (the control)
#           challoc-eager the same with MALLOC_CONF=lazy_band:false (the two-advice
#                         purge's control: MADV_DONTNEED in every band)
#           jemalloc      Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)
#           mimalloc      target/bench-bin/libmimalloc.so (bench/build.sh)
#           tcmalloc      Wolfi's /usr/lib/libtcmalloc_minimal.so.4 (gperftools)
#           glibc         no preload
#   MODE    purge | keep | report (tests/c/cgroup_churn.c)
#           lazy | eager | report (tests/c/cgroup_lazy.c)
#
# Compiles the program (tests/c/cgroup_churn.c, cgroup_oscillate.c or
# cgroup_lazy.c) with scripts/ctest-cc.sh into target/cgroup-tests/q<QUANTUM>/
# (the programs use QUANTUM only in `require_challoc`, which --any-allocator
# skips, so a missing library is fine there; one directory per quantum, so
# a QUANTUM=8 run never leaves a binary a QUANTUM=16 run reuses), prints the kernel, the cgroup's limits and
# `memory.events` before and after, runs the program under the allocator
# with a watchdog (WATCHDOG seconds, default 360: a hung oscillation is
# killed and fails) and exits with the program's status: 0 is a pass, 137
# is SIGKILL, which in a limited container means the OOM killer (the
# `oom_kill` count after the run says so). The RESULT line printed by the
# program and the `OUTCOME` line printed here are the record of a run.
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT" || exit 2
kind=${1:-}; alloc=${2:-}
[ -n "$kind" ] && [ -n "$alloc" ] || { sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
shift 2

cgdir() {
  path=$(sed -n 's/^0::\(.*\)$/\1/p' /proc/self/cgroup)
  mount=$(awk '$0 ~ / - cgroup2 / { print $5; exit }' /proc/self/mountinfo)
  [ -n "$mount" ] || return 1
  [ "$path" = "/" ] && path=
  echo "$mount$path"
}
cg=$(cgdir) || cg=
cgread() { [ -n "$cg" ] && [ -r "$cg/$1" ] && cat "$cg/$1" || echo n/a; }
events() { [ -n "$cg" ] && [ -r "$cg/memory.events" ] && tr '\n' ' ' <"$cg/memory.events" || echo n/a; }
bytes_mib() { case $1 in n/a|max) echo "$1";; *) echo "$(( $1 / 1048576 )) MiB";; esac; }

case $kind in
  churn)     prog=cgroup_churn; [ $# -ge 3 ] || { echo "churn needs MODE MIB THRESHOLD_MIB" >&2; exit 2; } ;;
  oscillate) prog=cgroup_oscillate; [ $# -ge 1 ] || { echo "oscillate needs SECONDS" >&2; exit 2; } ;;
  lazy)      prog=cgroup_lazy; [ $# -ge 1 ] || { echo "lazy needs MODE [MIB]" >&2; exit 2; } ;;
  *) echo "unknown test $kind" >&2; exit 2 ;;
esac

preload=; conf=; extra=
case $alloc in
  challoc)     preload=$ROOT/target/release/libchalloc.so ;;
  challoc-off) preload=$ROOT/target/release/libchalloc.so; conf=cgroup:false,abort_conf:true ;;
  challoc-eager) preload=$ROOT/target/release/libchalloc.so; conf=lazy_band:false,abort_conf:true ;;
  jemalloc)    preload=/usr/lib/libjemalloc.so.2; extra=--any-allocator ;;
  mimalloc)    preload=$ROOT/target/bench-bin/libmimalloc.so; extra=--any-allocator ;;
  tcmalloc)    preload=/usr/lib/libtcmalloc_minimal.so.4; extra=--any-allocator ;;
  glibc)       extra=--any-allocator ;;
  *) echo "unknown allocator $alloc" >&2; exit 2 ;;
esac
if [ -n "$preload" ] && [ ! -f "$preload" ]; then
  echo "$preload missing (challoc: cargo build --release; mimalloc: make bench-build)" >&2; exit 2
fi

quantum=$(sh scripts/ctest-cc.sh --quantum) || exit 2
mkdir -p "target/cgroup-tests/q$quantum"
bin=target/cgroup-tests/q$quantum/$prog
if [ ! -x "$bin" ] || [ "tests/c/$prog.c" -nt "$bin" ] || [ tests/c/common.h -nt "$bin" ] || [ tests/c/cgroup.h -nt "$bin" ]; then
  sh scripts/ctest-cc.sh "tests/c/$prog.c" -o "$bin" || exit 2
fi

version=
case $alloc in
  challoc*) version="challoc $(git -c safe.directory='*' rev-parse --short HEAD 2>/dev/null || echo tree)" ;;
  jemalloc) version="jemalloc $(apk info jemalloc 2>/dev/null | sed -n 's/^jemalloc-\([^ ]*\) .*/\1/p' | head -1)" ;;
  mimalloc) version="mimalloc $(sed -n 's/^mimalloc v\{0,1\}\([0-9][^ ]*\).*/\1/p' target/bench-bin/versions.txt 2>/dev/null | head -1)" ;;
  tcmalloc) version="tcmalloc-minimal $(apk info tcmalloc-minimal 2>/dev/null | sed -n 's/^tcmalloc-minimal-\([^ ]*\) .*/\1/p' | head -1)" ;;
  glibc)    version="glibc $(ldd --version 2>&1 | head -1 | sed 's/.* //')" ;;
esac
echo "== $kind under $alloc ($version): $prog $* $extra"
echo "kernel $(uname -r) $(uname -m); cgroup ${cg:-(none)}: memory.max $(bytes_mib "$(cgread memory.max)"), memory.swap.max $(bytes_mib "$(cgread memory.swap.max)"), memory.current $(bytes_mib "$(cgread memory.current)")"
[ -n "$conf" ] && echo "MALLOC_CONF=$conf"
ev0=$(events); echo "memory.events before: $ev0"

WATCHDOG=${WATCHDOG:-360}
if [ -n "$preload" ]; then export LD_PRELOAD="$preload"; fi
if [ -n "$conf" ]; then export MALLOC_CONF=$conf; fi
timeout -s KILL "$WATCHDOG" "$bin" "$@" $extra
status=$?
unset LD_PRELOAD MALLOC_CONF

ev1=$(events); echo "memory.events after: $ev1"
echo "memory.peak $(bytes_mib "$(cgread memory.peak)"), memory.current $(bytes_mib "$(cgread memory.current)")"
oomk=$(echo "$ev1" | sed -n 's/.*oom_kill \([0-9]*\).*/\1/p')
verdict=ok
if [ "$status" -eq 137 ] && [ "${oomk:-0}" -gt 0 ]; then verdict=OOM-killed
elif [ "$status" -eq 137 ]; then verdict="killed (watchdog $WATCHDOG s or the OOM killer without an oom_kill count)"
elif [ "$status" -ne 0 ]; then verdict=FAIL; fi
echo "OUTCOME $kind alloc=$alloc args='$*' status=$status verdict=$verdict oom_kill=${oomk:-n/a}"
exit "$status"

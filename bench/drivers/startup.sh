#!/bin/sh
# bench/drivers/startup.sh: the startup rows (exec-to-exit of a small
# program under the allocator). STARTUP picks the program, N the number of
# runs; the score is the wall time per run of N sequential fork+exec+exit
# (bench/tools/execloop.c), and one more run under `strace -f -c` gives the
# system calls of a single start (every process of it: gcc's cc1 and as
# too), all of them and the memory-management ones (mmap, munmap,
# mprotect, madvise, brk, mremap), which is where an allocator's
# initialisation shows.
#
#   STARTUP   true | cc | py | ruby | node | java
#   N         runs in the timed loop (the row's env)
#
# Metrics: per_run_ms (the score), syscalls, mm_syscalls, maxrss_mib (the
# largest process of one run).
. "$(dirname "$0")/cb.sh"
STARTUP=${STARTUP:?STARTUP=true|cc|py|ruby|node|java}
N=${N:-200}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
case $STARTUP in
  true) set -- /bin/true;;
  cc)   set -- gcc -O2 -c "$ROOT/bench/probes/startup/hello200.c" -o "$CB_OUT/hello.o";;
  py)   set -- python3 -c pass;;
  ruby) set -- ruby -e 1;;
  node) set -- node -e 0;;
  java) set -- /usr/lib/jvm/java-21-openjdk/bin/java -Xshare:auto -version;;
  *) die "unknown STARTUP=$STARTUP";;
esac
command -v "$1" >/dev/null 2>&1 || die "$1 is not installed in this image"

# One untimed run first: page cache, and a failure here is a clear error.
# shellcheck disable=SC2046
env $(alloc_env) "$@" > "$CB_OUT/first.out" 2>&1 || { cat "$CB_OUT/first.out" >&2; die "$* failed under $CB_ALLOC"; }

timed loop "$CB_TOOLS/execloop" "$N" "$@"
per_run=$(sed -n 's/.*per_run_ms=\([0-9.]*\).*/\1/p' "$CB_OUT/loop.out")
[ -n "$per_run" ] || { cat "$CB_OUT/loop.out" >&2; die "no execloop line"; }
metric per_run_ms "$per_run"
metric maxrss_mib "$(mib "$MAXRSS_KIB")"

if command -v strace >/dev/null 2>&1; then
  # shellcheck disable=SC2046
  if strace -f -c -U calls,name -o "$CB_OUT/strace.txt" env $(alloc_env) "$@" > /dev/null 2>&1; then
    # env's own execve and its few calls are in the count for every
    # variant alike.
    metric syscalls "$(awk '$2 == "total" { print $1 }' "$CB_OUT/strace.txt")"
    metric mm_syscalls "$(awk '$2 ~ /^(mmap|mmap2|munmap|mprotect|madvise|brk|mremap)$/ { n += $1 } END { print n + 0 }' "$CB_OUT/strace.txt")"
  else
    log "strace failed (ptrace refused in this container?); no syscall counts"
  fi
fi
emit

# shellcheck shell=sh
# bench/drivers/cb.sh: what the small cb drivers share (startup.sh,
# compile.sh, rocksdb.sh, nginx.sh, sidecar.sh, node-server.sh,
# postgres.sh). Sourced, POSIX sh. The contract is bench/README.md
# "Drivers": cb passes CB_ALLOC, CB_LIB (the variant's library, "" for
# glibc), CB_CONF, CB_OUT (a scratch directory), CB_TOOLS (rt, execloop),
# CB_BIN (the bench programs) and RESULT_JSONL; a driver runs one
# allocator and appends one {"metrics": {...}} line.

set -eu
: "${CB_OUT:?run under cb (bench/README.md, Drivers)}" "${RESULT_JSONL:?}" "${CB_TOOLS:?}" "${CB_BIN:?}"
CB_ALLOC=${CB_ALLOC:-glibc}
CB_LIB=${CB_LIB:-}
CB_CONF=${CB_CONF:-}
DRIVER=$(basename "$0" .sh)
METRICS=

die() { echo "$DRIVER: $*" >&2; exit 1; }
log() { echo "$DRIVER: $*"; }

# The allocator's environment for `env`: LD_PRELOAD and MALLOC_CONF, or
# nothing (glibc).
alloc_env() { printf '%s' "${CB_LIB:+LD_PRELOAD=$CB_LIB }${CB_CONF:+MALLOC_CONF=$CB_CONF}"; }

# metric NAME VALUE: add one figure (skipped when VALUE is empty or "-").
metric() {
  case ${2:-} in ''|-) return 0;; esac
  METRICS="${METRICS:+$METRICS, }\"$1\": $2"
}
# emit: the one JSON line cb reads.
emit() { printf '{"schema": "challoc.bench.driver/v1", "driver": "%s", "alloc": "%s", "metrics": {%s}}\n' "$DRIVER" "$CB_ALLOC" "$METRICS" >> "$RESULT_JSONL"; }

# timed TAG CMD...: run CMD under rt with the allocator in its environment
# (its own RT_OUT, not cb's); sets WALL_S and MAXRSS_KIB. Output to
# $CB_OUT/TAG.out. Fails the driver when CMD fails.
timed() {
  tag=$1; shift
  rt_file=$CB_OUT/$tag.rt
  rm -f "$rt_file"
  # shellcheck disable=SC2046 # alloc_env prints VAR=value words for env
  RT_OUT=$rt_file RT_JSON=1 "$CB_TOOLS/rt" env $(alloc_env) "$@" > "$CB_OUT/$tag.out" 2>&1 \
    || { tail -20 "$CB_OUT/$tag.out" >&2; die "$tag: $* failed"; }
  WALL_S=$(sed -n 's/.*"wall_s":\([0-9.]*\).*/\1/p' "$rt_file")
  # shellcheck disable=SC2034 # read by the sourcing driver
  MAXRSS_KIB=$(sed -n 's/.*"maxrss_kib":\([0-9]*\).*/\1/p' "$rt_file")
  [ -n "$WALL_S" ] || die "$tag: no rt record"
}

# mib KIB: KiB to MiB with three decimals.
mib() { awk -v k="$1" 'BEGIN { printf "%.3f", k / 1024 }'; }
# rss_kib PID / hwm_kib PID: VmRSS / VmHWM of a live process.
rss_kib() { awk '/^VmRSS:/ { print $2 }' "/proc/$1/status" 2>/dev/null || echo 0; }
hwm_kib() { awk '/^VmHWM:/ { print $2 }' "/proc/$1/status" 2>/dev/null || echo 0; }
# cpu_ticks PID...: utime + stime of the processes, in clock ticks.
cpu_ticks() {
  for p in "$@"; do cat "/proc/$p/stat" 2>/dev/null; done | awk '{ sub(/^.*\) /, ""); t += $12 + $13 } END { print t + 0 }'
}
# shellcheck disable=SC2034 # read by the sourcing driver
CLK_TCK=$(getconf CLK_TCK 2>/dev/null || echo 100)

# maps_check PID WHAT: the process must map the variant's library (by real
# path) and no other allocator library; glibc maps none.
maps_check() {
  want=
  [ -n "$CB_LIB" ] && want=$(readlink -f "$CB_LIB")
  got=$(grep -oE '/[^ ]*/lib(challoc|jemalloc|mimalloc|tcmalloc)[^/ ]*\.so[.0-9]*' "/proc/$1/maps" 2>/dev/null | sort -u | while read -r f; do readlink -f "$f"; done | sort -u | tr '\n' ' ' | sed 's/ $//')
  [ "$got" = "$want" ] || die "$2 (pid $1) maps '${got:-nothing}', expected '${want:-no allocator library}'"
}

# A free TCP port on 127.0.0.1 from a base: the base plus the driver's pid
# modulo 1000, so two drivers on one host do not collide.
port_from() { echo $(( $1 + $$ % 1000 )); }
# wait_port PORT [TRIES]: until something accepts on 127.0.0.1:PORT.
wait_port() {
  i=0
  until python3 -c "import socket,sys; socket.create_connection(('127.0.0.1', int(sys.argv[1])), 1).close()" "$1" 2>/dev/null; do
    i=$((i + 1)); [ $i -gt "${2:-100}" ] && return 1; sleep 0.1
  done
}

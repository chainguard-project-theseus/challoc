#!/bin/sh
# bench/drivers/kv-ops.sh: the paired Redis / Valkey command-mix rows
# (redis-ops*, valkey-ops*; bench/rows.toml). One run is one fresh server
# from bench/drivers/kv.sh under the variant's allocator (`jemalloc`: the
# bundled jemalloc 5.3.0 build; challoc: the server's runpath build with
# its challoc integration (contrib/valkey, contrib/redis), or preloaded on
# the libc build for a variant labelled `*preload*`; every other allocator
# preloaded on the libc build), a fill,
# then one <server>-benchmark pass over the command mix, then the server's
# memory figures. The client is the bundled-jemalloc build's benchmark,
# never the library under test. Active defrag is off (both servers'
# default), so every build runs the same server code.
#
#   SERVER=redis|valkey   which server (kv.sh pins the tags)
#   SIZE=64               -d, the value size of SET, the list, set, hash and zset members
#   KEYS=1000000          -r, the keyspace (the fill writes KEYS keys first)
#   REQS=1000000          -n per test; LRANGE_100 runs REQS/20 and MSET REQS/5
#                         (each request moves 100 values / sets 10 keys: at the
#                         full count they would be most of the run's time)
#   CLIENTS=50 PIPELINE=16 THREADS=1   the client's -c, -P and --threads (more
#                         than one: the client times each test in 250 ms steps)
#   IO_THREADS=           the server's --io-threads (empty: its default, 1)
#   TESTS=set,get,incr,lpush,rpush,lpop,rpop,sadd,hset,spop,zadd,zpopmin
#                         the single-element commands; LRANGE_100 and MSET follow
#
# Metrics: ops_gmean (the score: the geometric mean of the tests' req/s),
# <test>_rps and <test>_p99_ms per test, vmhwm_mib (the server's VmHWM),
# rss_end_mib (VmRSS after the mix), used_mib (INFO used_memory),
# frag_ratio (INFO mem_fragmentation_ratio, RSS over used_memory),
# fill_rps and keys.
. "$(dirname "$0")/cb.sh"
ROOT=${CB_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
SERVER=${SERVER:?SERVER=redis|valkey}
KV_SERVER=$SERVER
. "$ROOT/bench/drivers/kv.sh"
SIZE=${SIZE:-64}
KEYS=${KEYS:-1000000}
REQS=${REQS:-1000000}
CLIENTS=${CLIENTS:-50}
PIPELINE=${PIPELINE:-16}
THREADS=${THREADS:-1}
IO_THREADS=${IO_THREADS:-}
TESTS=${TESTS:-set,get,incr,lpush,rpush,lpop,rpop,sadd,hset,spop,zadd,zpopmin}
LINK=native
case ${CB_VARIANT:-} in *preload*) LINK=preload;; esac

# The server build for this allocator (built on first use, before the
# server starts, so a build never overlaps a measurement).
kv_ensure_jemalloc
kv_resolve "$CB_ALLOC" "$LINK"
PORT=$(port_from 16400)
D=$CB_OUT/kv
mkdir -p "$D"
cli() { "$KV_CLI" -p "$PORT" "$@"; }
info_field() { cli info "$1" | tr -d '\r' | awk -F: -v k="$2" '$1 == k { print $2 }'; }
SRV_PID=
cleanup() { [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null; :; }
trap cleanup EXIT

conf_env=${KV_PRELOAD:+LD_PRELOAD=$KV_PRELOAD}
# The variant's conf reaches whichever allocator it names (kv_conf_env).
conf_word=$(kv_conf_env "$CB_ALLOC" "${CB_CONF:-}") || exit 1
conf_env="$conf_env${conf_word:+ $conf_word}"
log "$CB_ALLOC ($KV_BUILD build): $KV_SRV${KV_PRELOAD:+ LD_PRELOAD=$KV_PRELOAD}"
# shellcheck disable=SC2086 # conf_env is VAR=value words; IO_THREADS empty means no flag
env $conf_env "$KV_SRV" --port "$PORT" --bind 127.0.0.1 --save "" --appendonly no --daemonize no \
  --logfile "$D/server.log" --activedefrag no ${IO_THREADS:+--io-threads $IO_THREADS} >/dev/null 2>&1 &
SRV_PID=$!
i=0
until [ "$(cli ping 2>/dev/null)" = PONG ]; do
  i=$((i + 1)); [ $i -gt 200 ] && { tail -20 "$D/server.log" >&2; die "$KV_SRV did not start"; }
  sleep 0.1
done
got=$(info_field memory mem_allocator)
[ "$got" = "$KV_EXPECT" ] || die "$CB_ALLOC server reports mem_allocator=$got, expected $KV_EXPECT"
kv_check_binding "$SRV_PID" "$CB_ALLOC"
"$KV_SRV" --version > "$D/version.txt"

bench() { # OUTFILE ARGS...: the benchmark client, --csv, quiet
  bf=$1; shift
  "$KV_BENCH" -p "$PORT" -c "$CLIENTS" -P "$PIPELINE" --threads "$THREADS" -r "$KEYS" -d "$SIZE" --csv "$@" > "$bf" 2> "$bf.err" \
    || { tail -5 "$bf.err" >&2; die "$(basename "$KV_BENCH") $* failed"; }
}
# The fill: KEYS SETs over the keyspace, so the mix runs on a populated heap.
bench "$D/fill.csv" -t set -n "$KEYS"
bench "$D/mix1.csv" -t "$TESTS" -n "$REQS"
bench "$D/mix2.csv" -t lrange_100 -n $((REQS / 20))
bench "$D/mix3.csv" -t mset -n $((REQS / 5))
# One table: mix1's header, then every data line.
{ cat "$D/mix1.csv"; tail -n +2 "$D/mix2.csv"; tail -n +2 "$D/mix3.csv"; } > "$D/mix.csv"

# "test","rps","avg_latency_ms","min_latency_ms","p50_latency_ms","p95_latency_ms","p99_latency_ms","max_latency_ms"
metric fill_rps "$(awk -F'","' 'NR > 1 { gsub(/"/, "", $2); printf "%.0f", $2; exit }' "$D/fill.csv")"
# One "test rps p99" line per command (the first line of a command wins),
# read from a file: a pipe into the loop would run metric in a subshell.
awk -F'","' 'NR > 1 {
    t = $1; gsub(/"/, "", t); t = tolower(t); sub(/ .*/, "", t)
    if (t in seen) next; seen[t] = 1
    r = $2; p = $7; gsub(/"/, "", r); gsub(/"/, "", p)
    printf "%s %.0f %s\n", t, r, p
  }' "$D/mix.csv" > "$D/mix.tests"
n=0
while read -r t rps p99; do
  metric "${t}_rps" "$rps"
  metric "${t}_p99_ms" "$p99"
  n=$((n + 1))
done < "$D/mix.tests"
[ $n -gt 0 ] || die "no test lines in $D/mix.csv"
# LRANGE_100's "LPUSH (needed to benchmark LRANGE)" line is lpush again and
# is skipped above (first line per command wins); lrange_100 reads "lrange_100".
metric ops_gmean "$(awk -F'","' 'NR > 1 {
    t = $1; gsub(/"/, "", t); if (t ~ /needed to benchmark/) next
    r = $2; gsub(/"/, "", r); if (r > 0) { s += log(r); c++ } }
  END { if (c) printf "%.0f", exp(s / c) }' "$D/mix.csv")"

metric vmhwm_mib "$(mib "$(hwm_kib "$SRV_PID")")"
metric rss_end_mib "$(mib "$(rss_kib "$SRV_PID")")"
metric used_mib "$(awk -v b="$(info_field memory used_memory)" 'BEGIN { printf "%.3f", b / 1048576 }')"
metric frag_ratio "$(info_field memory mem_fragmentation_ratio)"
metric keys "$(cli dbsize | tr -d '\r')"
cli info memory | tr -d '\r' > "$D/info-memory.txt"
cli shutdown nosave >/dev/null 2>&1 || true
wait "$SRV_PID" 2>/dev/null || true; SRV_PID=
emit

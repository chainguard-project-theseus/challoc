#!/bin/sh
# bench/drivers/node-server.sh: the Node.js http server row
# (bench/node/server.js under `node --max-old-space-size=256`, so the V8
# heap is bounded and the native allocations are what differ). Two `ab -k`
# processes (on glibc malloc) at once for SECS seconds, CONNS/2 keep-alive
# connections each, one on /json (a small JSON body built per request) and
# one on /blob (a 1 MiB Buffer.allocUnsafe per request: one malloc(1 MiB)
# backing store per request, freed when the GC collects it), after a short
# warm-up. The server's VmRSS is sampled every second during the load.
#
# Metrics: json_rps (the score), blob_rps, rss_peak_mib (the largest
# sample), rss_end_mib (the last), hwm_mib (VmHWM).
. "$(dirname "$0")/cb.sh"
SECS=${SECS:-30}
CONNS=${CONNS:-64}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
command -v node >/dev/null || die "no node in this image"
command -v ab >/dev/null || die "no ab (apache2-utils) in this image"
PORT=$(port_from 17000)
PID=
cleanup() { [ -z "$PID" ] || kill "$PID" 2>/dev/null || true; wait 2>/dev/null || true; }
trap cleanup EXIT
# shellcheck disable=SC2046
env $(alloc_env) node --max-old-space-size=256 "$ROOT/bench/node/server.js" "$PORT" "$CB_OUT/server.pid" > "$CB_OUT/server.out" 2>&1 &
wait_port "$PORT" 200 || { cat "$CB_OUT/server.out" >&2; die "server did not start"; }
PID=$(cat "$CB_OUT/server.pid")
maps_check "$PID" node
h=$((CONNS / 2))
ab -q -k -l -c "$h" -n 2000 "http://127.0.0.1:$PORT/json" > "$CB_OUT/warm.json" 2>&1 || die "ab warm-up /json failed"
ab -q -k -l -c "$h" -n 200 "http://127.0.0.1:$PORT/blob" > "$CB_OUT/warm.blob" 2>&1 || die "ab warm-up /blob failed"
# -t before -n: ab -t resets -n to 50000.
ab -q -k -l -c "$h" -t "$SECS" -n 100000000 "http://127.0.0.1:$PORT/json" > "$CB_OUT/json.ab" 2>&1 &
a1=$!
ab -q -k -l -c "$h" -t "$SECS" -n 100000000 "http://127.0.0.1:$PORT/blob" > "$CB_OUT/blob.ab" 2>&1 &
a2=$!
peak=0; last=0
while kill -0 "$a1" 2>/dev/null || kill -0 "$a2" 2>/dev/null; do
  sleep 1
  last=$(rss_kib "$PID"); [ "$last" -gt "$peak" ] && peak=$last
done
wait "$a1" || die "ab /json failed: $(tail -3 "$CB_OUT/json.ab")"
wait "$a2" || die "ab /blob failed: $(tail -3 "$CB_OUT/blob.ab")"
rps() { awk '/^Requests per second:/ { print $4 }' "$1"; }
failed() { awk '/^Failed requests:/ { print $3 }' "$1"; }
[ "$(failed "$CB_OUT/json.ab")" = 0 ] && [ "$(failed "$CB_OUT/blob.ab")" = 0 ] || die "ab counted failed requests"
metric json_rps "$(rps "$CB_OUT/json.ab")"
metric blob_rps "$(rps "$CB_OUT/blob.ab")"
metric rss_peak_mib "$(mib "$peak")"
metric rss_end_mib "$(mib "$last")"
metric hwm_mib "$(mib "$(hwm_kib "$PID")")"
emit

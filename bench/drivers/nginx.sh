#!/bin/sh
# bench/drivers/nginx.sh: the proxy class. Two nginx servers under the
# allocator: a front with WORKERS workers that gzips and proxies to a
# backend (keep-alive upstream) that serves a 32 KiB text file, driven by
# wrk2 at a fixed RATE for SECS seconds over CONNS connections (wrk2, the
# load generator, runs on glibc malloc). Every request allocates the
# front's request pool, the proxy buffers and a zlib deflate state, and the
# backend's request pool, so at a fixed rate the allocator shows up as CPU
# per request and as latency.
#
# Metrics: cpu_us_req (the score: CPU time of every nginx process per
# request, microseconds, lower is better), p99_ms and p999_ms (wrk2's
# corrected latency), rps (achieved), rss_mib (VmRSS summed over the nginx
# processes at the end of the load).
. "$(dirname "$0")/cb.sh"
WORKERS=${WORKERS:-4}
RATE=${RATE:-12000}
SECS=${SECS:-20}
CONNS=${CONNS:-64}
THREADS=${THREADS:-4}
WRK=$CB_BIN/wrk2
[ -x "$WRK" ] || die "no $WRK (bench/build.sh apps)"
command -v nginx >/dev/null || die "no nginx in this image"
FRONT=$(port_from 18000)
BACK=$((FRONT + 1000))
W=$CB_OUT/nginx
rm -rf "$W"; mkdir -p "$W/front" "$W/back" "$W/www"
# A compressible text body (gzip does real work on it).
python3 -c '
import random, sys
r = random.Random(1)
words = ["alloc", "free", "slab", "arena", "cache", "page", "thread", "heap", "chunk", "bin"]
out = []
while sum(len(w) + 1 for w in out) < 32768:
    out.append(r.choice(words))
sys.stdout.write(" ".join(out))' > "$W/www/body.txt"

conf() { # conf DIR PORT WORKERS BODY
  cat <<EOF
daemon off;
master_process on;
worker_processes $3;
user root;
pid $1/nginx.pid;
error_log $1/error.log warn;
events { worker_connections 4096; }
http {
  access_log off;
  client_body_temp_path $1/body; proxy_temp_path $1/proxy;
  fastcgi_temp_path $1/fcgi; uwsgi_temp_path $1/uwsgi; scgi_temp_path $1/scgi;
  keepalive_requests 100000;
  upstream back { server 127.0.0.1:$BACK; keepalive 64; }
  server {
    listen 127.0.0.1:$2 reuseport;
    $4
  }
}
EOF
}
conf "$W/back" "$BACK" 2 "location / { root $W/www; default_type text/plain; }" > "$W/back.conf"
conf "$W/front" "$FRONT" "$WORKERS" "location / { proxy_pass http://back; proxy_http_version 1.1; proxy_set_header Connection \"\"; gzip on; gzip_proxied any; gzip_comp_level 1; gzip_types text/plain; }" > "$W/front.conf"

PIDS=
cleanup() { for p in $PIDS; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; rm -rf "$W/www"; }
trap cleanup EXIT
# shellcheck disable=SC2046
env $(alloc_env) nginx -p "$W/back" -c "$W/back.conf" > "$W/back.out" 2>&1 &
PIDS="$PIDS $!"
# shellcheck disable=SC2046
env $(alloc_env) nginx -p "$W/front" -c "$W/front.conf" > "$W/front.out" 2>&1 &
PIDS="$PIDS $!"
wait_port "$BACK" || { cat "$W/back.out" "$W/back/error.log" >&2; die "backend nginx did not start"; }
wait_port "$FRONT" || { cat "$W/front.out" "$W/front/error.log" >&2; die "front nginx did not start"; }
url=http://127.0.0.1:$FRONT/body.txt
curl -sf -H 'Accept-Encoding: gzip' -o /dev/null "$url" || die "front nginx does not serve $url"

nginx_pids() { pgrep -x nginx | tr '\n' ' '; }
for p in $(nginx_pids); do maps_check "$p" nginx; done
# Warm-up (discarded), then the measured window.
"$WRK" -t "$THREADS" -c "$CONNS" -d 5s -R "$RATE" -H 'Accept-Encoding: gzip' "$url" > "$CB_OUT/warm.out" 2>&1 || true
pids=$(nginx_pids)
t0=$(cpu_ticks $pids)
"$WRK" -t "$THREADS" -c "$CONNS" -d "${SECS}s" -R "$RATE" --latency -H 'Accept-Encoding: gzip' "$url" > "$CB_OUT/wrk.out" 2>&1 \
  || { tail -20 "$CB_OUT/wrk.out" >&2; die "wrk2 failed"; }
t1=$(cpu_ticks $pids)
rss=0
for p in $pids; do rss=$((rss + $(rss_kib "$p"))); done

reqs=$(awk '/requests in/ { print $1 }' "$CB_OUT/wrk.out")
rps=$(awk '/^Requests\/sec:/ { print $2 }' "$CB_OUT/wrk.out")
# "99.000%    1.23ms" (or us / s) from the --latency distribution.
lat() { awk -v q="$1" '$1 == q { v = $2; u = v; sub(/[0-9.]+/, "", u); sub(/[a-z]+$/, "", v); m = (u == "us") ? v / 1000 : (u == "s") ? v * 1000 : (u == "m") ? v * 60000 : v; printf "%.3f", m; exit }' "$CB_OUT/wrk.out"; }
errs=$(awk '/Non-2xx|Socket errors/ { print }' "$CB_OUT/wrk.out")
[ -z "$errs" ] || die "wrk2 reported errors: $errs"
[ -n "$reqs" ] && [ "$reqs" -gt 0 ] || die "no requests counted"
metric cpu_us_req "$(awk -v t="$((t1 - t0))" -v hz="$CLK_TCK" -v n="$reqs" 'BEGIN { printf "%.3f", t / hz * 1e6 / n }')"
metric p99_ms "$(lat 99.000%)"
metric p999_ms "$(lat 99.900%)"
metric rps "$rps"
metric rss_mib "$(mib "$rss")"
emit

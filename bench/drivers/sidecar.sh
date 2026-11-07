#!/bin/sh
# bench/drivers/sidecar.sh: the idle-sidecar class. fluent-bit (Wolfi's,
# linked against libjemalloc.so.2: the row needs a challoc build with the
# jemalloc soname, so the preloaded library also satisfies that DT_NEEDED)
# takes an HTTP input, adds a field to every record and drops them in the
# null output. A burst of BATCHES posts of BATCH records each (mixed 64 B
# to 8 KiB messages, sent by a Python client on glibc malloc) fills its
# buffers; then it idles for IDLE seconds and VmRSS is sampled every
# second. What a sidecar costs a node is the resident memory after the
# burst has passed, which is where a returning allocator and a retaining
# one differ.
#
# Metrics: rss_settle_mib (the score: VmRSS at the end of the idle
# window), rss_peak_mib (VmHWM), rss_idle5_mib (5 s into the idle window),
# burst_s (the burst's wall time). REBURST=1 posts the burst a second time
# after the idle window, into memory the idle purged: burst2_s is its wall
# time (the refault cost) and rss_reburst_mib the VmRSS after it.
. "$(dirname "$0")/cb.sh"
BATCHES=${BATCHES:-400}
BATCH=${BATCH:-500}
IDLE=${IDLE:-60}
REBURST=${REBURST:-0}
command -v fluent-bit >/dev/null || die "no fluent-bit in this image"
PORT=$(port_from 19000)
W=$CB_OUT/fb
rm -rf "$W"; mkdir -p "$W"
PID=
cleanup() { [ -z "$PID" ] || kill "$PID" 2>/dev/null || true; wait 2>/dev/null || true; }
trap cleanup EXIT
# shellcheck disable=SC2046
env $(alloc_env) fluent-bit -q -f 1 -i http -p listen=127.0.0.1 -p port="$PORT" -p buffer_max_size=16M \
  -F modify -m '*' -p 'Add=sidecar challoc-bench' -o null -m '*' > "$W/fb.out" 2>&1 &
PID=$!
wait_port "$PORT" 200 || { cat "$W/fb.out" >&2; die "fluent-bit did not start"; }
maps_check "$PID" fluent-bit
rss0=$(rss_kib "$PID")
t0=$(date +%s.%N 2>/dev/null || date +%s)
cat > "$W/burst.py" <<'EOF'
import http.client, json, random, sys
port, batches, batch = int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3])
r = random.Random(1)
conn = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
for b in range(batches):
    recs = [{"seq": b * batch + i, "level": r.choice(["info", "warn", "error"]),
             "msg": "x" * int(64 * 2 ** (r.random() * 7))} for i in range(batch)]
    body = json.dumps(recs)
    conn.request("POST", "/app.log", body, {"Content-Type": "application/json"})
    resp = conn.getresponse()
    resp.read()
    if resp.status >= 300:
        sys.exit(f"post {b}: HTTP {resp.status}")
EOF
python3 "$W/burst.py" "$PORT" "$BATCHES" "$BATCH" || die "burst failed"
t1=$(date +%s.%N 2>/dev/null || date +%s)
i=0; rss5=
while [ $i -lt "$IDLE" ]; do
  sleep 1; i=$((i + 1))
  kill -0 "$PID" 2>/dev/null || { cat "$W/fb.out" >&2; die "fluent-bit exited during the idle window"; }
  [ $i -eq 5 ] && rss5=$(rss_kib "$PID")
done
metric rss_settle_mib "$(mib "$(rss_kib "$PID")")"
metric rss_peak_mib "$(mib "$(hwm_kib "$PID")")"
metric rss_idle5_mib "$(mib "${rss5:-0}")"
metric rss_start_mib "$(mib "$rss0")"
metric burst_s "$(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.3f", b - a }')"
if [ "$REBURST" = 1 ]; then
  t2=$(date +%s.%N 2>/dev/null || date +%s)
  python3 "$W/burst.py" "$PORT" "$BATCHES" "$BATCH" || die "second burst failed"
  t3=$(date +%s.%N 2>/dev/null || date +%s)
  metric burst2_s "$(awk -v a="$t2" -v b="$t3" 'BEGIN { printf "%.3f", b - a }')"
  metric rss_reburst_mib "$(mib "$(rss_kib "$PID")")"
fi
emit

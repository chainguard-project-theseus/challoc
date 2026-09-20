#!/bin/sh
# Node.js benchmark rows (docs/plan.md Phase 3 task 5 (c)): the image's node
# (Wolfi nodejs-22) measured under five allocators via LD_PRELOAD, the same
# binary every time. Runs inside the dev container: `make node-bench`, or on
# a VM `bench/gce/challoc-bench make arm64 node-bench HOST_KIND=...`.
#
# Rows (ROWS):
#   startup     `node -e 0`: wall time and max RSS from bench/rt, one process
#               per round (the V8 snapshot deserialisation plus the module
#               loader's allocations; nothing of the user's).
#   json_churn  bench/node/json_churn.js: JSON.parse(JSON.stringify(obj)) over
#               a 177 KiB object (180906 bytes) JSON_ITERS (2000) times in one process; wall
#               time and max RSS from bench/rt (the Python rows' shape).
#   server      bench/node/server.js under `node --max-old-space-size=256`
#               (the V8 heap bounded so native memory is what differs),
#               serving GET /json (a small JSON body built per request) and
#               GET /blob (a 1 MiB body from a per-request
#               Buffer.allocUnsafe(1 << 20), filled and written in 64 KiB
#               chunks, so 1 MiB ArrayBuffer backing stores churn through
#               malloc at the request rate) for DURATION (300) s under ab
#               (apache2-utils; Wolfi has no wrk or oha, checked 2026-09-20):
#               two ab processes at once, CONNS/2 keep-alive connections on
#               each endpoint, `-t DURATION -n <large>` (in that order: ab's
#               -t resets -n to 50000, so -n must follow it). Scored by each
#               endpoint's req/s and p99 (ms) from ab, and the server's RSS:
#               /proc/PID/smaps_rollup Rss sampled every SAMPLE (5) s during
#               the load (peak and final sample) and VmHWM from
#               /proc/PID/status at the end. Once per allocator, after the
#               rounds (5 x 5 min); a short warm-up (WARM_N requests per
#               endpoint) precedes the timed load. For every allocator the
#               end-of-run /proc/PID/smaps is summed by mapping name, so a
#               challoc RSS gap can be placed in classes, huge or meta.
#
# Allocators (ALLOCS): challoc (REV, default the working tree, built into
# target/node/lib), jemalloc (Wolfi's /usr/lib/libjemalloc.so.2), glibc (no
# preload), mimalloc (bench/build.sh's libmimalloc.so), tcmalloc (Wolfi's
# tcmalloc-minimal). Every allocator's /proc/self/maps is checked once
# before the rounds: exactly the expected library is mapped.
#
#   ROUNDS=5     timed rounds after one discarded warm-up for startup and
#                json_churn, allocators interleaved within a round, every
#                score listed sorted
#   DURATION=300 seconds of load per allocator for the server row
#   CONNS=64     keep-alive connections in total (half per endpoint)
#   RESULT=path  markdown output (default bench/results/<date>-<arch>-node.md)
#   OUT=dir      raw logs, score files, RSS samples (default target/node/bench-out)
#   REV=tree     challoc rev (any git rev; "tree" = the working tree)
#   HOST_CPU, HOST_KIND   recorded in the header (the Makefile passes HOST_CPU)
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
. "$ROOT/bench/lib.sh"
ALLOCS=${ALLOCS:-"challoc jemalloc glibc mimalloc tcmalloc"}
ROUNDS=${ROUNDS:-5}
ROWS=${ROWS:-"startup json_churn server"}
REV=${REV:-tree}
DURATION=${DURATION:-300}
CONNS=${CONNS:-64}
PORT=${PORT:-18080}
JSON_ITERS=${JSON_ITERS:-2000}
SAMPLE=${SAMPLE:-5}
WARM_N=${WARM_N:-2000}
AB_N=${AB_N:-20000000}   # ab's request cap after -t (sizes its per-request table: 32 B each, lazily touched)
OLD_SPACE=${OLD_SPACE:-256}
NODE=${NODE:-node}
AB=${AB:-ab}
WORK=$ROOT/target/node
OUT=${OUT:-$WORK/bench-out}
RESULT=${RESULT:-$ROOT/bench/results/$DATE-$ARCH-node.md}
SCRIPTS=$ROOT/bench/node
T_START=$(date +%s)
LOAD0=$(loadavg)

take_bench_lock
ensure_bench_programs
command -v "$NODE" >/dev/null 2>&1 || die "no $NODE in this container (rebuild the image from Dockerfile.dev: nodejs-22)"
for a in $ALLOCS; do case $a in challoc) ;; *) is_stock "$a" || die "unknown allocator '$a'";; esac; done
for row in $ROWS; do case $row in startup|json_churn|server) ;; *) die "unknown row '$row' (startup|json_churn|server)";; esac; done
has_row() { case " $ROWS " in *" $1 "*) return 0;; *) return 1;; esac; }
has_row server && { command -v "$AB" >/dev/null 2>&1 || die "no $AB (apache2-utils) in this container"; }
rm -rf "$OUT"; mkdir -p "$OUT" "$(dirname "$RESULT")"

# ---- the challoc library --------------------------------------------------------
CHALLOC_LIB=
case " $ALLOCS " in *" challoc "*)
  rev=$(resolve_rev "$REV")
  printf '== building challoc (%s) into target/node/lib ... ' "$(rev_label "$rev")"
  build_lib "$rev" "$WORK/lib"; CHALLOC_LIB=$WORK/lib/release/libchalloc.so
  echo "done, $(so_hash "$CHALLOC_LIB")";;
esac
lib_of() { default_lib_of "$1"; }

# ---- binding check ----------------------------------------------------------------
NODE_VERSION=$("$NODE" --version 2>&1)
NODE_PKG=$(apk list --installed 2>/dev/null | awk '/^nodejs-[0-9]+-[0-9]/ {print $1}' | head -1)
AB_VERSION=$("$AB" -V 2>&1 | head -1 || true)
echo "== node: $NODE_VERSION (Wolfi ${NODE_PKG:-nodejs}), V8 $("$NODE" -p 'process.versions.v8'); load generator: ${AB_VERSION:-none}"
for a in $ALLOCS; do
  lib=$(lib_of "$a")
  mapped=$(env ${lib:+LD_PRELOAD=$lib} "$NODE" -e 'const fs=require("fs"); const s=new Set(); for (const l of fs.readFileSync("/proc/self/maps","utf8").split("\n")) { const m=l.match(/\S+$/); if (m && /\/lib(challoc|jemalloc|mimalloc|tcmalloc)[^\/ ]*\.so/.test(m[0])) s.add(m[0]); } console.log([...s].sort().join(" "))')
  maps_expect "$a" node "$lib" "$mapped"
done

# ---- one process row ----------------------------------------------------------------
# run_node_row ROW ALLOC: appends "wall_s maxrss_kb exit" to $OUT/ROW.ALLOC.scores.
run_node_row() {
  row=$1; a=$2; tag=$row.$a
  [ -f "$OUT/$tag.scores" ] || : > "$OUT/$tag.scores"
  n=$(wc -l < "$OUT/$tag.scores")
  log=$OUT/$tag.$n.out; rt=$OUT/$tag.$n.rt
  lib=$(lib_of "$a")
  case $row in
    startup)    RT_OUT=$rt "$BIN/rt" env ${lib:+LD_PRELOAD=$lib} "$NODE" -e 0 > "$log" 2>&1 || true;;
    json_churn) RT_OUT=$rt "$BIN/rt" env ${lib:+LD_PRELOAD=$lib} "$NODE" "$SCRIPTS/json_churn.js" "$JSON_ITERS" > "$log" 2>&1 || true;;
  esac
  record_rt "$tag" "$rt" "$log"
}
PROC_ROWS=; for row in $ROWS; do [ "$row" = server ] || PROC_ROWS="$PROC_ROWS $row"; done; PROC_ROWS=${PROC_ROWS# }

# ---- rounds -------------------------------------------------------------------------
if [ -n "$PROC_ROWS" ]; then
  echo "== $ROUNDS rounds + warm-up x $(echo $PROC_ROWS | wc -w) rows x $(echo $ALLOCS | wc -w) allocators ($ALLOCS); load $LOAD0"
  r=0
  while [ $r -le "$ROUNDS" ]; do
    for a in $ALLOCS; do
      for row in $PROC_ROWS; do run_node_row "$row" "$a"; done
    done
    printf '   round %d done, load %s, %s\n' $r "$(loadavg)" "$(date -u +%T)"
    r=$((r+1))
  done
  check_score_lines "$PROC_ROWS" "$ALLOCS" $((ROUNDS+1))
fi

# ---- the server row ------------------------------------------------------------------
# rss_kb PID: smaps_rollup Rss in KiB ("-" when the process is gone).
rss_kb() { awk '$1=="Rss:" {print $2; f=1} END {if (!f) print "-"}' "/proc/$1/smaps_rollup" 2>/dev/null || echo "-"; }
# smaps_by_name PID: "name rss_kb" per mapping name, anonymous mappings
# grouped under their [anon:...] name or "[anon]", files under their basename.
smaps_by_name() {
  awk '/^[0-9a-f]+-[0-9a-f]+ / {name=(NF>=6)?$6:"[anon]"; sub(".*/", "", name)} $1=="Rss:" {r[name]+=$2} END {for (k in r) printf "%s %d\n", k, r[k]}' "/proc/$1/smaps" 2>/dev/null | sort -k2 -nr
}
# ab_field FILE: "reqs rps p99 failed" from ab's report ("-" when missing).
ab_fields() {
  awk '
    /^Complete requests:/ {c=$3}
    /^Requests per second:/ {r=$4}
    /^Failed requests:/ {f=$3}
    $1=="99%" {p=$2}
    END {printf "%s %s %s %s\n", c?c:"-", r?r:"-", (p!="")?p:"-", (f!="")?f:"-"}' "$1"
}
run_server_row() {
  a=$1; lib=$(lib_of "$a"); d=$OUT/server.$a; mkdir -p "$d"
  rm -f "$d/pid"
  env ${lib:+LD_PRELOAD=$lib} "$NODE" "--max-old-space-size=$OLD_SPACE" "$SCRIPTS/server.js" "$PORT" "$d/pid" > "$d/server.log" 2>&1 &
  wrapper=$!
  i=0; while [ ! -s "$d/pid" ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done
  [ -s "$d/pid" ] || { cat "$d/server.log" >&2; die "server ($a) did not start on port $PORT"; }
  pid=$(cat "$d/pid")
  [ -d "/proc/$pid" ] || die "server ($a) pid $pid gone"
  echo "$(rss_kb "$pid")" > "$d/rss.idle"
  # warm-up: WARM_N requests on each endpoint, sequentially, not scored
  "$AB" -q -k -l -c $((CONNS/2)) -n "$WARM_N" "http://127.0.0.1:$PORT/json" > "$d/warm.json.ab" 2>&1 || { tail -5 "$d/warm.json.ab" >&2; die "ab warm-up (/json, $a) failed"; }
  "$AB" -q -k -l -c $((CONNS/2)) -n "$WARM_N" "http://127.0.0.1:$PORT/blob" > "$d/warm.blob.ab" 2>&1 || { tail -5 "$d/warm.blob.ab" >&2; die "ab warm-up (/blob, $a) failed"; }
  echo "$(rss_kb "$pid")" > "$d/rss.warm"
  # the timed load: both endpoints at once for DURATION s, RSS sampled meanwhile
  : > "$d/rss.samples"
  t0=$(date +%s)
  "$AB" -q -k -l -c $((CONNS/2)) -t "$DURATION" -n "$AB_N" "http://127.0.0.1:$PORT/json" > "$d/json.ab" 2>&1 &
  ab1=$!
  "$AB" -q -k -l -c $((CONNS/2)) -t "$DURATION" -n "$AB_N" "http://127.0.0.1:$PORT/blob" > "$d/blob.ab" 2>&1 &
  ab2=$!
  while kill -0 $ab1 2>/dev/null || kill -0 $ab2 2>/dev/null; do
    echo "$(( $(date +%s) - t0 )) $(rss_kb "$pid")" >> "$d/rss.samples"
    sleep "$SAMPLE"
  done
  wait $ab1 || echo "  server.$a: ab (/json) exited non-zero, see $d/json.ab" >&2
  wait $ab2 || echo "  server.$a: ab (/blob) exited non-zero, see $d/blob.ab" >&2
  echo "$(( $(date +%s) - t0 )) $(rss_kb "$pid")" >> "$d/rss.samples"
  # the end-of-run picture of the server before it is stopped
  grep -E '^(VmHWM|VmRSS|Threads):' "/proc/$pid/status" | tr -s ' ' > "$d/status"
  smaps_by_name "$pid" > "$d/smaps.byname"
  kill -TERM "$pid" 2>/dev/null || true
  wait $wrapper 2>/dev/null || true
  [ -d "/proc/$pid" ] && { sleep 1; kill -KILL "$pid" 2>/dev/null || true; }
  printf '   server.%s done: /json %s, /blob %s, rss peak %s KiB, %s\n' "$a" "$(ab_fields "$d/json.ab" | awk '{print $2 " req/s p99 " $3 " ms"}')" "$(ab_fields "$d/blob.ab" | awk '{print $2 " req/s p99 " $3 " ms"}')" "$(awk '$2!="-" && $2>m {m=$2} END {print m+0}' "$d/rss.samples")" "$(date -u +%T)"
}
if has_row server; then
  echo "== server row: $DURATION s per allocator, $CONNS connections ($((CONNS/2)) per endpoint), RSS sampled every $SAMPLE s; load $(loadavg)"
  for a in $ALLOCS; do run_server_row "$a"; done
fi

# ---- report ---------------------------------------------------------------------------
JSON_BYTES=$(cat "$OUT"/json_churn.*.1.out 2>/dev/null | sed -n 's/^iterations=[0-9]* bytes=\([0-9]*\).*/\1/p' | head -1); JSON_BYTES=${JSON_BYTES:-?}
list_of() { sorted_scores "$OUT/$1.$2.scores" 1; }
rss_of() { median_rss "$OUT/$1.$2.scores" 1; }
med_of() { median_list "$(list_of "$1" "$2")"; }
srv_peak_kb() { awk '$2!="-" && $2>m {m=$2} END {if (m) print m; else print "-"}' "$OUT/server.$1/rss.samples"; }
srv_final_kb() { awk '$2!="-" {v=$2} END {print v?v:"-"}' "$OUT/server.$1/rss.samples"; }
srv_hwm_kb() { awk '$1=="VmHWM:" {print $2; f=1} END {if (!f) print "-"}' "$OUT/server.$1/status"; }
srv_ab() { ab_fields "$OUT/server.$1/$2.ab" | cut -d' ' -f"$3"; }   # 1 reqs, 2 rps, 3 p99, 4 failed
srv_name_kb() { awk -v n="$2" '$1==n {print $2; f=1} END {if (!f) print "-"}' "$OUT/server.$1/smaps.byname"; }
# rps_pct A B: challoc's req/s against B's (positive = challoc worse = fewer req/s)
rps_pct() { awk -v a="$1" -v b="$2" 'BEGIN {if (a=="-"||b=="-"||a==""||b==""||a==0) print "-"; else printf "%+.1f%%", (b/a-1)*100}'; }
describe() { case $1 in challoc) describe_challoc "$CHALLOC_LIB" "$REV";; *) describe_stock "$1";; esac; }
has() { case " $ALLOCS " in *" $1 "*) return 0;; *) return 1;; esac; }
{
  result_header "Node.js benchmark rows"
  echo "- node: $NODE_VERSION (Wolfi ${NODE_PKG:-nodejs}), V8 $("$NODE" -p 'process.versions.v8'); the same binary under every allocator, LD_PRELOAD only"
  has_row server && echo "- load generator: $AB_VERSION (Wolfi apache2-utils; no wrk or oha package in Wolfi, checked 2026-09-20), two processes at once, \`-k -l -c $((CONNS/2)) -t $DURATION -n $AB_N\` on /json and on /blob"
  echo "- rows: startup = \`node -e 0\`; json_churn = bench/node/json_churn.js $JSON_ITERS (JSON.parse(JSON.stringify(obj)) over a $JSON_BYTES-byte JSON document, $JSON_ITERS times, one process); server = bench/node/server.js under \`node --max-old-space-size=$OLD_SPACE\`, GET /json (a small JSON object built and stringified per request) and GET /blob (a 1 MiB body from a per-request Buffer.allocUnsafe(1 << 20), filled, written in 64 KiB chunks with Content-Length), $DURATION s per allocator after a $WARM_N-request warm-up per endpoint, once per allocator"
  echo "- load average at start: $LOAD0; at end: $(loadavg); $(( $(date +%s) - T_START )) s total"
  echo "- method: $ROUNDS timed rounds after one discarded warm-up for the process rows, allocators interleaved per round, one node process per row and round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB). The server row runs once per allocator: req/s and p99 (ms) are ab's, RSS is the server's /proc/PID/smaps_rollup Rss sampled every $SAMPLE s during the load (peak and final sample) and VmHWM from /proc/PID/status at the end. Percentages are challoc against the other allocator: positive means challoc is slower (fewer req/s) or larger."
  echo "- allocators:"
  for a in $ALLOCS; do echo "  - $a: $(describe "$a")"; done
  if [ -n "$PROC_ROWS" ]; then
    echo
    echo "## Sorted scores (wall seconds)"
    echo
    echo "| row | allocator | sorted wall s | maxrss KiB |"
    echo "|---|---|---|---:|"
    for row in $PROC_ROWS; do for a in $ALLOCS; do
      flag=; [ "$(any_failed "$OUT/$row.$a.scores" 1)" = 0 ] || flag=" (FAILED)"
      echo "| $row | $a | $(list_of "$row" "$a")$flag | $(rss_of "$row" "$a") |"
    done; done
    echo
    echo "## Medians"
    echo
    hdr="| row |"; sep="|---|"
    for a in $ALLOCS; do hdr="$hdr $a s | $a RSS MiB |"; sep="$sep---:|---:|"; done
    if has challoc; then for a in $ALLOCS; do [ "$a" = challoc ] || { hdr="$hdr challoc vs $a time | RSS |"; sep="$sep---:|---:|"; }; done; fi
    echo "$hdr"; echo "$sep"
    for row in $PROC_ROWS; do
      line="| $row |"; deltas=
      for a in $ALLOCS; do line="$line $(med_of "$row" "$a") | $(kib_to_mib "$(rss_of "$row" "$a")") |"; done
      if has challoc; then
        cm=$(med_of "$row" challoc); cr=$(rss_of "$row" challoc)
        for a in $ALLOCS; do [ "$a" = challoc ] || deltas="$deltas $(pct_of "$cm" "$(med_of "$row" "$a")") | $(pct_of "$cr" "$(rss_of "$row" "$a")") |"; done
      fi
      echo "$line$deltas"
    done
  fi
  if has_row server; then
    echo
    echo "## Server row ($DURATION s per allocator, $CONNS connections)"
    echo
    echo "| allocator | /json req/s | /json p99 ms | /blob req/s | /blob p99 ms | failed | RSS idle MiB | RSS peak MiB | RSS final MiB | VmHWM MiB |"
    echo "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    for a in $ALLOCS; do
      failed=$(( $(srv_ab "$a" json 4 | sed 's/-/0/') + $(srv_ab "$a" blob 4 | sed 's/-/0/') ))
      echo "| $a | $(srv_ab "$a" json 2) | $(srv_ab "$a" json 3) | $(srv_ab "$a" blob 2) | $(srv_ab "$a" blob 3) | $failed | $(kib_to_mib "$(cat "$OUT/server.$a/rss.idle")") | $(kib_to_mib "$(srv_peak_kb "$a")") | $(kib_to_mib "$(srv_final_kb "$a")") | $(kib_to_mib "$(srv_hwm_kb "$a")") |"
    done
    if has challoc; then
      echo
      echo "| challoc vs | /json req/s | /json p99 | /blob req/s | /blob p99 | RSS peak | RSS final | VmHWM |"
      echo "|---|---:|---:|---:|---:|---:|---:|---:|"
      for a in $ALLOCS; do [ "$a" = challoc ] && continue
        echo "| $a | $(rps_pct "$(srv_ab challoc json 2)" "$(srv_ab "$a" json 2)") | $(pct_of "$(srv_ab challoc json 3)" "$(srv_ab "$a" json 3)") | $(rps_pct "$(srv_ab challoc blob 2)" "$(srv_ab "$a" blob 2)") | $(pct_of "$(srv_ab challoc blob 3)" "$(srv_ab "$a" blob 3)") | $(pct_of "$(srv_peak_kb challoc)" "$(srv_peak_kb "$a")") | $(pct_of "$(srv_final_kb challoc)" "$(srv_final_kb "$a")") | $(pct_of "$(srv_hwm_kb challoc)" "$(srv_hwm_kb "$a")") |"
      done
    fi
    echo
    echo "Completed requests: $(for a in $ALLOCS; do printf '%s /json %s /blob %s; ' "$a" "$(srv_ab "$a" json 1)" "$(srv_ab "$a" blob 1)"; done | sed 's/; $//'). Requests per second are ab's mean over its run; p99 is ab's \"99%\" line (ms, client side). RSS idle is after the listener bound, before any request. Positive percentages mean challoc did worse (fewer req/s, higher p99, more RSS)."
    echo
    echo "### RSS by mapping name at the end of the load (KiB, /proc/PID/smaps summed per name; the top entries)"
    echo
    echo "| allocator | mappings (name: Rss KiB) |"
    echo "|---|---|"
    for a in $ALLOCS; do
      echo "| $a | $(head -8 "$OUT/server.$a/smaps.byname" | awk '{printf "%s: %s; ", $1, $2}' | sed 's/; $//') |"
    done
    echo
    echo "### RSS samples (s since load start: smaps_rollup Rss KiB)"
    echo
    for a in $ALLOCS; do
      echo "- $a: $(awk '{printf "%s:%s ", $1, $2}' "$OUT/server.$a/rss.samples")"
    done
  fi
} > "$RESULT"
echo "== wrote $RESULT"
sed -n '/^## Medians/,$p' "$RESULT" | grep -v '^- ' | head -40

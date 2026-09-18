#!/bin/sh
# calloc-zero-campaign.sh: the reproduction campaign for the 2026-09-18
# calloc failure (docs/review-2026-09-18.md 1.1 and 1.4, plan Phase 1.5
# task 7b). Runs inside the dev container (`make run
# CMD='sh bench/probes/calloc-zero-campaign.sh'`), writes everything to
# stdout, so on a VM run it detached with the log under ~/logs/.
#
#   1. environment: uname -r, the features probe line, the THP setting;
#   2. the full test suite once (the equivalent of `make test`), which also
#      builds the release library and the debug test binary;
#   3. under a continuous background `cargo test --features std api::` load
#      (the scheduling the one failure needed), ROUNDS runs of
#      `stress 8 50 10` for each MALLOC_CONF configuration with
#      `check_zero:true`: the default, `dirty_decay_ms:-1` (no purging: the
#      discard path is out), `tcache_compact_shift:0` (no compaction) and
#      `narenas:1`; a configuration stops early at MAX_SECS (the count
#      is what is reported);
#   4. STANDALONE runs of `stress 16 100 20` alone, `check_zero:true`.
#
# Every failing run's exit status and stderr are printed verbatim; a
# `check_zero` abort names the class, slab, prior state, arena, discard
# backend and word. The summary at the end is one line per configuration.
set -u
# ROUNDS, MAX_SECS, STANDALONE, CONFIGS and SKIP_SUITE=1 (reuse the build
# and skip step 2) are the knobs.
ROUNDS="${ROUNDS:-300}"
MAX_SECS="${MAX_SECS:-900}"
STANDALONE="${STANDALONE:-20}"
OUT="${OUT:-target/calloc-zero}"
mkdir -p "$OUT"
rm -f "$OUT/stop" "$OUT/load.log" "$OUT/load-exits"
SO="$(pwd)/target/release/libchalloc.so"

echo "== calloc-zero campaign $(date -u +%Y-%m-%dT%H:%M:%SZ) arch $(uname -m) ROUNDS=$ROUNDS MAX_SECS=$MAX_SECS STANDALONE=$STANDALONE"
echo "kernel: $(uname -r)"
echo "thp: $(cat /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null || echo unavailable)"
echo "nproc: $(nproc)  load: $(cut -d' ' -f1-3 /proc/loadavg)"

echo "== step 2: full suite"
t0=$(date +%s)
cargo build --release > "$OUT/build.log" 2>&1 || { echo "build FAILED"; tail -20 "$OUT/build.log"; exit 1; }
if [ -z "${SKIP_SUITE:-}" ]; then
  sh scripts/jemalloc-compat.sh > "$OUT/compat.log" 2>&1 || { echo "jemalloc-compat FAILED"; tail -20 "$OUT/compat.log"; exit 1; }
  cargo test --features std -- --nocapture --skip api:: > "$OUT/suite1.log" 2>&1; r1=$?
  cargo test --features std api:: -- --nocapture > "$OUT/suite2.log" 2>&1; r2=$?
  echo "suite: lib exit $r1 ($(grep -h '^test result' "$OUT/suite1.log" | tail -1)); api exit $r2 ($(grep -h '^test result' "$OUT/suite2.log" | tail -1)); $(( $(date +%s) - t0 )) s"
  if [ "$r1" != 0 ] || [ "$r2" != 0 ]; then
    echo "-- suite failures:"; grep -h -E '^test .* FAILED|panicked at|challoc:' "$OUT/suite1.log" "$OUT/suite2.log" | head -40
  fi
else
  cargo test --features std --no-run > "$OUT/suite1.log" 2>&1 || { echo "test build FAILED"; exit 1; }
  cargo test --features std probe_prints_and_matches_kernel_version -- --nocapture > "$OUT/suite1.log" 2>&1
  echo "suite: skipped (SKIP_SUITE)"
fi
echo "probe: $(grep -h '^kernel ' "$OUT/suite1.log" | head -1)"

cc -O2 -g -Wall -Wextra -Werror -pthread -DQUANTUM=16 -Itests/c -Wno-unused-parameter -Itests/mimalloc/include \
   tests/mimalloc/stress.c -o "$OUT/stress" -ldl || { echo "stress compile FAILED"; exit 1; }

# One stress run: $1 conf, $2 label, $3.. args. Prints nothing on success.
run_stress() {
  conf="$1"; who="$2"; shift 2
  err="$OUT/err.$$"
  LD_PRELOAD="$SO" MALLOC_CONF="$conf" "$OUT/stress" "$@" > /dev/null 2> "$err"; rc=$?
  if [ "$rc" != 0 ]; then
    echo "!! FAIL $who exit $rc conf=$conf args=$*"
    sed 's/^/!!   /' "$err"
  fi
  rm -f "$err"
  return $rc
}

echo "== step 3: stress 8 50 10 under the api:: load, $ROUNDS rounds per configuration"
(
  n=0
  while [ ! -f "$OUT/stop" ]; do
    cargo test --features std api:: >> "$OUT/load.log" 2>&1
    echo "$?" >> "$OUT/load-exits"
    n=$((n + 1))
  done
) &
LOAD=$!
sleep 5
# CONFIGS: label:conf pairs, space separated; the label ends at the first colon.
CONFIGS="${CONFIGS:-default:check_zero:true nodecay:check_zero:true,dirty_decay_ms:-1 nocompact:check_zero:true,tcache_compact_shift:0 narenas1:check_zero:true,narenas:1}"
for cfg in $CONFIGS; do
  label="${cfg%%:*}"; conf="${cfg#*:}"
  ok=0; fail=0; t0=$(date +%s)
  i=0
  while [ "$i" -lt "$ROUNDS" ] && [ $(( $(date +%s) - t0 )) -lt "$MAX_SECS" ]; do
    i=$((i + 1))
    if run_stress "$conf" "$label#$i" 8 50 10; then ok=$((ok + 1)); else fail=$((fail + 1)); fi
  done
  echo "RESULT $label: $ok ok, $fail failed, $i runs of stress 8 50 10, $(( $(date +%s) - t0 )) s, conf=$conf, load runs so far $(wc -l < "$OUT/load-exits" 2>/dev/null || echo 0)"
done
touch "$OUT/stop"
wait "$LOAD"
loads=$(wc -l < "$OUT/load-exits"); loadfail=$(grep -cv '^0$' "$OUT/load-exits")
echo "RESULT load: $loads runs of cargo test api::, $loadfail failed ($(grep -h '^test result' "$OUT/load.log" | sort | uniq -c | sed 's/^ *//' | tr '\n' ';'))"
if [ "$loadfail" != 0 ]; then
  echo "-- load failures:"; grep -h -E '^test .* FAILED|panicked at|challoc:|Assertion' "$OUT/load.log" | sort | uniq -c | head -40
fi

echo "== step 4: stress 16 100 20 standalone, $STANDALONE runs, check_zero:true"
ok=0; fail=0; t0=$(date +%s); i=0
while [ "$i" -lt "$STANDALONE" ]; do
  i=$((i + 1))
  if run_stress "check_zero:true" "standalone#$i" 16 100 20; then ok=$((ok + 1)); else fail=$((fail + 1)); fi
done
echo "RESULT standalone: $ok ok, $fail failed, $STANDALONE runs of stress 16 100 20, $(( $(date +%s) - t0 )) s"
echo "== done $(date -u +%Y-%m-%dT%H:%M:%SZ) load: $(cut -d' ' -f1-3 /proc/loadavg)"

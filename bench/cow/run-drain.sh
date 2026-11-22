#!/bin/sh
# The session matrix behind RESULTS.md's "Client throughput during the
# drain": Redis and Valkey, 64 B (10M keys, 1M overwritten during the save)
# and 1 KiB (2M keys, 200k), each run once with the fork epoch (cow) and once
# with MALLOC_CONF=cow_epoch:false (noepoch: the same build, no drain),
# cowbench --drain-bench: SET and GET throughput and p99 with activedefrag on,
# before the save and right after the child exits. Point the variables at
# the challoc builds (tests/consumers/redis-build.sh, valkey-build.sh: Redis
# with contrib/redis 0001 to 0006, Valkey with contrib/valkey 0001 to 0004,
# quantum8 challoc):
#   REDIS REDIS_BENCH VALKEY VALKEY_BENCH
# REP repetitions (default 1). DRAIN_WAIT seconds to watch for the drain's
# end after the passes (default 330: challoc's drain window is 300 s from
# the child's exit, so the end is seen even when the cycle runs it out).
# LATE seconds after the child's exit the passes run again (default 120;
# 0 for none).
# Session measurements, not a cb suite: see bench/cow/README.md.
set -eu
B=$(dirname "$0")/cowbench.py
OUT=${OUT:-drain.jsonl}
: "${REDIS:?}" "${REDIS_BENCH:?}" "${VALKEY:?}" "${VALKEY_BENCH:?}"
rep=1
while [ "$rep" -le "${REP:-1}" ]; do
  for srv in "redis $REDIS $REDIS_BENCH" "valkey $VALKEY $VALKEY_BENCH"; do
    # shellcheck disable=SC2086 # "name server bench": split on purpose
    set -- $srv
    for cell in "64,10000000,1000000,-10" "1024,2000000,200000,-2"; do
      IFS=, read -r v k w d <<EOT
$cell
EOT
      python3 "$B" --server "$2" --bench "$3" --drain-bench --drain-wait "${DRAIN_WAIT:-330}" --late-s "${LATE:-120}" --label "$1-cow" --vsize "$v" --keys "$k" \
        --writes "$w" --save-delay "$d" --seed "$rep" >> "$OUT"
      python3 "$B" --server "$2" --bench "$3" --drain-bench --drain-wait "${DRAIN_WAIT:-330}" --late-s "${LATE:-120}" --label "$1-noepoch" --malloc-conf cow_epoch:false \
        --vsize "$v" --keys "$k" --writes "$w" --save-delay "$d" --seed "$rep" >> "$OUT"
    done
  done
  rep=$((rep + 1))
done

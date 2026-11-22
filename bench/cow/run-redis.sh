#!/bin/sh
# The Redis 8.10.3 session matrix behind RESULTS.md's Redis section, one
# repetition: 64 B (raw in Redis) and 1 KiB values with a tenth of the keys
# overwritten during the save, 64 B with a tenth deleted, SET throughput,
# and the recovery with activedefrag on. Point the variables at
# redis-server binaries, the challoc ones linked against a quantum8
# libchalloc.so.0 with an rpath:
#   JE    stock 8.10.3, bundled jemalloc 5.3.0
#   JE5   + contrib/redis/0005, bundled jemalloc
#   CH    + 0001 to 0004 (MALLOC=challoc)
#   COW   + 0001 to 0006
#   BENCH a redis-benchmark
# The 0005-alone and 0006-alone points of RESULTS.md are COW with
# MALLOC_CONF=cow_epoch:false (cowbench --malloc-conf) and a build with
# 0001 to 0004 and 0006. Session measurements, not a cb suite: see
# bench/cow/README.md.
set -eu
B=$(dirname "$0")/cowbench.py
OUT=${OUT:-redis-cow.jsonl}
: "${JE:?}" "${JE5:?}" "${CH:?}" "${COW:?}" "${BENCH:?}"
rep=${REP:-1}
for cell in "64,10000000,1000000,-10" "1024,2000000,200000,-2"; do
  IFS=, read -r v k w d <<EOT
$cell
EOT
  for x in "je $JE" "je5 $JE5" "ch $CH" "cow $COW"; do
    # shellcheck disable=SC2086 # "label path": split on purpose
    set -- $x
    python3 "$B" --server "$2" --label "$1" --vsize "$v" --keys "$k" --writes "$w" --save-delay "$d" --seed "$rep" >> "$OUT"
  done
done
for x in "je $JE" "je5 $JE5" "ch $CH" "cow $COW"; do
  # shellcheck disable=SC2086
  set -- $x
  python3 "$B" --server "$2" --label "$1-del" --op DEL --vsize 64 --keys 10000000 --writes 1000000 --save-delay -10 --seed "$rep" >> "$OUT"
done
for v in 64 1024; do
  k=10000000; d=-10; [ "$v" = 1024 ] && { k=2000000; d=-2; }
  python3 "$B" --server "$CH" --label ch-tput --vsize "$v" --keys "$k" --save-delay "$d" --bench "$BENCH" --seed "$rep" >> "$OUT"
  python3 "$B" --server "$COW" --label cow-tput --vsize "$v" --keys "$k" --save-delay "$d" --bench "$BENCH" --seed "$rep" >> "$OUT"
done
for cell in "64,10000000,1000000,-10" "1024,2000000,200000,-2"; do
  IFS=, read -r v k w d <<EOT
$cell
EOT
  python3 "$B" --server "$CH" --label ch-rec --vsize "$v" --keys "$k" --writes "$w" --save-delay "$d" --recover 120 --defrag --seed "$rep" >> "$OUT"
  python3 "$B" --server "$COW" --label cow-rec --vsize "$v" --keys "$k" --writes "$w" --save-delay "$d" --recover 120 --defrag --seed "$rep" >> "$OUT"
done

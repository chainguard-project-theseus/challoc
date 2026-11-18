#!/bin/sh
# The fork/COW epoch measurement (docs/design.md section 5, "Fork epochs"):
# Valkey 9.1.2 under bundled jemalloc, challoc as shipped and challoc with
# the epoch, every build with contrib/valkey/0003 (no value write on
# unlink); the challoc ones with 0001 and 0002, the epoch one with 0004.
# Point JE, CH and COW at the three valkey-server binaries (the challoc
# ones linked against their library with an rpath, quantum8) and BENCH at
# a valkey-benchmark. Writes one JSON line per run to OUT; summarize.py
# reads them. Session measurements, not a cb suite: see bench/cow/README.md.
set -eu
B=$(dirname "$0")/cowbench.py
OUT=${OUT:-cow.jsonl}
: "${JE:?}" "${CH:?}" "${COW:?}" "${BENCH:?}"
CELLS="64,10000000,100000,-10 64,10000000,1000000,-10 1024,2000000,20000,-2 1024,2000000,200000,-2"
for rep in 1 2 3; do
  for cell in $CELLS; do
    IFS=, read v k w d <<EOT
$cell
EOT
    for x in "je $JE" "ch $CH" "cow $COW"; do
      set -- $x
      python3 "$B" --server "$2" --label "$1" --vsize "$v" --keys "$k" --writes "$w" --save-delay "$d" --seed "$rep" >> "$OUT"
    done
  done
done
for rep in 1 2 3; do
  for v in 64 1024; do
    k=10000000; d=-10; [ "$v" = 1024 ] && { k=2000000; d=-2; }
    python3 "$B" --server "$CH" --label ch-tput --vsize "$v" --keys "$k" --save-delay "$d" --bench "$BENCH" --seed "$rep" >> "$OUT"
    python3 "$B" --server "$COW" --label cow-tput --vsize "$v" --keys "$k" --save-delay "$d" --bench "$BENCH" --seed "$rep" >> "$OUT"
  done
done
for rep in 1 2; do
  for cell in "64,10000000,1000000,-10" "1024,2000000,200000,-2"; do
    IFS=, read v k w d <<EOT
$cell
EOT
    python3 "$B" --server "$CH" --label ch-rec --vsize "$v" --keys "$k" --writes "$w" --save-delay "$d" --recover 120 --defrag --seed "$rep" >> "$OUT"
    python3 "$B" --server "$COW" --label cow-rec --vsize "$v" --keys "$k" --writes "$w" --save-delay "$d" --recover 120 --defrag --seed "$rep" >> "$OUT"
  done
done

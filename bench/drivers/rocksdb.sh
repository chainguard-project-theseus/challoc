#!/bin/sh
# bench/drivers/rocksdb.sh: RocksDB db_bench (the storage-engine class),
# built in the bench image with DISABLE_JEMALLOC and no tcmalloc, so every
# allocation (memtable arenas, block cache entries, iterators, compaction
# buffers) goes through the allocator under test. Three db_bench processes
# on one fresh database under CB_OUT:
#
#   fillrandom        NUM keys (16-byte keys, 100-byte values), 1 thread
#   readrandom        READS lookups per thread on THREADS threads, the
#                     database reopened (block cache misses fill the cache)
#   readwhilewriting  THREADS readers and one writer for RWW_SECS seconds
#
# No compression, a 256 MiB block cache (the data set is about 250 MiB, so
# the steady state allocates and frees cache blocks). Metrics:
# readrandom_ops (ops/s, the score), fillrandom_ops, readwhilewriting_ops,
# maxrss_mib (the largest of the three processes).
. "$(dirname "$0")/cb.sh"
NUM=${NUM:-2000000}
READS=${READS:-400000}
THREADS=${THREADS:-8}
RWW_SECS=${RWW_SECS:-8}
DB_BENCH=$CB_BIN/db_bench
[ -x "$DB_BENCH" ] || die "no $DB_BENCH (bench/build.sh apps)"
DB=$CB_OUT/db
rm -rf "$DB"
trap 'rm -rf "$DB"' EXIT
COMMON="--db=$DB --key_size=16 --value_size=100 --compression_type=none --cache_size=268435456 --statistics=0 --histogram=0 --seed=1"

ops_of() { awk -v b="$1" '$1 == b && $2 == ":" { for (i = 3; i < NF; i++) if ($(i+1) == "ops/sec;" || $(i+1) == "ops/sec") { print $i; exit } }' "$CB_OUT/$1.out"; }

# shellcheck disable=SC2086
timed fillrandom "$DB_BENCH" $COMMON --benchmarks=fillrandom --num="$NUM" --threads=1 --use_existing_db=0
rss=$MAXRSS_KIB
# shellcheck disable=SC2086
timed readrandom "$DB_BENCH" $COMMON --benchmarks=readrandom --num="$NUM" --reads="$READS" --threads="$THREADS" --use_existing_db=1
[ "$MAXRSS_KIB" -gt "$rss" ] && rss=$MAXRSS_KIB
# shellcheck disable=SC2086
timed readwhilewriting "$DB_BENCH" $COMMON --benchmarks=readwhilewriting --num="$NUM" --threads="$THREADS" --duration="$RWW_SECS" --use_existing_db=1
[ "$MAXRSS_KIB" -gt "$rss" ] && rss=$MAXRSS_KIB

for b in fillrandom readrandom readwhilewriting; do
  v=$(ops_of $b)
  [ -n "$v" ] || { tail -5 "$CB_OUT/$b.out" >&2; die "no '$b : ... ops/sec' line"; }
  metric "${b}_ops" "$v"
done
metric maxrss_mib "$(mib "$rss")"
emit

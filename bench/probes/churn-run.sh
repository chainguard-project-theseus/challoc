#!/usr/bin/env bash
set -uo pipefail
ARCH=$(uname -m); case $ARCH in aarch64) T=arm64;; x86_64) T=amd64;; esac
cd ~/challoc
echo "== $(hostname) $(uname -m) load $(cut -d' ' -f1-3 /proc/loadavg) commit $(git rev-parse --short HEAD 2>/dev/null)"
docker run --rm -v $PWD:/src -w /src -v challoc-target-$T:/src/target -v challoc-cargo-registry:/root/.cargo/registry challoc-dev:$T sh -c '
cargo build --release 2>&1 | grep -E "^error|Finished"; ls -la target/release/libchalloc.so &&
cc -O2 -Wall -o target/churn bench/probes/churn.c &&
for a in challoc jemalloc; do
  case $a in challoc) L=/src/target/release/libchalloc.so;; jemalloc) L=/usr/lib/libjemalloc.so.2;; esac
  echo "=== $a S=1024 N=30000 T=20 (default conf)"; LD_PRELOAD=$L target/churn 1024 30000 20 0.56
  echo "=== $a S=1024 N=30000 T=20 rate-limited? no; second class 640"; LD_PRELOAD=$L target/churn 640 30000 20 0.56 | awk "NR==1||NR==3||/churn/&&(NR%5==0)||/end|ops/"
done
echo "=== challoc S=1024 with MALLOC_CONF=tcache_compact_shift:0 (scheduled pops off: the pre-P1 cache, nothing may move)"; MALLOC_CONF=tcache_compact_shift:0 LD_PRELOAD=/src/target/release/libchalloc.so target/churn 1024 30000 20 0.56 | awk "NR<=3||/churn/&&(NR%5==0)||/end|ops/"
echo "=== challoc S=1024 with MALLOC_CONF=tcache_compact_shift:5 (one pop in 32)"; MALLOC_CONF=tcache_compact_shift:5 LD_PRELOAD=/src/target/release/libchalloc.so target/churn 1024 30000 20 0.56 | awk "NR<=3||/churn/&&(NR%5==0)||/end|ops/"
echo "=== challoc S=1024 with MALLOC_CONF=tcache_nslots_small_max:64 (cap 8 for the 1 KiB class)"; MALLOC_CONF=tcache_nslots_small_max:64 LD_PRELOAD=/src/target/release/libchalloc.so target/churn 1024 30000 20 0.56 | awk "NR<=3||/churn/&&(NR%5==0)||/end|ops/"
echo "=== challoc S=1024 with MALLOC_CONF=tcache:false"; MALLOC_CONF=tcache:false LD_PRELOAD=/src/target/release/libchalloc.so target/churn 1024 30000 20 0.56 | awk "NR<=3||/churn/&&(NR%5==0)||/end|ops/"
'

#!/bin/sh
# scripts/pagemap-guard-test.sh: plan Phase 2.5 task 17, the guard slabs
# checked through PAGEMAP_SCAN (tests/c/pagemap_guard.c), run INSIDE the dev
# container by `make test-pagemap-guard` (part of `make test`). Both ways:
# the default configuration (guard slabs where the kernel installs guard
# regions, 6.13+; reported as PAGE_IS_GUARD from 6.16, checked by the
# fallback branches below that) and MALLOC_CONF=guards:false (no guard slab,
# no guard page). The program says which kernel case it saw; a missing
# capability is a documented fallback, not a failure. Exit status is the
# first failing run's.
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT" || exit 2
lib=target/release/libchalloc.so
[ -f "$lib" ] || { echo "$lib missing: cargo build --release first" >&2; exit 2; }
mkdir -p target/ctests
bin=target/ctests/pagemap_guard
if [ ! -x "$bin" ] || [ tests/c/pagemap_guard.c -nt "$bin" ] || [ tests/c/common.h -nt "$bin" ]; then
  cc -O2 -g -Wall -Wextra -Werror -pthread -DQUANTUM=16 -Itests/c tests/c/pagemap_guard.c -o "$bin" -ldl || exit 2
fi
echo "== pagemap_guard on (kernel $(uname -r) $(uname -m), default configuration)"
LD_PRELOAD=$ROOT/$lib "$bin" on || exit 1
echo "== pagemap_guard off (MALLOC_CONF=guards:false)"
MALLOC_CONF=guards:false,abort_conf:true LD_PRELOAD=$ROOT/$lib "$bin" off || exit 1

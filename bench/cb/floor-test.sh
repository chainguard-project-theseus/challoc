#!/bin/sh
# bench/cb/floor-test.sh: the `floor-test` row (bench/rows.toml): `make
# test` and `make fuzz` on the challoc variant's own source tree, as cb
# extracted it for the build (target/cb/src/<tree>), on the cell's kernel:
#
#   build, jemalloc-compat, unit, api   the in-container part of `make test`
#   cgroup-limit, cgroup-churn,         `make test`'s cgroup steps, each in
#   cgroup-lazy                         its own memory-limited cgroup; only
#                                       where the container may write its
#                                       cgroup (the privileged profile), as
#                                       a child cgroup per run with the
#                                       Makefile's limits (memory.max, swap
#                                       0) instead of a container per run;
#                                       elsewhere one SKIP line
#   fuzz                                `make fuzz`: fuzz/fuzz.c under
#                                       LD_PRELOAD, FUZZ_OPS (10M) on
#                                       FUZZ_THREADS (4), seed FUZZ_SEED
#
# CHALLOC_TEST_ALLOW_SKIP=1 as in `make test`: a kernel- or privilege-gated
# test prints `SKIP <test> <reason>` and passes. The last lines summarise
# the run: `floor: <step> ok|FAIL`, the test result lines, every SKIP and
# `floor: result pass|fail`. The exit status is 0 when every step passed.
set -u
unset LD_PRELOAD MALLOC_CONF CHALLOC_CONF
ROOT=${CB_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
n=0
for d in "$ROOT"/target/cb/src/*/; do
  [ -f "$d/Cargo.toml" ] || continue
  SRC=${d%/}; n=$((n + 1))
done
[ "$n" = 1 ] || { echo "floor: expected one challoc source tree under $ROOT/target/cb/src, found $n"; exit 2; }
cd "$SRC" || exit 2
export CARGO_TARGET_DIR="$SRC/target" CHALLOC_TEST_ALLOW_SKIP=1 QUANTUM=${QUANTUM:-16}
LOG=$CARGO_TARGET_DIR/floor-test.log
mkdir -p "$CARGO_TARGET_DIR"
: > "$LOG"
echo "floor: $(uname -r) $(uname -m) page $(getconf PAGESIZE) nproc $(nproc) uid $(id -u) caps $(grep CapEff /proc/self/status | cut -f2)"
rc=0
step() {
  name=$1; shift
  t=$(date +%s)
  if "$@" >> "$LOG" 2>&1; then
    echo "floor: $name ok ($(( $(date +%s) - t )) s)"
  else
    echo "floor: $name FAIL ($(( $(date +%s) - t )) s)"
    rc=1
  fi
}
step build cargo build --release
step jemalloc-compat sh scripts/jemalloc-compat.sh
step unit cargo test -- --nocapture --skip api::
step api cargo test api:: -- --nocapture

# ---- cgroup steps: a child cgroup per run ---------------------------------
# cgroup v2 allows controllers in a child only when the parent holds no
# processes (the container's own cgroup is not the root), so every process
# of the container moves to a leaf `cb-floor-init` first, then the memory
# controller is enabled for the children.
CGM=$(awk '$0 ~ / - cgroup2 / { print $5; exit }' /proc/self/mountinfo)
CGP=$(sed -n 's/^0::\(.*\)$/\1/p' /proc/self/cgroup); [ "$CGP" = / ] && CGP=
CGB=$CGM$CGP
cg_ready=
if [ -n "$CGM" ] && [ -w "$CGB/cgroup.subtree_control" ] && grep -qw memory "$CGB/cgroup.controllers" 2>/dev/null \
   && mkdir -p "$CGB/cb-floor-init" 2>/dev/null; then
  for p in $(cat "$CGB/cgroup.procs"); do echo "$p" > "$CGB/cb-floor-init/cgroup.procs" 2>/dev/null; done
  echo +memory > "$CGB/cgroup.subtree_control" 2>/dev/null && cg_ready=1
fi
cgn=0
# cg_step NAME LIMIT CMD...: CMD in a fresh child cgroup of LIMIT bytes
# (memory.swap.max 0, the Makefile's --memory-swap equal to --memory).
cg_step() {
  name=$1 lim=$2; shift 2
  cgn=$((cgn + 1)); d=$CGB/cb-floor-$cgn
  mkdir -p "$d" && echo "$lim" > "$d/memory.max" || { echo "floor: $name FAIL (cannot create $d)"; rc=1; return; }
  [ -f "$d/memory.swap.max" ] && echo 0 > "$d/memory.swap.max"
  step "$name" sh -c 'echo $$ > "$0/cgroup.procs" && exec "$@"' "$d" "$@"
  rmdir "$d" 2>/dev/null
}
M512=536870912 M256=268435456
if [ -n "$cg_ready" ]; then
  cg_step cgroup-limit $M512 env CHALLOC_EXPECT_CGROUP_LIMIT=$M512 cargo test cgroup -- --nocapture --test-threads=1
  cg_step cgroup-churn-512-purge $M512 sh scripts/cgroup-tests.sh churn challoc purge 400 100
  cg_step cgroup-churn-512-keep $M512 sh scripts/cgroup-tests.sh churn challoc-off keep 400 100
  cg_step cgroup-churn-256-purge $M256 sh scripts/cgroup-tests.sh churn challoc purge 200 50
  cg_step cgroup-churn-256-keep $M256 sh scripts/cgroup-tests.sh churn challoc-off keep 200 50
  cg_step cgroup-lazy-lazy $M512 sh scripts/cgroup-tests.sh lazy challoc lazy 200
  cg_step cgroup-lazy-eager $M512 sh scripts/cgroup-tests.sh lazy challoc-eager eager 200
else
  echo "SKIP floor-cgroup the container cannot write its cgroup (${CGB:-no cgroup2 mount}); run the privileged profile"
fi

# ---- make fuzz ----------------------------------------------------------------
FUZZ_OPS=${FUZZ_OPS:-10000000} FUZZ_THREADS=${FUZZ_THREADS:-4} FUZZ_SEED=${FUZZ_SEED:-0xC4A110C} FUZZ_TABLE=${FUZZ_TABLE:-2048}
step fuzz-build sh scripts/ctest-cc.sh fuzz/fuzz.c -o "$CARGO_TARGET_DIR/fuzz"
step fuzz env LD_PRELOAD="$CARGO_TARGET_DIR/release/libchalloc.so" "$CARGO_TARGET_DIR/fuzz" "$FUZZ_OPS" "$FUZZ_THREADS" "$FUZZ_SEED" "$FUZZ_TABLE"
grep -E '^test result:|^failures:|^    [a-z_:]+$|panicked at|^SKIP |^OUTCOME |^fuzz' "$LOG" | sort | uniq -c | sort -rn | head -80
grep -E '^---- .* stdout ----' "$LOG" | head -20
[ $rc = 0 ] || tail -60 "$LOG"
echo "floor: result $([ $rc = 0 ] && echo pass || echo fail)"
exit $rc

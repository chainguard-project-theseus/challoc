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
#                                       0 where cgroup_run sets it)
#                                       instead of a container per run;
#                                       elsewhere one SKIP line (a FAIL
#                                       under the privileged profile)
#   fuzz                                `make fuzz`: fuzz/fuzz.c under
#                                       LD_PRELOAD, FUZZ_OPS (10M) on
#                                       FUZZ_THREADS (4), seed FUZZ_SEED
#
# CHALLOC_TEST_ALLOW_SKIP=1 as in `make test`: a kernel- or privilege-gated
# test prints `SKIP <test> <reason>` and passes. The last lines summarise
# the run: `floor: <step> ok|FAIL`, every test result, SKIP, OUTCOME and
# fuzz line (uncapped, tagged with its step), the failing tests and
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
  echo "floor-step: $name" >> "$LOG"
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
  # A process forked between the move and the write keeps the parent busy
  # (EBUSY): move again and retry.
  for try in 1 2 3 4 5; do
    for p in $(cat "$CGB/cgroup.procs"); do echo "$p" > "$CGB/cb-floor-init/cgroup.procs" 2>/dev/null; done
    echo +memory > "$CGB/cgroup.subtree_control" 2>/dev/null && { cg_ready=1; break; }
    sleep 1
  done
fi
cgn=0
# cg_step NAME LIMIT SWAP CMD...: CMD in a fresh child cgroup of LIMIT
# bytes. SWAP is the memory.swap.max: 0 for the Makefile's cgroup_run
# (--memory-swap equal to --memory), max for test-cgroup-limit (--memory
# only).
cg_step() {
  name=$1 lim=$2 swap=$3; shift 3
  cgn=$((cgn + 1)); d=$CGB/cb-floor-$cgn
  mkdir -p "$d" && echo "$lim" > "$d/memory.max" || { echo "floor: $name FAIL (cannot create $d)"; rc=1; return; }
  [ -f "$d/memory.swap.max" ] && echo "$swap" > "$d/memory.swap.max"
  step "$name" sh -c 'echo $$ > "$0/cgroup.procs" && exec "$@"' "$d" "$@"
  rmdir "$d" 2>/dev/null
}
M512=536870912 M256=268435456
if [ -n "$cg_ready" ]; then
  cg_step cgroup-limit $M512 max env CHALLOC_EXPECT_CGROUP_LIMIT=$M512 cargo test cgroup -- --nocapture --test-threads=1
  cg_step cgroup-churn-512-purge $M512 0 sh scripts/cgroup-tests.sh churn challoc purge 400 100
  cg_step cgroup-churn-512-keep $M512 0 sh scripts/cgroup-tests.sh churn challoc-off keep 400 100
  cg_step cgroup-churn-256-purge $M256 0 sh scripts/cgroup-tests.sh churn challoc purge 200 50
  cg_step cgroup-churn-256-keep $M256 0 sh scripts/cgroup-tests.sh churn challoc-off keep 200 50
  cg_step cgroup-lazy-lazy $M512 0 sh scripts/cgroup-tests.sh lazy challoc lazy 200
  cg_step cgroup-lazy-eager $M512 0 sh scripts/cgroup-tests.sh lazy challoc-eager eager 200
elif [ "${CB_CONTAINER:-}" = privileged ]; then
  # The privileged profile can always write its cgroup: failing to set it
  # up is a failure, not a skip.
  echo "floor: cgroup-setup FAIL (privileged, cannot enable +memory under ${CGB:-no cgroup2 mount})"
  rc=1
else
  echo "SKIP floor-cgroup the container cannot write its cgroup (${CGB:-no cgroup2 mount}); run the privileged profile"
fi

# ---- make fuzz ----------------------------------------------------------------
FUZZ_OPS=${FUZZ_OPS:-10000000} FUZZ_THREADS=${FUZZ_THREADS:-4} FUZZ_SEED=${FUZZ_SEED:-0xC4A110C} FUZZ_TABLE=${FUZZ_TABLE:-2048}
step fuzz-build sh scripts/ctest-cc.sh fuzz/fuzz.c -o "$CARGO_TARGET_DIR/fuzz"
step fuzz env LD_PRELOAD="$CARGO_TARGET_DIR/release/libchalloc.so" "$CARGO_TARGET_DIR/fuzz" "$FUZZ_OPS" "$FUZZ_THREADS" "$FUZZ_SEED" "$FUZZ_TABLE"
# The summary in full: every test result, SKIP, OUTCOME and fuzz line (a
# skipped test passes, so the SKIP lines are what a pass covers). Panics
# are a separate, capped block: the should_panic tests print ~70 of them on
# a passing run, and they must not push the SKIP lines out of a capped list.
# Each line is tagged with its step, in log order.
awk '/^floor-step: / { st = $2; next } /^test result:|^SKIP |^OUTCOME |^fuzz/ { print "[" st "] " $0 }' "$LOG" | uniq -c
echo "floor: $(grep -c '^SKIP ' "$LOG") SKIP lines, $(grep -c '^OUTCOME ' "$LOG") OUTCOME lines"
grep -E '^failures:|^    [a-z_:]+$|^---- .* stdout ----' "$LOG" | head -40
[ $rc = 0 ] || grep -E 'panicked at' "$LOG" | sort | uniq -c | sort -rn | head -40
[ $rc = 0 ] || tail -60 "$LOG"
echo "floor: result $([ $rc = 0 ] && echo pass || echo fail)"
exit $rc

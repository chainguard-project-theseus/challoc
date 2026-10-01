#!/bin/sh
# bench/cb/floor-test.sh: the `floor-test` row (bench/rows.toml): the
# in-container part of `make test` (release build, the jemalloc-compat
# stage, the unit tests, the api:: tests) on the challoc variant's own
# source tree, as cb extracted it for the build (target/cb/src/<tree>).
# The cgroup container steps of `make test` need a container runtime
# inside the container and are not run here.
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
grep -E '^test result:|^failures:|^    [a-z_:]+$|panicked at|^SKIP ' "$LOG" | sort | uniq -c | sort -rn | head -80
grep -E '^---- .* stdout ----' "$LOG" | head -20
[ $rc = 0 ] || tail -60 "$LOG"
echo "floor: result $([ $rc = 0 ] && echo pass || echo fail)"
exit $rc

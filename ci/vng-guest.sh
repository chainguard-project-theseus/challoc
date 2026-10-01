#!/bin/bash
# ci/vng-guest.sh: runs as root inside the virtme-ng guest that
# ci/vng-cell.sh boots, on the kernel under test, with the Wolfi test image
# as / and the checkout (its target/ already built) at /src. It does what
# `make test` does after its builds, each step on this kernel directly:
#
#   1. both unit-test groups (`--skip api::`, then `api::`: the C programs
#      of tests/c through src/api/ctests.rs);
#   2. the cgroup-reading tests in a child cgroup with memory.max=512M
#      (make test-cgroup-limit);
#   3. the churn and lazy programs of scripts/cgroup-tests.sh, each in a
#      fresh child cgroup with memory.max=L and memory.swap.max=0 (make
#      test-cgroup-churn, test-cgroup-lazy).
#
# Skips are allowed (CHALLOC_TEST_ALLOW_SKIP=1) and compared with the
# cell's manifest by ci/skip-summary.sh on the runner. The exit status goes
# to /src/.ci/vng-status, which the runner reads (a guest that never gets
# here leaves none).
set -uo pipefail
cd /src || exit 2
export CHALLOC_TEST_ALLOW_SKIP=1 HOME=/root PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
rc=0
step() { echo "=== $*"; "$@" || { echo "=== FAILED ($?): $*"; rc=1; }; }

echo "kernel $(uname -r) $(uname -m), $(nproc) CPUs, $(awk '/MemTotal/ { print $2 " kB" }' /proc/meminfo)"
cg=/sys/fs/cgroup
if ! grep -q ' /sys/fs/cgroup cgroup2 ' /proc/mounts; then
  mount -t cgroup2 none "$cg" || echo "cannot mount cgroup2"
fi
echo "+memory" >"$cg/cgroup.subtree_control" || echo "cannot enable the memory controller"

# in_cgroup NAME LIMIT CMD...: CMD in a fresh child cgroup (LIMIT: max or a
# byte count; swap off).
# shellcheck disable=SC2317,SC2329 # called through step
in_cgroup() {
  local name=$1 limit=$2; shift 2
  local dir=$cg/ci-$name
  [ -d "$dir" ] && rmdir "$dir" 2>/dev/null
  mkdir -p "$dir"
  echo "$limit" >"$dir/memory.max"
  [ ! -e "$dir/memory.swap.max" ] || echo 0 >"$dir/memory.swap.max"
  bash -c 'echo $$ >"$1/cgroup.procs" && shift && exec "$@"' _ "$dir" "$@"
}

bin=$(cat target/ci-testbin)
step in_cgroup unit max "$bin" --nocapture --skip api::
step in_cgroup api max "$bin" api:: --nocapture
step in_cgroup limit 536870912 env CHALLOC_EXPECT_CGROUP_LIMIT=536870912 "$bin" cgroup --nocapture --test-threads=1
m512=536870912; m256=268435456
step in_cgroup churn1 $m512 sh scripts/cgroup-tests.sh churn challoc purge 400 100
step in_cgroup churn2 $m512 sh scripts/cgroup-tests.sh churn challoc-off keep 400 100
step in_cgroup churn3 $m256 sh scripts/cgroup-tests.sh churn challoc purge 200 50
step in_cgroup churn4 $m256 sh scripts/cgroup-tests.sh churn challoc-off keep 200 50
step in_cgroup lazy1 $m512 sh scripts/cgroup-tests.sh lazy challoc lazy 200
step in_cgroup lazy2 $m512 sh scripts/cgroup-tests.sh lazy challoc-eager eager 200

echo "=== guest done: rc=$rc"
mkdir -p /src/.ci && echo "$rc" >/src/.ci/vng-status
sync
exit "$rc"

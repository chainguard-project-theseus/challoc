#!/bin/bash
# ci/cell.sh KIND: one test cell of the presubmit checks
# (.chainguard/ci/test.yaml), in the check guest with a Docker daemon.
#
#   linux        `make test` at QUANTUM (16 or 8, from the environment),
#                then the GLIBC <= 2.44 readelf assertion, the features
#                line and the built quantum
#   privileged   `make test` privileged (seccomp off, host cgroupns, rw
#                cgroupfs); the features line must show mseal=yes
#
# Either way the run's SKIP lines are compared with
# ci/expected-skips/<cell>.txt (ci/skip-summary.sh), even when an earlier
# step failed, and the cell fails if any step did. CI_CELL names the cell
# for the cell-scoped entries of ci/known-failures.txt.
set -uo pipefail
kind=${1:?usage: ci/cell.sh linux|privileged}
plan=$(bash ci/image-build.sh test) || exit 1
arch=$(sed -n 's/^arch=//p' <<<"$plan")
IMAGE=$(sed -n 's/^image=//p' <<<"$plan")
export IMAGE
log=.ci/test.log
status=0
case $kind in
  linux)
    : "${QUANTUM:?QUANTUM must be 16 or 8}"
    export CI_CELL=hosted-$arch-q$QUANTUM
    if bash ci/test-cell.sh "$log" QUANTUM="$QUANTUM"; then
      bash ci/glibc-max.sh .ci/target/release/libchalloc.so 2.44 || status=1
      bash ci/features-assert.sh "$log" || status=1
      # scripts/ctest-cc.sh caches the library's `arenas.quantum` here.
      got=$(cat .ci/target/release/ctest-quantum)
      echo "arenas.quantum $got"
      [ "$got" = "$QUANTUM" ] || { echo "error: built quantum $got, wanted $QUANTUM"; status=1; }
    else
      status=1
    fi
    ;;
  privileged)
    export CI_CELL=hosted-$arch-privileged
    flags='--privileged --security-opt seccomp=unconfined --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw'
    bash ci/test-cell.sh "$log" EXTRA_DOCKER_FLAGS="$flags" || status=1
    bash ci/features-assert.sh "$log" mseal=yes || status=1
    ;;
  *) echo "unknown cell kind $kind" >&2; exit 2 ;;
esac
bash ci/skip-summary.sh "$log" "ci/expected-skips/$CI_CELL.txt" || status=1
exit $status

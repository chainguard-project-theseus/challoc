#!/bin/bash
# ci/extended.sh TASK: one task of the nightly workflow (extended.yml), a
# Makefile target run through ci/make.sh with the log in .ci/TASK.log.
#
#   fuzz            make fuzz, 50M operations, a random seed (printed)
#   fuzz-hardened   the same under zero_on_free, slot_randomize and
#                   double_free_scan
#   fuzz-q8         the same with QUANTUM=8
#   oscillate       make test-cgroup-oscillate OSC_SECS=600 CONTROL=1
#   check-needed    make check-needed (DT_NEEDED names the allocator)
#   valkey | ruby   the consumer builds and their suites, STAGE=all
set -euo pipefail
task=${1:?usage: ci/extended.sh TASK}
log=.ci/$task.log
fuzz=(FUZZ_OPS=50000000 FUZZ_SEED=random)
case $task in
  fuzz)          bash ci/make.sh "$log" fuzz "${fuzz[@]}" ;;
  fuzz-hardened) bash ci/make.sh "$log" fuzz "${fuzz[@]}" \
                   CHALLOC_CONF=zero_on_free:true,slot_randomize:true,double_free_scan:true ;;
  fuzz-q8)       bash ci/make.sh "$log" fuzz "${fuzz[@]}" QUANTUM=8 ;;
  oscillate)     bash ci/make.sh "$log" test-cgroup-oscillate OSC_SECS=600 CONTROL=1 CGROUP_WATCHDOG=900 ;;
  check-needed)  bash ci/make.sh "$log" check-needed ;;
  valkey|ruby)   bash ci/make.sh "$log" "$task" STAGE=all ;;
  *) echo "unknown task $task" >&2; exit 2 ;;
esac

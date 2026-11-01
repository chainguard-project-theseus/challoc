#!/bin/bash
# ci/extended.sh [TASK]: one task of the on-request extended checks
# (.chainguard/ci/extended.yaml), a Makefile target run through ci/make.sh
# with the log in .ci/TASK.log. TASK defaults to $TASK. The dev image
# (Dockerfile.dev's `test` stage, `consumers` for the consumer builds) is
# built first unless IMAGE names one already.
#
#   fuzz            make fuzz, 50M operations, a random seed (printed)
#   fuzz-hardened   the same under zero_on_free, slot_randomize and
#                   double_free_scan
#   fuzz-q8         the same with QUANTUM=8
#   oscillate       make test-cgroup-oscillate OSC_SECS=600 CONTROL=1
#   check-needed    make check-needed (DT_NEEDED names the allocator)
#   valkey | ruby   the consumer builds and their suites, STAGE=all
set -euo pipefail
task=${1:-${TASK:?usage: ci/extended.sh TASK}}
log=.ci/$task.log
fuzz=(FUZZ_OPS=50000000 FUZZ_SEED=random)
if [ -z "${IMAGE:-}" ]; then
  stage="test"
  case $task in valkey|ruby) stage="consumers" ;; esac
  plan=$(bash ci/image-build.sh "$stage")
  IMAGE=$(sed -n 's/^image=//p' <<<"$plan")
  export IMAGE
fi
status=0
# The consumer scripts send each step's output to a log file; print the
# newest lines of the test logs every two minutes, so a run that hits the
# time limit shows where it was.
( while sleep 120; do
    for f in .ci/target/"$task"/logs/test*.log .ci/target/"$task"/logs/full*.log; do
      [ -f "$f" ] && echo "--- $(date -u +%H:%M:%S) $f: $(tail -n 2 "$f" | tr '\n' ' ' | cut -c1-300)"
    done
  done ) &
watcher=$!
trap 'kill "$watcher" 2>/dev/null || true' EXIT
case $task in
  fuzz)          bash ci/make.sh "$log" fuzz "${fuzz[@]}" || status=$? ;;
  fuzz-hardened) bash ci/make.sh "$log" fuzz "${fuzz[@]}" \
                   CHALLOC_CONF=zero_on_free:true,slot_randomize:true,double_free_scan:true || status=$? ;;
  fuzz-q8)       bash ci/make.sh "$log" fuzz "${fuzz[@]}" QUANTUM=8 || status=$? ;;
  oscillate)     bash ci/make.sh "$log" test-cgroup-oscillate OSC_SECS=600 CONTROL=1 CGROUP_WATCHDOG=900 || status=$? ;;
  check-needed)  bash ci/make.sh "$log" check-needed || status=$? ;;
  valkey)        bash ci/make.sh "$log" valkey STAGE=all || status=$? ;;
  # The guest's egress proxy re-signs TLS and its CA reaches containers in
  # the system bundle; Ruby's downloader pins rubygems.org to its own CAs,
  # so update-gems is told to trust the bundle too.
  ruby)          bash ci/make.sh "$log" ruby STAGE=all \
                   RUBY_GEMS_CA=/etc/ssl/certs/ca-certificates.crt || status=$? ;;
  *) echo "unknown task $task" >&2; exit 2 ;;
esac
# A failed fuzz run is reproduced from its seed.
if [ "$status" != 0 ] && [[ $task == fuzz* ]]; then
  echo "error: $task failed; seed: $(grep -a -i -m 3 'seed' "$log" | tr '\n' ' ')"
fi
# The consumer scripts keep each step's full output under target/<task>/logs,
# which goes with the guest; print the end of each one here.
if [ "$status" != 0 ] && [ -d ".ci/target/$task/logs" ]; then
  for f in ".ci/target/$task/logs"/*.log; do
    [ -f "$f" ] || continue
    echo "=== tail of $f"
    tail -n 80 "$f"
  done
fi
exit "$status"

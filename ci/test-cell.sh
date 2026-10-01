#!/bin/bash
# ci/test-cell.sh LOG [VAR=value...]: `make test` for one hosted cell
# through ci/make.sh, tolerating only the failures ci/known-failures.txt
# lists.
#
# `make test` stops at its first failing command, and its unit tests run
# before the cgroup steps. When the only failures are known ones, the
# cgroup steps (test-cgroup-limit, test-cgroup-churn, test-cgroup-lazy)
# run again with -k, so a known failure does not cost the cell the rest of
# its coverage; their output is appended to LOG and checked the same way.
# CI_CELL names the cell for the cell-scoped entries.
set -euo pipefail
log=$1; shift
if bash ci/make.sh "$log" test "$@"; then
  exit 0
fi
bash ci/known-failures.sh "$log"
# Both unit-test groups must have finished (the known failures are in the
# second, `api::`); a first group that failed never ran the second.
if [ "$(grep -ac '^test result: ' "$log")" -lt 2 ]; then
  echo "::error::make test stopped before its second unit-test group ran"
  exit 1
fi
echo "=== make test failed only on known failures; running its cgroup steps"
rest=.ci/test-rest.log
status=0
bash ci/make.sh "$rest" -k test-cgroup-limit test-cgroup-churn test-cgroup-lazy "$@" || status=$?
cat "$rest" >>"$log"
[ "$status" = 0 ] || bash ci/known-failures.sh "$rest"

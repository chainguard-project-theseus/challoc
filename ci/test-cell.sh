#!/bin/bash
# ci/test-cell.sh LOG [VAR=value...]: `make test` for one hosted cell
# through ci/make.sh, tolerating only the failures ci/known-failures.txt
# lists.
#
# `make test` stops at its first failing command, and its unit tests run
# before the cgroup steps. When the only failures are known ones, every
# step of `make test` after the unit tests (the `$(MAKE) ...` lines of its
# recipe, read from the Makefile, so a step added there is run here too)
# runs again, each on its own, so a known failure does not cost the cell
# the rest of its coverage; each step's output is appended to LOG and
# checked the same way, and a step that fails without a recognisable
# failure (a crash) fails the cell. CI_CELL names the cell for the
# cell-scoped entries.
set -euo pipefail
log=$1; shift
if bash ci/make.sh "$log" test "$@"; then
  exit 0
fi
bash ci/known-failures.sh "$log"
# Both unit-test groups must have finished (the known failures are in the
# second, `api::`); a first group that failed never ran the second.
if [ "$(grep -ac '^test result: ' "$log")" -lt 2 ]; then
  echo "error: make test stopped before its second unit-test group ran"
  exit 1
fi
# The recipe of `test`: its first line runs the unit tests, every other
# line must be a sub-make step.
# shellcheck disable=SC2016 # $(MAKE) and $(DOCKER_RUN) are Makefile text
{
  recipe=$(awk '/^test: image/ { on = 1; next } on && /^\t/ { print; next } on { exit }' Makefile)
  steps=$(sed -n 's/^\t\$(MAKE) --no-print-directory \([a-z0-9-]*\)$/\1/p' <<<"$recipe")
  first=$(head -n 1 <<<"$recipe")
  ncmd=$(awk '/^\t[^ \t]/' <<<"$recipe" | wc -l | tr -d ' ')
  nstep=$(grep -c . <<<"$steps" || true)
  unit_first=0
  [[ $first == *'$(DOCKER_RUN)'* ]] && unit_first=1
}
if [ "$nstep" = 0 ] || [ "$unit_first" != 1 ] || [ "$ncmd" != "$((nstep + 1))" ]; then
  echo "error: the recipe of make test is not the unit-test command followed by \$(MAKE) steps; ci/test-cell.sh needs updating"
  exit 1
fi
echo "=== make test failed only on known failures; running its remaining steps: $(tr '\n' ' ' <<<"$steps")"
status=0
for step in $steps; do
  slog=.ci/test-$step.log
  if bash ci/make.sh "$slog" "$step" "$@"; then
    cat "$slog" >>"$log"
    continue
  fi
  cat "$slog" >>"$log"
  bash ci/known-failures.sh "$slog" || status=1
done
exit $status

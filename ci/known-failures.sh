#!/bin/bash
# ci/known-failures.sh LOG: after a failed test run, pass (exit 0) only if
# every failure in LOG is listed in ci/known-failures.txt. A failure is a
# unit test (`test NAME ... FAILED`, listed by its full name) or a
# scripts/cgroup-tests.sh run whose OUTCOME verdict is not ok (listed as
# `cgroup-tests KIND ALLOC`, e.g. `cgroup-tests lazy challoc-eager`). No
# failure found at all (a build error, a crashed step) or an unlisted one
# fails. Each known failure is a warning annotation, so it stays visible
# on a green run. An entry ending in ` @GLOB` applies only to the cells
# whose name ($CI_CELL, e.g. hosted-amd64-privileged) matches GLOB.
set -euo pipefail
log=$1
list=$(dirname "$0")/known-failures.txt
cell=${CI_CELL:-}
# entries: the list without comments, the cell-scoped lines reduced to
# their name when CI_CELL matches and dropped when it does not.
entries() {
  local line
  sed -e 's/#.*//' -e 's/[[:space:]]*$//' "$list" | while read -r line; do
    [ -n "$line" ] || continue
    case $line in
      *" @"*)
        # shellcheck disable=SC2254 # the glob is the point
        case $cell in ${line##* @}) echo "${line% @*}" ;; esac ;;
      *) echo "$line" ;;
    esac
  done
}
known=$(entries)
failed=$(
  { grep -aoE '^test [^ ]+ \.\.\. FAILED' "$log" || true; } | awk '{ print $2 }'
  { grep -a '^OUTCOME ' "$log" || true; } | awk '
    { v = ""; a = "" }
    { for (i = 3; i <= NF; i++) { if ($i ~ /^verdict=/) v = substr($i, 9); if ($i ~ /^alloc=/) a = substr($i, 7) } }
    v != "ok" { print "cgroup-tests " $2 " " a }'
)
failed=$(sort -u <<<"$failed" | { grep -v '^$' || true; })
if [ -z "$failed" ]; then
  echo "::error::the run failed, but no unit test or cgroup program did (a build or other step failed; see the log)"
  exit 1
fi
rc=0
while read -r t; do
  if grep -qxF "$t" <<<"$known"; then
    echo "::warning::known failure (ci/known-failures.txt): $t"
  else
    echo "::error::failed: $t"
    rc=1
  fi
done <<<"$failed"
exit $rc

#!/bin/bash
# ci/skip-summary.sh LOG MANIFEST: compare the skips a test run printed with
# the cell's expected-skip manifest (ci/expected-skips/<cell>.txt).
#
# `make test` runs with CHALLOC_TEST_ALLOW_SKIP=1, so a kernel- or
# privilege-gated test that cannot run prints `SKIP <test> <reason>` and
# passes (docs/testing.md, "Skips"). This script keeps that from hiding
# lost coverage. Each skip is keyed by its first word: the test (`SKIP
# c_mseal ...`) or a program's claim name (`SKIP thp/auto ...`). Only the
# first word: parallel tests write to the same stream and another test's
# output can land inside a line after it (a lone `test` key, from a libtest
# line cut in two the same way, is dropped). Then
#
#   - a key not in the manifest fails (exit 1): coverage was lost;
#   - a manifest key that did not appear is only reported: coverage was
#     gained, drop the line from the manifest;
#   - a missing manifest fails and prints the one to commit.
#
# The table goes to $GITHUB_STEP_SUMMARY when set, else stdout. Manifest
# lines are keys; `#` starts a comment. CI_SKIP_UPDATE=1 writes the
# observed keys to MANIFEST and passes (bootstrapping a new cell).
set -euo pipefail
log=$1; manifest=$2
out=${GITHUB_STEP_SUMMARY:-/dev/stdout}
[ -f "$log" ] || { echo "::error::$log missing: the test step did not run"; exit 1; }

seen=$(mktemp); want=$(mktemp)
trap 'rm -f "$seen" "$want"' EXIT
{ grep -a '^SKIP ' "$log" || true; } | awk '$2 != "" && $2 != "test" { print $2 }' | sort -u >"$seen"
if [ -f "$manifest" ]; then
  sed -e 's/#.*//' -e 's/[[:space:]]*$//' "$manifest" | { grep -v '^$' || true; } | sort -u >"$want"
fi

new=$(comm -23 "$seen" "$want")
gone=$(comm -13 "$seen" "$want")
{
  echo "### Skips: $(basename "$manifest" .txt)"
  echo
  echo "$(wc -l <"$seen" | tr -d ' ') distinct skips."
  echo
  if [ -s "$seen" ] || [ -n "$gone" ]; then
    echo '| skip | status |'
    echo '|---|---|'
    while read -r k; do
      if grep -qxF "$k" "$want"; then s=expected; else s='**NEW**'; fi
      echo "| \`$k\` | $s |"
    done <"$seen"
    if [ -n "$gone" ]; then
      while read -r k; do echo "| \`$k\` | no longer skipped |"; done <<<"$gone"
    fi
  fi
} >>"$out"

if [ "${CI_SKIP_UPDATE:-}" = 1 ]; then
  mkdir -p "$(dirname "$manifest")"
  cp "$seen" "$manifest"
  echo "wrote $manifest"
  exit 0
fi
if [ ! -f "$manifest" ]; then
  echo "::error::no skip manifest $manifest; commit this one:"
  cat "$seen"
  exit 1
fi
if [ -n "$gone" ]; then
  while read -r k; do echo "::notice::no longer skipped, drop it from $manifest: $k"; done <<<"$gone"
fi
if [ -n "$new" ]; then
  while read -r k; do echo "::error::new skip (lost coverage) not in $manifest: $k"; done <<<"$new"
  echo "observed skip keys (the manifest if all are expected):"
  cat "$seen"
  exit 1
fi
echo "skips match $manifest"

#!/bin/bash
# ci/features-assert.sh LOG KEY=VALUE...: the `features:` line a test run
# printed (tests/c/features.c through the c_features row of `make test`:
# the challoc_features() line) must carry every KEY=VALUE. The privileged
# job asserts mseal=yes here, so a run that silently lost its privilege (a
# Makefile that drops EXTRA_DOCKER_FLAGS, a seccomp profile) fails instead
# of passing with skips.
set -euo pipefail
log=$1; shift
line=$({ grep -aoE 'features: page_size=[^[:cntrl:]]*' "$log" || true; } | head -n 1)
[ -n "$line" ] || { echo "::error::no 'features: page_size=...' line in $log"; exit 1; }
echo "$line"
# shellcheck disable=SC2016 # the backticks are markdown
[ -z "${GITHUB_STEP_SUMMARY:-}" ] || printf '### Features\n\n```\n%s\n```\n' "$line" >>"$GITHUB_STEP_SUMMARY"
rc=0
for kv in "$@"; do
  case " ${line#features: } " in
    *" $kv "*) echo "ok $kv" ;;
    *) echo "::error::the features line lacks $kv"; rc=1 ;;
  esac
done
exit $rc

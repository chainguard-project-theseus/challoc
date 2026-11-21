#!/bin/sh
# A stand-in for scripts/codegen-diff.sh, for ci/codegen-fncmp-test.sh's
# test of ci/codegen.sh: `capture` does nothing, and `compare` prints
# report.txt and exits with $STANDIN_RC.
set -eu
case $1 in
  capture) echo "stand-in: captured nothing" ;;
  compare) cat "$(dirname "$0")/report.txt"; exit "${STANDIN_RC:?}" ;;
  *) exit 1 ;;
esac

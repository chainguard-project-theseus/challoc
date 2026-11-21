#!/bin/sh
# ci/codegen.sh: scripts/codegen-diff.sh from the pull request's base to
# this tree (the codegen checks): capture the base revision's two quantum
# builds in a worktree, then compare this tree's with them. Prints the
# per-variant report (.text hash; exact, reloc-only, padding-only,
# erratum-stub and changed functions, with the full diff of each changed
# one) and exits 2 when a function changed. Each variant's verdict (the
# `functions old ..` line, or `.text` identical) and each changed
# function's `### changed:` line are printed again at the end as
# `::notice` lines, which Checks lists at the top of the GitHub check
# run's output text, so they can be read and quoted without the log.
# CODEGEN_DIFF names another script to run in place of
# scripts/codegen-diff.sh (ci/codegen-fncmp-test.sh runs a stand-in).
set -eu
base=${CHAINGUARD_BASE_REVISION:?no base revision to compare the machine code with}
diff=${CODEGEN_DIFF:-scripts/codegen-diff.sh}
git="git -c safe.directory=*"
dir=$(mktemp -d)
trap '$git worktree remove --force "$dir/base" >/dev/null 2>&1 || true; rm -rf "$dir"' EXIT
$git worktree add --detach "$dir/base" "$base" >/dev/null
(cd "$dir/base" && sh "$diff" capture "$dir/baseline")
# The report streams to the log as before; a copy feeds the notices, and
# compare's own exit status stays the check's.
{ rc=0; sh "$diff" compare "$dir/baseline" || rc=$?; echo "$rc" > "$dir/rc"; } |
  tee "$dir/report.txt"
sh "$(dirname "$0")/codegen-notices.sh" "$(uname -m)" "$dir/report.txt"
exit "$(cat "$dir/rc")"

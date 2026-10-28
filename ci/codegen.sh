#!/bin/sh
# ci/codegen.sh: scripts/codegen-diff.sh from the pull request's base to
# this tree (the codegen checks): capture the base revision's two quantum
# builds in a worktree, then compare this tree's with them. Prints the
# per-variant report (.text hash, exact, reloc-only and changed functions)
# and exits 2 when a function changed.
set -eu
base=${CHAINGUARD_BASE_REVISION:?no base revision to compare the machine code with}
git="git -c safe.directory=*"
dir=$(mktemp -d)
trap '$git worktree remove --force "$dir/base" >/dev/null 2>&1 || true; rm -rf "$dir"' EXIT
$git worktree add --detach "$dir/base" "$base" >/dev/null
(cd "$dir/base" && sh scripts/codegen-diff.sh capture "$dir/baseline")
sh scripts/codegen-diff.sh compare "$dir/baseline"

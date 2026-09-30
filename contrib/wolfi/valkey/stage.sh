#!/bin/sh
# stage.sh OUT: assemble the melange `--source-dir` for a local build of
# contrib/wolfi/valkey/valkey-9.1.yaml with `--build-option local-challoc`:
# the two patches the pipeline applies and a copy of this challoc tree
# under challoc/ (what the Wolfi copy of the definition gets from its
# challoc git-checkout). Run from anywhere; the tree is the one this
# script lives in. `target/`, `.git` and `.tmp-*` stay out.
#
#   sh contrib/wolfi/valkey/stage.sh /tmp/stage
#   melange build contrib/wolfi/valkey/valkey-9.1.yaml --source-dir /tmp/stage \
#     --build-option local-challoc ...
set -eu
OUT=${1:?usage: stage.sh <out-dir>}
cd "$(dirname "$0")/../../.."
# The package carries a copy of contrib/valkey's patch (Wolfi keeps patches
# next to the YAML); the two must not drift.
cmp contrib/valkey/0001-Add-MALLOC-challoc.patch \
    contrib/wolfi/valkey/valkey-9.1/0001-Add-MALLOC-challoc.patch
rm -rf "$OUT"
mkdir -p "$OUT/challoc"
cp contrib/wolfi/valkey/valkey-9.1/*.patch "$OUT/"
tar --exclude=./target --exclude=./.git --exclude='./.tmp-*' \
    -cf - . | tar -C "$OUT/challoc" -xf -
echo "staged $(find "$OUT" -type f | wc -l) files in $OUT ($(du -sh "$OUT" | cut -f1))"
ls "$OUT"

#!/bin/sh
# geometry-variant.sh NAME LG_SLAB [LO HI]: make the A/B source copy
# `.tmp-NAME` of this tree whose classes with LO < size <= HI (default
# 1024 < size <= 4096, the 1280..4096-byte classes that today share 64 KiB
# slabs) get 2^LG_SLAB-byte slabs, every other class unchanged. The copy
# holds `src Cargo.toml Cargo.lock build.rs scripts contrib`, so
# bench/lib.sh's `dir:.tmp-NAME` variant (quick OLD=/NEW=, ab VARIANTS=),
# bench/ruby-soak.sh's CHALLOC_SRC and a quantum8 build for
# bench/valkey-soak.sh's CHALLOC_LIB can all be made from it. `.tmp-*` is
# gitignored and excluded from `challoc-bench sync`, so run this where the
# variant is built (on the VM: `challoc-bench run <vm> 'sh
# bench/geometry-variant.sh slab16 14'`). Nothing under src/ changes.
#
# The edit is one early return in `layout::lg_slab_for` (src/layout/mod.rs),
# in front of the medium rule `max(64 KiB, 16 * size)`; the descriptor
# stride stays the 16-byte header for every slab of 16 KiB or more with at
# most 64 slots (`slab::strides_match_every_page` holds), and only the
# geometry unit test's medium-tier expectations would object, which a
# variant never runs. The exact per-class table the variant produces is the
# `slab_size` / `nregs` columns of `malloc_stats_print` under the variant.
#
# Measured in bench/results/2026-09-21-*-policy-cost-geometry.md.
set -eu
NAME=${1:?variant name, e.g. slab16}
LG=${2:?log2 of the slab size, e.g. 14 for 16 KiB}
LO=${3:-1024}
HI=${4:-4096}
cd "$(dirname "$0")/.."
case $NAME in *[!A-Za-z0-9_-]*) echo "geometry-variant: NAME must be [A-Za-z0-9_-]" >&2; exit 2;; esac
[ "$LG" -ge 12 ] && [ "$LG" -le 22 ] || { echo "geometry-variant: LG_SLAB must be 12..22" >&2; exit 2; }
DST=.tmp-$NAME
rm -rf "$DST"; mkdir -p "$DST"
cp -R src Cargo.toml Cargo.lock build.rs scripts contrib "$DST"/
F=$DST/src/layout/mod.rs
anchor='    let want = (16 * size).next_power_of_two();'
grep -qF "$anchor" "$F" || { echo "geometry-variant: anchor line not found in $F (lg_slab_for changed?)" >&2; exit 1; }
# One inserted early return, right before the medium rule.
awk -v a="$anchor" -v lo="$LO" -v hi="$HI" -v lg="$LG" '
  $0 == a && !done { printf "    // A/B variant (bench/geometry-variant.sh): 2^%s-byte slabs for %s < size <= %s.\n    if size > %s && size <= %s {\n        return %s;\n    }\n", lg, lo, hi, lo, hi, lg; done = 1 }
  { print }' "$F" > "$F.new" && mv "$F.new" "$F"
grep -q "return $LG;" "$F" || { echo "geometry-variant: the edit did not land" >&2; exit 1; }
# Fresh mtimes so cargo never mistakes a previous build for this one.
find "$DST" -type f -exec touch {} +
echo "geometry-variant: $DST = this tree with 2^$LG-byte slabs for $LO < size <= $HI"
diff -u src/layout/mod.rs "$F" || true

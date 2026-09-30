#!/bin/sh
# Build libchalloc with the `jemalloc-soname` feature and stage it under
# jemalloc's name for link-only consumers (design 13.2).
# Runs inside the dev container (`make jemalloc-compat`; `make test` runs it
# after the release build so the tests in src/api/ctests.rs can check it).
#
# Output, under $OUT (default target/jemalloc-compat/stage):
#   lib/libjemalloc.so.2       the library; DT_SONAME libjemalloc.so.2
#   lib/libjemalloc.so         link name, so `-ljemalloc` finds it
#   lib/pkgconfig/jemalloc.pc  pkg-config file; `Libs: -L${libdir} -ljemalloc`
#   include/jemalloc/jemalloc.h
#                              contrib/jemalloc-compat/include/jemalloc/jemalloc.h:
#                              jemalloc 5.3's unprefixed API as challoc exports
#                              it, for a consumer that includes
#                              <jemalloc/jemalloc.h> (Ruby's
#                              --with-jemalloc). The pc file's Cflags point
#                              at it. The Wolfi package puts it in
#                              challoc-jemalloc-compat-dev.
#
# Environment:
#   QUANTUM     16 (default) or 8; picks the quantum8/quantum16 feature
#   PREFIX      prefix written into jemalloc.pc; default $OUT, so the pc
#               file points at the staged library. A package build passes
#               PREFIX=/usr and installs lib/* under it.
#   OUT         staging directory; default $TARGET_DIR/stage
#   TARGET_DIR  cargo target dir; default target/jemalloc-compat (or
#               $CARGO_TARGET_DIR/jemalloc-compat), kept apart from
#               target/release so the feature set of `make test` artifacts is
#               never flipped by this build.
set -eu
cd "$(dirname "$0")/.."

QUANTUM=${QUANTUM:-16}
case "$QUANTUM" in
    8|16) ;;
    *) echo "jemalloc-compat: QUANTUM must be 8 or 16, got '$QUANTUM'" >&2; exit 2 ;;
esac
TARGET_DIR=${TARGET_DIR:-${CARGO_TARGET_DIR:-target}/jemalloc-compat}
OUT=${OUT:-$TARGET_DIR/stage}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
PREFIX=${PREFIX:-$OUT}

# The version jemalloc consumers compare against (`pkg-config
# --atleast-version`) is the one `mallctl("version")` reports, minus the
# `-<nrev>-g<sha>` tail that pkg-config would not parse.
VERSION=$(sed -n 's/^pub const VERSION_STR: &str = "\([0-9][0-9.]*\)-.*/\1/p' src/api/mallctl.rs)
[ -n "$VERSION" ] || { echo "jemalloc-compat: cannot read VERSION_STR from src/api/mallctl.rs" >&2; exit 1; }

cargo build --release --target-dir "$TARGET_DIR" \
    --no-default-features --features "quantum$QUANTUM,jemalloc-soname"

so="$TARGET_DIR/release/libchalloc.so"
readelf -d "$so" | grep -q 'Library soname: \[libjemalloc.so.2\]' \
    || { echo "jemalloc-compat: $so has no libjemalloc.so.2 soname" >&2; exit 1; }

rm -rf "$OUT/lib" "$OUT/include"
mkdir -p "$OUT/lib/pkgconfig" "$OUT/include/jemalloc"
cp contrib/jemalloc-compat/include/jemalloc/jemalloc.h "$OUT/include/jemalloc/jemalloc.h"
cp "$so" "$OUT/lib/libjemalloc.so.2"
ln -s libjemalloc.so.2 "$OUT/lib/libjemalloc.so"
cat > "$OUT/lib/pkgconfig/jemalloc.pc" <<PC
prefix=$PREFIX
exec_prefix=\${prefix}
libdir=\${exec_prefix}/lib
includedir=\${prefix}/include
install_suffix=

Name: jemalloc
Description: challoc, a jemalloc-compatible malloc replacement, under jemalloc's soname (libjemalloc.so.2)
URL: https://github.com/chainguard-dev/challoc
Version: $VERSION
Cflags: -I\${includedir}/jemalloc\${install_suffix}
Libs: -L\${libdir} -ljemalloc\${install_suffix}
PC

echo "jemalloc-compat: quantum$QUANTUM, version $VERSION, prefix $PREFIX"
ls -l "$OUT/lib" "$OUT/lib/pkgconfig" "$OUT/include/jemalloc"

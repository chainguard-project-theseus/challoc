#!/bin/sh
# Build libchalloc with the `jemalloc-soname` feature and stage it under
# jemalloc's name for link-only consumers (design 13.2). The file set is
# scripts/install.sh's jemalloc-compat flavour.
# Runs inside the dev container (`make jemalloc-compat`; `make test` runs it
# after the release build so the tests in src/api/ctests.rs can check it).
#
# Output, under $OUT (default target/jemalloc-compat/stage):
#   lib/libjemalloc.so.2       the library; DT_SONAME libjemalloc.so.2
#   lib/libjemalloc.so         link name, so `-ljemalloc` finds it
#   lib/pkgconfig/jemalloc.pc  pkg-config file; `Libs: -L${libdir} -ljemalloc`
#   include/jemalloc/jemalloc.h
#                              include/jemalloc-compat/jemalloc/jemalloc.h:
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

cargo build --release --target-dir "$TARGET_DIR" \
    --no-default-features --features "quantum$QUANTUM,jemalloc-soname"

# scripts/install.sh writes the file set; the stage keeps lib/ and include/
# directly under $OUT whatever PREFIX the pc file names, so install under a
# scratch DESTDIR and move the two directories up.
scratch=$OUT/.install
rm -rf "$scratch" "$OUT/lib" "$OUT/include"
DESTDIR=$scratch PREFIX=$PREFIX LIBDIR=$PREFIX/lib INCLUDEDIR=$PREFIX/include \
    LIB=$TARGET_DIR/release/libchalloc.so sh scripts/install.sh jemalloc-compat
mv "$scratch$PREFIX/lib" "$OUT/lib"
mv "$scratch$PREFIX/include" "$OUT/include"
rm -rf "$scratch"

echo "jemalloc-compat: quantum$QUANTUM, prefix $PREFIX, staged under $OUT"

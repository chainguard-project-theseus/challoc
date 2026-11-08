#!/bin/sh
# Build Valkey against challoc and run its test suite.
# Runs inside the dev container: `make valkey` (see the Makefile for the
# variables it forwards) or `make run CMD='sh tests/consumers/valkey-build.sh'`.
#
# What it does:
#   1. builds libchalloc.so with the quantum8 feature into $CHALLOC_TARGET
#      (allocator_defrag.c asserts arenas.quantum == 8, the lg_quantum=3 of
#      Valkey's bundled jemalloc);
#   2. clones the Valkey tag Wolfi's valkey-9.1.yaml ships into
#      target/valkey/src-<tag>, checks the commit, and applies the
#      contrib/valkey/0*.patch series in order (idempotent: a stamp of the
#      series sits beside the tree);
#   3. `make MALLOC=challoc BUILD_TLS=yes` with include/ on the
#      include path and the challoc build directory as -L and rpath, so the
#      binaries run from the tree without LD_LIBRARY_PATH;
#   4. checks that valkey-server reports malloc=jemalloc-<v>-challoc and is
#      dynamically linked against libchalloc.so.0 (the soname, design 13.6;
#      cargo writes the file as libchalloc.so, so the link is made here);
#   5. runs the test suite (tests/test_helper.tcl, as ./runtest does, from
#      any tclsh 8.5 or newer): `unit/memefficiency` alone (the defrag
#      tests), then the full suite.
#
# The steps shared with redis-build.sh are in tests/consumers/lib.sh.
#
# Environment:
#   STAGE          build | memefficiency | full | all (default all)
#   VALKEY_TAG     tag to build (default 9.1.2); VALKEY_COMMIT the commit
#                  the tag must resolve to (Wolfi's expected-commit)
#   TEST_FLAGS     extra test_helper.tcl flags (e.g. "--clients 8 --verbose");
#                  --dump-logs is always passed
#   CHALLOC_TARGET cargo target dir for the quantum8 build
#                  (default target/valkey/challoc)
#   JOBS           make -j (default nproc)
set -eu
PROG=valkey-build.sh
. "$(dirname "$0")/lib.sh"
cd "$(dirname "$0")/../.."
ROOT=$(pwd)

check_stage
VALKEY_TAG=${VALKEY_TAG:-9.1.2}
VALKEY_COMMIT=${VALKEY_COMMIT:-7f1dffedff6de73058b2c2a389422b6ecd56c8fb}
JOBS=${JOBS:-$(nproc)}
WORK=$ROOT/target/valkey
SRC=$WORK/src-$VALKEY_TAG
LOGS=$WORK/logs
CHALLOC_TARGET=${CHALLOC_TARGET:-$WORK/challoc}
INCLUDE=$ROOT/include
ARCH=$(uname -m)
mkdir -p "$WORK" "$LOGS"

# 1. challoc, quantum8.
step "challoc (quantum8) -> $CHALLOC_TARGET"
cargo build --release --target-dir "$CHALLOC_TARGET" --no-default-features --features quantum8
LIB=$CHALLOC_TARGET/release
[ -f "$LIB/libchalloc.so" ] || fail "$LIB/libchalloc.so missing"
readelf -d "$LIB/libchalloc.so" | grep -q 'Library soname: \[libchalloc.so.0\]' \
    || fail "$LIB/libchalloc.so has no libchalloc.so.0 soname"
# `-lchalloc` resolves the file; the loader then wants the soname next to it.
ln -sf libchalloc.so "$LIB/libchalloc.so.0"

# 2. Valkey source at the pinned tag, patched.
step "valkey $VALKEY_TAG -> $SRC"
checkout_patched "$SRC" https://github.com/valkey-io/valkey "$VALKEY_TAG" "$VALKEY_COMMIT" "$ROOT"/contrib/valkey/0*.patch

# 3. Build. CFLAGS/LDFLAGS are what Valkey's Makefile already honours; the
# patch itself only adds -DUSE_JEMALLOC -DUSE_CHALLOC and -lchalloc.
step "make MALLOC=challoc BUILD_TLS=yes -j$JOBS"
# Valkey's Makefile does not list libchalloc.so among valkey-server's
# prerequisites, so a cached tree keeps a binary linked against an older
# library (and its DT_NEEDED). Drop the linked binaries; the objects stay.
rm -f src/valkey-server src/valkey-cli src/valkey-benchmark
make -j"$JOBS" MALLOC=challoc BUILD_TLS=yes \
    CFLAGS="-I$INCLUDE" LDFLAGS="-L$LIB -Wl,-rpath,$LIB" \
    all > "$LOGS/build-$ARCH.log" 2>&1 || { tail -40 "$LOGS/build-$ARCH.log"; exit 1; }
tail -3 "$LOGS/build-$ARCH.log"

# 4. Sanity: the binary says challoc and loads it from our build directory.
step "check"
check_server ./src/valkey-server "$LIB"
[ "$STAGE" = build ] && exit 0

# 5. Tests.
find_tclsh
# Every memefficiency test runs at Valkey's stock targets: slab pages
# (wolfi-dev/challoc#604, #660) release the free pages of sparse slabs, so
# "Active Defrag eval scripts: standalone", skipped here before, passes.
run_stages

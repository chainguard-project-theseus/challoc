#!/bin/sh
# Build Redis against challoc and run its test suite.
# Runs inside the dev container: `make redis` (see the Makefile for the
# variables it forwards) or `make run CMD='sh tests/consumers/redis-build.sh'`.
#
# What it does:
#   1. builds libchalloc.so at QUANTUM (default 8, the lg_quantum=3
#      size-class geometry of Redis's bundled jemalloc; QUANTUM=16, the
#      system challoc's, works too, but one test, unit/type/string's
#      "Check MEMORY USAGE for embedded key strings with jemalloc", asserts
#      jemalloc's 40-byte class, which quantum16 does not have, and is
#      skipped there) and stages it with scripts/install.sh, the way challoc-dev
#      installs it: lib/libchalloc.so.0, include/challoc/*.h and
#      lib/pkgconfig/challoc.pc under target/redis/challoc-q<QUANTUM>;
#   2. clones Redis at REDIS_TAG into target/redis/src-<tag>, checks the
#      commit, and applies the contrib/redis/0*.patch series in order
#      (idempotent: a stamp of the series sits beside the tree);
#   3. `make MALLOC=challoc BUILD_TLS=yes` with PKG_CONFIG_PATH at the stage,
#      so the build takes its include path and link flags from challoc.pc as
#      it would from challoc-dev, and the stage's lib/ as rpath, so the
#      binaries run from the tree without LD_LIBRARY_PATH;
#   4. checks that redis-server reports malloc=jemalloc-5.3.0-challoc, names
#      libchalloc.so.0 in DT_NEEDED (the soname, design 13.6) ahead of
#      libc.so.6, so challoc serves malloc for the whole process, and calls
#      challoc's usable-size entry points (patch 0002) rather than falling
#      back to zmalloc_size() lookups;
#   5. runs the test suite (tests/test_helper.tcl, as ./runtest does, from
#      any tclsh 8.5 or newer): `unit/memefficiency` alone (the active
#      defrag tests), then the full suite, every active-defrag test at its
#      stock targets (slab pages, wolfi-dev/challoc#604, removed the slab
#      floor that used to fail eight of them).
#
# The steps shared with valkey-build.sh are in tests/consumers/lib.sh.
#
# Environment:
#   STAGE          build | memefficiency | full | all (default all)
#   REDIS_TAG      tag to build (default 8.10.3); REDIS_COMMIT the commit
#                  the tag must resolve to
#   TEST_FLAGS     extra test_helper.tcl flags (e.g. "--clients 8 --verbose");
#                  --dump-logs is always passed
#   QUANTUM        challoc's size-class quantum, 8 or 16 (default 8)
#   CHALLOC_TARGET cargo target dir for that build
#                  (default target/redis/challoc-q<QUANTUM>/build)
#   JOBS           make -j (default nproc)
set -eu
PROG=redis-build.sh
. "$(dirname "$0")/lib.sh"
cd "$(dirname "$0")/../.."
ROOT=$(pwd)

check_stage
REDIS_TAG=${REDIS_TAG:-8.10.3}
REDIS_COMMIT=${REDIS_COMMIT:-1a2574ce1551d3de6e0b4a56756c0fdbc6659e4f}
JOBS=${JOBS:-$(nproc)}
WORK=$ROOT/target/redis
SRC=$WORK/src-$REDIS_TAG
LOGS=$WORK/logs
QUANTUM=${QUANTUM:-8}
case "$QUANTUM" in
    8|16) ;;
    *) echo "redis-build.sh: QUANTUM must be 8 or 16, got '$QUANTUM'" >&2; exit 2 ;;
esac
STAGED=$WORK/challoc-q$QUANTUM
CHALLOC_TARGET=${CHALLOC_TARGET:-$STAGED/build}
ARCH=$(uname -m)
mkdir -p "$WORK" "$LOGS"

# 1. challoc at QUANTUM, staged as challoc-dev installs it.
step "challoc (quantum$QUANTUM) -> $STAGED"
cargo build --release --target-dir "$CHALLOC_TARGET" --no-default-features --features "quantum$QUANTUM"
LIB=$STAGED/lib
# install.sh reads the built library from its own LIB.
env LIB="$CHALLOC_TARGET/release/libchalloc.so" PREFIX="$STAGED" LIBDIR="$STAGED/lib" \
    INCLUDEDIR="$STAGED/include" sh scripts/install.sh challoc > "$LOGS/challoc-install-$ARCH.log" 2>&1 \
    || { cat "$LOGS/challoc-install-$ARCH.log"; exit 1; }
[ -f "$LIB/pkgconfig/challoc.pc" ] || fail "$LIB/pkgconfig/challoc.pc missing"

# 2. Redis source at the pinned tag, patched.
step "redis $REDIS_TAG -> $SRC"
checkout_patched "$SRC" https://github.com/redis/redis "$REDIS_TAG" "$REDIS_COMMIT" "$ROOT"/contrib/redis/0*.patch

# 3. Build. The include path and -lchalloc come from challoc.pc (patch
# 0001's Makefile); LDFLAGS adds only the rpath to the stage.
step "make MALLOC=challoc BUILD_TLS=yes -j$JOBS"
# Redis's Makefile does not list libchalloc.so among redis-server's
# prerequisites, so a cached tree keeps a binary linked against an older
# library (and its DT_NEEDED). Drop the linked binaries; the objects stay.
rm -f src/redis-server src/redis-cli src/redis-benchmark
make -j"$JOBS" MALLOC=challoc BUILD_TLS=yes \
    PKG_CONFIG_PATH="$LIB/pkgconfig" LDFLAGS="-Wl,-rpath,$LIB" \
    all > "$LOGS/build-$ARCH.log" 2>&1 || { tail -40 "$LOGS/build-$ARCH.log"; exit 1; }
tail -3 "$LOGS/build-$ARCH.log"

# 4. Sanity: the binary says challoc and loads it from our build directory.
step "check"
check_server ./src/redis-server "$LIB"
# libchalloc.so.0 ahead of libc.so.6: the loader searches in DT_NEEDED order,
# so challoc's malloc is the one every library in the process binds to.
needed=$(echo "$dyn" | awk '/NEEDED/ { gsub(/[][]/, "", $NF); print $NF }')
first=$(printf '%s\n' "$needed" | grep -n -E '^(libchalloc\.so\.0|libc\.so\.6)$' | head -1 | cut -d: -f2)
[ "$first" = libchalloc.so.0 ] \
    || fail "redis-server lists libc.so.6 before libchalloc.so.0 in DT_NEEDED: $(printf '%s\n' "$needed" | paste -sd' ' -)"
# Patch 0002: the usable-size entry points, not a zmalloc_size() per call.
undef=$(nm -D --undefined-only ./src/redis-server | awk '{ print $NF }')
for f in challoc_malloc_usable challoc_calloc_usable challoc_realloc_usable challoc_free_usable; do
    printf '%s\n' "$undef" | grep -qx "$f" \
        || fail "redis-server does not call $f (HAVE_ALLOC_WITH_USIZE off?)"
done
echo "DT_NEEDED: $(printf '%s\n' "$needed" | paste -sd' ' -); usable-size entry points: linked"
[ "$STAGE" = build ] && exit 0

# 5. Tests.
find_tclsh
# The tests that assert jemalloc's lg_quantum=3 size classes, skipped at
# QUANTUM=16: unit/type/string's MEMORY USAGE / DEBUG SDSLEN test expects
# 33- and 40-byte kvobjs in jemalloc's 40-byte class, which quantum16 rounds
# to 48.
set --
[ "$QUANTUM" = 16 ] && set -- --skiptest "Check MEMORY USAGE for embedded key strings with jemalloc"
run_stages "$@"

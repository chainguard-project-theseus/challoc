#!/bin/sh
# Build Valkey against challoc and run its test suite (plan Phase 1 task 7).
# Runs inside the dev container: `make valkey` (see the Makefile for the
# variables it forwards) or `make run CMD='sh contrib/valkey/build.sh'`.
#
# What it does:
#   1. builds libchalloc.so with the quantum8 feature into $CHALLOC_TARGET
#      (allocator_defrag.c asserts arenas.quantum == 8, the lg_quantum=3 of
#      Valkey's bundled jemalloc);
#   2. clones the Valkey tag Wolfi's valkey-9.1.yaml ships into
#      target/valkey/src-<tag>, checks the commit, and applies
#      0001-Add-MALLOC-challoc.patch (idempotent);
#   3. `make MALLOC=challoc BUILD_TLS=yes` with contrib/valkey/include on the
#      include path and the challoc build directory as -L and rpath, so the
#      binaries run from the tree without LD_LIBRARY_PATH;
#   4. checks that valkey-server reports malloc=jemalloc-<v>-challoc and is
#      dynamically linked against libchalloc.so.0 (the soname, design 13.6;
#      cargo writes the file as libchalloc.so, so the link is made here);
#   5. runs the test suite through ./runtest: `unit/memefficiency` alone (the
#      defrag tests), then the full suite. runtest wants a tclsh8.5, tclsh8.6
#      or tclsh9.0 binary; Wolfi's tcl provides tclsh9.0.
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
cd "$(dirname "$0")/../.."
ROOT=$(pwd)

STAGE=${STAGE:-all}
case "$STAGE" in
    build|memefficiency|full|all) ;;
    *) echo "valkey/build.sh: STAGE must be build, memefficiency, full or all, got '$STAGE'" >&2; exit 2 ;;
esac
VALKEY_TAG=${VALKEY_TAG:-9.1.2}
VALKEY_COMMIT=${VALKEY_COMMIT:-7f1dffedff6de73058b2c2a389422b6ecd56c8fb}
JOBS=${JOBS:-$(nproc)}
WORK=$ROOT/target/valkey
SRC=$WORK/src-$VALKEY_TAG
LOGS=$WORK/logs
CHALLOC_TARGET=${CHALLOC_TARGET:-$WORK/challoc}
PATCH=$ROOT/contrib/valkey/0001-Add-MALLOC-challoc.patch
INCLUDE=$ROOT/contrib/valkey/include
ARCH=$(uname -m)
mkdir -p "$WORK" "$LOGS"

step() { printf '\n== %s\n' "$*"; }

# 1. challoc, quantum8.
step "challoc (quantum8) -> $CHALLOC_TARGET"
cargo build --release --target-dir "$CHALLOC_TARGET" --no-default-features --features quantum8
LIB=$CHALLOC_TARGET/release
[ -f "$LIB/libchalloc.so" ] || { echo "valkey/build.sh: $LIB/libchalloc.so missing" >&2; exit 1; }
readelf -d "$LIB/libchalloc.so" | grep -q 'Library soname: \[libchalloc.so.0\]' \
    || { echo "valkey/build.sh: $LIB/libchalloc.so has no libchalloc.so.0 soname" >&2; exit 1; }
# `-lchalloc` resolves the file; the loader then wants the soname next to it.
ln -sf libchalloc.so "$LIB/libchalloc.so.0"

# 2. Valkey source at the pinned tag, patched.
step "valkey $VALKEY_TAG -> $SRC"
if [ ! -d "$SRC/.git" ]; then
    rm -rf "$SRC"
    git clone -q --depth 1 --branch "$VALKEY_TAG" https://github.com/valkey-io/valkey "$SRC"
fi
cd "$SRC"
head=$(git rev-parse HEAD)
if [ "$head" != "$VALKEY_COMMIT" ]; then
    echo "valkey/build.sh: tag $VALKEY_TAG is $head, expected $VALKEY_COMMIT" >&2
    exit 1
fi
if git apply --reverse --check "$PATCH" 2>/dev/null; then
    echo "patch already applied"
else
    # A clone that carries an older version of the patch fails both checks:
    # drop its working-tree changes and apply the current patch.
    if ! git apply --check "$PATCH" 2>/dev/null; then
        echo "stale patch in the tree, resetting it"
        git checkout -q -- .
    fi
    git apply --check "$PATCH"
    git apply "$PATCH"
    echo "patch applied"
fi
git diff --stat | tail -1

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
version=$(./src/valkey-server --version)
echo "$version"
case "$version" in
    *"malloc=jemalloc-5.3.0-challoc"*) ;;
    *) echo "valkey/build.sh: valkey-server does not report malloc=jemalloc-5.3.0-challoc" >&2; exit 1 ;;
esac
# No ldd in the image; read the dynamic section instead. The DT_NEEDED
# check is scripts/check-needed.sh, the same one a package test runs
# (Phase 3 prerequisite (e): --as-needed drops the entry from a binary
# that references no symbol of the library).
sh "$ROOT/scripts/check-needed.sh" ./src/valkey-server libchalloc.so.0 \
    || { echo "valkey/build.sh: valkey-server has no DT_NEEDED libchalloc.so.0" >&2; exit 1; }
dyn=$(readelf -d ./src/valkey-server)
echo "$dyn" | grep -E 'RUNPATH|RPATH' | grep -q "$LIB" \
    || { echo "valkey/build.sh: valkey-server has no rpath to $LIB" >&2; exit 1; }
[ -d deps/jemalloc/lib ] && { echo "valkey/build.sh: deps/jemalloc was built" >&2; exit 1; }
[ "$STAGE" = build ] && exit 0

# 5. Tests.
TCLSH=$(command -v tclsh8.6 || command -v tclsh9.0 || command -v tclsh8.5) \
    || { echo "valkey/build.sh: no tclsh8.5/8.6/9.0 for ./runtest" >&2; exit 1; }
echo "tclsh: $TCLSH ($(echo 'puts [info patchlevel]' | "$TCLSH"))"
run_tests() { # name, flags...
    name=$1; shift
    step "./runtest $* (log: $LOGS/$name-$ARCH.log)"
    start=$(date +%s)
    if ./runtest --dump-logs ${TEST_FLAGS:-} "$@" 2>&1 | tee "$LOGS/$name-$ARCH.log" \
        | grep -E '^\[(ok|err|exception|skip|ignore)\]|All tests passed|tests failed|\[[0-9]+/[0-9]+ done\]|Killed by signal|Timed out' ; then :; fi
    end=$(date +%s)
    if grep -q 'All tests passed without errors' "$LOGS/$name-$ARCH.log"; then
        echo "== $name: PASS ($((end - start)) s)"
    else
        echo "== $name: FAIL ($((end - start)) s); see $LOGS/$name-$ARCH.log" >&2
        grep -E '^\[err\]|^\[exception\]|Killed by signal' "$LOGS/$name-$ARCH.log" | head -40 >&2
        return 1
    fi
}
rc=0
if [ "$STAGE" = memefficiency ] || [ "$STAGE" = all ]; then
    run_tests memefficiency --single unit/memefficiency || rc=1
fi
if [ "$STAGE" = full ] || [ "$STAGE" = all ]; then
    run_tests full || rc=1
fi
exit $rc

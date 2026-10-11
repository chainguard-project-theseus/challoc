#!/bin/sh
# Build Ruby 3.4 `--with-jemalloc` against challoc's libjemalloc.so.2 stage
# and run `make test-all`. Runs inside the dev container: `make ruby` (see
# the Makefile for the variables it forwards) or
# `make run CMD='sh tests/consumers/ruby-build.sh'`.
#
# What it does:
#   1. stages libjemalloc.so.2 (scripts/jemalloc-compat.sh, quantum16: Ruby
#      assumes nothing about the quantum) under target/ruby/jemalloc-compat;
#   2. clones the Ruby tag Wolfi's ruby-3.4.yaml ships into
#      target/ruby/src-<tag>, checks the commit, applies a cherry-pick when
#      RUBY_CHERRY names one (fetched by hash, idempotent), and, as Wolfi
#      does for OpenSSL 4.0, replaces ext/openssl with the ruby/openssl
#      release Wolfi pins (cloned into target/ruby/openssl, the path
#      tool/sync_default_gems.rb reads, commit-checked) carrying Wolfi's
#      patch (contrib/ruby/);
#   3. autogen, then configure in an out-of-tree build directory with
#      Wolfi's flags (--enable-shared --disable-rpath --enable-mkmf-verbose
#      --enable-yjit --with-jemalloc, CFLAGS=-DOPENSSL_NO_ENGINE but not
#      Wolfi's -DOPENSSL_NO_DSA -DOPENSSL_NO_DH, which test-all needs; the prefix
#      is under target/ and docs are off), LDFLAGS pointing -L and -rpath at
#      the stage (--disable-rpath only drops Ruby's own libdir rpath);
#   4. make, plus `update-gems extract-gems` as Wolfi does; then checks that
#      ruby and libruby record libjemalloc.so.2 in DT_NEEDED with a runpath
#      to the stage, that config.h has HAVE_MALLOC_CONF, and that a running
#      ruby maps the stage's file. The last check is the one that matters:
#      Wolfi's real jemalloc has the same soname in /usr/lib, so a binary
#      that lost its rpath would silently run under real jemalloc;
#   5. `make test-all TESTOPTS="-j<jobs> -q --tty=no"` (TESTS selects files)
#      and prints the summary line and every failure/error header.
#
# ALLOC=glibc builds the same source without --with-jemalloc into
# target/ruby/build-glibc (glibc malloc) so a failing test can be rerun
# there: `make ruby ALLOC=glibc STAGE=all TESTS='ruby/test_gc.rb'`.
#
# Environment:
#   STAGE        build | test | all (default all)
#   ALLOC        challoc (default) | glibc
#   RUBY_TAG     tag to build (default v3_4_11); RUBY_COMMIT the commit it
#                must resolve to (Wolfi's expected-commit); RUBY_CHERRY a
#                commit to cherry-pick on top (default none)
#   OPENSSL_TAG  the ruby/openssl tag synced into ext/openssl (default
#                v4.0.2); OPENSSL_COMMIT the commit it must resolve to
#   TESTS        Ruby's TESTS (test files relative to test/; default all)
#   TESTOPTS     Ruby's TESTOPTS (default "-j$JOBS -q --tty=no")
#   JOBS         make -j and test workers (default nproc)
#   RUBY_GEMS_CA a PEM bundle trusted for update-gems' rubygems.org
#                downloads beside the CAs Ruby's tool/downloader.rb pins
#                (lib/rubygems/ssl_certs); for a network whose proxy
#                re-signs TLS, such as a Chainguard Checks guest (unset:
#                the pinned CAs only)
set -eu
cd "$(dirname "$0")/../.."
ROOT=$(pwd)

STAGE=${STAGE:-all}
case "$STAGE" in
    build|test|all) ;;
    *) echo "ruby-build.sh: STAGE must be build, test or all, got '$STAGE'" >&2; exit 2 ;;
esac
ALLOC=${ALLOC:-challoc}
case "$ALLOC" in
    challoc|glibc) ;;
    *) echo "ruby-build.sh: ALLOC must be challoc or glibc, got '$ALLOC'" >&2; exit 2 ;;
esac
RUBY_TAG=${RUBY_TAG:-v3_4_11}
RUBY_COMMIT=${RUBY_COMMIT:-592f1ffdb36153e8be83603ade3c2e9ab6138a77}
RUBY_CHERRY=${RUBY_CHERRY-}
OPENSSL_TAG=${OPENSSL_TAG:-v4.0.2}
OPENSSL_COMMIT=${OPENSSL_COMMIT:-6e410b804f4c6a52c146fe651941bafdbb332754}
JOBS=${JOBS:-$(nproc)}
TESTOPTS=${TESTOPTS:--j$JOBS -q --tty=no}
WORK=$ROOT/target/ruby
SRC=$WORK/src-$RUBY_TAG
BUILD=$WORK/build-$ALLOC
LOGS=$WORK/logs
COMPAT=$WORK/jemalloc-compat
STAGE_LIB=$COMPAT/stage/lib
ARCH=$(uname -m)
mkdir -p "$WORK" "$LOGS"

step() { printf '\n== %s\n' "$*"; }

# 1. The stage.
if [ "$ALLOC" = challoc ]; then
    step "challoc as libjemalloc.so.2 -> $STAGE_LIB"
    TARGET_DIR=$COMPAT QUANTUM=16 sh scripts/jemalloc-compat.sh
    [ -f "$STAGE_LIB/libjemalloc.so.2" ] || { echo "ruby-build.sh: $STAGE_LIB/libjemalloc.so.2 missing" >&2; exit 1; }
fi

# 2. Ruby source at the pinned tag, with Wolfi's ext/openssl.
step "ruby $RUBY_TAG -> $SRC"
if [ ! -d "$SRC/.git" ]; then
    rm -rf "$SRC"
    git clone -q --depth 1 --branch "$RUBY_TAG" https://github.com/ruby/ruby "$SRC"
fi
cd "$SRC"
head=$(git rev-parse HEAD)
if [ "$head" != "$RUBY_COMMIT" ]; then
    echo "ruby-build.sh: tag $RUBY_TAG is $head, expected $RUBY_COMMIT" >&2
    exit 1
fi
if [ -n "$RUBY_CHERRY" ]; then
    PATCH=$WORK/cherry-$RUBY_CHERRY.patch
    if [ ! -s "$PATCH" ]; then
        git fetch -q --depth 2 origin "$RUBY_CHERRY"
        git diff "$RUBY_CHERRY~1" "$RUBY_CHERRY" > "$PATCH"
        [ -s "$PATCH" ] || { echo "ruby-build.sh: empty diff for $RUBY_CHERRY" >&2; exit 1; }
    fi
    if git apply --reverse --check "$PATCH" 2>/dev/null; then
        echo "cherry-pick $RUBY_CHERRY already applied"
    else
        git apply --check "$PATCH"
        git apply "$PATCH"
        echo "cherry-pick $RUBY_CHERRY applied"
    fi
    git diff --stat | tail -1
fi
# Wolfi builds Ruby 3.4 against OpenSSL 4.0 with the ruby/openssl release
# synced over ext/openssl (ruby-3.4.yaml); the extension Ruby 3.4 ships
# does not build against it. sync_default_gems.rb copies from
# ../../ruby/openssl, which from $SRC is $WORK/openssl.
OSSL=$WORK/openssl
step "ruby/openssl $OPENSSL_TAG -> ext/openssl"
if [ ! -d "$OSSL/.git" ]; then
    rm -rf "$OSSL"
    git clone -q --depth 1 --branch "$OPENSSL_TAG" https://github.com/ruby/openssl "$OSSL"
fi
head=$(git -C "$OSSL" rev-parse HEAD)
if [ "$head" != "$OPENSSL_COMMIT" ]; then
    echo "ruby-build.sh: ruby/openssl $OPENSSL_TAG is $head, expected $OPENSSL_COMMIT" >&2
    exit 1
fi
for p in "$ROOT"/contrib/ruby/*.patch; do
    if git -C "$OSSL" apply --reverse --check "$p" 2>/dev/null; then
        echo "$(basename "$p") already applied"
    else
        git -C "$OSSL" apply "$p"
        echo "$(basename "$p") applied"
    fi
done
ruby tool/sync_default_gems.rb openssl > "$LOGS/sync-openssl.log" 2>&1 || { tail -20 "$LOGS/sync-openssl.log"; exit 1; }

# 3. autogen + configure (out of tree, once per flag set).
[ -x configure ] || { step "autogen.sh"; ./autogen.sh > "$LOGS/autogen-$ARCH.log" 2>&1 || { tail -20 "$LOGS/autogen-$ARCH.log"; exit 1; }; }
mkdir -p "$BUILD"
cd "$BUILD"
# Wolfi also passes -DOPENSSL_NO_DSA -DOPENSSL_NO_DH, which compile the DH
# and DSA classes out of the extension; OpenSSL 4.0 still has both, and
# test-all needs DH (test/net/http/test_https.rb builds one at load time),
# so this build keeps them.
CFLAGS_WOLFI="-DOPENSSL_NO_ENGINE"
if [ "$ALLOC" = challoc ]; then
    ALLOC_FLAGS="--with-jemalloc LDFLAGS=-L$STAGE_LIB -Wl,-rpath,$STAGE_LIB"
else
    ALLOC_FLAGS="--without-jemalloc"
fi
ARGS="--prefix=$WORK/install-$ALLOC --enable-shared --disable-rpath --sysconfdir=/etc --enable-mkmf-verbose --enable-yjit --disable-install-doc CFLAGS=$CFLAGS_WOLFI $ALLOC_FLAGS"
if [ ! -f Makefile ] || [ "$(cat .configure-args 2>/dev/null)" != "$ARGS" ]; then
    step "configure ($ALLOC): $ARGS"
    rm -f .configure-args
    # LDFLAGS carries a space, so the argument list is built by hand.
    if [ "$ALLOC" = challoc ]; then
        "$SRC/configure" --prefix="$WORK/install-$ALLOC" --enable-shared --disable-rpath --sysconfdir=/etc \
            --enable-mkmf-verbose --enable-yjit --disable-install-doc "CFLAGS=$CFLAGS_WOLFI" \
            --with-jemalloc "LDFLAGS=-L$STAGE_LIB -Wl,-rpath,$STAGE_LIB" \
            > "$LOGS/configure-$ALLOC-$ARCH.log" 2>&1 || { tail -40 "$LOGS/configure-$ALLOC-$ARCH.log"; exit 1; }
    else
        "$SRC/configure" --prefix="$WORK/install-$ALLOC" --enable-shared --disable-rpath --sysconfdir=/etc \
            --enable-mkmf-verbose --enable-yjit --disable-install-doc "CFLAGS=$CFLAGS_WOLFI" \
            --without-jemalloc \
            > "$LOGS/configure-$ALLOC-$ARCH.log" 2>&1 || { tail -40 "$LOGS/configure-$ALLOC-$ARCH.log"; exit 1; }
    fi
    printf '%s' "$ARGS" > .configure-args
fi
grep -E 'checking for jemalloc library|checking jemalloc/jemalloc.h|checking for malloc_usable_size' "$LOGS/configure-$ALLOC-$ARCH.log" || true

# 4. Build, bundled gems, checks.
step "make -j$JOBS ($ALLOC; log: $LOGS/build-$ALLOC-$ARCH.log)"
start=$(date +%s)
make -j"$JOBS" > "$LOGS/build-$ALLOC-$ARCH.log" 2>&1 || { tail -40 "$LOGS/build-$ALLOC-$ARCH.log"; exit 1; }
if [ -n "${RUBY_GEMS_CA:-}" ]; then
    mkdir -p "$SRC/lib/rubygems/ssl_certs/ci"
    cp "$RUBY_GEMS_CA" "$SRC/lib/rubygems/ssl_certs/ci/ca.pem"
fi
make -j"$JOBS" update-gems extract-gems >> "$LOGS/build-$ALLOC-$ARCH.log" 2>&1 || { tail -40 "$LOGS/build-$ALLOC-$ARCH.log"; exit 1; }
echo "built in $(( $(date +%s) - start )) s"
grep -E 'Failed to configure' "$LOGS/build-$ALLOC-$ARCH.log" || echo "every extension configured"
LD_LIBRARY_PATH=$BUILD ./ruby --disable-gems -v

step "check ($ALLOC)"
libruby=$(ls libruby.so.3.4* | head -1)
if [ "$ALLOC" = challoc ]; then
    # DT_NEEDED through scripts/check-needed.sh, the check a package test
    # runs (--as-needed drops the entry from a binary that references no
    # symbol of the library); then the rpath.
    for bin in ruby "$libruby"; do
        sh "$ROOT/scripts/check-needed.sh" "$bin" libjemalloc.so.2 \
            || { echo "ruby-build.sh: $bin has no DT_NEEDED libjemalloc.so.2" >&2; exit 1; }
        dyn=$(readelf -d "$bin")
        echo "$dyn" | grep -E 'RUNPATH|RPATH' | grep -q "$STAGE_LIB" \
            || { echo "ruby-build.sh: $bin has no rpath to $STAGE_LIB" >&2; exit 1; }
    done
    grep -rh 'define HAVE_MALLOC_CONF\|define RUBY_ALTERNATIVE_MALLOC_HEADER' .ext/include/*/ruby/config.h \
        || { echo "ruby-build.sh: config.h lacks HAVE_MALLOC_CONF" >&2; exit 1; }
    mapped=$(LD_LIBRARY_PATH=$BUILD ./ruby --disable-gems -e \
        'puts File.readlines("/proc/self/maps").grep(/libjemalloc/).map { |l| l.split[5] }.uniq')
    echo "mapped: $mapped"
    [ "$mapped" = "$STAGE_LIB/libjemalloc.so.2" ] \
        || { echo "ruby-build.sh: ruby maps '$mapped', not the stage's libjemalloc.so.2" >&2; exit 1; }
else
    ! readelf -d ruby | grep -q 'NEEDED.*jemalloc' || { echo "ruby-build.sh: glibc build needs jemalloc" >&2; exit 1; }
    ! readelf -d "$libruby" | grep -q 'NEEDED.*jemalloc' || { echo "ruby-build.sh: glibc libruby needs jemalloc" >&2; exit 1; }
    echo "ruby and $libruby do not need libjemalloc"
fi
[ "$STAGE" = build ] && exit 0

# 5. test-all.
LOG=$LOGS/test-all-$ALLOC-$ARCH.log
step "make test-all TESTOPTS='$TESTOPTS' ${TESTS:+TESTS='$TESTS'} ($ALLOC; log: $LOG)"
start=$(date +%s)
rm -f "$BUILD/.test-rc"
{ make test-all TESTOPTS="$TESTOPTS" ${TESTS:+TESTS="$TESTS"} 2>&1; echo $? > "$BUILD/.test-rc"; } \
    | tee "$LOG" | grep -E '^Finished|^[0-9]+ tests, |^ *[0-9]+\) (Failure|Error|Timeout)|^Retrying|^Some tests|^\[ *[0-9]+/[0-9]+\] [^ ]+ = [0-9.]+ s$' || true
rc=$(cat "$BUILD/.test-rc" 2>/dev/null || echo 1)
end=$(date +%s)
summary=$(grep -E '^[0-9]+ tests, [0-9]+ assertions' "$LOG" | tail -1)
echo
echo "== test-all ($ALLOC, $ARCH): rc=$rc, $((end - start)) s"
echo "== $summary"
grep -E '^ *[0-9]+\) (Failure|Error|Timeout):' -A1 "$LOG" | grep -v '^--$' || true
exit "$rc"

#!/bin/sh
# Build everything the Phase 0 benchmark protocol needs, inside the dev
# container (run via `make bench-build` or `make run CMD='sh bench/build.sh'`).
# Sources and binaries live under target/bench-src and target/bench-bin, which
# are in the per-architecture Docker volume, never in the repo.
#
#   mimalloc-bench (pinned)  cfrac espresso larson mstress xmalloc-test
#                            alloc-test glibc-simple
#   redis 6.2.7              redis-server redis-benchmark redis-cli, MALLOC=libc
#   mimalloc (pinned tag)    libmimalloc.so, for the optional third column
#   bench/rt.c bench/sampler.c
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
SRC=$ROOT/target/bench-src
BIN=$ROOT/target/bench-bin
mkdir -p "$SRC" "$BIN"
PROCS=$(nproc)

MIMALLOC_BENCH_REV=06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
REDIS_TAG=6.2.7
MIMALLOC_TAG=v3.5.2

echo "== mimalloc-bench $MIMALLOC_BENCH_REV"
if [ ! -d "$SRC/mimalloc-bench" ]; then
  git clone -q https://github.com/daanx/mimalloc-bench "$SRC/mimalloc-bench"
fi
( cd "$SRC/mimalloc-bench" && git fetch -q --depth 1 origin "$MIMALLOC_BENCH_REV" 2>/dev/null || true
  git checkout -q "$MIMALLOC_BENCH_REV" )
# shbench sources are a separate download in build-bench-env.sh; cmake wants
# the files to exist even though the targets are never built here.
touch "$SRC/mimalloc-bench/bench/shbench/sh6bench-new.c" "$SRC/mimalloc-bench/bench/shbench/sh8bench-new.c"
mkdir -p "$SRC/mimalloc-bench/out/bench"
( cd "$SRC/mimalloc-bench/out/bench"
  cmake -Wno-author -DCMAKE_BUILD_TYPE=Release ../../bench >/dev/null
  cmake --build . --parallel "$PROCS" --target cfrac espresso larson mstress xmalloc-test alloc-test glibc-simple >/dev/null )
for p in cfrac espresso larson mstress xmalloc-test alloc-test glibc-simple; do
  cp "$SRC/mimalloc-bench/out/bench/$p" "$BIN/$p"
done

echo "== redis $REDIS_TAG"
if [ ! -d "$SRC/redis" ]; then
  git clone -q --depth 1 --branch "$REDIS_TAG" https://github.com/redis/redis "$SRC/redis"
fi
if [ ! -x "$SRC/redis/src/redis-server" ]; then
  ( cd "$SRC/redis" && USE_JEMALLOC=no MALLOC=libc BUILD_TLS=no make -j "$PROCS" >/dev/null 2>"$SRC/redis-build.log" ) \
    || { tail -30 "$SRC/redis-build.log"; exit 1; }
fi
for p in redis-server redis-benchmark redis-cli; do cp "$SRC/redis/src/$p" "$BIN/$p"; done

echo "== mimalloc $MIMALLOC_TAG"
if [ ! -d "$SRC/mimalloc" ]; then
  git clone -q --depth 1 --branch "$MIMALLOC_TAG" https://github.com/microsoft/mimalloc "$SRC/mimalloc"
fi
if [ ! -f "$BIN/libmimalloc.so" ]; then
  mkdir -p "$SRC/mimalloc/out"
  ( cd "$SRC/mimalloc/out"
    cmake -DCMAKE_BUILD_TYPE=Release -DMI_BUILD_TESTS=OFF -DMI_BUILD_STATIC=OFF -DMI_BUILD_OBJECT=OFF .. >/dev/null
    cmake --build . --parallel "$PROCS" >/dev/null )
  cp "$(ls "$SRC/mimalloc/out"/libmimalloc.so.*.* | head -1)" "$BIN/libmimalloc.so"
fi

echo "== helpers"
cc -O2 -Wall -Wextra -Werror "$ROOT/bench/rt.c" -o "$BIN/rt"
cc -O2 -Wall -Wextra -Werror -fPIC -shared "$ROOT/bench/sampler.c" -o "$BIN/sampler.so" -ldl

echo "== versions"
{
  echo "mimalloc-bench $MIMALLOC_BENCH_REV"
  echo "redis $REDIS_TAG"
  echo "mimalloc $MIMALLOC_TAG"
  apk list --installed 2>/dev/null | grep -E '^(jemalloc|glibc-2\.44|gcc-15|rust-)' | sed 's/ .*//' | sort
  echo "cmake $(cmake --version | head -1 | sed 's/cmake version //')"
} > "$BIN/versions.txt"
cat "$BIN/versions.txt"
ls -l "$BIN"

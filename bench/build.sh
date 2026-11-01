#!/bin/sh
# Build the benchmark programs the rows of bench/rows.toml run, inside the
# bench image (bench/Dockerfile.cb runs this once per architecture) or the
# dev container (`make bench-build`). Sources go under target/bench-src,
# programs and their inputs under target/bench-bin (BIN), never into the
# repository. Every download is pinned by sha256 (fetch() refuses other
# bytes) and every git checkout by commit.
#
#   mimalloc-bench (pinned)  cfrac espresso barnes larson larson-sized
#                            mstress xmalloc-test alloc-test glibc-simple
#                            glibc-thread cache-scratch cache-thrash
#                            malloc-large mleak rptest sh6bench sh8bench,
#                            and the inputs of barnes, espresso, z3, gs,
#                            rbstress and lua
#   redis 6.2.7              redis-server redis-benchmark redis-cli, MALLOC=libc
#   mimalloc (pinned tag)    libmimalloc.so
#   jemalloc 5.3.0, 5.4.0    jemalloc-<v>/libjemalloc.so.2 from the release
#                            tarballs (the jemalloc53 / jemalloc54 variants)
#   z3                       the release binary (mimalloc-bench's z3 row)
#   rocksdb                  db_bench (DISABLE_JEMALLOC, no tcmalloc)
#   wrk2                     the fixed-rate HTTP load generator, on the
#                            image's LuaJIT 2.1 (wrk2's bundled 2.0 has no
#                            arm64 port)
#   sqlite, lua sources      the compile rows (bench/drivers/compile.sh)
#   ruby benchmark/          the Ruby rows' scripts, from the release tarball
#   onnxruntime              pip --target pylib-ort, hash-pinned
#                            (bench/probes/ort/requirements.txt), and the
#                            ort-mlp model it runs
#   bench/tools/rt.c bench/sampler.c, the probes
#
# BENCH_BUILD="core apps" (default) picks the sections: core is everything
# mimalloc-bench, redis, mimalloc, jemalloc and the helpers; apps is z3,
# rocksdb, wrk2, the compile and Ruby sources and onnxruntime. A section
# whose tool is missing (no `patch`, no luajit) prints a SKIP line; the
# rows that need it are then skipped by cb (their `needs`).
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
SRC=$ROOT/target/bench-src
BIN=$ROOT/target/bench-bin
IN=$BIN/inputs
mkdir -p "$SRC" "$BIN" "$IN" "$SRC/dl"
PROCS=$(nproc)
SECTIONS=${BENCH_BUILD:-core apps}
case $(uname -m) in aarch64|arm64) ARCH=arm64;; x86_64|amd64) ARCH=amd64;; *) ARCH=$(uname -m);; esac

MIMALLOC_BENCH_REV=06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
REDIS_TAG=6.2.7
MIMALLOC_TAG=v3.5.2
JEMALLOC_53=5.3.0 JEMALLOC_53_SHA=2db82d1e7119df3e71b7640219b6dfe84789bc0537983c3b7ac4f7189aecfeaa
JEMALLOC_54=5.4.0 JEMALLOC_54_SHA=200776fac271093e7c2f21edd6d62657ecd2be578d9328633f2a86bfa6ef4f1d
SH6_SHA=506354d66b9eebef105d757e055bc55e8d4aea1e7b51faab3da35b0466c923a1
SH8_SHA=12a8e75248c9dcbfee28245c12bc937a16ef56ec9cbfab88d0e348271667726f
PDF_URL=https://raw.githubusercontent.com/geekaaron/Resources/master/resources/Writing_a_Simple_Operating_System--from_Scratch.pdf
PDF_SHA=49541882e9e5c049cc47e4fd78aa625ba32c0ae53961291374a2130fdb6eed12
LUA_TAG=v5.4.7 LUA_SHA=5c39111b3fc4c1c9e56671008955a1730f54a15b95e1f1bd0752b868b929d8e3
Z3_VER=5.1.0
# shellcheck disable=SC2034 # read by name through the eval in the apps section
Z3_SHA_arm64=2832cfd43c6862fbaf78834b5f1e35d031b4a87e85456b696c1c6f609cd5609e Z3_ZIP_arm64=z3-5.1.0-arm64-glibc-2.38
# shellcheck disable=SC2034 # read by name through the eval in the apps section
Z3_SHA_amd64=f47be8d27d3230e823bf1eeede2fe0abaca55bb78d0b59974370e6689a92284a Z3_ZIP_amd64=z3-5.1.0-x64-glibc-2.39
ROCKSDB_VER=10.10.1 ROCKSDB_SHA=df2ff348f3fac8578fd4b727eee7267aaf90cd403c99b55e898d1db63fa8cff5
WRK2_REV=44a94c17d8e6a0bac8559b53da76848e430cb7a7 WRK2_SHA=40fb577303ae83bfc211855880696b6e14daf1eaea2a684fe8cf2252c735b47e
SQLITE_ZIP=sqlite-amalgamation-3530400 SQLITE_SHA=1e71ddf93849c6a6ecf58b827c0692073d2dd7ee40196158068f7b29f422e87d
RUBY_VER=3.4.11 RUBY_SHA=5c22be44524312b3d433d68739bcc530633b1da5ef8ba0afa0a37680da17d3de

want() { case " $SECTIONS " in *" $1 "*) return 0;; *) return 1;; esac; }
have() { command -v "$1" >/dev/null 2>&1; }
skip() { echo "SKIP $1: $2"; }
# fetch URL SHA256 [NAME]: download once into $SRC/dl, refuse other bytes.
fetch() {
  out=$SRC/dl/${3:-$(basename "$1")}
  if [ ! -f "$out" ] || [ "$(sha256sum "$out" | cut -d' ' -f1)" != "$2" ]; then
    rm -f "$out"
    if have curl; then curl -sSfL --retry 3 -o "$out.part" "$1"
    else python3 -c 'import sys, urllib.request; urllib.request.urlretrieve(sys.argv[1], sys.argv[2])' "$1" "$out.part"; fi
    got=$(sha256sum "$out.part" | cut -d' ' -f1)
    [ "$got" = "$2" ] || { echo "fetch $1: sha256 $got, expected $2" >&2; rm -f "$out.part"; exit 1; }
    mv "$out.part" "$out"
  fi
  FETCHED=$out
}
unzip_to() { python3 -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$1" "$2"; }

MB=$SRC/mimalloc-bench
if want core; then
echo "== mimalloc-bench $MIMALLOC_BENCH_REV"
if [ ! -d "$MB" ]; then
  git clone -q https://github.com/daanx/mimalloc-bench "$MB"
fi
( cd "$MB" && git fetch -q --depth 1 origin "$MIMALLOC_BENCH_REV" 2>/dev/null || true
  git checkout -q "$MIMALLOC_BENCH_REV" )
# shbench: the two SmartHeap sources are separate downloads, patched with
# the normal diffs mimalloc-bench ships (build-bench-env.sh).
SHB=$MB/bench/shbench
TARGETS="cfrac espresso barnes larson larson-sized mstress xmalloc-test alloc-test glibc-simple glibc-thread cache-scratch cache-thrash malloc-large mleak rptest"
if have patch; then
  if [ ! -s "$SHB/sh6bench-new.c" ] || [ ! -s "$SHB/sh8bench-new.c" ]; then
    fetch http://www.microquill.com/smartheap/shbench/bench.zip "$SH6_SHA"; unzip_to "$FETCHED" "$SHB"
    fetch http://www.microquill.com/smartheap/SH8BENCH.zip "$SH8_SHA"; unzip_to "$FETCHED" "$SHB"
    for f in sh6bench.c sh6bench.patch SH8BENCH.C sh8bench.patch; do sed -i 's/\r$//' "$SHB/$f"; done
    ( cd "$SHB" && patch -s -p1 -o sh6bench-new.c sh6bench.c sh6bench.patch && patch -s -p1 -o sh8bench-new.c SH8BENCH.C sh8bench.patch )
  fi
  TARGETS="$TARGETS sh6bench sh8bench"
else
  skip "sh6bench sh8bench" "no patch(1) in this container (bench/Dockerfile.cb has it)"
  # cmake wants the files to exist even though the targets are not built.
  [ -s "$SHB/sh6bench-new.c" ] || : > "$SHB/sh6bench-new.c"
  [ -s "$SHB/sh8bench-new.c" ] || : > "$SHB/sh8bench-new.c"
fi
mkdir -p "$MB/out/bench"
( cd "$MB/out/bench"
  cmake -Wno-author -Wno-dev -DCMAKE_BUILD_TYPE=Release ../../bench >/dev/null
  # shellcheck disable=SC2086
  cmake --build . --parallel "$PROCS" --target $TARGETS >/dev/null )
for p in $TARGETS; do
  cp "$MB/out/bench/$p" "$BIN/$p"
done
# Inputs, so the rows need nothing from target/bench-src (the image keeps
# only BIN).
cp "$MB/bench/espresso/largest.espresso" "$IN/largest.espresso"
cp "$MB/bench/barnes/input" "$IN/barnes.input"
cp "$MB/bench/z3/test1.smt2" "$IN/z3-test1.smt2"
cp "$MB/bench/rbstress/stress_mem.rb" "$IN/rbstress.rb"
fetch "$PDF_URL" "$PDF_SHA" large.pdf
cp "$FETCHED" "$IN/large.pdf"

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
  for f in "$SRC/mimalloc/out"/libmimalloc.so.*.*; do cp "$f" "$BIN/libmimalloc.so"; break; done
fi

# jemalloc from the release tarballs, upstream's default configuration
# (no prefix, so the library exports malloc itself; the page size is the
# build host's, 4 KiB on every reference cell). Wolfi's package stays the
# `jemalloc` variant.
for v in "$JEMALLOC_53:$JEMALLOC_53_SHA" "$JEMALLOC_54:$JEMALLOC_54_SHA"; do
  ver=${v%%:*} sha=${v#*:}
  [ -f "$BIN/jemalloc-$ver/libjemalloc.so.2" ] && continue
  echo "== jemalloc $ver"
  fetch "https://github.com/jemalloc/jemalloc/releases/download/$ver/jemalloc-$ver.tar.bz2" "$sha"
  rm -rf "$SRC/jemalloc-$ver"; tar -xjf "$FETCHED" -C "$SRC"
  ( cd "$SRC/jemalloc-$ver" && ./configure --disable-cxx && make -j "$PROCS" build_lib_shared ) >"$SRC/jemalloc-$ver.log" 2>&1 \
    || { tail -30 "$SRC/jemalloc-$ver.log"; exit 1; }
  mkdir -p "$BIN/jemalloc-$ver"
  cp "$SRC/jemalloc-$ver/lib/libjemalloc.so.2" "$BIN/jemalloc-$ver/libjemalloc.so.2"
done

echo "== helpers"
cc -O2 -Wall -Wextra -Werror "$ROOT/bench/tools/rt.c" -o "$BIN/rt"
cc -O2 -Wall -Wextra -Werror -fPIC -shared "$ROOT/bench/sampler.c" -o "$BIN/sampler.so" -ldl
cc -O2 -Wall -Wextra -Werror "$ROOT/bench/probes/churn.c" -o "$BIN/churn"
cc -O2 -Wall -Wextra -Werror "$ROOT/bench/probes/valkey_model.c" -o "$BIN/valkey_model"
fi

if want apps; then
echo "== lua $LUA_TAG (the compile row's tree, and mimalloc-bench's lua row)"
fetch "https://github.com/lua/lua/archive/refs/tags/$LUA_TAG.tar.gz" "$LUA_SHA" "lua-$LUA_TAG.tar.gz"
cp "$FETCHED" "$IN/lua.tar.gz"

echo "== sqlite amalgamation ($SQLITE_ZIP, the cc1 compile row)"
fetch "https://sqlite.org/2026/$SQLITE_ZIP.zip" "$SQLITE_SHA"
rm -rf "$SRC/${SQLITE_ZIP:?}"; unzip_to "$FETCHED" "$SRC"
cp "$SRC/$SQLITE_ZIP/sqlite3.c" "$SRC/$SQLITE_ZIP/sqlite3.h" "$IN/"

echo "== ruby $RUBY_VER benchmark/ (the Ruby rows' scripts)"
fetch "https://cache.ruby-lang.org/pub/ruby/3.4/ruby-$RUBY_VER.tar.gz" "$RUBY_SHA"
rm -rf "$IN/ruby-benchmark"; mkdir -p "$IN/ruby-benchmark"
tar -xzf "$FETCHED" -C "$IN/ruby-benchmark" --strip-components=2 "ruby-$RUBY_VER/benchmark"

eval "z3_sha=\$Z3_SHA_$ARCH z3_zip=\${Z3_ZIP_$ARCH:-}"
if [ -n "$z3_zip" ]; then
  if [ ! -x "$BIN/z3/bin/z3" ]; then
    echo "== z3 $Z3_VER ($z3_zip)"
    # shellcheck disable=SC2154 # z3_sha is assigned by the eval above
    fetch "https://github.com/Z3Prover/z3/releases/download/z3-$Z3_VER/$z3_zip.zip" "$z3_sha"
    rm -rf "$SRC/${z3_zip:?}" "$BIN/z3"; unzip_to "$FETCHED" "$SRC"
    mkdir -p "$BIN/z3"; cp -r "$SRC/$z3_zip/bin" "$BIN/z3/bin"; chmod +x "$BIN/z3/bin/z3"
  fi
else
  skip z3 "no release binary for $ARCH"
fi

if [ ! -x "$BIN/db_bench" ]; then
  # Without gflags db_bench builds as a stub that only prints "Please
  # install gflags".
  if have bash && have perl && [ -f /usr/include/gflags/gflags.h ]; then
    echo "== rocksdb $ROCKSDB_VER db_bench (minutes)"
    fetch "https://github.com/facebook/rocksdb/archive/refs/tags/v$ROCKSDB_VER.tar.gz" "$ROCKSDB_SHA" "rocksdb-$ROCKSDB_VER.tar.gz"
    rm -rf "$SRC/rocksdb-$ROCKSDB_VER"; tar -xzf "$FETCHED" -C "$SRC"
    ( cd "$SRC/rocksdb-$ROCKSDB_VER" && DISABLE_WARNING_AS_ERROR=1 DISABLE_JEMALLOC=1 ROCKSDB_DISABLE_TCMALLOC=1 \
        PORTABLE=1 DEBUG_LEVEL=0 USE_RTTI=1 make -j "$PROCS" db_bench >"$SRC/rocksdb-build.log" 2>&1 ) \
      || { tail -40 "$SRC/rocksdb-build.log"; exit 1; }
    "$SRC/rocksdb-$ROCKSDB_VER/db_bench" --benchmarks=fillseq --num=1000 --compression_type=none --db="$SRC/rocksdb-check" >/dev/null \
      || { echo "db_bench does not run" >&2; exit 1; }
    cp "$SRC/rocksdb-$ROCKSDB_VER/db_bench" "$BIN/db_bench"
    rm -rf "$SRC/rocksdb-$ROCKSDB_VER" "$SRC/rocksdb-check"
  else
    skip rocksdb "needs bash, perl and gflags-dev (bench/Dockerfile.cb has them)"
  fi
fi

if [ ! -x "$BIN/wrk2" ]; then
  if have luajit && [ -f /usr/include/luajit-2.1/luajit.h ] && [ -f /usr/include/openssl/ssl.h ] && [ -f /usr/include/zlib.h ]; then
    echo "== wrk2 $WRK2_REV"
    fetch "https://github.com/giltene/wrk2/archive/$WRK2_REV.tar.gz" "$WRK2_SHA" "wrk2-$WRK2_REV.tar.gz"
    W=$SRC/wrk2-$WRK2_REV
    rm -rf "$W"; tar -xzf "$FETCHED" -C "$SRC"
    mkdir -p "$W/obj"
    # (Each step is checked: set -e does not apply inside a list whose
    # status is tested.)
    ( cd "$W" || exit 1
      luajit -b src/wrk.lua obj/bytecode.o || exit 1
      # hdr_histogram.c includes <x86intrin.h> for nothing but a builtin;
      # LuaJIT 2.1 spells luaL_reg luaL_Reg; wrk.c keeps getopt_long's
      # result in a char, which is unsigned on aarch64 (-1 never matches).
      sed -i '/#include <x86intrin.h>/d' src/hdr_histogram.c || exit 1
      sed -i 's/^    char c, \*\*header = headers;/    int c; char **header = headers;/' src/wrk.c || exit 1
      grep -q '^    int c; char \*\*header = headers;' src/wrk.c || exit 1
      for f in wrk net ssl aprintf stats script units ae zmalloc http_parser tinymt64 hdr_histogram; do
        cc -std=gnu99 -O2 -D_REENTRANT -D_POSIX_C_SOURCE=200809L -D_DEFAULT_SOURCE -DluaL_reg=luaL_Reg -w \
          -I/usr/include/luajit-2.1 -c "src/$f.c" -o "obj/$f.o" || exit 1
      done
      cc -Wl,-E -o wrk obj/*.o -lluajit-5.1 -lpthread -lm -lcrypto -lssl -lz -ldl ) >"$SRC/wrk2-build.log" 2>&1 \
      || { tail -30 "$SRC/wrk2-build.log"; exit 1; }
    cp "$W/wrk" "$BIN/wrk2"
  else
    skip wrk2 "needs luajit, luajit-dev, openssl-dev and zlib-dev (bench/Dockerfile.cb has them)"
  fi
fi

if [ ! -f "$IN/ort-mlp.onnx" ]; then
  if python3 -m pip --version >/dev/null 2>&1; then
    echo "== onnxruntime (bench/probes/ort/requirements.txt)"
    rm -rf "$BIN/pylib-ort"
    python3 -m pip install -q --disable-pip-version-check --no-cache-dir --only-binary=:all: --no-deps \
      --require-hashes -r "$ROOT/bench/probes/ort/requirements.txt" --target "$BIN/pylib-ort"
    PYTHONPATH=$BIN/pylib-ort python3 "$ROOT/bench/probes/ort/mlp.py" make "$IN/ort-mlp.onnx"
  else
    skip onnxruntime "no pip (py3.13-pip)"
  fi
fi
fi

echo "== versions"
{
  echo "mimalloc-bench $MIMALLOC_BENCH_REV"
  echo "redis $REDIS_TAG"
  echo "mimalloc $MIMALLOC_TAG"
  echo "jemalloc tarballs $JEMALLOC_53 $JEMALLOC_54"
  echo "z3 $Z3_VER, rocksdb $ROCKSDB_VER, wrk2 $WRK2_REV, lua $LUA_TAG, sqlite $SQLITE_ZIP, ruby benchmark $RUBY_VER"
  grep -E "^[a-z]" "$ROOT/bench/probes/ort/requirements.txt" | cut -d" " -f1 | tr "\n" " "; echo
  apk list --installed 2>/dev/null | grep -E '^(jemalloc|glibc-2\.44|gcc-15|rust-|tcmalloc-minimal-[0-9]|python-3\.13-[0-9]|nginx|ghostscript|ruby-3|nodejs|postgresql-17-[0-9]|openjdk-21-[0-9]|fluent-bit|luajit-2)' | sed 's/ .*//' | sort
  echo "cmake $(cmake --version | head -1 | sed 's/cmake version //')"
} > "$BIN/versions.txt"
cat "$BIN/versions.txt"
ls -l "$BIN"

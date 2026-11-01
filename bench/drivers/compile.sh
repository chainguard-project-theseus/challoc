#!/bin/sh
# bench/drivers/compile.sh: the compile rows (the build-tool class). The
# compiler runs with the allocator preloaded, so make, the gcc driver,
# cc1, as and ld all allocate through it; the source tree is unpacked
# fresh before the timed part.
#
#   PROJECT=lua      Lua 5.4.7 (mimalloc-bench's lua row): `make -j JOBS`
#                    of the interpreter, about 35 translation units
#   PROJECT=sqlite   `gcc -O2 -c sqlite3.c`: one cc1 over the 9 MB
#                    amalgamation, the long single-process compile
#   JOBS             parallel jobs for lua (default nproc)
#
# Metrics: wall_s (the score), maxrss_mib (the largest process: cc1).
. "$(dirname "$0")/cb.sh"
PROJECT=${PROJECT:?PROJECT=lua|sqlite}
JOBS=${JOBS:-$(nproc)}
IN=$CB_BIN/inputs
W=$CB_OUT/src
rm -rf "$W"; mkdir -p "$W"
trap 'rm -rf "$W"' EXIT
case $PROJECT in
  lua)
    [ -f "$IN/lua.tar.gz" ] || die "no $IN/lua.tar.gz (bench/build.sh apps)"
    tar -xzf "$IN/lua.tar.gz" -C "$W" --strip-components=1
    # No readline (not in the image) and no -march=native (the same code
    # for every cell of an architecture).
    # shellcheck disable=SC2016 # $(MYCFLAGS) is make's variable, not a command substitution
    timed build make -C "$W" -j "$JOBS" CC=gcc MYCFLAGS="-std=c99 -DLUA_USE_LINUX" MYLIBS=-ldl \
      CFLAGS='-Wall -O2 $(MYCFLAGS) -fno-stack-protector -fno-common'
    [ -x "$W/lua" ] || die "lua did not build"
    ;;
  sqlite)
    [ -f "$IN/sqlite3.c" ] || die "no $IN/sqlite3.c (bench/build.sh apps)"
    cp "$IN/sqlite3.c" "$IN/sqlite3.h" "$W/"
    timed build gcc -O2 -c "$W/sqlite3.c" -o "$W/sqlite3.o"
    ;;
  *) die "unknown PROJECT=$PROJECT";;
esac
metric wall_s "$WALL_S"
metric maxrss_mib "$(mib "$MAXRSS_KIB")"
emit

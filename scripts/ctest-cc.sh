#!/bin/sh
# scripts/ctest-cc.sh: the one compiler line for the C test programs
# (tests/c/*.c, fuzz/fuzz.c, the vendored suites), run inside the dev
# container. Every driver (src/api/ctests.rs, scripts/cgroup-tests.sh,
# tests/as-needed.sh, the Makefile's `fuzz` target) compiles through this script, so the flags and the QUANTUM the
# programs are built for live in one place.
#
#   sh scripts/ctest-cc.sh [--cxx] ARGS...   $CC $FLAGS ARGS... -ldl
#   sh scripts/ctest-cc.sh --flags           print $FLAGS and exit
#   sh scripts/ctest-cc.sh --quantum         print the QUANTUM and exit
#
# FLAGS is `-O2 -g -Wall -Wextra -Werror -pthread -DQUANTUM=<n> -I<root>/tests/c`
# (`--cxx` uses $CXX and adds -std=c++17). ARGS is the source, `-o OUT` and
# any extra -D/-I/-Wno-/-L/-l options; `-ldl` (for `dlsym` in common.h) is
# appended after them, where the linker wants libraries.
#
# QUANTUM comes from the built library, not from a constant: the library
# named by CHALLOC_LIB (default target/release/libchalloc.so under
# CARGO_TARGET_DIR or <root>/target) is loaded once by a tiny probe that
# reads `arenas.quantum` through mallctl, and the answer is cached next to
# the library (`ctest-quantum`, refreshed when the library is newer). The
# QUANTUM environment variable overrides the probe; without a library and
# without QUANTUM the default build's 16 is used with a note (the
# --any-allocator runs of scripts/cgroup-tests.sh need no library).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
LIB=${CHALLOC_LIB:-${CARGO_TARGET_DIR:-$ROOT/target}/release/libchalloc.so}
CC=${CC:-cc}
CXX=${CXX:-c++}

quantum() {
    if [ -n "${QUANTUM:-}" ]; then echo "$QUANTUM"; return; fi
    if [ ! -f "$LIB" ]; then
        echo "ctest-cc: $LIB missing, assuming QUANTUM=16 (set QUANTUM or CHALLOC_LIB)" >&2
        echo 16; return
    fi
    cache=$(dirname "$LIB")/ctest-quantum
    if [ -f "$cache" ] && [ ! "$LIB" -nt "$cache" ]; then cat "$cache"; return; fi
    tmp=$(mktemp -d)
    cat >"$tmp/probe.c" <<'EOF'
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
int main(void)
{
    int (*ctl)(const char *, void *, size_t *, void *, size_t) = dlsym(RTLD_DEFAULT, "mallctl");
    size_t q = 0, len = sizeof q;
    if (!ctl || ctl("arenas.quantum", &q, &len, NULL, 0) != 0 || (q != 8 && q != 16)) {
        fprintf(stderr, "ctest-cc: the preloaded library does not answer arenas.quantum\n");
        return 1;
    }
    printf("%zu\n", q);
    return 0;
}
EOF
    "$CC" -O2 -Wall "$tmp/probe.c" -o "$tmp/probe" -ldl
    q=$(LD_PRELOAD=$LIB "$tmp/probe")
    echo "$q" >"$tmp/quantum" && mv -f "$tmp/quantum" "$cache"
    rm -rf "$tmp"
    echo "$q"
}

case ${1:-} in
    --quantum) quantum; exit 0 ;;
esac
Q=$(quantum)
FLAGS="-O2 -g -Wall -Wextra -Werror -pthread -DQUANTUM=$Q -I$ROOT/tests/c"
case ${1:-} in
    --flags) echo "$FLAGS"; exit 0 ;;
    --cxx) shift; exec "$CXX" -std=c++17 $FLAGS "$@" -ldl ;;
    '') echo "usage: sh scripts/ctest-cc.sh [--cxx] SRC -o OUT [flags...]" >&2; exit 2 ;;
esac
exec "$CC" $FLAGS "$@" -ldl

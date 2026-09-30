#!/bin/sh
# tests/as-needed.sh: the `-Wl,--as-needed` truth for challoc consumers
# (design 13.6), run in the dev container by
# `make check-needed` after `cargo build --release`. Links tests/c/as_needed.c
# against target/release/libchalloc.so three ways, each with and without
# `-Wl,--as-needed`, and checks the DT_NEEDED list with
# scripts/check-needed.sh:
#
#   malloc    the program references malloc only (every real consumer):
#             DT_NEEDED libchalloc.so.0 must be present either way, because
#             -lchalloc precedes the implicit -lc on the link line and
#             --as-needed keeps a library that resolves a reference made
#             before it is scanned; malloc is resolved by libchalloc first
#   version   the program calls challoc_version(): present either way
#   none      the program references nothing from the library: present
#             with -Wl,--no-as-needed, dropped with --as-needed (the failure
#             the melange check guards against: the binary then runs on
#             glibc malloc); the plain link records the toolchain's default
#             (Wolfi's gcc drops it: --as-needed is the default there)
#
#   via-lib   the two-level shape (Ruby): the executable references nothing
#             of the allocator, only libas_needed_lib.so does, and the
#             library keeps DT_NEEDED libchalloc.so.0. Under --as-needed the
#             executable drops its own entry, and then the loader's
#             breadth-first search order puts libc.so.6 (a direct dependency
#             of the executable) before libchalloc.so.0 (a dependency of the
#             library): malloc binds to glibc for the whole process, the
#             library's own calls included. This is the silent failure
#             scripts/check-needed.sh exists for, and why Ruby's configure
#             links -ljemalloc with --no-as-needed.
#
# Then runs the malloc variant with a libchalloc.so.0 link on
# LD_LIBRARY_PATH and checks malloc is served from libchalloc.so.0, and
# without the path checks it fails to load (the soname, not the file name,
# is what the loader wants). Also prints the toolchain's default: whether a
# plain link (no flag) already behaves as --as-needed.
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
LIB=${LIB:-$ROOT/target/release}
[ -f "$LIB/libchalloc.so" ] || { echo "as-needed: $LIB/libchalloc.so missing: cargo build --release first" >&2; exit 1; }
# Every link goes through scripts/ctest-cc.sh (the test programs' one
# compiler line: it appends -ldl and reads QUANTUM from the library).
CHALLOC_LIB=$LIB/libchalloc.so; export CHALLOC_LIB
CTEST_CC="sh $ROOT/scripts/ctest-cc.sh"
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
ln -s "$LIB/libchalloc.so" "$T/libchalloc.so.0"
fail=0
check() { # NAME EXPECT(yes|no) flags...
    name=$1; expect=$2; shift 2
    $CTEST_CC "$@" "$ROOT/tests/c/as_needed.c" -o "$T/$name" -L"$LIB" -lchalloc
    if sh "$ROOT/scripts/check-needed.sh" "$T/$name" libchalloc.so.0 >"$T/$name.out" 2>&1; then got=yes; else got=no; fi
    if [ "$expect" = any ]; then
        printf '%-28s DT_NEEDED libchalloc.so.0: %-3s (the toolchain default with no flag: %s)\n' "$name" "$got" "$([ "$got" = no ] && echo '--as-needed' || echo '--no-as-needed')"
    else
        printf '%-28s DT_NEEDED libchalloc.so.0: %-3s (expected %s) %s\n' "$name" "$got" "$expect" "$([ "$got" = "$expect" ] && echo ok || echo FAIL)"
        [ "$got" = "$expect" ] || fail=1
    fi
    sed 's/^/    /' "$T/$name.out" | head -1
}
echo "== $(${CC:-cc} --version | head -1); $(ld --version 2>/dev/null | head -1 || true); $($CTEST_CC --flags)"
check malloc                 yes
check malloc-as-needed       yes -Wl,--as-needed
check version                yes -DREF_VERSION
check version-as-needed      yes -DREF_VERSION -Wl,--as-needed
check none-no-as-needed      yes -DREF_NONE -Wl,--no-as-needed
check none-as-needed         no  -DREF_NONE -Wl,--as-needed
check none-default           any -DREF_NONE
# The two-level shape: the library needs libchalloc, the executable does or does not.
$CTEST_CC -shared -fPIC "$ROOT/tests/c/as_needed_lib.c" -o "$T/libas_needed_lib.so" -L"$LIB" -lchalloc
sh "$ROOT/scripts/check-needed.sh" "$T/libas_needed_lib.so" libchalloc.so.0 | head -1 | sed 's/^/    /'
via_lib() { # NAME flags...
    name=$1; shift
    $CTEST_CC "$@" -DREF_NONE -DVIA_LIB "$ROOT/tests/c/as_needed.c" -o "$T/$name" -L"$T" -L"$LIB" -las_needed_lib -lchalloc
    if sh "$ROOT/scripts/check-needed.sh" "$T/$name" libchalloc.so.0 >"$T/$name.out" 2>&1; then got=yes; else got=no; fi
    out=$(LD_LIBRARY_PATH=$T "$T/$name" 2>&1 | head -1)
    printf '%-28s executable DT_NEEDED libchalloc.so.0: %-3s; at run time %s\n' "$name" "$got" "$out"
    echo "$out"
}
echo "== the two-level shape (the executable allocates only through the library):"
r1=$(via_lib via-lib-no-as-needed -Wl,--no-as-needed | tail -1)
r2=$(via_lib via-lib-as-needed -Wl,--as-needed | tail -1)
case $r1 in *"libchalloc.so.0"*) ;; *) echo "    FAIL: with --no-as-needed the library's malloc should be challoc's: $r1"; fail=1;; esac
case $r2 in *"libc.so.6"*) echo "    confirmed: with --as-needed the executable drops the entry and the library's malloc is glibc's";;
            *"libchalloc.so.0"*) echo "    note: with --as-needed the library's malloc is still challoc's here ($r2)";;
            *) echo "    FAIL: unexpected: $r2"; fail=1;; esac
# -Wl,-y,malloc: the linker's own trace of who defines and references malloc.
echo "== linker trace for the malloc variant under --as-needed:"
$CTEST_CC -Wl,--as-needed -Wl,-y,malloc "$ROOT/tests/c/as_needed.c" -o "$T/trace" -L"$LIB" -lchalloc 2>&1 | sed 's/^/    /' || true
echo "== running the --as-needed malloc variant:"
out=$(LD_LIBRARY_PATH=$T "$T/malloc-as-needed" 2>&1) || { echo "    FAIL: did not run: $out"; fail=1; }
echo "    with libchalloc.so.0 on LD_LIBRARY_PATH: $(echo "$out" | head -1)"
echo "$out" | grep -q '^malloc from libchalloc.so.0$' || { echo "    FAIL: malloc not served by libchalloc.so.0"; fail=1; }
if out=$("$T/malloc-as-needed" 2>&1); then echo "    FAIL: ran without the library on the path: $out"; fail=1
else echo "    without it: $(echo "$out" | head -1 | sed 's/.*: //') (expected: cannot open libchalloc.so.0)"; fi
echo "== running the --as-needed none variant (no reference: glibc malloc, the silent failure):"
"$T/none-as-needed" | sed 's/^/    /'
# The built consumers, when present: tests/consumers/valkey-build.sh's valkey-server
# must need libchalloc.so.0; tests/consumers/ruby-build.sh's ruby and libruby.so must
# need libjemalloc.so.2 (both builds run the same check themselves).
echo "== built consumers:"
found=0
consumer() { # BINARY SONAME: run the check, keep its exit status (not sed's)
    found=1
    if out=$(sh "$ROOT/scripts/check-needed.sh" "$1" "$2" 2>&1); then :; else fail=1; fi
    echo "$out" | sed 's/^/    /'
}
for b in "$ROOT"/target/valkey/src-*/src/valkey-server; do
    [ -f "$b" ] || continue
    case $b in *-jemalloc/src/*) continue;; esac   # the soak's bundled-jemalloc baseline tree
    consumer "$b" libchalloc.so.0
done
for b in "$ROOT"/target/ruby/build-challoc/ruby "$ROOT"/target/ruby/build-challoc/libruby.so.3.4*; do
    [ -f "$b" ] || continue
    consumer "$b" libjemalloc.so.2
done
[ $found = 1 ] || echo "    none built (make valkey STAGE=build, make ruby STAGE=build)"
if [ $fail = 0 ]; then echo "ok as-needed"; else echo "FAIL as-needed" >&2; exit 1; fi

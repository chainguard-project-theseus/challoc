#!/bin/sh
# check-needed.sh BINARY SONAME...: print BINARY's DT_NEEDED list (and its
# RUNPATH/RPATH) and exit 1 unless every SONAME given is in it (design
# 13.6). For the melange pipeline: Wolfi links with
# `-Wl,--as-needed`, so a consumer that references no symbol of the
# allocator library drops its DT_NEEDED entry and silently runs on glibc
# malloc; a package test runs this on every installed binary that must
# carry the allocator (`libchalloc.so.0`, or `libjemalloc.so.2` for
# challoc-jemalloc-compat consumers). readelf only: no ldd, no loading.
#
#   scripts/check-needed.sh target/valkey/src-9.1.2/src/valkey-server libchalloc.so.0
#   scripts/check-needed.sh target/ruby/build-challoc/ruby libjemalloc.so.2
#
# Exit 0: every soname present; 1: one missing; 2: usage or not an ELF file.
set -eu
[ $# -ge 2 ] || { echo "usage: check-needed.sh <binary> <soname>..." >&2; exit 2; }
bin=$1; shift
[ -f "$bin" ] || { echo "check-needed: $bin: no such file" >&2; exit 2; }
dyn=$(readelf -d "$bin" 2>/dev/null) || { echo "check-needed: $bin: readelf -d failed (not an ELF file?)" >&2; exit 2; }
needed=$(echo "$dyn" | sed -n 's/.*(NEEDED)[^[]*\[\(.*\)\].*/\1/p')
runpath=$(echo "$dyn" | sed -n 's/.*(\(RUNPATH\|RPATH\))[^[]*\[\(.*\)\].*/\1=\2/p' | tr '\n' ' ')
# shellcheck disable=SC2086 # joins the sonames onto one line
echo "$bin: NEEDED $(echo $needed | tr '\n' ' ')${runpath:+ ($runpath)}"
rc=0
for want in "$@"; do
    if echo "$needed" | grep -qx "$want"; then
        echo "  ok      $want"
    else
        echo "  MISSING $want (linked with --as-needed and no symbol of it referenced? see design 13.6)" >&2
        rc=1
    fi
done
exit $rc

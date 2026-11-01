#!/bin/bash
# ci/glibc-max.sh SO MAX: SO must need no GLIBC_ symbol version above MAX.
# glibc 2.44 (Wolfi's stream) is the only libc the shipped library supports;
# a newer version need would make it unloadable there. Reads the
# version-needs section (.gnu.version_r) with readelf.
set -euo pipefail
so=$1; max=$2
[ -f "$so" ] || { echo "error: $so missing"; exit 1; }
vers=$(readelf -V --wide "$so" | { grep -oE 'Name: GLIBC_[0-9]+(\.[0-9]+)*' || true; } | sed 's/Name: GLIBC_//' | sort -uV)
[ -n "$vers" ] || { echo "error: $so needs no GLIBC_ version at all (not a glibc build?)"; exit 1; }
echo "GLIBC_ versions $so needs: $(echo "$vers" | tr '\n' ' ')"
highest=$(printf '%s\n' "$vers" | tail -n 1)
if [ "$(printf '%s\n%s\n' "$highest" "$max" | sort -V | tail -n 1)" != "$max" ]; then
  echo "error: $so needs GLIBC_$highest, above GLIBC_$max"
  exit 1
fi
echo "ok: highest GLIBC_$highest <= GLIBC_$max"

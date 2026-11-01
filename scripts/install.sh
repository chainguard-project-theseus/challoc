#!/bin/sh
# Install a built libchalloc with its headers and pkg-config file.
#
#   sh scripts/install.sh [challoc|jemalloc-compat]
#
# Runs inside the dev container after the build (`make install`, `make
# install-jemalloc-compat`); scripts/jemalloc-compat.sh stages through it.
# The flavour (the argument, else $FLAVOR, default challoc) picks the
# library and the file set, with L = $DESTDIR$LIBDIR and I =
# $DESTDIR$INCLUDEDIR:
#
#   challoc          L/libchalloc.so.0          the library (its soname)
#                    L/libchalloc.so            link name -> libchalloc.so.0
#                    L/pkgconfig/challoc.pc     Libs keeps DT_NEEDED under
#                                               -Wl,--as-needed
#                    I/challoc/challoc.h        challoc's own entry points
#                    I/challoc/jemalloc.h       the je_-prefixed jemalloc API
#
#   jemalloc-compat  L/libjemalloc.so.2         the library built with the
#                                               jemalloc-soname feature
#                    L/libjemalloc.so           link name -> libjemalloc.so.2
#                    L/pkgconfig/jemalloc.pc    Version: jemalloc's 5.3.0
#                    I/jemalloc/jemalloc.h      jemalloc 5.3's unprefixed API
#
# Environment:
#   FLAVOR      challoc or jemalloc-compat, when no argument is given
#   PREFIX      installation prefix written into the .pc file; default
#               /usr/local
#   LIBDIR      default $PREFIX/lib
#   INCLUDEDIR  default $PREFIX/include
#   DESTDIR     staging root prepended to every path written, never to the
#               paths inside the .pc file; default empty
#   LIB         the built library; default target/release/libchalloc.so
#               for challoc, target/jemalloc-compat/release/libchalloc.so
#               (scripts/jemalloc-compat.sh's build) for jemalloc-compat,
#               under $CARGO_TARGET_DIR instead of target/ when that is set
#
# The installed file name is the library's DT_SONAME, read with readelf, and
# the flavour must match it: a challoc install refuses a library built with
# jemalloc's soname and the other way round.
set -eu
cd "$(dirname "$0")/.."

FLAVOR=${1:-${FLAVOR:-challoc}}
PREFIX=${PREFIX:-/usr/local}
LIBDIR=${LIBDIR:-$PREFIX/lib}
INCLUDEDIR=${INCLUDEDIR:-$PREFIX/include}
DESTDIR=${DESTDIR:-}
target=${CARGO_TARGET_DIR:-target}

die() { echo "install: $*" >&2; exit 1; }

case "$FLAVOR" in
    challoc)
        LIB=${LIB:-$target/release/libchalloc.so}
        want='libchalloc.so.*' link=libchalloc.so pc=challoc ;;
    jemalloc-compat)
        LIB=${LIB:-$target/jemalloc-compat/release/libchalloc.so}
        want=libjemalloc.so.2 link=libjemalloc.so pc=jemalloc ;;
    *) die "unknown flavour '$FLAVOR' (challoc or jemalloc-compat)" ;;
esac
case "$DESTDIR$PREFIX$LIBDIR$INCLUDEDIR" in
    *[!A-Za-z0-9/._+-]*) die "DESTDIR, PREFIX, LIBDIR and INCLUDEDIR must be plain paths" ;;
esac
case "$PREFIX" in /*) ;; *) die "PREFIX must be absolute, got '$PREFIX'" ;; esac
[ -f "$LIB" ] || die "$LIB missing: build it first (make install builds it)"

soname=$(readelf -d "$LIB" | sed -n 's/.*Library soname: \[\(.*\)\]/\1/p')
# shellcheck disable=SC2254 # want is a glob for the challoc flavour's versioned soname
case "$soname" in
    $want) ;;
    *) die "$LIB has soname '$soname', not $want: wrong build for flavour $FLAVOR" ;;
esac

# A directory under the prefix is written relative to ${prefix} in the .pc
# file, as pkg-config's own files do; anything else verbatim.
pcdir() { # pcdir DIR SUFFIX VAR
    if [ "$1" = "$PREFIX/$2" ]; then echo "\${$3}/$2"; else echo "$1"; fi
}
pc_libdir=$(pcdir "$LIBDIR" lib exec_prefix)
pc_includedir=$(pcdir "$INCLUDEDIR" include prefix)

lib=$DESTDIR$LIBDIR
inc=$DESTDIR$INCLUDEDIR
mkdir -p "$lib/pkgconfig"
rm -f "$lib/$soname" "$lib/$link" "$lib/pkgconfig/$pc.pc"
cp "$LIB" "$lib/$soname"
chmod 755 "$lib/$soname"
ln -s "$soname" "$lib/$link"

if [ "$FLAVOR" = challoc ]; then
    VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
    [ -n "$VERSION" ] || die "cannot read the version from Cargo.toml"
    headers="$inc/challoc/challoc.h $inc/challoc/jemalloc.h"
    mkdir -p "$inc/challoc"
    for h in challoc.h jemalloc.h; do
        cp "include/challoc/$h" "$inc/challoc/$h"
        chmod 644 "$inc/challoc/$h"
    done
    # Libs scopes --no-as-needed to -lchalloc: Wolfi's gcc links with
    # --as-needed, which would drop DT_NEEDED from a program that calls no
    # allocator symbol by name, and the program would run on glibc's malloc
    # (design 13.6).
    cat > "$lib/pkgconfig/challoc.pc" <<PC
prefix=$PREFIX
exec_prefix=\${prefix}
libdir=$pc_libdir
includedir=$pc_includedir

Name: challoc
Description: A memory-safe, jemalloc-compatible malloc replacement
URL: https://github.com/wolfi-dev/challoc
Version: $VERSION
Cflags: -I\${includedir}
Libs: -L\${libdir} -Wl,--push-state,--no-as-needed -lchalloc -Wl,--pop-state
PC
else
    # The version jemalloc consumers compare against (`pkg-config
    # --atleast-version`) is the one `mallctl("version")` reports, minus
    # the `-<nrev>-g<sha>` tail that pkg-config would not parse.
    VERSION=$(sed -n 's/^pub const VERSION_STR: &str = "\([0-9][0-9.]*\)-.*/\1/p' src/api/mallctl.rs)
    [ -n "$VERSION" ] || die "cannot read VERSION_STR from src/api/mallctl.rs"
    headers=$inc/jemalloc/jemalloc.h
    mkdir -p "$inc/jemalloc"
    cp include/jemalloc-compat/jemalloc/jemalloc.h "$inc/jemalloc/jemalloc.h"
    chmod 644 "$inc/jemalloc/jemalloc.h"
    cat > "$lib/pkgconfig/jemalloc.pc" <<PC
prefix=$PREFIX
exec_prefix=\${prefix}
libdir=$pc_libdir
includedir=$pc_includedir
install_suffix=

Name: jemalloc
Description: challoc, a jemalloc-compatible malloc replacement, under jemalloc's soname (libjemalloc.so.2)
URL: https://github.com/wolfi-dev/challoc
Version: $VERSION
Cflags: -I\${includedir}/jemalloc\${install_suffix}
Libs: -L\${libdir} -ljemalloc\${install_suffix}
PC
fi
chmod 644 "$lib/pkgconfig/$pc.pc"

echo "install: $FLAVOR $VERSION ($soname), prefix $PREFIX${DESTDIR:+, destdir $DESTDIR}"
# shellcheck disable=SC2086 # $headers is a list of plain paths
ls -l "$lib/$soname" "$lib/$link" "$lib/pkgconfig/$pc.pc" $headers

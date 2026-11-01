#!/bin/sh
# bench/cb/perf-build.sh: build the perf bundle that `cb launch` ships to
# every cell VM (bench/README.md, "cb launch"). Neither the bench image
# (Wolfi has no perf package) nor a Chainguard OS host (no perf in its
# repositories, #95) has perf, so cb carries its own: tools/perf from a
# pinned kernel.org release, built in a Wolfi container against glibc
# 2.44, with every shared library it needs beside it.
#
#   docker run --rm -v "$PWD:/src" -w /src cgr.dev/chainguard/wolfi-base \
#     sh bench/cb/perf-build.sh [OUTDIR]          (default target/perf)
#
# Native architecture only (run it on the arm64 and the amd64 dev VM). The
# result is OUTDIR/perf-<kver>-<arch>.tar.gz: bin/perf (DT_RPATH
# $ORIGIN/../lib, so the bundled libraries win wherever it is unpacked)
# and lib/ (libdw, libelf, libtraceevent and their compressors). It runs
# in the bench image (mounted at /opt/perf, on PATH for every row) and on
# a Chainguard OS host itself (same glibc). Upload it with
# `sh bench/cb/perf-upload.sh OUTDIR/perf-*.tar.gz`, which pins its object
# and sha256 in bench/internal-image.lock (perf-<arch>).
#
# Features: libelf/libdw (symbols, DWARF unwinding, annotate), libtraceevent
# (tracepoints), zstd/zlib (compressed records), the jevents tables (named
# Neoverse and Intel events, arm_spe). Left out: python and perl
# scripting, the TUI, libbpf skeletons, LLVM/capstone disassembly,
# debuginfod (it brings in the whole curl stack).
set -eu
KVER=${KVER:-7.2.9}
KSHA=${KSHA:-b4c5dfbe51a364a6c7f03869200f88c8e1f77403539005f14b7fc6bc91b8d8ba}
OUT=${1:-target/perf}
case $(uname -m) in aarch64) ARCH=arm64 ;; x86_64) ARCH=amd64 ;; *) echo "perf-build: unsupported $(uname -m)" >&2; exit 2 ;; esac

apk add --no-cache build-base bash curl xz gnutar flex bison bc perl python-3.13 python-3.13-dev patchelf \
  elfutils-dev zlib-dev zstd-dev xz-dev bzip2-dev libtraceevent-dev libcap-dev linux-headers >/dev/null

W=$(mktemp -d)
curl -sfL -o "$W/linux.tar.xz" "https://cdn.kernel.org/pub/linux/kernel/v${KVER%%.*}.x/linux-$KVER.tar.xz"
echo "$KSHA  $W/linux.tar.xz" | sha256sum -c - >/dev/null || { echo "perf-build: linux-$KVER.tar.xz sha256 mismatch" >&2; exit 1; }
tar -xJf "$W/linux.tar.xz" -C "$W" "linux-$KVER/tools" "linux-$KVER/include" "linux-$KVER/arch" "linux-$KVER/scripts" "linux-$KVER/Makefile"

mkdir -p "$W/build"
make -C "$W/linux-$KVER/tools/perf" -j"$(nproc)" O="$W/build" prefix=/usr PYTHON="$(command -v python3 || command -v python3.13)" \
  NO_LIBPYTHON=1 NO_LIBPERL=1 NO_SLANG=1 NO_GTK2=1 NO_LIBBPF=1 BUILD_BPF_SKEL=0 \
  NO_LIBUNWIND=1 NO_LIBNUMA=1 NO_LIBBABELTRACE=1 NO_CAPSTONE=1 NO_LIBLLVM=1 \
  NO_SDT=1 NO_JVMTI=1 NO_LIBDEBUGINFOD=1 NO_LIBPFM4=1 NO_SHELLCHECK=1 WERROR=0 >"$W/build.log" 2>&1 \
  || { tail -40 "$W/build.log"; exit 1; }

B=$W/bundle
mkdir -p "$B/bin" "$B/lib"
cp "$W/build/perf" "$B/bin/perf"
strip "$B/bin/perf"
# Every library ld.so resolves for perf except glibc's own, which the
# host provides (2.44 in the bench image and on Chainguard OS). Wolfi has
# no ldd; the loader lists them itself.
for LDSO in /lib/ld-linux-* /lib64/ld-linux-* /usr/lib/ld-linux-*; do [ -e "$LDSO" ] && break; done
"$LDSO" --list "$B/bin/perf" | awk '$3 ~ /^\// { print $3 }' | while read -r lib; do
  case ${lib##*/} in
    libc.so*|libm.so*|libpthread.so*|libdl.so*|librt.so*|ld-linux*|libresolv.so*) ;;
    *) cp -L "$lib" "$B/lib/" ;;
  esac
done
# shellcheck disable=SC2016 # $ORIGIN is the loader's, not the shell's
patchelf --force-rpath --set-rpath '$ORIGIN/../lib' "$B/bin/perf"
# shellcheck disable=SC2016 # $ORIGIN is the loader's, not the shell's
for l in "$B"/lib/*; do [ -f "$l" ] || continue; patchelf --force-rpath --set-rpath '$ORIGIN' "$l"; done
"$B/bin/perf" version >"$B/VERSION"
{ echo "linux-$KVER sha256 $KSHA"; "$B/bin/perf" version --build-options; ls "$B/lib"; } >"$B/BUILD"
mkdir -p "$OUT"
NAME=perf-$KVER-$ARCH.tar.gz
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -C "$B" -czf "$OUT/$NAME" bin lib VERSION BUILD
echo "perf-build: $OUT/$NAME $(sha256sum "$OUT/$NAME" | cut -d' ' -f1) ($(cat "$B/VERSION"); $(find "$B/lib" -mindepth 1 -maxdepth 1 | wc -l) libraries)"
rm -rf "$W"

#!/bin/bash
# ci/vng-cell.sh: the floor cells of the per-PR workflow. A kernel.org
# stable kernel boots under virtme-ng (KVM on the x86_64 runner) with the
# Wolfi test image as its root filesystem, and ci/vng-guest.sh runs the
# test binaries and the cgroup programs directly on that kernel, with no
# Docker in between.
#
#   ci/vng-cell.sh resolve SERIES   print version=<latest SERIES.y> (5.10 -> 5.10.N)
#   ci/vng-cell.sh kernel VERSION   download and build .ci/kernel/VERSION/bzImage
#   ci/vng-cell.sh run VERSION      build the tests in $IMAGE, export the rootfs, boot
#
# The kernel config is virtme-ng's (`vng --kconfig`) with the options the
# allocator probes merged in (ci/vng-kernel.config); options a kernel does
# not have (ANON_VMA_NAME before 5.17, say) fall away in olddefconfig and
# are listed. The workflow
# caches the bzImage by version and config hash, so `kernel` runs once per
# stable release.
set -euo pipefail
cmd=${1:?usage: ci/vng-cell.sh resolve|kernel|run ARG}; arg=${2:?missing argument}
root=$PWD
cdn=https://cdn.kernel.org/pub/linux/kernel

major() { echo "v${1%%.*}.x"; }

case $cmd in
resolve)
  latest=$(curl -fsSL "$cdn/$(major "$arg")/" \
    | { grep -oE "linux-${arg//./\\.}\\.[0-9]+\\.tar\\.xz" || true; } \
    | sed -e 's/^linux-//' -e 's/\.tar\.xz$//' | sort -uV | tail -n 1)
  [ -n "$latest" ] || { echo "::error::no $arg.y release on $cdn" >&2; exit 1; }
  echo "version=$latest"
  ;;

kernel)
  ver=$arg; out=$root/.ci/kernel/$ver
  mkdir -p "$out" .ci/src
  src=$root/.ci/src/linux-$ver
  if [ ! -d "$src" ]; then
    curl -fsSL "$cdn/$(major "$ver")/linux-$ver.tar.xz" | tar -xJ -C .ci/src
  fi
  cd "$src"
  # virtme-ng's config (its --configitem is newer than Ubuntu's 1.22),
  # then ours merged on top.
  # gcc-11, the compiler of the 5.10..5.15 era: the runner's gcc 13
  # fails the older trees' -Werror host tools (objtool's
  # -Wuse-after-free in 5.14).
  cc=(CC=gcc-11 HOSTCC=gcc-11)
  vng --kconfig
  scripts/kconfig/merge_config.sh -m .config "$root/ci/vng-kernel.config"
  make "${cc[@]}" olddefconfig >/dev/null
  sed -n 's/^\(CONFIG_[A-Z0-9_]*\)=y$/\1/p' "$root/ci/vng-kernel.config" | while read -r opt; do
    grep -q "^$opt=y" .config || echo "$opt: not in linux-$ver"
  done
  make "${cc[@]}" -j"$(nproc)" bzImage
  cp arch/x86/boot/bzImage "$out/bzImage"
  cp .config "$out/config"
  echo "built $out/bzImage"
  ;;

run)
  ver=$arg; kernel=$root/.ci/kernel/$ver/bzImage
  : "${IMAGE:?IMAGE must name the image built by .github/actions/ci-image}"
  [ -f "$kernel" ] || { echo "::error::$kernel missing (ci/vng-cell.sh kernel $ver)"; exit 1; }
  # The release library, the jemalloc-compat stage and the unit-test
  # binary (the guest has no network, so no cargo runs there), built in the image with
  # target/ in the checkout, so the guest sees them at the paths the
  # binary was built for (/src, CARGO_MANIFEST_DIR). The guest's 9p mount
  # writes as the runner user: hand the tree back to it.
  docker run --rm -v "$root":/src -w /src "$IMAGE" sh -c '
      cargo build --release \
      && sh scripts/jemalloc-compat.sh \
      && cargo test --no-run --message-format=json \
         | sed -n "s/.*\"executable\":\"\([^\"]*\)\".*/\1/p" | grep "/deps/challoc-" | tail -n 1 >target/ci-testbin \
      && test -s target/ci-testbin && cat target/ci-testbin'
  sudo chown -R "$(id -u):$(id -g)" "$root/target"
  # The guest's root: the same image, plus what virtme-ng's init runs
  # (bash, mount, ip, setsid, udevd for /dev/virtio-ports; Wolfi's busybox
  # has none of them) and a poweroff (Wolfi packages none) through sysrq.
  rootfs=$root/.ci/rootfs
  if [ ! -d "$rootfs/usr" ]; then
    mkdir -p "$rootfs"
    cid=$(docker run -d "$IMAGE" sh -c 'apk add --no-cache bash mount umount util-linux-misc iproute2 eudev >/dev/null')
    docker wait "$cid" >/dev/null
    docker export "$cid" | tar -x -C "$rootfs" --exclude='dev/*'
    docker rm -f "$cid" >/dev/null
    printf '#!/bin/sh\nsync\necho o >/proc/sysrq-trigger\nsleep 10\n' >"$rootfs/usr/sbin/poweroff"
    chmod +x "$rootfs/usr/sbin/poweroff"
  fi
  rm -f "$root/.ci/vng-status"
  timeout 1500 vng --verbose -r "$kernel" --root "$rootfs" --user root \
    --cpus "$(nproc)" --memory 4G --rwdir "/src=$root" \
    --exec "bash /src/ci/vng-guest.sh" 2>&1 | tee "$root/.ci/vng-$ver.log" || true
  status=$(cat "$root/.ci/vng-status" 2>/dev/null || echo "none")
  echo "guest status: $status"
  [ "$status" != 0 ] || exit 0
  [ "$status" = 1 ] || exit 1
  # Every step ran (vng-guest.sh does not stop at a failure); pass only if
  # the failed ones are unit-test groups (libtest exits 101) whose
  # failures are all known.
  if grep -a '^=== FAILED (' "$root/.ci/vng-$ver.log" | grep -avq '^=== FAILED (101): '; then
    echo "::error::a guest step other than a unit-test group failed"
    exit 1
  fi
  bash "$root/ci/known-failures.sh" "$root/.ci/vng-$ver.log"
  ;;

*) echo "unknown command $cmd" >&2; exit 2 ;;
esac

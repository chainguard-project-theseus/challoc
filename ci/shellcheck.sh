#!/bin/sh
# ci/shellcheck.sh: lint every shell script in the tree (the lint check):
# ci/, verus/, scripts/, tests/ (with tests/loom and tests/consumers),
# bench/ (the build script, cb, the drivers and the probes) and the CI
# services. .shellcheckrc at the root lets it follow the drivers'
# `. "$(dirname "$0")/cb.sh"` includes. Run it from the repository root.
# Wolfi does not package the linter, so this fetches the pinned upstream
# release and checks its SHA-256 (the digests GitHub publishes for the
# release assets) before running it.
set -eu
version=v0.11.0
case $(uname -m) in
  x86_64) sum=8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198 ;;
  aarch64) sum=12b331c1d2db6b9eb13cfca64306b1b157a86eb69db83023e261eaa7e7c14588 ;;
  *) echo "unsupported machine $(uname -m)" >&2; exit 1 ;;
esac
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
tarball=$dir/shellcheck.tar.xz
curl -sSfL -o "$tarball" \
  "https://github.com/koalaman/shellcheck/releases/download/$version/shellcheck-$version.linux.$(uname -m).tar.xz"
echo "$sum  $tarball" | sha256sum -c -
tar -xJf "$tarball" -C "$dir"
"$dir/shellcheck-$version/shellcheck" --version
"$dir/shellcheck-$version/shellcheck" \
  ci/*.sh verus/*.sh scripts/*.sh \
  tests/*.sh tests/loom/*.sh tests/consumers/*.sh \
  bench/*.sh bench/cb/*.sh bench/drivers/*.sh bench/probes/*.sh \
  .chainguard/ci/services/*/*.sh

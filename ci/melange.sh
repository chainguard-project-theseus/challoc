#!/bin/sh
# ci/melange.sh: build the Wolfi package definition with melange and check
# it (contrib/wolfi/README.md, "Building locally").
#
# contrib/wolfi/challoc.yaml defines four packages: challoc
# (libchalloc.so.0), challoc-dev, challoc-jemalloc-compat (libjemalloc.so.2)
# and challoc-jemalloc-compat-dev. This script builds them from this tree
# (`--build-option local-source`), runs the definition's test pipelines
# (melange test: the SONAMEs, loadability, an LD_PRELOAD run, pkg-config
# links and DT_NEEDED), and then checks the APKs' provides and depends
# against what the definition promises Wolfi's consumers:
#
#   challoc                       provides so:libchalloc.so.0
#   challoc-dev                   depends on challoc=<version>
#   challoc-jemalloc-compat       provides so:libjemalloc.so.2 and libjemalloc2
#   challoc-jemalloc-compat-dev   provides jemalloc-dev, depends on
#                                 challoc-jemalloc-compat=<version>
#
# It fails when there is no definition (a requested run must build
# something). It builds a copy of the definition at a local epoch above
# any Wolfi publishes, so melange test cannot install Wolfi's challoc in
# place of this build, and checks that it did not (below). It needs
# melange, bubblewrap (user namespaces), git and tar; the melange check's
# environment has them,
# and in a development session `apk add melange bubblewrap` does. The test/tw
# and test/pkgconf pipelines are wolfi-dev/os's, fetched at WOLFI_OS_REV.
# The output goes outside the tree (MELANGE_OUT, default
# ${TMPDIR:-/tmp}/challoc-melange), since --source-dir copies the whole tree
# into the build.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
recipe=$root/contrib/wolfi/challoc.yaml
out=${MELANGE_OUT:-${TMPDIR:-/tmp}/challoc-melange}
arch=$(uname -m)
# wolfi-dev/os at the commit whose pipelines/ this was checked against.
WOLFI_OS_REV=129c3a0be22d68cbbb6c996bebb3b7014f4d16be

echo "=== Melange: contrib/wolfi/challoc.yaml ==="
if [ ! -f "$recipe" ]; then
  echo "FAIL melange: no package definition at contrib/wolfi/challoc.yaml to build"
  exit 1
fi
for tool in melange bwrap git tar; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "FAIL melange: $tool is not installed (apk add melange bubblewrap git)"
    exit 1
  fi
done
case $out in
  "$root" | "$root"/*)
    echo "FAIL melange: MELANGE_OUT ($out) is inside the tree, which --source-dir copies into the build"
    exit 1 ;;
esac
mkdir -p "$out"

os=$out/os
if [ "$(git -C "$os" rev-parse HEAD 2>/dev/null || true)" != "$WOLFI_OS_REV" ]; then
  rm -rf "$os"
  git init -q "$os"
  git -C "$os" sparse-checkout set pipelines
  git -C "$os" fetch -q --depth 1 --filter=blob:none https://github.com/wolfi-dev/os "$WOLFI_OS_REV"
  git -C "$os" checkout -q FETCH_HEAD
fi

[ -f "$out/melange.rsa" ] || melange keygen "$out/melange.rsa"
rm -rf "$out/packages"

# melange test installs each package under test, and the definition's test
# environments install challoc-dev and the others, by bare name from both
# repositories, and the resolver takes the highest version; Wolfi publishes
# challoc. So the build is of a copy of the definition whose epoch is
# LOCAL_EPOCH, above any Wolfi will reach, and the local repository is
# listed first. After the tests, every "installing NAME (VERSION)" line
# melange printed for the four names must name this build's version, so
# a published copy that still wins (a newer upstream version) fails the
# check rather than passing on Wolfi's packages.
LOCAL_EPOCH=999999
names="challoc challoc-dev challoc-jemalloc-compat challoc-jemalloc-compat-dev"
version=$(sed -n 's/^  version: "\(.*\)"$/\1/p' "$recipe" | head -n 1)
full=$version-r$LOCAL_EPOCH
build_recipe=$out/challoc.yaml
sed 's/^  epoch: [0-9]*$/  epoch: '"$LOCAL_EPOCH"'/' "$recipe" >"$build_recipe"
if ! grep -qx "  epoch: $LOCAL_EPOCH" "$build_recipe"; then
  echo "FAIL melange: no '  epoch: N' line in contrib/wolfi/challoc.yaml to replace"
  exit 1
fi

# The build fetches crates. Where the host reaches crates.io through a
# TLS-intercepting egress proxy (a development session, a check guest),
# the build environment's own ca-certificates-bundle does not trust it,
# so cargo gets the host's bundle through melange's cache directory
# (mounted at /var/cache/melange).
mkdir -p "$out/cache"
host_ca=/etc/ssl/certs/ca-certificates.crt
: >"$out/build.env"
if [ -f "$host_ca" ]; then
  cp "$host_ca" "$out/cache/host-ca.crt"
  echo "CARGO_HTTP_CAINFO=/var/cache/melange/host-ca.crt" >"$out/build.env"
fi

echo "=== melange build ==="
melange build "$build_recipe" --arch "$arch" \
  --source-dir "$root" --build-option local-source \
  --cache-dir "$out/cache" --env-file "$out/build.env" \
  --pipeline-dir "$os/pipelines" \
  --repository-append https://packages.wolfi.dev/os \
  --keyring-append https://packages.wolfi.dev/os/wolfi-signing.rsa.pub \
  --signing-key "$out/melange.rsa" --out-dir "$out/packages" --runner bubblewrap

echo "=== melange test ==="
rc=0
melange test "$build_recipe" --arch "$arch" \
  --pipeline-dirs "$os/pipelines" \
  --repository-append "$out/packages" --keyring-append "$out/melange.rsa.pub" \
  --repository-append https://packages.wolfi.dev/os \
  --keyring-append https://packages.wolfi.dev/os/wolfi-signing.rsa.pub \
  --runner bubblewrap >"$out/test.log" 2>&1 || rc=$?
cat "$out/test.log"
[ "$rc" -eq 0 ] || { echo "FAIL melange: melange test exited $rc"; exit 1; }

echo "=== The tests ran on this build ==="
bad=0
# A name no test environment installs is listed; at least one must be
# installed.
tested=0
for n in $names; do
  installed=$(sed -n "s/.* INFO installing $n (\([^)]*\))\$/\1/p" "$out/test.log" | sort -u)
  if [ -z "$installed" ]; then
    echo "note: no test environment installs $n"
    continue
  fi
  tested=1
  if [ "$installed" != "$full" ]; then
    echo "FAIL melange: melange test installed $n $(printf '%s' "$installed" | tr '\n' ' '), not this build's $full"
    bad=1
  else
    echo "PASS $n: every test environment installed $full"
  fi
done
if [ "$tested" -eq 0 ]; then
  echo "FAIL melange: melange test installed none of the four packages"
  bad=1
fi
[ "$bad" -eq 0 ] || exit 1

echo "=== Provides and depends ==="
# pkginfo NAME: the .PKGINFO of NAME's APK.
pkginfo() {
  apk_file=$out/packages/$arch/$1-$full.apk
  [ -f "$apk_file" ] || { echo "FAIL melange: no $1-$full.apk in $out/packages/$arch"; return 1; }
  # tar warns about the APK-TOOLS.checksum.* extended headers it skips.
  tar -xzOf "$apk_file" .PKGINFO 2>/dev/null
}
# has NAME LINE: NAME's .PKGINFO has a line that starts with LINE.
has() {
  if pkginfo "$1" | grep -q -- "^$2"; then
    echo "PASS $1: $2"
  else
    echo "FAIL $1: no '$2' in .PKGINFO"
    bad=1
  fi
}
has challoc "provides = so:libchalloc.so.0="
has challoc-dev "depend = challoc=$full"
has challoc-jemalloc-compat "provides = so:libjemalloc.so.2="
has challoc-jemalloc-compat "provides = libjemalloc2=$full"
has challoc-jemalloc-compat-dev "provides = jemalloc-dev=$full"
has challoc-jemalloc-compat-dev "depend = challoc-jemalloc-compat=$full"
[ "$bad" -eq 0 ] || exit 1
echo "PASS melange: the four packages build, pass their tests and carry their provides and depends"

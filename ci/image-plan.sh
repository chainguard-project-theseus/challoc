#!/bin/bash
# ci/image-plan.sh STAGE: the key=value lines (for $GITHUB_OUTPUT) of
# .github/actions/ci-image: the architecture, the Dockerfile.dev target to
# build, the local image tag and the cache scope. A Dockerfile.dev without
# a `FROM ... AS STAGE` line (the single-stage file) builds its last stage.
set -euo pipefail
stage=${1:-test}
case $(uname -m) in
  x86_64) arch=amd64 ;;
  aarch64) arch=arm64 ;;
  *) echo "unsupported machine $(uname -m)" >&2; exit 1 ;;
esac
target=
if grep -Eiq "^FROM .* AS $stage\$" Dockerfile.dev; then
  target=$stage
else
  echo "::notice::Dockerfile.dev has no '$stage' stage; building the full image" >&2
fi
echo "arch=$arch"
echo "target=$target"
echo "image=challoc-ci-${target:-full}:$arch"
echo "scope=${target:-full}-$arch"

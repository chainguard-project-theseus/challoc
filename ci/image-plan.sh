#!/bin/bash
# ci/image-plan.sh STAGE: the key=value lines (for $GITHUB_OUTPUT) of
# .github/actions/ci-image: the architecture, the Dockerfile.dev target to
# build, the local image tag and the cache scope.
set -euo pipefail
stage=${1:-test}
case $(uname -m) in
  x86_64) arch=amd64 ;;
  aarch64) arch=arm64 ;;
  *) echo "unsupported machine $(uname -m)" >&2; exit 1 ;;
esac
grep -Eiq "^FROM .* AS $stage\$" Dockerfile.dev || { echo "::error::Dockerfile.dev has no '$stage' stage" >&2; exit 1; }
echo "arch=$arch"
echo "target=$stage"
echo "image=challoc-ci-$stage:$arch"
echo "scope=$stage-$arch"

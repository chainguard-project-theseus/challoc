#!/bin/bash
# ci/image-build.sh STAGE: build one stage of Dockerfile.dev into the
# check guest's Docker daemon as challoc-ci-<stage>:<arch> and print the
# key=value lines the cell scripts read (arch, image). The Makefile
# targets then run with IMAGE=<that tag> and IMAGE_BUILD= (ci/make.sh), so
# `make` never rebuilds the image itself. A guest starts with an empty
# daemon, so every check builds its stage once.
set -euo pipefail
stage=${1:-test}
case $(uname -m) in
  x86_64) arch=amd64 ;;
  aarch64) arch=arm64 ;;
  *) echo "unsupported machine $(uname -m)" >&2; exit 1 ;;
esac
grep -Eiq "^FROM .* AS $stage\$" Dockerfile.dev || { echo "error: Dockerfile.dev has no '$stage' stage" >&2; exit 1; }
image=challoc-ci-$stage:$arch
docker build -f Dockerfile.dev --target "$stage" -t "$image" . >&2
echo "arch=$arch"
echo "image=$image"

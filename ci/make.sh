#!/bin/bash
# ci/make.sh LOG TARGET [VAR=value...]: run one Makefile target the way CI
# does and tee its output to LOG.
#
#   - IMAGE (from the environment, .github/actions/ci-image) is used as it
#     is (`IMAGE_BUILD=`; `-o image` as well, so make never considers
#     rebuilding it).
#   - target/ is a bind mount of .ci/target on the runner (TARGET_VOLUME is
#     an absolute path), so the artifacts are visible to later steps
#     (readelf, the vng guest, upload-artifact).
#
# The other arguments (QUANTUM=8, EXTRA_DOCKER_FLAGS=..., more targets,
# make options) go to make unchanged.
set -euo pipefail
log=$1; target=$2; shift 2
: "${IMAGE:?IMAGE must name the image built by .github/actions/ci-image}"
mkdir -p "$(dirname "$log")" .ci/target .ci/cargo
make -o image "$target" IMAGE="$IMAGE" IMAGE_BUILD= \
  TARGET_VOLUME="$PWD/.ci/target" CARGO_VOLUME="$PWD/.ci/cargo" "$@" 2>&1 | tee "$log"

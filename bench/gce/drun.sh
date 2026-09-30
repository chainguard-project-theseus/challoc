# bench/gce/drun.sh: the one `docker run` line of the scripts that run on a
# VM host beside the dev container (quick-perf.sh, perf-record.sh).
# Source it (bash) after T is set to arm64|amd64:
#
#   T=arm64; . bench/gce/drun.sh
#   drun sh -c '...'                       # docker run --rm <mounts> $IMAGE sh -c '...'
#   docker run -d "${DRUN_ARGS[@]}" "$IMAGE" sh -c '...'   # a detached container, the same mounts
#
# The repository is mounted at /src (the working directory), the
# architecture's target volume at /src/target. The Makefile's DOCKER_RUN
# (`make print-docker-run`) is the container-side twin and adds --platform,
# the cargo registry volume and, in a git worktree, the common .git mount;
# a VM host holds a plain checkout and these scripts run no cargo step, so
# they never needed those.
#
#   REPO           the repository (default: this file's ../..)
#   TARGET_VOLUME  default challoc-target-$T (a lane's TARGET_VOLUME applies)
#   IMAGE          default challoc-dev:$T
REPO=${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}
IMAGE=${IMAGE:-challoc-dev:$T}
# target/results is the host directory over the volume (as in the Makefile).
mkdir -p "$REPO/target/results"
DRUN_ARGS=(-v "$REPO":/src -w /src -v "${TARGET_VOLUME:-challoc-target-$T}":/src/target
  -v "$REPO/target/results":/src/target/results)
drun() { docker run --rm "${DRUN_ARGS[@]}" "$IMAGE" "$@"; }

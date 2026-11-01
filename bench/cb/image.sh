#!/bin/sh
# Build the cb bench image for one or more architectures, push it to
# Artifact Registry and record the digests in bench/internal-image.lock
# (bench/README.md, "cb launch"). Runs on the host (docker + gcloud).
#
#   sh bench/cb/image.sh [arm64] [amd64]      (default: both)
#
# Environment: CB_IMAGE_REPO (default the lock's `repo`), CB_IMAGE_LOCK
# (default bench/internal-image.lock),
# NO_PUSH=1 builds only. amd64 on an Apple-silicon host is emulated: it
# takes a while (bench/build.sh compiles mimalloc-bench and redis), which
# is why the result is pushed and pinned instead of rebuilt per run.
set -eu
cd "$(dirname "$0")/../.."
LOCK=${CB_IMAGE_LOCK:-bench/internal-image.lock}
REPO=${CB_IMAGE_REPO:-$(sed -n 's/^repo *= *"\(.*\)"/\1/p' "$LOCK" 2>/dev/null || true)}
[ -n "$REPO" ] || { echo "image.sh: no repository: set CB_IMAGE_REPO (or a repo line in $LOCK)" >&2; exit 2; }
ARCHES=${*:-arm64 amd64}
REV=$(git rev-parse --short=12 HEAD)
REGISTRY=${REPO%%/*}

if [ -z "${NO_PUSH:-}" ]; then
  gcloud auth print-access-token | docker login -u oauth2accesstoken --password-stdin "https://$REGISTRY" >/dev/null
fi

for arch in $ARCHES; do
  echo "== $arch: Dockerfile.dev test stage"
  docker build --platform "linux/$arch" -f Dockerfile.dev --target test -t "cb-dev-test:$arch" .
  echo "== $arch: bench/Dockerfile.cb"
  docker build --platform "linux/$arch" -f bench/Dockerfile.cb --build-arg "DEV_IMAGE=cb-dev-test:$arch" \
    --label "org.opencontainers.image.revision=$REV" -t "cb-bench:$arch" -t "$REPO:$arch-$REV" .
  [ -n "${NO_PUSH:-}" ] && continue
  docker push -q "$REPO:$arch-$REV" >/dev/null
  digest=$(docker image inspect -f '{{range .RepoDigests}}{{println .}}{{end}}' "$REPO:$arch-$REV" | grep "^$REPO@" | head -1)
  [ -n "$digest" ] || { echo "no repo digest for $REPO:$arch-$REV" >&2; exit 1; }
  echo "== $arch: $digest"
  # Rewrite this architecture's line of the lock, keep the others.
  tmp=$(mktemp)
  { [ -f "$LOCK" ] && grep -v "^$arch *=" "$LOCK" || printf '# The cb bench image per architecture, by digest (bench/cb/image.sh).\nrepo = "%s"\n' "$REPO"; } > "$tmp"
  printf '%s = "%s"  # %s %s\n' "$arch" "$digest" "$REV" "$(date -u +%Y-%m-%d)" >> "$tmp"
  mv "$tmp" "$LOCK"
done
[ -z "${NO_PUSH:-}" ] && cat "$LOCK"
exit 0

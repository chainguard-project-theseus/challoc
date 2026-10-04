#!/bin/sh
# Upload perf bundles built by bench/cb/perf-build.sh to the bench bucket
# and pin them in bench/internal-image.lock (bench/README.md, "cb launch").
# Runs on the host (gcloud).
#
#   sh bench/cb/perf-upload.sh target/perf/perf-<kver>-<arch>.tar.gz ...
#
# Each bundle goes to gs://<bucket>/tools/perf-<kver>-<arch>.<sha16>.tar.gz
# (never overwritten: the name carries the hash) and the lock's
# `perf-<arch>` line becomes `<object>=<sha256>`; cb launch passes it to
# every VM of that architecture as the cb-perf metadata, and boot.sh
# refuses bytes with another hash. Environment: CB_BUCKET (default the
# `bucket` of bench/cells/internal-project.toml), CB_IMAGE_LOCK.
set -eu
cd "$(dirname "$0")/../.."
LOCK=${CB_IMAGE_LOCK:-bench/internal-image.lock}
BUCKET=${CB_BUCKET:-$(sed -n 's/^bucket *= *"\(.*\)"/\1/p' bench/cells/internal-project.toml 2>/dev/null | head -1)}
[ -n "$BUCKET" ] || { echo "perf-upload: no bucket: set CB_BUCKET" >&2; exit 2; }
[ $# -gt 0 ] || { sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
for f in "$@"; do
  base=$(basename "$f" .tar.gz)
  arch=${base##*-}
  case $arch in arm64|amd64) ;; *) echo "perf-upload: $f: no -arm64/-amd64 suffix" >&2; exit 2 ;; esac
  sha=$(shasum -a 256 "$f" 2>/dev/null || sha256sum "$f")
  sha=${sha%% *}
  obj=tools/$base.$(printf %.16s "$sha").tar.gz
  gcloud storage cp -q "$f" "gs://$BUCKET/$obj"
  tmp=$(mktemp)
  grep -v "^perf-$arch *=" "$LOCK" > "$tmp" || true
  printf 'perf-%s = "%s=%s"  # %s %s\n' "$arch" "$obj" "$sha" "$base" "$(date -u +%Y-%m-%d)" >> "$tmp"
  mv "$tmp" "$LOCK"
  echo "perf-upload: $f -> gs://$BUCKET/$obj"
done
cat "$LOCK"

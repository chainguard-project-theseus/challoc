#!/bin/bash
# ci/floor.sh CELLS: the kernel floor checks (.chainguard/ci/floor.yaml).
# `cb launch exp-floor` on the named GCE cells (bench/cells/public.toml):
# one spot VM per cell boots the cell's public distribution image, runs the
# floor-test row (`make test`, the cgroup steps in child cgroups under the
# privileged profile, and `make fuzz`) on this commit in the dev container,
# and is deleted when its result lands (max-run-duration deletes it if the
# launcher dies). The launch's report is the check's verdict: PASS (0) when
# every cell finished and passed.
#
# The check holds the challoc-gce workload identity
# (.chainguard/ci/identities.yaml): GOOGLE_APPLICATION_CREDENTIALS is an
# external-account ADC file for the federated principal, whose token the
# guest only holds as a sealed stand-in (the egress proxy swaps in the real
# one in Authorization headers to *.googleapis.com). gcloud introspects a
# bare external account's token to name it, which the stand-in fails, so
# this writes a copy that impersonates the floor service account
# (FLOOR_SA; the principal may impersonate it and nothing else, see
# docs/runbooks/checks-gce-identity.md) and points gcloud at it. The proxy
# also re-signs TLS, and gcloud keeps its own CA setting.
set -euo pipefail
cells=${1:?usage: ci/floor.sh CELLS}
: "${GOOGLE_APPLICATION_CREDENTIALS:?the check must hold the challoc-gce identity}"
export CLOUDSDK_CORE_CUSTOM_CA_CERTS_FILE=${SSL_CERT_FILE:-/etc/ssl/certs/ca-certificates.crt}
export CLOUDSDK_CORE_DISABLE_PROMPTS=1
: "${FLOOR_SA:?the check names the floor service account}"
# Where cb launch runs: the floor service account's project, its bench
# bucket and its VMs' service account, named by convention (the runbook).
project=${FLOOR_SA#*@}
project=${project%%.*}
export CB_PROJECT=${CB_PROJECT:-$project}
export CB_BUCKET=${CB_BUCKET:-$project-challoc-bench}
export CB_SERVICE_ACCOUNT=${CB_SERVICE_ACCOUNT:-cb-bench-vm@$project.iam.gserviceaccount.com}
adc=$(mktemp)
python3 - "$GOOGLE_APPLICATION_CREDENTIALS" "$FLOOR_SA" "$adc" <<'PY'
import json, sys
src, sa, out = sys.argv[1:]
d = json.load(open(src))
d["service_account_impersonation_url"] = (
    "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/"
    f"{sa}:generateAccessToken")
json.dump(d, open(out, "w"))
PY
export CLOUDSDK_AUTH_CREDENTIAL_FILE_OVERRIDE=$adc
# The workspace's .git is a view the platform assembled, owned by another
# account; cb runs git archive over it.
git config --global --add safe.directory '*'
# exp-floor is a smoke suite with the one variant `head`; --base names a
# commit the workspace has so nothing resolves `main`.
status=0
python3 bench/cb launch exp-floor --cells "$cells" --head HEAD --base HEAD \
  --out .ci/floor || status=$?
# Each cell's floor-test summary (the `floor:` lines: kernel, steps, SKIPs)
# from the VM logs cb fetched.
find .ci/floor -path '*/logs/*' -name '*.out' -exec grep -H '^floor: ' {} + 2>/dev/null || true
[ ! -f .ci/floor/report.md ] || cat .ci/floor/report.md
exit "$status"

# Runbook: the `challoc-gce` identity of the floor checks

The floor checks (`.chainguard/ci/floor.yaml`) launch GCE VMs with `cb
launch exp-floor`, so they hold a GCP credential: the `challoc-gce`
workload identity of `.chainguard/ci/identities.yaml`. This is the GCP
side of that identity, in the bench project (`dlorenc-dev`, number
505132528589, the project `bench/cells/internal-project.toml` names), and
how it was brought up.

## What exists

- Workload identity pool `challoc-checks`, provider `workflow`: issuer
  `https://issuer.enforce.dev` (the Chainguard workflow issuer), allowed
  audience
  `https://iam.googleapis.com/projects/505132528589/locations/global/workloadIdentityPools/challoc-checks/providers/workflow`,
  `google.subject = assertion.sub`.
- The check's token subject is `chainguard:<repository-id>:challoc-gce`.
  The repository ID is a random 32-hex-character ID the Checks platform
  allocates the first time it issues one of this repository's identities;
  nothing shows it before then.
- Data-access audit logs for `sts.googleapis.com` on the project, so a
  token exchange records its subject.

The guest's ADC file uses the federated principal directly (no service
account impersonation), so the roles go to the principal
`principal://iam.googleapis.com/projects/505132528589/locations/global/workloadIdentityPools/challoc-checks/subject/chainguard:<repository-id>:challoc-gce`.

## Bring-up (once, after identities.yaml lands on main)

Done on 2026-10-12: the repository's subject is
`chainguard:90d0c3d145808bf716af24faab4b19aa:challoc-gce`, the provider
is pinned to it, the roles below are granted, and `floor` blocks. Repeat
these steps only for a recreated repository (it gets a new ID) or a new
identity.

The provider starts with a provisional condition,
`assertion.sub.startsWith('chainguard:') && assertion.sub.endsWith(':challoc-gce')`,
and no IAM grant names the principal, so a token that passes it can
exchange for a federated token that can do nothing. The first floor run
after the merge fails on a permission error; then:

1. Read the subject from the exchange:

   ```sh
   gcloud logging read --project=dlorenc-dev --freshness=1d \
     'protoPayload.serviceName="sts.googleapis.com"' \
     --format='value(protoPayload.authenticationInfo.principalSubject)' | sort -u
   ```

2. Pin the provider to that exact subject:

   ```sh
   SUBJECT=chainguard:<repository-id>:challoc-gce
   gcloud iam workload-identity-pools providers update-oidc workflow \
     --project=dlorenc-dev --location=global --workload-identity-pool=challoc-checks \
     --attribute-condition="assertion.sub == '$SUBJECT'"
   ```

3. Grant the principal what `cb launch` needs, and nothing else:

   ```sh
   P=principal://iam.googleapis.com/projects/505132528589/locations/global/workloadIdentityPools/challoc-checks/subject/$SUBJECT
   gcloud projects add-iam-policy-binding dlorenc-dev --member="$P" \
     --role=roles/compute.instanceAdmin.v1 --condition=None
   gcloud iam service-accounts add-iam-policy-binding \
     cb-bench-vm@dlorenc-dev.iam.gserviceaccount.com --project=dlorenc-dev \
     --member="$P" --role=roles/iam.serviceAccountUser
   gcloud storage buckets add-iam-policy-binding gs://dlorenc-dev-challoc-bench \
     --member="$P" --role=roles/storage.objectAdmin
   ```

4. Re-run the floor check (GitHub's Re-run on a pull request), and once it
   passes, delete `advisory: true` from `floor` in floor.yaml.

## Who gets it

`ring: 1.member`: pull requests opened by chainguard-dev members (and
Dependabot), and pushes to main. The floor checks also skip any pull
request whose head is a fork, so an outside contributor's pull request
never asks for the credential. To require review before a member's pull
request can launch VMs, move the identity to `ring: 2` (released to a pull
request by the platform's two-person approval, once that lands).

# Runbook: the `challoc-gce` identity of the floor checks

The floor checks (`.chainguard/ci/floor.yaml`) launch GCE VMs with `cb
launch exp-floor`, so they hold a GCP credential: the `challoc-gce`
workload identity of `.chainguard/ci/identities.yaml`. This is the GCP
side of that identity, in the bench project (`dlorenc-dev`, number
505132528589), and how to set it up for a new or recreated repository.

## What exists

- Workload identity pool `challoc-checks`, provider `workflow`: issuer
  `https://issuer.enforce.dev` (the Chainguard workflow issuer), allowed
  audience
  `https://iam.googleapis.com/projects/505132528589/locations/global/workloadIdentityPools/challoc-checks/providers/workflow`,
  `google.subject = assertion.sub`, and an attribute condition pinned to
  this repository's exact subject,
  `chainguard:90d0c3d145808bf716af24faab4b19aa:challoc-gce`.
- The check's token subject is `chainguard:<repository-id>:challoc-gce`.
  The repository ID is a random 32-hex-character ID the Checks platform
  allocates the first time it issues one of this repository's identities;
  nothing shows it before then, and a recreated repository gets a new one.
- Data-access audit logs for `sts.googleapis.com` on the project, so a
  token exchange records its subject.
- Service account `challoc-floor@dlorenc-dev.iam.gserviceaccount.com`
  (`FLOOR_SA` in floor.yaml) holds the roles `cb launch` needs. The
  federated principal
  `principal://iam.googleapis.com/projects/505132528589/locations/global/workloadIdentityPools/challoc-checks/subject/chainguard:<repository-id>:challoc-gce`
  holds nothing but `roles/iam.workloadIdentityUser` and
  `roles/iam.serviceAccountTokenCreator` on that account.
- The bench bucket `gs://dlorenc-dev-challoc-bench` and the VMs' service
  account `cb-bench-vm@dlorenc-dev.iam.gserviceaccount.com`. `ci/floor.sh`
  derives the project from `FLOOR_SA` and these two from the project
  (`<project>-challoc-bench`, `cb-bench-vm@<project>`), and passes all
  three to `cb launch` as `CB_PROJECT`, `CB_BUCKET` and
  `CB_SERVICE_ACCOUNT`. It takes the bench image lock from
  `gs://dlorenc-dev-challoc-bench/internal-image.lock` (bench/README.md,
  "Operating on GCE"); when that object is missing, every floor VM builds
  the bench image itself.

Why the service account: the platform's ADC file uses the federated
principal directly, and the guest holds its token only as a sealed
stand-in. gcloud introspects a bare external account's token to name the
account, the stand-in fails that (`{"active": false}`), and a federated
principal cannot read public images either. So `ci/floor.sh` writes a copy
of the ADC file with `service_account_impersonation_url` set to
`FLOOR_SA` (floor.yaml) and gcloud works as that account. The cost: the
guest holds a plain one-hour token for `challoc-floor`, which can do only
what the floor needs.

## Setting it up (a new or recreated repository, or a new identity)

Start the provider with a provisional condition,
`assertion.sub.startsWith('chainguard:') && assertion.sub.endsWith(':challoc-gce')`,
and no IAM grant naming the principal, so a token that passes it can
exchange for a federated token that can do nothing. Once identities.yaml
is on main, the first floor run fails on a permission error; then:

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

3. Let the principal impersonate the floor service account, and give the
   account what `cb launch` needs:

   ```sh
   P=principal://iam.googleapis.com/projects/505132528589/locations/global/workloadIdentityPools/challoc-checks/subject/$SUBJECT
   SA=challoc-floor@dlorenc-dev.iam.gserviceaccount.com
   for role in roles/iam.workloadIdentityUser roles/iam.serviceAccountTokenCreator; do
     gcloud iam service-accounts add-iam-policy-binding $SA --project=dlorenc-dev \
       --member="$P" --role=$role
   done
   gcloud projects add-iam-policy-binding dlorenc-dev --member="serviceAccount:$SA" \
     --role=roles/compute.instanceAdmin.v1 --condition=None
   gcloud iam service-accounts add-iam-policy-binding \
     cb-bench-vm@dlorenc-dev.iam.gserviceaccount.com --project=dlorenc-dev \
     --member="serviceAccount:$SA" --role=roles/iam.serviceAccountUser
   gcloud storage buckets add-iam-policy-binding gs://dlorenc-dev-challoc-bench \
     --member="serviceAccount:$SA" --role=roles/storage.objectAdmin
   ```

4. Re-run the floor check on main (GitHub's Re-run on the postsubmit
   check of main's latest commit) and confirm it passes. While these
   steps are pending, mark `floor` `advisory: true` in floor.yaml so the
   expected failure does not block; remove it once the check passes.

## Who gets it

`ring: 1.member`, with a single `allow` entry: postsubmit runs on main.
The floor checks are postsubmit-only, so no pull request, a member's or a
fork's, asks for the credential. To require review before a member's
change can launch VMs at all, move the identity to `ring: 2` (released by
the platform's two-person approval, once that lands).

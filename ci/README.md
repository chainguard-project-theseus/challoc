# ci/

The scripts the GitHub Actions workflows call. Each one also runs by hand
on a Linux host with Docker (the floor cells need KVM and virtme-ng).

| script | what |
|---|---|
| `make.sh LOG TARGET [VAR=v...]` | one Makefile target the CI way: `IMAGE` prebuilt (`IMAGE_BUILD=`, `make -o image`), `target/` bound to `.ci/target` |
| `test-cell.sh LOG [VAR=v...]` | `make test` for a hosted cell; a failure passes only if every failed test is in `known-failures.txt`, and then every step of `make test` after the unit tests (read from its recipe) runs on its own, each failing the cell unless its failures are known |
| `known-failures.sh LOG` | every failed unit test and cgroup program of LOG is listed in `known-failures.txt` (each entry a TODO, optionally scoped to cells by `@glob`) |
| `image-plan.sh STAGE` | tag, target and cache scope for `.github/actions/ci-image` |
| `skip-summary.sh LOG MANIFEST` | the `SKIP` lines of a run against `expected-skips/<cell>.txt`: a new skip fails, a vanished one is a notice |
| `features-assert.sh LOG KEY=V...` | the `features:` line of a run must carry each KEY=V (the privileged job: `mseal=yes`) |
| `glibc-max.sh SO MAX` | SO needs no `GLIBC_` version above MAX (2.44) |
| `vng-cell.sh resolve\|latest\|kernel\|run` | the floor cells: the kernel.org release pinned in `floor-kernels.txt` (version and tarball sha256; `latest` shows what a bump would be), built with virtme-ng's config plus `vng-kernel.config`, booted under KVM with the Wolfi test image as root |
| `vng-guest.sh` | inside that guest: both unit-test groups, the cgroup-limit tests and the churn/lazy programs, each in its own child cgroup |
| `extended.sh TASK` | one task of the nightly workflow |

`expected-skips/<cell>.txt` holds the skip keys (the first word after
`SKIP`) a cell is known to print. Regenerate one with
`CI_SKIP_UPDATE=1 bash ci/skip-summary.sh <log> <manifest>`, and review
the diff: a key that appears is coverage lost.

## Workflows

- `ci.yml` (every PR, merge queue, pushes to `main` and `oss/**`): lint,
  msrv-build, linux {x86_64, aarch64} x quantum {16, 8}, privileged per
  arch, floor {5.10, 5.14} on x86_64 KVM.
- `extended.yml` (nightly, dispatch): long fuzz (default, hardened, q8),
  the 600 s oscillation soak, check-needed, the Valkey and Ruby builds.

## Not here yet, and why

- **kernels.yml** (the GCE kernel cells: 5.15, 6.1, COS, 6.8, 64k arm64,
  6.12, 7.0 and the Chainguard OS cells). Needs a CI identity in GCP
  (workload identity federation, a launcher service account); the owner
  deferred that. The cells run by hand through `bench/gce/challoc-bench
  matrix` in the dev project until then. The Chainguard OS cells stay out
  of this public tree.
- **bench.yml** (`bench:quick` / `bench:ab` labels, the nightly
  scorecard). The same identity, plus the `cb` launcher of the benchmark
  system; CI treats it as one command (`ci/bench-submit`) once it exists.
- **release.yml**, SBOM, cosign, Pages: nothing is published.
- **Miri, checked-release `.so`, valgrind** in the nightly: phase (e).

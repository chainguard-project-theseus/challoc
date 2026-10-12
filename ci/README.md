# ci/

The scripts the Chainguard Checks (`.chainguard/ci/`) call. Each check runs
in a microVM guest; the test cells start a Docker daemon there (the
`docker` service) and drive the Makefile's container targets unchanged.
Each script also runs by hand on a Linux host with Docker.

| script | what |
|---|---|
| `image-build.sh STAGE` | build a Dockerfile.dev stage as `challoc-ci-<stage>:<arch>`; prints `arch=` and `image=` |
| `cell.sh linux\|privileged` | one test cell: the image, `make test` through `test-cell.sh`, the cell's assertions, then the skip summary |
| `make.sh LOG TARGET [VAR=v...]` | one Makefile target the CI way: `IMAGE` prebuilt (`IMAGE_BUILD=`, `make -o image`), `target/` bound to `.ci/target` |
| `test-cell.sh LOG [VAR=v...]` | `make test` for a cell; a failure passes only if every failed test is in `known-failures.txt`, and then every step of `make test` after the unit tests (read from its recipe) runs on its own, each failing the cell unless its failures are known |
| `known-failures.sh LOG` | every failed unit test and cgroup program of LOG is listed in `known-failures.txt` (each entry a TODO, optionally scoped to cells by `@glob`) |
| `skip-summary.sh LOG MANIFEST` | the `SKIP` lines of a run against `expected-skips/<cell>.txt`: a new skip fails, a vanished one is a notice |
| `features-assert.sh LOG KEY=V...` | the `features:` line of a run must carry each KEY=V (the privileged cells: `mseal=yes`) |
| `glibc-max.sh SO MAX` | SO needs no `GLIBC_` version above MAX (2.44) |
| `shellcheck.sh` | shellcheck (the pinned upstream release, SHA-256 checked) over `ci/*.sh` |
| `extended.sh [TASK]` | one task of the on-request extended checks |
| `floor.sh CELLS` | `cb launch exp-floor` on GCE cells with the check's `challoc-gce` identity |

`expected-skips/<cell>.txt` holds the skip keys (the first word after
`SKIP`) a cell is known to print. Regenerate one with
`CI_SKIP_UPDATE=1 bash ci/skip-summary.sh <log> <manifest>`, and review
the diff: a key that appears is coverage lost.

## Checks

Every pull request (`Chainguard Checks (presubmit)` is the aggregate to
require); each file is a workflow:

- `lint.yaml`: rustfmt, the loom models, shellcheck, msrv-build (1.94);
  clippy is advisory until a clippy baseline.
- `test.yaml`: `linux` {x86_64, aarch64} x quantum {16, 8}, `privileged`
  per arch (`hosted-<arch>-*` cells in `expected-skips/`).
- `floor.yaml`: `floor` (AlmaLinux 9's 5.14, both arches, default and
  privileged containers) and the advisory `tripwire` (Debian 11's 5.10),
  on GCE VMs through `cb launch`; member pull requests and main only.

On request (`ci/run:<check>` on a pull request, or `chainctl checks run .
--check <check>` for any pushed commit), until Checks has a schedule:

- `extended.yaml`: `fuzz` (50M operations: default, hardened, q8),
  `oscillate` (the 600 s soak), `check-needed`, `consumers` (the Valkey and
  Ruby builds), each on both arches.

`chainctl checks run` plans a commit without a pull request, so it cannot
plan the floor checks (their condition reads the pull request); name the
checks to run with `--check`.

`identities.yaml` holds the trust policy (which pull requests get which
credentials) and the `challoc-gce` identity; its header says how to tighten
it when the repository is public.

## Not here yet, and why

- **The other GCE kernel cells** (6.1, COS, 6.8, 64k arm64, 6.12 and
  7.0). `floor.yaml` can launch any of them; they run
  by hand as `cb launch exp-floor` until there is a reason to gate on
  them.
- **Benchmarks on a label** (`bench:quick` / `bench:full`). The same
  identity and `cb launch quick`, as a requested check.
- **Release**, SBOM, cosign, Pages: nothing is published.
- **Miri, checked-release `.so`, valgrind** in the extended checks: phase
  (e).

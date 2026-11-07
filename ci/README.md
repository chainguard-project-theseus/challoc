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
| `shellcheck.sh` | shellcheck (the pinned upstream release, SHA-256 checked) over every shell script: `ci/`, `verus/`, `scripts/`, `tests/` (with `tests/loom/`, `tests/genmc/`, `tests/consumers/` and `tests/tlscheck/`), `bench/` and `.chainguard/ci/services/` |
| `extended.sh [TASK]` | one task of the on-request extended checks |
| `floor.sh CELLS` | `cb launch exp-floor` on GCE cells with the check's `challoc-gce` identity |
| `codegen.sh` | `scripts/codegen-diff.sh` from the change's merge base (`CHAINGUARD_BASE_REVISION`) to this tree, both quantum builds; exits 2 when a function changed |
| `heap-noalias.sh [q16\|q8\|self-test]...` | the release library's own LLVM IR before any LLVM pass and without LTO (one `cargo rustc --release` per quantum, both by default, with `-C no-prepopulate-passes` and `CARGO_PROFILE_RELEASE_LTO=false`, so every `Heap` method rustc emits out of line has its own `define`): no `Heap` method's `&mut self` is `noalias`; `noalias readonly` `&self` receivers are listed, not failed; fails when it finds no `Heap` method with a `self` parameter, or only `&self` ones. Every run first scans the saved `define` lines in `scripts/testdata/heap-noalias` (each file's verdict must be the one its `; want:` lines give: `red.ll` fails, `clean.ll` passes, `none.ll` and `ro-only.ll` fail); `self-test` alone runs only that |
| `kani-boolcmp-test.sh` | `scripts/kani-boolcmp.sh` (ordering comparisons on bools in Kani code) over `src/`, which must be clean, and over its fixtures in `scripts/testdata/kani-boolcmp` |
| `kani-regular.sh SHARD SHARDS` | one shard of the Kani harnesses in regular format with CBMC's verbose output; fails on a `warning: ignoring` line (the `kani-ignoring` check) |

`expected-skips/<cell>.txt` holds the skip keys (the first word after
`SKIP`) a cell is known to print. Regenerate one with
`CI_SKIP_UPDATE=1 bash ci/skip-summary.sh <log> <manifest>`, and review
the diff: a key that appears is coverage lost.

## Checks

Every pull request (`Chainguard Checks (presubmit)` is the aggregate to
require); each file is a workflow:

- `lint.yaml`: rustfmt, clippy (`-D warnings`, the release library and
  every target, quantum 16 and 8), the `ctest-cc.sh` quantum-cache race
  test (`tests/ctest-cc-race.sh`), the loom models (the slow count model
  in `loom-count`, on changes to `tests/loom/` or the code it copies)
  and the pins of the lines the loom models and the GenMC harnesses copy
  (`tests/loom/tests/pins.rs`, both sides for GenMC's),
  shellcheck, the codegen-fncmp fixtures, the Kani bool-comparison lint
  (`ci/kani-boolcmp-test.sh`), the bench harness's unit tests (`make
  bench-unit`), msrv-build (1.94).
- `test.yaml`: `linux` {x86_64, aarch64} x quantum {16, 8}, `privileged`
  per arch (`hosted-<arch>-*` cells in `expected-skips/`).
- `tlscheck.yaml`: `tlscheck` (`amd64-q16`, `arm64-q16`), the debug TLS
  check build (`RUSTFLAGS="--cfg challoc_tls_check"`, `src/heap/tlscheck.rs`)
  with clippy over every target, the release build, the jemalloc-compat
  stage, both cargo test groups and `tests/tlscheck/run.sh` (the lifecycle
  modes and a 120 s fuzz), in the guest directly; on pull requests that
  change `src/`, the build, `scripts/jemalloc-compat.sh`, the C test
  programs (`tests/c/`), `tests/tlscheck/` or `tlscheck.yaml` itself.
- `floor.yaml`: `floor` (AlmaLinux 9's 5.14, both arches, default and
  privileged containers) and the advisory `tripwire` (Debian 11's 5.10;
  off until its bench image is rebuilt, #463, see `floor.yaml`),
  on GCE VMs through `cb launch`; on pushes to main only (postsubmit), so
  a change that raises the floor shows up after it lands, not before.
- `verus.yaml`: `verus` and `verus-arm64`, the seam ledger, the proofs at quantum 16 and 8, and the coverage floor; `verus-main` the same on main.
- `isla.yaml`: `isla` (`arm64-q16`, `arm64-q8`), `make isla` (`verus/ISLA.md`) natively on arm64 at each quantum: the asm templates and the Isla seams against the ledger, then the aarch64 Isla jobs and negative controls; on pull requests that change `src/sys/**`, `src/api/cxx.rs`, `src/config.rs`, `src/rseq.rs`, the build, the Isla harness, the ledger overrides or `isla.yaml` itself. `isla-main` (`arm64-q16`, `arm64-q8`, `amd64-q16`, `amd64-q8`) runs `make isla` on both architectures after every landing on main and gates nothing; only its amd64 legs run the x86_64 half, which fetches sail-x86-from-acl2 from github.com to build the x86 model, so the presubmit `isla` needs no github.com.
- `codegen.yaml`: the advisory `codegen` and `codegen-arm64`, the machine-code diff from the merge base (`ci/codegen.sh`), and `heap-noalias` (`ci/heap-noalias.sh`), which gates: the home cache headers are written through a raw pointer while a `&mut Heap` is live, which relies on `Heap` being `!Unpin` (its `_pin` field) so that rustc emits no `noalias` for it; the check reads that off the IR at both quanta on x86_64 (`noalias` on a `&mut Heap` receiver; a `&self` receiver is `noalias readonly` and only listed).
- `perf-budget.yaml`: `perf-budget`, the hot exports' fast-path cost table on x86_64 at both quanta, and `perf-budget-main` the same on main (`scripts/perf-budget.sh`, theorem 45); a change to their machine code updates `verus/perf-budget.txt` and `verus/perf-budget-q8.txt`.
- `kani.yaml`: `kani` (`q16`, `q8`), `cargo kani` over every harness (`src/kani/` and the `kani_*` modules of other files; `verus/README.md`, "Kani") at each quantum, on x86_64; on pull requests that change `src/kani/**`, `src/vspec/**`, a file in its `changed:` list or `kani.yaml` itself; and `kani-ignoring` (`q16-1` to `q8-4`), every harness after every landing on main, which covers the `kani_*` modules in hot files (`heap/cache.rs`, `purge.rs`), one at a time in regular format in 4 shards per quantum (`ci/kani-regular.sh`), failing on a harness that does not verify or on CBMC's `warning: ignoring`.
- `genmc.yaml`: `genmc`, rustfmt over the harness templates (`tests/genmc/run.sh --templates`), the control that a template no case names stops the run at setup (`--self-test`), then `tests/genmc/run.sh` (`verus/WEAKMEM.md`, "GenMC"): every case in `tests/genmc/cases`, GenMC ports of loom models that run the `src/vspec` wrappers and `src/sync.rs` unchanged over `core`'s atomics, under each memory model the case names (RC11, IMM, ARMv8); a pass case must pass and a control must fail with its expected report; on pull requests that change `src/sync.rs`, `src/vspec/**`, the source files the harnesses copy steps from, `Cargo.toml`, `tests/genmc/**`, `tests/loom/src/vspec.rs` or `genmc.yaml` itself. The pins that tie the harnesses' copies to the source run in `lint.yaml`'s loom check, on every pull request.

On request (`ci/run:<check>` on a pull request, or `chainctl checks run .
--check <check>` for any pushed commit), until Checks schedules this
repository (`extended.yaml`'s header says why it does not yet):

- `extended.yaml`: `fuzz` (50M operations: default, hardened, sized, q8),
  `oscillate` (the 600 s soak), `check-needed`, `consumers` (the Valkey and
  Ruby builds), each on both arches.
- `weakmem.yaml`: `weakmem-stress`, the three weak-memory stress tests
  (`verus/WEAKMEM.md`, "Hardware stress tests"), 60 s each, on aarch64
  only.

`chainctl checks run` plans a commit without a pull request; name the
checks to run with `--check`. The floor checks run only on main, where
their identity is released.

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
- **Checked-release `.so`, valgrind** in the extended checks: not
  written yet.

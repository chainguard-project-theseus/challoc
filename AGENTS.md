# Notes for coding agents

Follow `CONTRIBUTING.md`: its hard rules, code style, performance rules
and test rules apply to every change. Read `README.md`, `docs/design.md`
and `docs/benchmarking.md` first; `docs/testing.md` is the test layout,
`docs/tuning.md` the operator's guide, and `docs/research.md` the sources
behind design decisions (consult it before re-researching anything).

- Nothing builds or runs on macOS or any non-Linux host. Builds and tests
  run in the `Dockerfile.dev` container on any Docker host: `make dev` for
  a shell, `make run CMD='...'` for one command (`$` as `$$`).
- `target/` in the container is a named Docker volume per architecture
  (`challoc-target-<arch>`), visible only inside it; a second git worktree
  sets `TARGET_VOLUME=challoc-target-<name>-<arch>` on every make command.
- `PLATFORM=linux/amd64` on an arm64 host runs emulated x86_64: for
  correctness only, never for numbers.
- Benchmark numbers come only from the harness tiers in `bench/README.md`
  (`make quick` first, then `make bench-launch SUITE=...`); local runs are
  indicative.
- Run `cargo fmt` before committing; CI requires `cargo fmt --check`.

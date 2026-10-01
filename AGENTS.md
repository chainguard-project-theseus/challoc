# Notes for coding agents

Follow `CONTRIBUTING.md`: its hard rules, code style, performance rules
and test rules apply to every change. Read `README.md`, `docs/design.md`
and `docs/benchmarking.md` first; `docs/testing.md` is the test layout,
`docs/tuning.md` the operator's guide, and `docs/research.md` the sources
behind design decisions (consult it before re-researching anything).

Environment facts:

- Builds and tests go through `scripts/devvm.sh` (remote Linux VMs, native
  arm64 and amd64), never local Docker: `scripts/devvm.sh run arm64 <name>
  -- make test`. The `make` targets below run unchanged inside it.
- Nothing builds or runs on macOS or any non-Linux host. Use the dev
  container from `Dockerfile.dev`: `make dev` for a shell, or
  `make run CMD='...'` for one command (escape `$` as `$$` in CMD).
- `target/` in the container is a named Docker volume per architecture
  (`challoc-target-arm64`, `challoc-target-amd64`), so Linux artifacts are
  only visible inside the container. In a second git worktree set
  `TARGET_VOLUME=challoc-target-<name>-<arch>` on every make command.
- Native aarch64 is the fast path on Apple silicon. Add
  `PLATFORM=linux/amd64` to any make target to also check x86_64; it is
  emulated and only for correctness, never for numbers.
- Reference benchmark numbers and hardware counters come from the GCE VMs
  (C4A for aarch64, C4 for x86_64) driven by `bench/gce/challoc-bench`;
  see `docs/benchmarking.md` and `bench/gce/README.md`. Stop every VM you
  start, and never compare a Mac number with a GCE number.
- Two benchmark tiers (`bench/README.md`): `make quick` (or `challoc-bench
  quick <vm>`) is the ~2-minute go/no-go and comes first; `make ab` /
  `challoc-bench ab` is the protocol tier and the only source of numbers
  for pull requests, issues and acceptance.
- Never run `cargo fmt` over files you did not edit; format only the
  lines you touched.

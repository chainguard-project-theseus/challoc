# challoc: notes for agents working in this repo

Read `README.md`, then `docs/design.md`, then `docs/plan.md`. The plan is the
task list; work through it phase by phase and do not skip acceptance criteria.
`docs/research.md` has the sources behind design decisions; consult it before
re-researching anything.

Hard rules:

- This is a C-ABI malloc replacement written in Rust. Consumers are C
  programs such as Valkey and Ruby. It is `no_std`, `panic = "abort"`, and
  must never allocate through anything but its own machinery.
- Stable Rust. TLS goes through the asm accessor in `src/tls.rs`, never
  `thread_local!`.
- All exported functions are top-level `pub extern "C"` with
  `#[unsafe(no_mangle)]` or `#[unsafe(export_name = "...")]`.
- Every kernel feature is probed at runtime and has a fallback. Never assume
  a kernel version from a header constant.
- Abort on any heap inconsistency. Never try to continue.
- Any change to the hot paths in `src/heap.rs` needs a benchmark row in
  `bench/results/` before and after.

Environment:

- The host is macOS. Nothing here builds or runs on macOS. Use the dev
  container from `Dockerfile.dev`: `make dev` for a shell, or
  `make run CMD='...'` for one command (escape `$` as `$$` in CMD). Do not
  `apk add rust` by hand on `wolfi-base`: Wolfi's Rust needs the
  `glibc-2.44` stream and the Dockerfile performs that swap.
- `target/` in the container is a named Docker volume per architecture
  (`challoc-target-arm64`, `challoc-target-amd64`; `TARGET_VOLUME=` renames
  it, a lane worktree does), so Linux artifacts are only visible inside the
  container, never mixed with host artifacts.
- Native aarch64 is the fast path on Apple silicon. Add
  `PLATFORM=linux/amd64` to any make target to also check x86_64; it is
  emulated and only for correctness, never for numbers.
- Host-side `cargo test --features std` covers pure logic only.
- Reference benchmark numbers and hardware counters come from the GCE VMs
  described in `docs/plan.md` Phase 1.5 (C4A for aarch64, C4 for x86_64, zone
  `us-east4-a` since 2026-09-17,
  driven by `bench/gce/challoc-bench` once it exists). `gcloud` on the host
  is authenticated; stop every VM you start before the iteration ends, and
  never compare a Mac number with a GCE number.
- Two benchmark tiers (`bench/README.md`): `make quick` (or `challoc-bench
  quick <vm>`) is the ~2-minute go/no-go for a change and comes first;
  `make ab` / `challoc-bench ab` is the protocol tier and the only source of
  numbers for `bench/results/` and acceptance. During a driving session the
  VMs stay warm (`challoc-bench session start|stop`, `CHALLOC_GCE_WARM=1`);
  an iteration then does not start or stop them.
- The scorecard is the trend record: after a change lands, `challoc-bench
  scorecard <vm>` on both VMs (`make scorecard` in a container) appends one
  line per (date, commit, arch) to `bench/results/scorecard-<arch>.md`;
  commit those lines with the PROGRESS commit and read the trend with
  `bench/scorecard.sh --render`.
- No fast-path change (`src/heap.rs`, `src/class.rs`, `src/slab.rs`,
  `src/api/*.rs`) without a per-operation cycle attribution on the workload
  it is meant to help (`challoc-bench quick-perf`, the soak's perf sidecar,
  or the sampler), recorded in the results file, and the change names the
  mechanism it attacks; larson counters do not stand in for Valkey or Ruby.
- Lanes (since 2026-09-19; `docs/plan.md` Phase 2.5, "Execution in
  lanes"; `scripts/ralph-prompt.md`, "Lane mode"): the remaining Phase 2.5
  tasks run in parallel lanes, each in its own git worktree under
  `../challoc-wt/<lane>` on branch `loop/<lane>` (`scripts/ralph-lane.sh
  <lane>` creates it and prints the environment), each with its own VMs
  under its own `CHALLOC_GCE_PREFIX` (`challoc-<lane>`, so `challoc-bench`
  names only the lane's VMs and matrix cells) and its own Docker target
  volume `TARGET_VOLUME=challoc-target-<lane>-$ARCH`. A lane records its
  progress in `PROGRESS.d/<lane>-<task>.md` and never edits PROGRESS.md;
  the coordinator merges lane branches into `main` one at a time
  (`scripts/ralph-lane.sh --merge <lane>`: rebase, fast-forward, fold the
  notes) and is the only one who runs the scorecard, from `main` on the
  reference pair `challoc-bench-{arm64,amd64}`, once per merge batch;
  lanes never touch `bench/results/scorecard-*.md`. The partition, so
  lanes never conflict in `src/`: lane A (on `main`) owns `src/heap.rs`,
  `src/arena.rs` and the claim/refill code of `src/class.rs`; lane B owns
  `src/purge.rs`, `src/policy.rs`, `src/meta.rs`, the commit path of
  `src/class.rs` and the probes in `src/sys/features.rs`; lane C owns
  `src/lock.rs`, `src/config.rs` and a new test-only model module; lane D
  changes nothing under `src/`. A change that needs another lane's file
  waits for the merge.
- The repository is not `rustfmt`-clean. Never run `cargo fmt` over files
  you did not edit (it would rewrite every lane's files and make every
  merge conflict); format only the lines you touched, by hand if need be.

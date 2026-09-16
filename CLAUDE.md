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
  (`challoc-target-arm64`, `challoc-target-amd64`), so Linux artifacts are
  only visible inside the container, never mixed with host artifacts.
- Native aarch64 is the fast path on Apple silicon. Add
  `PLATFORM=linux/amd64` to any make target to also check x86_64; it is
  emulated and only for correctness, never for numbers.
- Host-side `cargo test --features std` covers pure logic only.
- Reference benchmark numbers and hardware counters come from the GCE VMs
  described in `docs/plan.md` Phase 1.5 (C4A for aarch64, C4 for x86_64,
  driven by `bench/gce/challoc-bench` once it exists). `gcloud` on the host
  is authenticated; stop every VM you start before the iteration ends, and
  never compare a Mac number with a GCE number.

# challoc: notes for agents working in this repo

Read `README.md`, `docs/design.md` and `docs/benchmarking.md` first;
`docs/tuning.md` is the operator's guide and `docs/testing.md` the test
layout. `docs/research.md` has the sources behind design decisions;
consult it before re-researching anything.

Hard rules:

- A C-ABI malloc replacement written in Rust for C programs such as Valkey
  and Ruby. It is `no_std`, `panic = "abort"`, and must never allocate
  through anything but its own machinery.
- Stable Rust. TLS goes through the asm accessor in `src/tls.rs`, never
  `thread_local!`.
- All exported functions are top-level `pub extern "C"` with
  `#[unsafe(no_mangle)]` or `#[unsafe(export_name = "...")]`.
- Every kernel feature is probed at runtime and has a fallback. Never assume
  a kernel version from a header constant. The floor is a Chainguard OS
  (Wolfi, glibc 2.44+) container on host kernels from 5.10; newer kernels
  and CPUs should be faster, never required.
- Abort on any heap inconsistency. Never try to continue.
- No fast-path change (`src/heap/`, `src/class/`, `src/slab.rs`,
  `src/api/*.rs`) without a per-operation cycle attribution on the workload
  it is meant to help (`challoc-bench quick-perf`, the soak's perf sidecar,
  or the sampler), recorded in the results file, and the change names the
  mechanism it attacks; larson counters do not stand in for Valkey or Ruby.
- The repository is not `rustfmt`-clean. Do not run `cargo fmt` over files
  you did not edit; format only the lines you touched.

Environment:

- Nothing builds or runs on macOS. Use the dev container from
  `Dockerfile.dev`: `make dev` for a shell, or `make run CMD='...'` for one
  command (escape `$` as `$$` in CMD).
- `target/` in the container is a named Docker volume per architecture
  (`challoc-target-arm64`, `challoc-target-amd64`; `TARGET_VOLUME=` renames
  it), so Linux artifacts are only visible inside the container.
- Native aarch64 is the fast path on Apple silicon. Add
  `PLATFORM=linux/amd64` to any make target to also check x86_64; it is
  emulated and only for correctness, never for numbers.
- Host-side `cargo test --features std` covers pure logic only.
- Reference benchmark numbers and hardware counters come from the GCE VMs
  (C4A for aarch64, C4 for x86_64) driven by `bench/gce/challoc-bench`;
  see `docs/benchmarking.md` and `bench/gce/README.md`. Stop every VM you
  start, and never compare a Mac number with a GCE number.
- Two benchmark tiers (`bench/README.md`): `make quick` (or `challoc-bench
  quick <vm>`) is the ~2-minute go/no-go and comes first; `make ab` /
  `challoc-bench ab` is the protocol tier and the only source of numbers
  for `bench/results/` and acceptance.

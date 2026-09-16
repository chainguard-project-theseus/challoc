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
  container: `make dev` once `Dockerfile.dev` exists, or
  `docker run --rm -it -v "$PWD":/src -w /src cgr.dev/chainguard/wolfi-base sh`
  and `apk add rust build-base gcc jemalloc` inside it.
- Native aarch64 is the fast path on Apple silicon. Add
  `--platform linux/amd64` to also check x86_64; it is emulated and only for
  correctness, never for numbers.
- Host-side `cargo test --features std` covers pure logic only.

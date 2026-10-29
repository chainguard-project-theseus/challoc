# Contributing to challoc

We don't accept pull requests from external contributors. If you and your
organization would seriously like to get involved, email
dlorenc@chainguard.dev.

The rest of this file is how maintainers work on challoc: the
development environment, the rules every change follows, how changes to
performance-sensitive code are justified, and how to send a change. Read
`README.md` first, then `docs/design.md` (the design, section by
section), `docs/testing.md` (the test layout) and `docs/benchmarking.md`
(how numbers are produced).
`docs/tuning.md` is the operator's guide and `docs/research.md` has the
sources behind design decisions; check it before re-researching anything.

## Support scope

challoc supports one platform: Chainguard OS and Wolfi containers with
glibc 2.44 or newer, on host kernels 5.14 and newer, on x86_64 and
aarch64 (the latter with 4, 16 or 64 KiB pages). Newer kernels and CPUs turn on more
features at run time; they are never required. Other distributions,
other libcs, older kernels and non-Linux hosts are out of scope. A change
must not raise the floor.

## Development environment

Nothing builds or runs outside Linux. Every build and test runs in the
Wolfi dev container defined by `Dockerfile.dev`, whatever machine runs
Docker. `make help` lists the targets.

```sh
make dev                       # interactive shell in the container, repo at /src
make run CMD='cargo test'      # one command in the container (escape $ as $$)
make build                     # cargo build --release
make test                      # the full suite (docs/testing.md)
make fuzz                      # fuzz/fuzz.c, 10M random operations on 4 threads
```

Variables that apply to every target:

| variable | default | effect |
|---|---|---|
| `PLATFORM` | `linux/<host arch>` | `PLATFORM=linux/amd64` runs the x86_64 container. On Apple silicon it is emulated: use it to check correctness, never for numbers. |
| `QUANTUM` | `16` | The size-class quantum feature of every cargo build and test, `16` (the system library) or `8` (Valkey's private build). Run `make test QUANTUM=8` as well when a change touches size classes. |
| `EXTRA_DOCKER_FLAGS` | empty | Extra `docker run` flags for every container, the memory-limited ones included. `docs/testing.md` has the privileged invocation. |
| `TARGET_VOLUME` | `challoc-target-<arch>` | The named Docker volume mounted at `/src/target`. A second git worktree must use its own (`TARGET_VOLUME=challoc-target-<name>-<arch>`) so two trees never share build artifacts. |
| `CARGO_VOLUME` | `challoc-cargo-registry` | The named volume that caches the cargo registry. |
| `IMAGE_TARGET` | empty (the full image) | Stop the image at a `Dockerfile.dev` stage: `base`, `test` (what `make test` needs) or `consumers`. |
| `IMAGE_BUILD` | `1` | Empty: use `IMAGE` as it is (a prebuilt image) instead of building it. |

`target/` inside the container is the named volume, so Linux artifacts are
visible only inside the container and never mix with host artifacts. The
one exception is `target/results/`, bind-mounted from the host.
`make clean-volumes` removes the two volumes the variables name.

### Remote dev hosts

The dev container does not need a local Docker: any Linux host with
Docker that you can reach over ssh works, and is the usual setup on a
Mac. Sync the tree to the host, keeping it a git checkout (`cb run` and
scripts/codegen-diff.sh read revisions with git), and run
the same `make` targets there; a native x86_64 host replaces the emulated
`PLATFORM=linux/amd64`. Give every tree on a shared host its own
`TARGET_VOLUME`.

`scripts/devvm.sh` is one way to do this on GCE: `devvm.sh up` creates
(or starts) an aarch64 and an x86_64 VM, installs Docker and builds the
dev image; `devvm.sh run ARCH NAME -- make test` pushes the current
commit to a mirror on the VM, syncs uncommitted changes on top into
`~/w/NAME`, runs the command there with
`TARGET_VOLUME=challoc-target-NAME-ARCH` and returns its exit status.
The job runs detached on the VM, so `devvm.sh attach ARCH NAME` picks
its output up again after an ssh drop; `--base REF` also checks out REF
for baselines, `fetch` copies results back, `down` stops the VMs.
`scripts/devvm.sh help` has the details.

## Hard rules

- challoc is a C-ABI `malloc` replacement written in Rust for C programs
  such as Valkey and Ruby. It is `no_std` with `panic = "abort"`, and it
  never allocates through anything but its own machinery.
- Stable Rust only. Thread-local state goes through the asm accessor in
  `src/sys/tls.rs`, never `thread_local!`.
- The build needs a C++ compiler for one file, `src/api/cxx_handler.cpp`:
  the catch that lets the `nothrow_t` operators return null when the
  `new_handler` throws (Rust cannot catch a C++ exception). build.rs
  compiles it with `CXX_<target>`, `CXX` or `c++` (`<arch>-linux-gnu-g++`
  when cross-compiling). The file includes no headers and references the
  C++ runtime only weakly, so the library never needs libstdc++;
  `cxx_symbols_in_release_so` checks that. No other C or C++ goes into
  the library.
- Every exported function is a top-level `pub extern "C"` function with
  `#[unsafe(no_mangle)]` or `#[unsafe(export_name = "...")]`.
- Every kernel and CPU feature is probed at run time and has a fallback.
  Never infer a kernel version from a header constant, and never raise
  the compiler baseline (`-C target-cpu`). A feature must not change
  observable behaviour (allocation results, abort conditions) between
  hosts, only cost, memory and detection. A probed feature gets a test on
  a host that has it and a test on a host that lacks it.
- Abort on any heap inconsistency. Never try to continue.

## Code style

- Run `cargo fmt` before committing. CI runs `cargo fmt --check` and
  fails on any difference.
- Comments describe the code as it is. History belongs in commit
  messages, not in comments.
- `unsafe` stays confined to the syscall layer, the thread-local accessor
  and the slab allocator's pointer arithmetic.

## Performance rules

challoc's throughput claim is parity with jemalloc, so every change to
code on or near the hot paths is justified with evidence.

- **Refactors and cleanups** that are not meant to change generated code
  prove it: `sh scripts/codegen-diff.sh capture` on the base,
  `sh scripts/codegen-diff.sh compare` on the change (both in the dev
  container, on the same architecture). The verdict must read
  `changed 0` on the hot set: `malloc`, `free`, `calloc`, `realloc`,
  `sdallocx`, `free_sized` and everything inlined into them. Functions
  that differ only in relocations (`reloc-only`) are fine. Put the
  verdict in the pull request.
- **Fast-path changes** (`src/heap/`, `src/class/`, `src/slab.rs`,
  `src/api/*.rs`) need two things in the pull request. First, the go/no-go
  verdict (`make quick`: `cb launch quick` on the gate cells,
  `docs/benchmarking.md`). Second, a per-operation cycle attribution on the
  workload the change is meant to help (the per-run counters in that
  workload's cb report, the soak's perf sidecar, or the sampler). The
  change names the mechanism it attacks. larson counters do not stand in
  for Valkey or Ruby.
- Cite the cb run id for every number in a pull
  request. Reference numbers come only from the reference cells described
  in `docs/benchmarking.md`. A laptop or emulated number is
  never compared with a reference number.

## Tests

Every behaviour change comes with a test, and `make test` must pass on
both architectures (`PLATFORM=linux/amd64` for the second one).
`docs/testing.md` explains the layers, how skips are reported and how to
add a C test program. A test that cannot check its claim on a host prints
a `SKIP` line; it never passes silently.

## Sending a change

- Keep a pull request to one logical change, with a description of what
  it changes and why, and the evidence the rules above ask for.
- Sign off every commit under the
  [Developer Certificate of Origin](https://developercertificate.org/):
  `git commit -s` adds the `Signed-off-by:` line, which certifies that
  you wrote the change or otherwise have the right to submit it under the
  project's licence.
- By contributing you agree that your contribution is licensed under the
  Apache License 2.0 (`LICENSE`).
- Report security issues privately as described in `SECURITY.md`, not in
  a public issue.

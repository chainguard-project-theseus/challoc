STATUS: RUNNING

Progress log for the unattended implementation loop (scripts/ralph.sh).
One line per task, newest at the bottom of each section. Keep it terse.

## Done

- Phase 0 task 1: no_std crate skeleton, profiles, libc dep, std feature, Dockerfile.dev, `make dev`. Verified aarch64 + emulated x86_64. Commit 1def4a3.

## In progress

(none)

## Design decisions

Format: `Phase N task M: <decision>. Reason: <one line>.`

- Phase 0 task 1: Dockerfile.dev swaps wolfi-base's pinned `glibc`, `glibc-locale-posix`, `ld-linux`, `libcrypt1` (2.43) for the `glibc-2.44` stream before `apk add rust`. Reason: every Wolfi rust stream links libLLVM-22 built against glibc 2.44; plain `apk add rust` gives a rustc that fails to start.
- Phase 0 task 1: `target/` in the container is a named Docker volume per architecture; the cargo registry is a shared named volume. Reason: keeps Linux artifacts out of the host `target/`, avoids the slow macOS bind mount, and lets arm64/amd64 builds coexist.
- Phase 0 task 1: `quantum16`/`quantum8` are Cargo features with a `compile_error!` if both or neither is set; `lib.rs` exposes `QUANTUM`. Reason: design.md section 3 says build-time feature but does not say how to guard misuse.
- Phase 0 task 1: the `no_std` panic handler is a bare trap (`ud2` / `brk #1`) until `sys::abort` exists in task 2. Reason: no syscall layer yet; `panic = "abort"` means it is only reached by explicit panics.

## Blocked

(none)

## Notes for the human

(observations worth reading in the morning)

- Docker Desktop's kernel here is 6.10.14-linuxkit. `MADV_GUARD_INSTALL` (6.13) will probe absent locally, so guard-slab code paths can only be tested on a newer kernel; the fallbacks will get the coverage. `mseal` (6.10) and `process_madvise` (5.10) should be present.
- Container toolchain: rustc 1.98.1, gcc 15.3.0 (Wolfi). Host has rustc 1.94.1; `cargo test --features std` passes on both.
- The Makefile's `run` target passes CMD through make, so `$` must be written `$$`.
- The staticlib carries an undefined weak `rust_eh_personality` reference from the prebuilt `core`; a trivial C program links against `libchalloc.a` fine, but if a future consumer's link fails on it, add a no-op `rust_eh_personality` export.

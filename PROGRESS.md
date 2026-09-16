STATUS: RUNNING

Progress log for the unattended implementation loop (scripts/ralph.sh).
One line per task, newest at the bottom of each section. Keep it terse.

## Done

- Phase 0 task 1: no_std crate skeleton, profiles, libc dep, std feature, Dockerfile.dev, `make dev`. Verified aarch64 + emulated x86_64. Commit 1def4a3.
- Phase 0 task 2: `src/sys` asm syscall stubs (x86_64, aarch64), header-checked syscall numbers, errno decoding (host tests), typed wrappers, `abort` via write(2)+SIGABRT+exit_group; panic handler routed through it. 28 tests pass on aarch64 + emulated x86_64. Commit 36f992f.

## In progress

- Phase 0 task 3: `src/sys/features.rs` kernel feature probe (page size, MADV_GUARD_INSTALL, mseal, process_madvise/PIDFD_SELF, PR_SET_VMA_ANON_NAME).

## Design decisions

Format: `Phase N task M: <decision>. Reason: <one line>.`

- Phase 0 task 1: Dockerfile.dev swaps wolfi-base's pinned `glibc`, `glibc-locale-posix`, `ld-linux`, `libcrypt1` (2.43) for the `glibc-2.44` stream before `apk add rust`. Reason: every Wolfi rust stream links libLLVM-22 built against glibc 2.44; plain `apk add rust` gives a rustc that fails to start.
- Phase 0 task 1: `target/` in the container is a named Docker volume per architecture; the cargo registry is a shared named volume. Reason: keeps Linux artifacts out of the host `target/`, avoids the slow macOS bind mount, and lets arm64/amd64 builds coexist.
- Phase 0 task 1: `quantum16`/`quantum8` are Cargo features with a `compile_error!` if both or neither is set; `lib.rs` exposes `QUANTUM`. Reason: design.md section 3 says build-time feature but does not say how to guard misuse.
- Phase 0 task 1: the `no_std` panic handler is a bare trap (`ud2` / `brk #1`) until `sys::abort` exists in task 2. Reason: no syscall layer yet; `panic = "abort"` means it is only reached by explicit panics.
- Phase 0 task 2: syscall numbers are hand-declared per arch in `src/sys/nr.rs`; a Linux-only unit test compiles `tests/c/sysnr.c` and checks every constant against the container's kernel headers. Reason: explicit and independent of the `libc` crate version; the header check keeps them honest on both arches.
- Phase 0 task 2: `sys::abort` writes `challoc: <msg>\n` with one buffered `write(2)`, unblocks SIGABRT, `tgkill`s the calling thread, resets the disposition to SIG_DFL and re-raises, then `exit_group(134)`. Reason: mirrors glibc's `abort` so a handler or blocked mask cannot let the process continue past an inconsistency.
- Phase 0 task 2: Linux-side coverage for `sys` is Rust unit tests under `cargo test --features std` run inside the container (mmap/mprotect/madvise, mseal, process_madvise, prctl, getrandom, futex, clocks, files, abort in a child process). Reason: no exported entry points exist yet to drive from C via `LD_PRELOAD`.
- Phase 0 task 2: `sys::open` always adds `O_CLOEXEC`; `PIDFD_SELF` is defined as -10000 but only task 3's probe decides whether the kernel honours it. Reason: the allocator never wants its fds inherited across exec; the sentinel's presence varies by kernel.

## Blocked

(none)

## Notes for the human

(observations worth reading in the morning)

- Docker Desktop's kernel here is 6.10.14-linuxkit. `MADV_GUARD_INSTALL` (6.13) will probe absent locally, so guard-slab code paths can only be tested on a newer kernel; the fallbacks will get the coverage.
- Kernel-feature reality in the dev container (measured in Phase 0 task 2): under Docker's default seccomp profile `mseal` returns ENOSYS and `process_madvise` returns EPERM even for a bad pidfd (seccomp, not the kernel). With `make run DOCKER_FLAGS='--security-opt seccomp=unconfined'`: `mseal` works (sealed page refuses `munmap`/`mprotect` with EPERM); `process_madvise` on our own pidfd still fails with EPERM because kernels before the 6.13-era self-exemption require CAP_SYS_NICE unconditionally; `PR_SET_VMA_ANON_NAME` returns EINVAL (CONFIG_ANON_VMA_NAME off in linuxkit). Emulated amd64 additionally reports ENOSYS for `pidfd_open` and delivers stray EINTR to futex waits. Consequence for task 3: the probe must treat any error, not only EINVAL/ENOSYS, as "unavailable".
- Container toolchain: rustc 1.98.1, gcc 15.3.0 (Wolfi). Host has rustc 1.94.1; `cargo test --features std` passes on both.
- The Makefile's `run` target passes CMD through make, so `$` must be written `$$`.
- The staticlib carries an undefined weak `rust_eh_personality` reference from the prebuilt `core`; a trivial C program links against `libchalloc.a` fine, but if a future consumer's link fails on it, add a no-op `rust_eh_personality` export.

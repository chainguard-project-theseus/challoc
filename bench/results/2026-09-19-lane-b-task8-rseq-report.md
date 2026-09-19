# 2026-09-19: the rseq / `mm_cid` line of the feature report (Phase 2.5 task 8, lane B)

- change: `challoc_features()` and the `Kernel and CPU features:` line gain `rseq=yes|no
  rseq_size=<n> mm_cid=yes|no` after `collapse` (`src/sys/features.rs`, `Rseq`, `Features::rseq`);
  cold path only (one probe at init: three GOT loads of glibc's weak `__rseq_size`,
  `__rseq_offset`, `__rseq_flags` and one read of the area's `cpu_id`), no fast path touched, no
  feature turned on; lane A's per-CPU mode consumes `Features::rseq.mm_cid()` for its `auto`
  verdict and reads the `mm_cid` value through its own accessor
- base: 675f357 (main, second lane batch)
- hosts: the lane's VMs `challoc-lane-b-arm64` (c4a-standard-16, Axion) and `challoc-lane-b-amd64`
  (c4-standard-16, Xeon 8581C), both Ubuntu 24.04 kernel 7.0.0-1011-gcp; the Mac's Docker Desktop
  VM (Apple silicon, linuxkit 6.10.14); all inside the dev container (Wolfi, glibc 2.44)

## 1. The both-ways test

| host | kernel | `__rseq_size` | `__rseq_offset` | `__rseq_flags` | `cpu_id` | line |
|---|---|---:|---:|---:|---:|---|
| C4A (aarch64) | 7.0.0-1011-gcp | 33 | 320 (from `tpidr_el0`) | 0 | 0 | `rseq=yes rseq_size=33 mm_cid=yes` |
| C4 (x86_64) | 7.0.0-1011-gcp | 33 | -384 (from `%fs:0`) | 0 | 12 | `rseq=yes rseq_size=33 mm_cid=yes` |
| Mac Docker VM (aarch64) | 6.10.14-linuxkit | 20 | 320 | 0 | 2..4 | `rseq=yes rseq_size=20 mm_cid=no` |

The symbol values are the test binary's (`cargo test` under the container, `sys::features::
linux_tests::probe_prints_and_matches_kernel_version -- --nocapture`); the prerequisites note in
PROGRESS.md measured 192 / -192 / -256 for the same symbols under a C program with and without the
preload. The offset moves with the static TLS layout of the executable and the preloaded objects,
which is why the probe reads the symbol and nothing here is a constant. `__rseq_size` is the
kernel's `AT_RSEQ_FEATURE_SIZE` as glibc registered it (33 on 7.0; the 6.10 kernel reports none
and glibc registers the original 20 bytes), so the `mm_cid` verdict is `size >= 28` (field at
24) after `cpu_id >= 0` (the kernel wrote the area).

The lines, verbatim (`tests/c/features.c` under the preload, identical to the report):

```
C4A:  page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed rseq=yes rseq_size=33 mm_cid=yes mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
C4:   page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed rseq=yes rseq_size=33 mm_cid=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
Mac:  page_size=4096 guard_install=no mseal=no process_madvise=no process_madvise_free=no anon_name=no thp=always mthp=2M collapse=unprobed rseq=yes rseq_size=20 mm_cid=no mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=no cacheline=64 l1d=unknown l2=unknown
```

(`mseal=no process_madvise=no` on the VMs are Docker's default seccomp profile inside `make
test`, as in the task 2 file.)

## 2. Tests

- Host (`cargo test --features std`): `sys::features::tests::rseq_verdicts_follow_size_and_cpu_id`
  (the recorded values of both hosts, the three offsets, `cpu_id` -1 and -2 meaning no
  registration whatever the size, the 27/28/32 boundary, 19 below the original struct), plus the
  `Display`, `FeatureLine` and longest-line tests carrying the new keys (the longest line stays
  under 400 bytes of the 512 cap).
- Container (`make test`): `probe_prints_and_matches_kernel_version` cross-checks the probe against
  `dlsym(__rseq_size|__rseq_offset|__rseq_flags)`, `getauxval(AT_RSEQ_FEATURE_SIZE)` and the kernel
  version (`mm_cid=yes` needs 6.3+), and asserts the line's `mm_cid` token both ways;
  `tests/c/features.c` counts the three new keys once each and checks `mm_cid=yes` implies
  `rseq=yes` and `rseq_size >= 28`.
- `make test` green with the final code: C4A 376 + 115 + 23 pass, churn/lazy containers
  `verdict=ok`; C4 376 + 115 + 23 pass, `verdict=ok`; Mac VM 376 + 115 + 23 pass, `verdict=ok`
  (logs: `~/logs/test-rseq.log` on the VMs, `.tmp-mactest.log` locally).

## 3. Cost

Init only: three GOT loads and one 4-byte read, no syscall; not measurable against the task 2
probe cost table (36.7 us on the C4A). No `quick` run: the shipped fast paths are byte-identical
in source, and the change is not on any path a benchmark row exercises.

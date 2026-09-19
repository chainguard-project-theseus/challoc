# Phase 2.5 task 15, lane C: rseq time slice extension for lock holders, aarch64 (the fallback)

Date 2026-09-19 (23:20..23:45 UTC), lane C (`loop/lane-c`), base commit
299461b (`old`), the working tree of the task 15 commit (`new`). VM
`challoc-lane-c-arm64`, c4a-standard-16 (Google Axion, Neoverse V2, 16
vCPUs), us-east4-a, `ubuntu-2404-lts-arm64`, kernel `7.0.0-1011-gcp`, the
Wolfi container (glibc 2.44, rust 1.98.1, jemalloc 5.3.1, mimalloc-bench
06a4c4f), deleted at the end. The x86_64 side, with the kernel that has
the extension, is `2026-09-19-x86_64-lane-c-task15-slice-ext.md`.

## 1. The probe on arm64

`__rseq_size` is 33 (glibc 2.44 registers the v2 size) but the area's
`flags` stay 0: Ubuntu's arm64 `7.0.0-1011-gcp` has no
`CONFIG_RSEQ_SLICE_EXTENSION` (`prctl(79, SET, 1)` -> `ENOTSUPP`,
`rseq_slice_yield` -> `ENOSYS`), and no arm64 kernel can have it yet: the
option depends on `GENERIC_ENTRY` and `HAVE_GENERIC_TIF_BITS`, which
`arch/arm64/Kconfig` selects neither of through 7.2 and master. So on
aarch64 `slice::probe` reads `kernel-disabled` on every host, the module is
off, and a lock hold costs one relaxed load of the state byte. This file
measures that.

## 2. Tests

`make test` on the VM, twice (the tree before and after the grant test
gained its CPU hogs): 393 + 117 + 23 passed, `EXIT 0`. The slice tests
took the fallback branch and said so:

```
slice_ext: rseq size 33 offset 320 available=false enabled=false yield_syscall=false verdict=kernel-disabled
slice_ext: 64 lock threads and 16 hogs on 16 CPUs, 13974 holds, 0 grants yielded via rseq_slice_yield, threads enabled 0, state kernel-disabled
slice_ext: mode on, __rseq_size 33, available 0, main thread enabled -1 (flags bit 5: 0)
slice_ext: 32 threads: 0 enabled before their first allocation, 0 after
Slice extension: off (kernel-disabled); slice_ext:on requested, unavailable
slice_ext: mode off ...  Slice extension: off (slice_ext:off)
```

(`tests/c/slice_ext.c` under `on` asserts nothing is enabled and the
report notes the refused request; under `off` the same with
`slice_ext:off` as the reason.)

## 3. `quick`: go

`challoc-bench quick arm64` (old = HEAD 299461b, new = tree, one round):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---|---|---|---|---|
| larson1 | M ops/s | 41.9 (15.9 MiB) | 41.7 (16.0 MiB) | 44.6 (18.1 MiB) | +0.5% | +7.0% |
| larson10 | M ops/s | 362.5 (111.5 MiB) | 362.6 (111.8 MiB) | 387.7 (132.1 MiB) | -0.0% | +6.9% |
| xmalloc | M frees/s | 578.187 (53.5 MiB) | 578.162 (52.6 MiB) | 483.148 (83.0 MiB) | +0.0% | -16.4% |
| alloc-test1 | s | 2.746 (13.0 MiB) | 2.731 (13.0 MiB) | 2.548 (12.3 MiB) | -0.5% | +7.2% |
| valkey1 | M ops/s | 6.964 (584.2 MiB) | 6.957 (584.2 MiB) | 6.959 (585.3 MiB) | +0.1% | +0.0% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go (new within 5% of old on every row; churn compacts)`.

## 4. `ab`, 5 rounds: within noise

`challoc-bench ab arm64 VARIANTS='old=HEAD new=tree jemalloc'
BENCHES='larson10 larson16 alloc-testN' ROUNDS=5`, load 2.65 at start,
10.80 at the end, 311 s; the full file is
`2026-09-19-aarch64-lane-c-task15-ab.md`:

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson10 | old | 363.0 363.2 365.7 369.7 370.0 | 115080 |
| larson10 | new | 360.8 362.4 363.1 366.6 367.1 | 114864 |
| larson10 | jemalloc | 382.5 383.9 385.5 386.2 390.4 | 134832 |
| larson16 | old | 582.5 583.1 589.5 589.8 592.1 | 168460 |
| larson16 | new | 588.6 590.2 591.0 592.8 599.2 | 169136 |
| larson16 | jemalloc | 612.9 615.6 615.9 617.4 618.3 | 211500 |
| alloc-testN | old | 2.288 2.301 2.302 2.302 2.314 | 16728 |
| alloc-testN | new | 2.287 2.297 2.303 2.305 2.308 | 16888 |
| alloc-testN | jemalloc | 2.200 2.201 2.202 2.204 2.207 | 16860 |

| program | unit | old | new | jemalloc | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|
| larson10 | M ops/s | 365.7 | 363.1 | 385.5 | +0.7% | -5.1% |
| larson16 | M ops/s | 589.5 | 591.0 | 615.9 | -0.3% | -4.3% |
| alloc-testN | s | 2.302 | 2.303 | 2.202 | +0.0% | -4.3% |

Every old/new pair overlaps (positive = new worse). The fallback's one
load per hold is not visible; `auto` is safe as the default here because
it does nothing here.

# Phase 2 task 2: mseal of the never-committed reservation ranges (aarch64, GCE C4A)

- date: 2026-09-18
- host: challoc-bench-arm64 (C4A, Neoverse-V2, 16 vCPU, kernel 7.0.0-1011-gcp, Docker 29.1.3)
- old: 50f0610 (library 41c7585ea8de3b8d), new: working tree of the task commit (library 65c6d818040dcefa)
- change: cold path only (`api::init` calls `Regions::seal` after `Regions::reserve` when the probe says `mseal=yes`); no fast path touched, no scorecard

## What is sealed

57 ranges at quantum 16 (`Regions::never_committed`, `MAX_SEALED = NCLASSES + 5`): the gap on either
side of each of the 52 class regions (53 contiguous ranges) and the alignment-plus-slide slack before
and after the huge and metadata regions (4). Not sealed: the class, huge and metadata regions
(committed by `mprotect`, purged by `MADV_DONTNEED` for the life of the process), the `PROT_NONE`
guard pages after huge runs in fallback mode (re-committed on reuse), and the region table
(`Shared.regions`, a static in the library's data segment shared with run-time-written locks).

## Where sealing happens

Docker's default seccomp profile filters `mseal` on both hosts (Mac: EPERM on the 6.10 linuxkit
kernel; VM: ENOSYS from Docker 29.1's libseccomp on kernel 7.0), so under `make test` the probe says
`mseal=no` and nothing is sealed; `make test DOCKER_FLAGS='--security-opt seccomp=unconfined'` seals.

| run | profile | lib tests | api tests | `seal_locks_the_gaps...` | `c_mseal` |
|---|---|---:|---:|---|---|
| Mac (6.10 linuxkit) | default | 280 pass | 91 pass | skipped (`mseal unavailable`) | `allocator sealed nothing; maps lines 23 -> 38` |
| Mac (6.10 linuxkit) | unconfined | 280 pass | 91 pass | `57 of 57 ranges sealed, maps lines +108` | `sealed 57 of 57 ranges; 56 sealed VMAs (0 named: no CONFIG_ANON_VMA_NAME), 55 of 4 MiB or more; maps lines 23 -> 145 (+122) across init` |
| VM (7.0 gcp) | default | 280 pass | 91 pass | skipped | `allocator sealed nothing; maps lines 23 -> 40` |
| VM (7.0 gcp) | unconfined | 280 pass | 91 pass (`c_mseal` fixed after one bound failure, rerun alone: pass) | `57 of 57 ranges sealed, maps lines 791 -> 899 (+108)` | `sealed 57 of 57 ranges; 58 sealed VMAs (57 named challoc, 57 of 4 MiB or more); maps lines 23 -> 147 (+124) across init` |

VMA count: sealing 57 ranges adds exactly 108 lines to `/proc/self/maps` in the Rust test (the bound
in the test is `2 * ranges + 2`); the C program's `+122/+124` across the first `malloc` includes the
three reservations, two probe pages and the first metadata and slab commits (default profile: +15/+17).
The one `c_mseal` failure on the VM was the test's own bound (`58 sealed VMAs larger than a page,
expected at most 57`): the two single probe pages (the program's and the allocator's) were adjacent
and merged into one two-page sealed VMA. The test now counts VMAs of 4 MiB or more (a class gap is at
least one maximal slab) against the allocator's own `Sealed ranges: N of M` line.

## quick (plumbing check; the quick container runs under the default profile, so sealing is skipped there)

`bench/gce/challoc-bench quick arm64 OLD=50f0610`, one round each, load at start 3.82, 121 s:

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.2 (15.7 MiB) | 42.2 (15.5 MiB) | 45.1 (18.8 MiB) | +0.0% | +6.9% |
| larson10 | M ops/s | 372.5 (113.5 MiB) | 369.5 (111.9 MiB) | 376.7 (130.4 MiB) | +0.8% | +1.9% |
| xmalloc | M frees/s | 600.058 (53.6 MiB) | 601.752 (56.3 MiB) | 476.340 (80.0 MiB) | -0.3% | -20.8% |
| alloc-test1 | s | 2.764 (13.0 MiB) | 2.754 (13.0 MiB) | 2.606 (12.3 MiB) | -0.4% | +5.7% |
| valkey1 | M ops/s | 6.312 (584.1 MiB) | 6.659 (584.2 MiB) | 6.723 (585.4 MiB) | -5.2% | +1.0% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go` (new within 5% of old on every row; churn compacts). Positive percentages mean new did worse.

## Init cost (the review's +1..2 ms concern; budget for this task 0.2 ms)

Inside the dev container on the VM, `target/quick/{old,new}/release/libchalloc.so` under `LD_PRELOAD`.
`first`: microseconds from before to after the first `malloc(1)` of a fresh process (init: probe,
reserve, seal, tables), 100 processes per row, two rounds. `spawn`: fork + `execv("/bin/true")` +
wait with the library preloaded into the child, 200 per row. `bench/rt` prints milliseconds and read
`0.001` for every run of both libraries (10 runs each, both profiles), so it could not resolve this.

| profile (sealing) | lib | first malloc median / min / p90 (us), round 1 | round 2 | spawn /bin/true median / min / p90 (us), round 1 | round 2 |
|---|---|---|---|---|---|
| unconfined (57 sealed) | old | 100 / 92 / 112 | 100 / 92 / 107 | 664 / 600 / 700 | 672 / 616 / 706 |
| unconfined (57 sealed) | new | 187 / 158 / 203 | 189 / 171 / 205 | 738 / 679 / 773 | 743 / 686 / 785 |
| default (skipped) | old | 110 / 100 / 124 | 108 / 99 / 119 | 679 / 613 / 713 | 672 / 618 / 706 |
| default (skipped) | new | 108 / 100 / 116 | 108 / 100 / 121 | 681 / 627 / 716 | 688 / 618 / 720 |
| glibc control | - | | | 510 / 459 / 539 | 522 / 457 / 549 |

Sealing costs +88 us on the first malloc (57 `mseal` calls, each splitting a VMA: about 1.5 us per
call) and +70 us on a whole `/bin/true` process, under the 0.2 ms budget, so the one-call-per-range
form stays. With `mseal` unavailable the new library's init is unchanged (108 vs 108..110 us).
`ruby -e 1` (about 40 ms) gains 0.2%.

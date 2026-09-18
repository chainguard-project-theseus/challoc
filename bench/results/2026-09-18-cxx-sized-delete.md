# Sized `operator delete` through the sized path (Phase 1.5 task 13)

Date: 2026-09-18. Base 9df1411; new = the task 13 tree (`src/api/cxx.rs`:
`_ZdlPvm`/`_ZdaPvm` are `free_sized`, `_ZdlPvmSt11align_val_t`/
`_ZdaPvmSt11align_val_t` are `free_aligned_sized`; the plain build gains
the soname `libchalloc.so.0`, `challoc_version()` and `je_pvalloc`, the TLS
accessors are no longer exported). Design 13.1 and 13.6.

## Mechanism

The sized C++ forms take task 4e's sized fast path
(`2026-09-18-aarch64-gce-sized-free.md`): the hint `class_for(size, align)`
is the class the matching `new` used, so the slot is pushed onto the home
cache after an arithmetic check and without loading the slab descriptor's
bitmap line; a mismatched size aborts as `free_sized` does.

## Workload

No row in the benchmark set calls a sized `delete`. The built larson
(`target/bench-bin/larson`, mimalloc-bench pinned by `bench/build.sh`)
imports only

```
_Znam@GLIBCXX_3.4   operator new[](unsigned long)
_ZdaPv@GLIBCXX_3.4  operator delete[](void*)
```

(its `SIZED` variant, `operator delete[](p, size)`, is not compiled), so
larson frees through the unsized form on both old and new, and there is no
before/after per-operation attribution to record for this change. The
sized forms are exercised by `tests/c/cxx_new.cpp` under the preload
(8 threads x 20000 operations, every sized and aligned pairing).

## Regression check: `quick`, one round each, new vs 9df1411

aarch64 (C4A, load at start 3.06, 121 s):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.4 (15.7 MiB) | 42.6 (15.7 MiB) | 45.1 (18.9 MiB) | -0.5% | +5.9% |
| larson10 | M ops/s | 369.2 (112.4 MiB) | 367.1 (112.0 MiB) | 387.0 (131.5 MiB) | +0.6% | +5.4% |
| xmalloc | M frees/s | 588.691 (57.1 MiB) | 589.595 (54.5 MiB) | 472.809 (81.9 MiB) | -0.2% | -19.8% |
| alloc-test1 | s | 2.607 (13.0 MiB) | 2.593 (13.0 MiB) | 2.468 (12.3 MiB) | -0.5% | +5.1% |
| valkey1 | M ops/s | 7.460 (584.2 MiB) | 7.420 (584.1 MiB) | 8.386 (585.4 MiB) | +0.5% | +13.0% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

x86_64 (C4, load at start 2.93, 124 s):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 46.5 (15.0 MiB) | 45.7 (15.2 MiB) | 46.0 (18.0 MiB) | +1.8% | +0.7% |
| larson10 | M ops/s | 277.7 (94.6 MiB) | 269.5 (92.9 MiB) | 284.0 (92.2 MiB) | +3.0% | +5.4% |
| xmalloc | M frees/s | 332.053 (53.3 MiB) | 331.066 (52.7 MiB) | 334.158 (91.3 MiB) | +0.3% | +0.9% |
| alloc-test1 | s | 3.024 (13.5 MiB) | 2.991 (13.6 MiB) | 2.876 (13.1 MiB) | -1.1% | +4.0% |
| valkey1 | M ops/s | 13.924 (585.0 MiB) | 13.887 (585.0 MiB) | 13.618 (591.7 MiB) | +0.3% | -1.9% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6673 (49.8%) | new compacts | |

Positive percentages mean new did worse. Both runs: `quick: go` (every
row within 5% of old, churn compacts). The x86_64 larson10 +3.0% is one
round of a row whose round-to-round spread on this VM is of that size
(the scorecard trend); the library's hot paths for `malloc`/`free` are
unchanged by this commit.

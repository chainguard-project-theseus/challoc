# challoc Phase 1 task 3 (arenas) fast-path A/B, 2026-09-16, aarch64

Rule from CLAUDE.md: any change to the hot paths in `src/heap.rs` needs a
before/after row. This task split `SlabTable` out of `ClassState`, routed
`free` by the descriptor's arena behind a `Shared::multi_arena` flag, and
added per-arena cache lists to `Heap`.

- old: HEAD tree (8449c3a code, built from `git archive HEAD` into `.tmp-old`)
- new: the working tree of this task (committed as the task's code commit)
- jemalloc: 5.3.1-r3 from Wolfi
- host: Apple M4, 10 CPUs visible to the container; kernel 6.10.14-linuxkit
- same binaries from `target/bench-bin` for every allocator, only `LD_PRELOAD` changes
- 4 runs each, interleaved old/new/jemalloc per run; no warm-up discarded (read the first column as the warm-up)
- larson `5 8 1000 5000 100 4141 10` (M ops/s, higher is better); xmalloc-test `-w 10 -t 5 -s 64` (M frees/s); alloc-test1, cfrac, glibc-simple wall seconds (lower is better); RSS in KiB from `bench/rt`

## Run 1 (all five programs)

| benchmark | old | new | jemalloc |
|---|---|---|---|
| larson M ops/s | 273.4 271.0 268.6 245.1 (median 270) | 239.8 246.0 242.1 226.9 (median 241) | 296.3 292.1 305.6 294.9 (median 295) |
| larson RSS KiB | 80440 79904 79492 76784 | 78368 78824 78204 74480 | 172208 170116 175948 173016 |
| xmalloc-test M frees/s | 618.9 612.6 600.8 598.3 (median 607) | 575.1 579.9 575.1 569.5 (median 575) | 262.3 260.4 257.8 255.7 (median 259) |
| alloc-test1 s | 1.979 2.005 2.011 2.012 | 1.992 1.978 1.965 2.023 | 1.930 1.924 1.922 1.928 |
| cfrac s | 2.283 2.453 2.272 2.275 | 2.296 2.296 2.297 2.290 | 2.578 2.350 2.387 2.580 |
| glibc-simple s | 1.381 1.367 1.367 1.380 | 1.348 1.331 1.348 1.354 | 1.622 1.619 1.630 1.623 |

Single-threaded programs are unchanged (within noise). The 10-thread
programs regressed: larson -11% (270 -> 241 M ops/s, now +19% behind
jemalloc where the old tree was +9%), xmalloc-test -5% (607 -> 575).

## Run 2: attribution probes (larson and xmalloc-test only, 3 runs each)

- v1: new tree with the `multi_arena` check removed from `Shared::checked_slot` (unsound for multiple arenas; probe only)

| variant | larson M ops/s | xmalloc-test M frees/s |
|---|---|---|
| old | 267.8 263.6 257.2 | 613.9 576.8 578.0 |
| new | 219.8 232.0 236.9 | 573.7 566.7 556.4 |
| v1 | 240.7 240.3 236.3 | 564.9 551.0 553.8 |

v1 is as slow as new, so the flag check on the `free` fast path is not the
cause. The disassembly of the exported `free` differs from old only by the
`multi_arena` load, its branch, and a hoisted load of the heap's home arena
(67 -> 74 instructions); `malloc` is byte-for-byte the same 47 instructions.

## Run 3: `SlabTable` stride probes (noisy run: run 1 of every variant, including old, dipped from host load)

- v3: `SlabTable` padded to 640 bytes (the old `ClassState` stride); v5: no tail, 256 bytes

| variant | larson M ops/s | xmalloc-test M frees/s |
|---|---|---|
| old | 262.2 239.9 230.6 | 587.4 334.3 567.2 |
| new | 228.9 191.6 224.5 | 537.2 481.5 539.7 |
| v3 | 220.7 125.6 206.4 | 551.2 531.2 519.5 |
| v5 | 224.2 96.3 207.6 | 536.8 529.8 500.1 |

The stride of the table array is not the cause either.

## Open

The regression is real (old ahead in every paired run) and unattributed.
Remaining suspects, in order: the slow paths' extra indirection
(`ClassState -> *const SlabTable` reloaded per slot in `claim` because the
raw-pointer deref cannot be hoisted across the bitmap `fetch_or`; the extra
descriptor `arena` load per slab in `release`), and code placement of the
hot `free` (7 more instructions). Fix before Phase 1 acceptance: the Phase 0
criterion "larson within 10% of jemalloc" does not hold on this tree.

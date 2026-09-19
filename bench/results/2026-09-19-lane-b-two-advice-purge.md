# Phase 2.5 task 10: the two-advice purge (lane B)

Library at 089c465 (`loop/lane-b`, based on main a3ddd6d), test fix at
9a19632 (tests only). Throwaway VMs `challoc-lane-b-arm64` (c4a-standard-16,
Neoverse V2) and `challoc-lane-b-amd64` (c4-standard-16, Xeon 8581C), zone
`us-east4-a`, Ubuntu 24.04 / 7.0.0-1011-gcp, 4 KiB pages, Docker 29.1.3,
cgroup2 at `/sys/fs/cgroup` (`nsdelegate`), created and deleted this
iteration. Companion tables from the same run: `2026-09-19-lane-b-two-advice-purge-ab-{aarch64,x86_64}.md`
(the protocol tier, default decay) and `...-ab-decay0-{aarch64,x86_64}.md`
(the same with `dirty_decay_ms:0`).

## 1. What changed

Design 8 and 11. A purge batch is issued with `MADV_FREE` (lazy) only under
the cgroup policy's `Low` band, usage under 70% of a known limit, and with
`MADV_DONTNEED` (eager) everywhere else: no limit, the scaled and immediate
bands, and every explicit purge (`malloc_trim(0)`, `arena.<i>.purge`, the
full purge a `memory.events` `high` rise owes). `lazy_free:true` is the
override (lazy in every band); the new `lazy_band` key (default true) turns
the saving off. The advice is read at each `Batch::flush` from
`policy::evaluate`'s mirror of the band (`purge::set_headroom`), so it is at
most one 10 ms sample behind. Init probes `MADV_FREE` through the
`process_madvise` target the `MADV_DONTNEED` probe found (feature-line key
`process_madvise_free`, `yes` on both reference kernels); where it fails,
lazy batches alone take the `madvise` loop. `stats.cgroup.purge_advice`,
`stats.cgroup.lazy_purges`, `stats.cgroup.eager_purges` (readable without a
tracked cgroup), a `purge advice:` report line and JSON fields say which
advice is in force and how many batches each issued. No fast path changed
(`src/heap.rs`, `src/class.rs`, `src/slab.rs`, `src/arena.rs`, `src/huge.rs`
untouched).

### Decision: no limit is eager

With no cgroup limit (the common case) the advice is `MADV_DONTNEED`.
`MADV_FREE` pages stay charged to the cgroup and visible in RSS until the
kernel reclaims them, which misleads Valkey's `mem_fragmentation_ratio`
(RSS over used memory; Valkey's defrag decisions read the allocator's own
`active`/`allocated`, which are unaffected, but the reported ratio is RSS)
and any autoscaler reading RSS or `memory.current`, exactly as design 8
already said about `lazy_free`. The lazy advice is therefore a saving the
policy grants only where it knows the headroom: under 70% of a known
limit, where the pages' charge is provably harmless (the kernel reclaims
lazy-free pages first when the cgroup nears `memory.high`/`memory.max`;
section 3 shows it) and the reuse saves the refault.

### Decision: the sticky zero flag

`purged_is_zero()` was per process (`lazy_free`); with a per-batch advice
the `Purged` state's "reads as zero" promise is per slab or per extent.
`class::take` and `huge::take` read `purge::purged_is_zero()` under their
locks and lane B may not edit them, so the choice was between (a) a
descriptor bit read there, (b) re-`MADV_DONTNEED`ing lazily purged memory
before hand-back (impossible from the purge side without those files), and
(c) a process-wide sticky flag: true until the first lazy batch, false for
good after it. (c) is implemented: `LAZY_EVER` is set in `Batch::flush`
before the discard, under the lock the walk holds, so a claim of one of
those ranges after the lock drops sees it. Cost: `calloc` on a `Purged`
slab or run reuse does a `memset` from then on (pages mostly still
resident under the lazy advice, so no fault), `Fresh` memory stays known
zero, `malloc` is unaffected. Measured in the container test (section 3):
`calloc(1 MiB)` after the lazy purge is zero (the run it got had the old
`0x5a`/`0x6a` fill under `MADV_FREE`, the `memset` fixed it), and the
protocol rows (section 4) move by less than noise, `alloc-testN` and
`mstress` included. A per-slab bit is the finer form if `calloc` on
`Purged` reuse ever shows in an attribution; it needs `class.rs` and
`huge.rs`.

## 2. Tests

Host: `cargo test --features std` 273 passed (policy `advice_at_the_band_edges`:
lazy for `L*0.7-1`, eager at `L*0.7`, `Scaled`, `Immediate`, `Unlimited`,
`limit 0`; `lazy_free` lazy everywhere including explicit purges;
`lazy_band:false` eager everywhere; explicit eager in every band; purge
`advice_follows_headroom_band_key_and_explicit_purges`; features Display and
line; config key round trip, 23 keys; mallctl `[7,7,8..10]`; report
needles).

VMs, `make test` at 9a19632 (both): container `cargo test` 345 + 108, the
`--memory=512m` group 23, `test-cgroup-churn` four rows pass unchanged
(`purge 400 100` at 512m: 89.8% usage, scaled band, eager; `purge 200 50` at
256m: 79.7%, eager), `test-cgroup-lazy` two rows pass (section 3); the
`decay.c` modes including `lazy` (`lazy_free:true`) pass inside the
`api::` group; `features.c` sees 16 (arm64) / 18 (x86_64) tokens with the
new key. At 089c465 the arm64 run was green and the amd64 run failed two
purge unit tests on an exact batch-count assertion racing other tests'
flushes (715 vs 714) and the poisoned test mutex; 9a19632 makes the count a
lower bound and the lock poison-tolerant (test code only).
`make test-cgroup-oscillate` (300 s, 512 MiB, 200..480 MiB live): arm64 98
cycles, `current_peak` 502 MiB, `max 0 oom_kill 0`, `current 293 MiB
empty_bytes 20 MiB` at the end; amd64 97 cycles, 503 MiB, `max 0 oom_kill
0`, `current 294 MiB empty_bytes 24 MiB`: the same as
`2026-09-18-aarch64-gce-cgroup-container-tests.md` (98 cycles, 502 MiB).

## 3. The container variant (`tests/c/cgroup_lazy.c`, `make test-cgroup-lazy`)

`docker run --memory=512m --memory-swap=512m`, one container per row; 200
MiB in the churn test's mix (100 x 1 MiB, 1600 x 32 KiB, 51200 x 1 KiB,
every byte written), freed, then the idle trickle. Usage at the frees is
204 MiB of 512 (39%): the `Low` band. Kernel figures from the cgroup's
`memory.stat` (`pglazyfree`: pages marked lazy-free by `MADV_FREE`;
`pglazyfreed`: of those, pages the kernel then reclaimed) and
`memory.current`; the program's RSS from `/proc/self/statm`.

| VM | mode | first purge after | RSS peak / at 2 s | `memory.current` peak / at 2 s | `pglazyfree` rise | `lazy_purges` / `eager_purges` at 2 s | `purge_advice` | `empty_bytes` at 2 s (cap 25) | alloc 200 MiB / realloc same shape | pressure: 380 MiB of 8 KiB, `memory.current`, `pglazyfreed` rise | result |
|---|---|---:|---:|---:|---:|---:|---|---:|---:|---|---|
| arm64 | lazy (defaults) | 80 ms | 202 / 202 MiB | 205 / 205 MiB | 48374 pages (188 MiB) | 25 / 0 | lazy | 10 MiB | 57 ms / **5 ms** | 99 ms, 511 MiB (`memory.peak` 512), 15554 pages (60 MiB), `max 283` events, alive | pass |
| arm64 | eager (`lazy_band:false`) | 79 ms | 202 / 13 MiB | 203 / 14 MiB | 0 | 0 / 25 | eager | 10 MiB | 64 ms / **44 ms** | 87 ms, 387 MiB, 0 | pass |
| amd64 | lazy (defaults) | 79 ms | 203 / 203 MiB | 205 / 205 MiB | 48576 pages (189 MiB) | 35 / 0 | lazy | 9 MiB | 88 ms / **42 ms** | 159 ms, 511 MiB (`memory.peak` 512), 16431 pages (64 MiB), alive | pass |
| amd64 | eager (`lazy_band:false`) | 77 ms | 203 / 13 MiB | 203 / 14 MiB | 0 | 0 / 35 | eager | 9 MiB | 90 ms / **72 ms** | 152 ms, 388 MiB, 0 | pass |

What the rows show. The allocator's side is identical in both modes: the
first walk after the frees (79..80 ms) finds retention over the 5% cap and
purges the whole excess, `empty_bytes` is under the cap at 2 s, the batch
count is the same. The kernel's side differs exactly as advertised: under
`MADV_FREE` RSS and `memory.current` do not move (202 and 205 MiB, the
pages are mapped and charged), `memory.stat` moves 189 MiB from the anon
LRU to `inactive_file` and `pglazyfree` counts them; under `MADV_DONTNEED`
RSS is 13 MiB within 80 ms and `pglazyfree` stays 0. The refault saving:
allocating the same 200 MiB again costs 5 ms after the lazy purge against
44 ms after the eager one on the C4A (the eager first allocation was 64
ms), 42 against 72 ms on the C4 (first 88 ms). Reclaimability: with 200
MiB lazy-free still charged, a fresh 380 MiB set of 8 KiB objects (a class
the program had not used, so the allocator cannot hand the lazy pages
back to it) drives `memory.current` to 511 of 512 MiB; the kernel reclaims
60..64 MiB of lazy-free pages (`pglazyfreed`) plus the container's page
cache at the limit (`memory.events` `max 283` on arm64, no `oom_kill`),
and the program exits 0. `memory.reclaim` is not writable in the default
Docker profile (`EROFS`), so the pressure set is the proof.

The `calloc(1 MiB)` after the lazy purge reads zero in every row (the
sticky flag's `memset`; without it the run would carry the old fill).

## 4. The protocol tier, default decay (`ab`, 5 rounds, `--memory=8g`)

`make ab` inside one `docker run --memory=8g --memory-swap=8g` container
per VM (usage stays under 70% of the limit through the run, so the default
variant's advice is lazy; the `stats.cgroup.purge_advice` read of the
container test confirms the band at this usage), three variants built from
git: `base` a3ddd6d, `eager` 089c465 with `MALLOC_CONF=lazy_band:false`,
`lazy` 089c465 defaults. Medians of 5 timed rounds after a warm-up,
interleaved. Positive is worse than `base`.

| VM | program | unit | base | eager | lazy | eager vs base | lazy vs base | RSS base / eager / lazy MiB |
|---|---|---|---:|---:|---:|---:|---:|---|
| arm64 | mstress | s | 0.377 | 0.377 | 0.375 | +0.0% | -0.5% | 318.2 / 323.5 / 316.9 |
| arm64 | alloc-testN | s | 2.277 | 2.282 | 2.281 | +0.2% | +0.2% | 16.4 / 16.4 / 16.3 |
| arm64 | larson10 | M ops/s | 370.2 | 367.9 | 368.4 | +0.6% | +0.5% | 113.2 / 112.2 / 113.6 |
| amd64 | mstress | s | 0.598 | 0.596 | 0.596 | -0.3% | -0.3% | 329.8 / 339.1 / 335.8 |
| amd64 | alloc-testN | s | 4.421 | 4.403 | 4.368 | -0.4% | -1.2% | 17.1 / 17.3 / 17.2 |
| amd64 | larson10 | M ops/s | 285.6 | 293.4 | 289.4 | -2.7% | -1.3% | 96.4 / 97.3 / 96.4 |

Every cell is within the run's noise (the sorted lists overlap on every
row). These rows hardly purge within their run under the default 10 s
window, so they measure the plumbing (nothing on a fast path changed) and
not the saving; the saving is section 3's reallocation and section 5.
The RSS column is `rt`'s peak `maxrss`, which the advice does not change.

## 5. The protocol tier with `dirty_decay_ms:0` (the purge path exercised)

Same container and rows, both variants 9a19632 (the same library as
089c465; the fix is test code) with `MALLOC_CONF=dirty_decay_ms:0`, which
purges every `Empty` slab and extent at the next periodic tick, so every
reuse of freed memory goes through a purge: `eager0` adds
`lazy_band:false`, `lazy0` is the default advice (lazy under the 8 GiB
limit). 5 timed rounds after a warm-up, interleaved; positive is worse
than `eager0`.

| VM | program | unit | eager0 | lazy0 | lazy0 vs eager0 | sorted eager0 | sorted lazy0 | RSS eager0 / lazy0 MiB |
|---|---|---|---:|---:|---:|---|---|---|
| arm64 | mstress | s | 1.002 | 0.516 | **-48.5%** | 0.964 0.981 1.002 1.003 1.006 | 0.511 0.513 0.516 0.517 0.524 | 259.6 / 313.9 |
| arm64 | alloc-testN | s | 2.280 | 2.281 | +0.0% | 2.265 2.267 2.280 2.280 2.283 | 2.268 2.274 2.281 2.283 2.290 | 16.1 / 16.4 |
| arm64 | larson10 | M ops/s | 348.1 | 359.9 | **-3.3%** (faster) | 347.4 348.1 348.1 352.2 353.5 | 357.6 359.5 359.9 361.2 361.8 | 97.2 / 113.1 |
| amd64 | mstress | s | 1.087 | 0.915 | **-15.8%** | 1.074 1.079 1.087 1.088 1.093 | 0.908 0.914 0.915 0.918 0.919 | 264.0 / 323.5 |
| amd64 | alloc-testN | s | 4.449 | 4.380 | -1.6% | 4.363 4.424 4.449 4.481 4.500 | 4.358 4.377 4.380 4.397 4.398 | 17.1 / 17.3 |
| amd64 | larson10 | M ops/s | 236.1 | 238.6 | -1.0% (faster) | 233.5 235.8 236.1 236.7 238.9 | 235.4 235.7 238.6 239.5 244.4 | 79.0 / 94.3 |

This is the refault saving with the purge path exercised: mstress, whose
threads free whole working sets and allocate them again, runs in half the
time on the C4A (the sorted lists do not overlap) and 16% faster on the
C4; larson10 gains 3% on the C4A (lists disjoint), 1% on the C4 (within
noise); alloc-testN, whose freed memory is small, is unchanged. The other
side of the trade in the RSS column: the lazily returned pages stay in
`maxrss` until the kernel wants them, 54..59 MiB more on mstress, 15..16
MiB more on larson10, the RSS-as-seen-by-the-cgroup difference the plan
asks to document (section 3 has the same under a 512 MiB limit:
`memory.current` 205 MiB lazy against 14 MiB eager 2 s after the frees,
and 511 MiB at the limit with the lazy pages then reclaimed). With the
default 10 s window (section 4) neither effect is visible in these rows.

## 6. `quick` against a3ddd6d (default container, no limit: eager, plumbing)

| VM | larson1 | larson10 | xmalloc | alloc-test1 | valkey1 | churn | verdict |
|---|---:|---:|---:|---:|---:|---|---|
| arm64 | 42.2 -> 42.6 M ops/s (-0.9%) | 368.8 -> 372.3 (-0.9%) | 601.2 -> 602.0 M frees/s (-0.1%) | 2.629 -> 2.660 s (+1.2%) | 7.031 -> 6.994 M ops/s (+0.5%) | 6801 -> 3462 slabs (97.0%), compacts | go |
| amd64 | 46.1 -> 46.2 (-0.2%) | 282.1 -> 288.5 (-2.2%) | 333.9 -> 350.5 (-4.7%) | 3.075 -> 3.093 (+0.6%) | 15.564 -> 15.449 (+0.7%) | 6801 -> 3462 (97.0%), compacts | go |

(old = a3ddd6d, new = 089c465 tree; one round each, load 4.2 at start
after the `ab`; the libraries differ: `f075875a` vs `ab7ebcdd` on arm64,
`41d8017c` vs `b056297c` on amd64.)

## 7. Notes

- Lazily returned pages stay charged, so the `memory.current` sample
  counts them and the band reaches 70% sooner than live memory alone
  would, flipping the advice to eager and shortening the window while the
  memory is in fact plentiful: a conservative error, recorded in design
  11. The oscillation (section 2) is unchanged by it: its frees happen at
  90%+ and are eager.
- `process_madvise(MADV_FREE)` on self works on both 7.0 reference
  kernels (`process_madvise_free=yes`); the fallback path (loop for lazy
  batches only) has run in the host-side wiring test but not on a kernel
  that refuses it; the matrix of plan task 3 is where such a cell would
  appear (5.10..6.12 refuse `MADV_DONTNEED` too, so there both advices
  take the loop).

# challoc opt-in tunables (`zero_on_free`, `slot_randomize`), 2026-09-18, aarch64 (GCE C4A)

Phase 2 task 3 (`docs/plan.md`; review 2026-09-18 section 5.1): the two
hardening keys implemented as opt-in, and their price. Code commits
5c2c741 (the keys) and 7087c44 (the clear moved off the fast push, section
3) on top of 320bbfb (`base`); `off`, `zero` and `rand` in section 4 are
the 7087c44 library with the key off or on through `MALLOC_CONF`.
`double_free_scan`, the third key of the task, was implemented and priced
by Phase 1.5 task 9c: `bench/results/2026-09-18-aarch64-gce-detection-gaps.md`
section 4 (+3.1% / +3.7% larson1, up to +14% valkey1 on x86_64; off by
default); it is not redone here.

- host: challoc-bench-arm64 (c4a-standard-16, Google Axion / Neoverse V2), us-east4-a, started, synced, driven and stopped by this
  iteration (CHALLOC_GCE_WARM unset). Zone `us-east4-a`.

## 1. The mechanisms

- `zero_on_free` (`Shared::push_flags` bit `PUSH_ZERO`, one byte on the
  first line of `Shared` next to `multi_arena`, the line `superblock` and
  `lg_stride` come from; `PUSH_SCAN` on the same byte is `double_free_scan`).
  The push (`Heap::push_cache_flags`) tests the byte once; zero, the
  default, is the plain push. With the bit set, `heap::zero_slot` clears
  the slot's class-size bytes before the push on the plain path (`free`,
  after the bit test), on the sized path (`sdallocx`, `free_sized`; the
  class from the size hint, as the push trusts it) and on a direct release
  (`MALLOCX_TCACHE_NONE`); a slot that goes on to the flush path is not
  cleared twice. A huge free clears the run's usable bytes
  (`Shared::zero_huge`, under the huge lock for the size) before the
  release. Cost: a `memset` of the class size per free, which is why the
  key is off: a 1 KiB store per Valkey value free is more than the 3%
  hardening budget (review 5.1).
- `slot_randomize` (`ClassState::claim_random`, `Heap::claim`): a cache
  refill or run rebuild plans the same slabs and reservations as the
  ordered claim (`plan` is untouched, so the lowest partial slab is still
  taken and a `Fresh`/`Purged` slab's prefix is still claimed under the
  lock), but every other pick's bit scan starts at a bitmap word drawn from
  the heap's xorshift instead of word 0, and the batch is shuffled
  (Fisher-Yates) before it becomes the cache's stack or run. The shuffle
  changes the order, not the slot set, so `Claim::zeroed` and the zero
  prefix are unaffected (`check_zero` stays clean, section 4). The
  generator is per heap (`Heap::rng`, 8 bytes below the header array),
  seeded from `getrandom` on the heap's first randomised claim, mixed with
  the heap address and tid: the default configuration makes no syscall
  per heap creation (unit test `slot_randomize_seeds_the_heap_lazily`).
  Read from `Shared::config` on the slow path only; the fast paths carry
  nothing for it. Cost: the shuffle per refill, and the loss of address
  order: the compacting run (design 6.4) serves a random slot of the
  lowest slab with room instead of its lowest, and LIFO reuse no longer
  hands out ascending addresses, which is the documented trade. Direct
  claims stay address-ordered.

## 2. `quick`, keys off: 7087c44 against 320bbfb (go)

`bench/gce/challoc-bench quick arm64 OLD=320bbfb` (one round, jemalloc in the
same run; the churn probe under new compacts):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.5 (15.5 MiB) | 42.4 (15.8 MiB) | 45.2 (18.2 MiB) | +0.2% | +6.6% |
| larson10 | M ops/s | 368.8 (112.4 MiB) | 374.1 (113.5 MiB) | 386.0 (131.8 MiB) | -1.4% | +3.2% |
| xmalloc | M frees/s | 613.974 (53.8 MiB) | 618.104 (55.2 MiB) | 482.987 (81.1 MiB) | -0.7% | -21.9% |
| alloc-test1 | s | 2.644 (13.0 MiB) | 2.627 (13.0 MiB) | 2.508 (12.3 MiB) | -0.6% | +4.7% |
| valkey1 | M ops/s | 7.077 (584.2 MiB) | 6.925 (584.2 MiB) | 7.013 (585.6 MiB) | +2.2% | +1.3% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24525090 ops/s |

Positive percentages mean new did worse (slower, lower throughput). One round each, aarch64, nproc 16, load at start 2.95, 121 s total. old 9958c54e2b1faef2, new c84cb6b229e38f29.

## 3. The first form (5c2c741) and why it was reworked

5c2c741 called the `memset` from inside the inlined push
(`push_cache_flags`, behind the flag test). Its `quick` against 320bbfb was
"go" on both VMs (aarch64 larson1 +1.4%, larson10 +0.1%, valkey1 +0.2%;
x86_64 larson1 +2.5%, larson10 +2.1%, valkey1 +10.6% in the single round),
but the 5-round `ab` (`VARIANTS="base=320bbfb off=5c2c741
zero=5c2c741:zero_on_free:true rand=5c2c741:slot_randomize:true jemalloc"
BENCHES="larson1 larson10 redis valkey1" ROUNDS=5`; A/B: 2026-09-19T01:07:45Z aarch64 nproc 16 load 0.15 0.79 0.45, HEAD 5c2c741) put
the off state behind base with non-overlapping lists on aarch64 larson:

| program | unit | base | base RSS MiB | off | off RSS MiB | zero | zero RSS MiB | rand | rand RSS MiB | jemalloc | jemalloc RSS MiB | off vs base | zero vs base | rand vs base | jemalloc vs base |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.3 | 15.7 | 41.5 | 15.5 | 22.6 | 15.1 | 41.0 | 17.8 | 45.1 | 18.6 | +1.9% | +87.2% | +3.2% | -6.2% |
| larson10 | M ops/s | 369.4 | 112.5 | 361.2 | 111.6 | 208.6 | 98.1 | 357.7 | 119.4 | 387.7 | 132.8 | +2.3% | +77.1% | +3.3% | -4.7% |
| redis | k req/s | 502.5 | 29.0 | 499.8 | 29.0 | 494.6 | 31.7 | 499.8 | 29.3 | 413.2 | 31.1 | +0.5% | +1.6% | +0.5% | +21.6% |
| valkey1 | M ops/s | 7.059 | 584.5 | 7.109 | 584.5 | 4.554 | 584.4 | 7.007 | 584.9 | 7.242 | 585.5 | -0.7% | +55.0% | +0.7% | -2.5% |
Positive percentages mean the variant did worse than base (slower or lower throughput).

Attribution, `bench/gce/challoc-bench quick-perf arm64 OLD=320bbfb ROWS="larson1
valkey1"` (one run per cell, perf on the VM host; `new` = 5c2c741):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 42.02 | 71.4 | 249.8 | 2.106 | 4.092 |
| larson1 | new | 41.14 | 72.9 | 253.9 | 2.096 | 4.110 |
| larson1 | jemalloc | 44.72 | 66.6 | 224.8 | 2.072 | 1.741 |
| valkey1 | old | 13.16 | 230.2 | 258.2 | 3.086 | 2.899 |
| valkey1 | new | 12.56 | 241.4 | 262.9 | 3.100 | 2.949 |
| valkey1 | jemalloc | 13.84 | 218.7 | 169.5 | 2.983 | 2.696 |

Read with the disassembly (`objdump -d` of `free`, `sdallocx` and `malloc`
in the 320bbfb and 5c2c741 libraries, addresses masked, in the dev
container): `malloc` was identical; `free` and `sdallocx` carried the
inlined `memset` call, and a call inside the inlined fast path made the
compiler save a callee-saved register in the entry point's prologue
(`sdallocx`: frame 0x20 -> 0x50, `str x19`/`ldr x19` around the whole
function), an unconditional store and load per default free. That is the
+4 / +7 instructions per larson1 op and +5 / +10 per valkey1 op above
(aarch64 / x86_64), with L1D and dTLB misses per op unchanged in every
cell. 7087c44 makes the push decline under the key (`false`, the same
answer as a full cache) and lets `free_slow_in`, the cold path the caller
already has, clear and push; the default `free`'s disassembly is then the
old loads with the flag test ahead of the `len` test and no spill, which
section 2 and 4 measure. The x86_64 valkey1 single round of 5c2c741's
`quick` (+10.6%) also exposed a gate bug: `quick.sh` compared the
percentage as a string after stripping `%`, so a two-digit regression
passed ("10.6" < "5" lexically) and was never confirmed; fixed in this
iteration (`d+0 > t+0`).

Sorted scores of that `ab`, for the record (the `zero` and `rand` prices
are re-measured for 7087c44 in section 4):

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | base | 42.1 42.3 42.3 42.4 42.4 | 16028 |
| larson1 | off | 41.4 41.4 41.5 41.6 41.6 | 15896 |
| larson1 | zero | 22.6 22.6 22.6 22.6 22.7 | 15512 |
| larson1 | rand | 40.9 40.9 41.0 41.0 41.0 | 18188 |
| larson1 | jemalloc | 45.1 45.1 45.1 45.2 45.2 | 19044 |
| larson10 | base | 366.3 369.2 369.4 369.9 374.4 | 115228 |
| larson10 | off | 359.9 360.9 361.2 362.2 364.8 | 114328 |
| larson10 | zero | 207.5 208.3 208.6 208.8 209.7 | 100468 |
| larson10 | rand | 355.3 355.7 357.7 358.3 358.7 | 122260 |
| larson10 | jemalloc | 387.0 387.5 387.7 389.6 390.7 | 135968 |
| redis | base | 499.0 499.8 502.5 503.3 503.5 | 29736 |
| redis | off | 495.8 496.5 499.8 503.3 503.5 | 29732 |
| redis | zero | 490.7 491.6 494.6 495.0 497.0 | 32420 |
| redis | rand | 497.8 498.8 499.8 500.5 502.3 | 29988 |
| redis | jemalloc | 409.3 409.5 413.2 414.3 415.3 | 31804 |
| valkey1 | base | 6.971 7.020 7.059 7.151 7.165 | 598528 |
| valkey1 | off | 7.044 7.107 7.109 7.125 7.150 | 598544 |
| valkey1 | zero | 4.406 4.536 4.554 4.690 4.698 | 598380 |
| valkey1 | rand | 6.886 6.956 7.007 7.093 7.183 | 598928 |
| valkey1 | jemalloc | 7.123 7.241 7.242 7.267 7.287 | 599556 |

## 4. The price list: `ab`, 5 rounds, keys off and on (7087c44 against 320bbfb)

`bench/gce/challoc-bench ab arm64 VARIANTS="base=320bbfb off=7087c44
zero=7087c44:zero_on_free:true rand=7087c44:slot_randomize:true jemalloc"
BENCHES="larson1 larson10 redis valkey1" ROUNDS=5` (A/B: 2026-09-19T01:29:46Z aarch64 nproc 16 load 1.79 1.74 1.50, HEAD 7087c44; `off`, `zero`
and `rand` are one library, the key set through `MALLOC_CONF`):

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | base | 42.2 42.4 42.4 42.4 42.4 | 16028 |
| larson1 | off | 42.3 42.3 42.4 42.4 42.4 | 16144 |
| larson1 | zero | 17.3 17.3 17.3 17.3 17.3 | 14992 |
| larson1 | rand | 41.5 41.7 41.8 41.8 41.8 | 18072 |
| larson1 | jemalloc | 45.0 45.1 45.1 45.1 45.2 | 19104 |
| larson10 | base | 367.3 370.7 371.7 371.8 373.8 | 116104 |
| larson10 | off | 366.8 367.1 368.4 369.1 369.8 | 114968 |
| larson10 | zero | 104.8 105.7 110.4 111.8 114.4 | 91928 |
| larson10 | rand | 361.8 362.4 366.6 368.3 368.9 | 123784 |
| larson10 | jemalloc | 384.1 389.0 391.2 391.8 393.0 | 135720 |
| redis | base | 498.5 502.0 502.5 503.8 505.1 | 29736 |
| redis | off | 499.3 501.8 502.8 505.1 507.4 | 29732 |
| redis | zero | 445.4 451.1 451.9 452.5 453.3 | 32420 |
| redis | rand | 505.1 505.6 506.1 506.6 506.8 | 29988 |
| redis | jemalloc | 410.2 410.7 412.9 413.1 413.2 | 31804 |
| valkey1 | base | 6.923 6.961 7.075 7.136 7.144 | 598532 |
| valkey1 | off | 7.110 7.131 7.140 7.171 7.176 | 598544 |
| valkey1 | zero | 3.812 3.831 3.838 3.869 3.882 | 598252 |
| valkey1 | rand | 7.130 7.142 7.157 7.161 7.171 | 598920 |
| valkey1 | jemalloc | 7.029 7.066 7.234 7.274 7.337 | 599856 |

Medians:

| program | unit | base | base RSS MiB | off | off RSS MiB | zero | zero RSS MiB | rand | rand RSS MiB | jemalloc | jemalloc RSS MiB | off vs base | zero vs base | rand vs base | jemalloc vs base |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.4 | 15.7 | 42.4 | 15.8 | 17.3 | 14.6 | 41.8 | 17.6 | 45.1 | 18.7 | +0.0% | +145.1% | +1.4% | -6.0% |
| larson10 | M ops/s | 371.7 | 113.4 | 368.4 | 112.3 | 110.4 | 89.8 | 366.6 | 120.9 | 391.2 | 132.5 | +0.9% | +236.7% | +1.4% | -5.0% |
| redis | k req/s | 502.5 | 29.0 | 502.8 | 29.0 | 451.9 | 31.7 | 506.1 | 29.3 | 412.9 | 31.1 | -0.1% | +11.2% | -0.7% | +21.7% |
| valkey1 | M ops/s | 7.075 | 584.5 | 7.140 | 584.5 | 3.838 | 584.2 | 7.157 | 584.9 | 7.234 | 585.8 | -0.9% | +84.3% | -1.1% | -2.2% |
Positive percentages mean the variant did worse than base (slower or lower throughput).

The off state is at parity with base on every row (larson1 +0.0%, larson10
+0.9%, redis -0.1%, valkey1 -0.9%; the sorted lists overlap on every row).
`zero_on_free` costs larson1 +145%, larson10 +237% (a `memset` of the class
size plus the slow-path call per free, on a benchmark whose operation is
the malloc/free pair itself; RSS falls because every free is now a slow
path that ticks the periodic work, so the cache GC and decay run at every
64th free instead of every 64th refill or flush), redis +11.2% (memtier: the server's frees are a
fraction of a request) and the Valkey model +84% (the 1 KiB `memset` per
value free on a loop of SET/DEL). `slot_randomize` costs larson1 +1.4% and
larson10 +1.4% (non-overlapping lists on larson1: 41.5..41.8 against
42.2..42.4), redis -0.7% and valkey1 -1.1% (noise), and +12% / +7% RSS on
larson1 / larson10 (17.6 against 15.7 MiB, 120.9 against 113.4 MiB): the
cache hands out random slots of the low slabs, so the live set is spread
over more slabs than the address-ordered claim leaves it in, the cost the
design predicts for a key that works against 6.4's compaction.

## 5. Attribution of the landed default paths (`quick-perf`, 7087c44)

`bench/gce/challoc-bench quick-perf arm64 OLD=320bbfb ROWS="larson1 valkey1"`
(one run per cell, `sudo perf stat --inherit -p` on the container's init
pid from the VM host; `new` = 7087c44 with both keys off):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 41.84 | 71.7 | 249.8 | 2.093 | 4.143 |
| larson1 | new | 42.07 | 71.3 | 249.8 | 2.099 | 4.024 |
| larson1 | jemalloc | 44.69 | 66.6 | 224.8 | 2.068 | 1.734 |
| valkey1 | old | 12.81 | 236.4 | 259.4 | 3.087 | 2.928 |
| valkey1 | new | 12.91 | 234.7 | 257.1 | 3.086 | 2.871 |
| valkey1 | jemalloc | 13.71 | 220.7 | 169.7 | 2.979 | 2.729 |

Instructions per larson1 op are identical (249.8 against 249.8: the flag
byte was already loaded and tested by 320bbfb for `double_free_scan`, and
7087c44 tests the same byte one instruction earlier); valkey1 is -2.3
instructions per op (the plain push no longer tests `len` for the scan)
and -0.7% cycles; L1D and dTLB misses per op are unchanged. The default
paths carry nothing of either key but that test.

## 6. Decision

Both keys land as opt-in and stay off by default (review 2026-09-18
section 5.1): `zero_on_free` costs a `memset` per free (+84% / +242% on the
Valkey model, +145..237% on larson, +11..14% on redis), far beyond the 3%
hardening budget; `slot_randomize` is within noise on throughput but costs
+7..12% RSS on larson (the live set spread over more slabs) and gives up
the address order that 6.4's compaction relies on. The off state is at
parity with 320bbfb (section 4 aarch64, section 5 both). The Phase 2
acceptance criterion ("default hardening costs no more than 3%") is not
touched by either key, since neither is on by default; these rows are the
operator's price list. `double_free_scan`'s rows are in the detection-gaps
files.

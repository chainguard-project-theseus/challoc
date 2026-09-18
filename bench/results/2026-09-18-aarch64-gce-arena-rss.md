# challoc narenas, 2026-09-18, aarch64

- commit: 523ad84
- date: 2026-09-18T08:04:48Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce-c4a), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.00 1.54 3.85; at end: 13.23 11.30 8.18; 681 s total
- method: 3 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than def.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - def: 523ad843725b built into target/ab/523ad843725b
  - n4: 523ad843725b built into target/ab/523ad843725b, MALLOC_CONF=narenas:4
  - n8: 523ad843725b built into target/ab/523ad843725b, MALLOC_CONF=narenas:8
  - n16: 523ad843725b built into target/ab/523ad843725b, MALLOC_CONF=narenas:16
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | def | 42.4 42.5 42.5 | 16820 |
| larson1 | n4 | 42.6 42.6 42.7 | 16820 |
| larson1 | n8 | 42.4 42.6 42.6 | 16820 |
| larson1 | n16 | 42.3 42.4 42.6 | 16948 |
| larson1 | jemalloc | 45.0 45.1 45.1 | 19064 |
| larson10 | def | 367.4 368.1 374.2 | 119456 |
| larson10 | n4 | 349.5 351.3 354.5 | 118572 |
| larson10 | n8 | 361.8 363.6 364.7 | 126888 |
| larson10 | n16 | 368.0 369.5 369.5 | 119348 |
| larson10 | jemalloc | 382.1 386.7 389.3 | 135400 |
| larson16 | def | 594.4 597.0 599.7 | 186932 |
| larson16 | n4 | 566.4 567.9 570.0 | 179380 |
| larson16 | n8 | 579.0 579.9 584.0 | 187700 |
| larson16 | n16 | 588.7 594.0 595.5 | 193716 |
| larson16 | jemalloc | 609.9 614.9 619.0 | 211568 |
| xmalloc | def | 554.914 561.794 562.311 | 86436 |
| xmalloc | n4 | 714.858 717.214 718.025 | 67704 |
| xmalloc | n8 | 641.158 643.890 648.877 | 84520 |
| xmalloc | n16 | 584.215 587.779 598.121 | 80312 |
| xmalloc | jemalloc | 468.296 479.748 480.121 | 82536 |
| xmallocN | def | 487.305 487.894 489.590 | 182708 |
| xmallocN | n4 | 646.225 649.919 651.013 | 94776 |
| xmallocN | n8 | 608.886 614.867 618.102 | 129064 |
| xmallocN | n16 | 515.668 528.405 530.405 | 175540 |
| xmallocN | jemalloc | 482.995 487.122 488.411 | 112360 |
| mstress | def | 0.398 0.398 0.398 | 394604 |
| mstress | n4 | 0.376 0.376 0.377 | 329808 |
| mstress | n8 | 0.385 0.385 0.386 | 364956 |
| mstress | n16 | 0.394 0.398 0.399 | 380336 |
| mstress | jemalloc | 0.641 0.642 0.649 | 299024 |
| alloc-testN | def | 2.280 2.285 2.446 | 21364 |
| alloc-testN | n4 | 2.262 2.271 2.283 | 17280 |
| alloc-testN | n8 | 2.265 2.269 2.271 | 18808 |
| alloc-testN | n16 | 2.273 2.288 2.464 | 21248 |
| alloc-testN | jemalloc | 2.206 2.207 2.215 | 16720 |

## Medians

| program | unit | def | def RSS MiB | n4 | n4 RSS MiB | n8 | n8 RSS MiB | n16 | n16 RSS MiB | jemalloc | jemalloc RSS MiB | n4 vs def | n8 vs def | n16 vs def | jemalloc vs def |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.5 | 16.4 | 42.6 | 16.4 | 42.6 | 16.4 | 42.4 | 16.6 | 45.1 | 18.6 | -0.2% | -0.2% | +0.2% | -5.8% |
| larson10 | M ops/s | 368.1 | 116.7 | 351.3 | 115.8 | 363.6 | 123.9 | 369.5 | 116.6 | 386.7 | 132.2 | +4.8% | +1.2% | -0.4% | -4.8% |
| larson16 | M ops/s | 597.0 | 182.6 | 567.9 | 175.2 | 579.9 | 183.3 | 594.0 | 189.2 | 614.9 | 206.6 | +5.1% | +2.9% | +0.5% | -2.9% |
| xmalloc | M frees/s | 561.794 | 84.4 | 717.214 | 66.1 | 643.890 | 82.5 | 587.779 | 78.4 | 479.748 | 80.6 | -21.7% | -12.8% | -4.4% | +17.1% |
| xmallocN | M frees/s | 487.894 | 178.4 | 649.919 | 92.6 | 614.867 | 126.0 | 528.405 | 171.4 | 487.122 | 109.7 | -24.9% | -20.7% | -7.7% | +0.2% |
| mstress | s | 0.398 | 385.4 | 0.376 | 322.1 | 0.385 | 356.4 | 0.398 | 371.4 | 0.642 | 292.0 | -5.5% | -3.3% | +0.0% | +61.3% |
| alloc-testN | s | 2.285 | 20.9 | 2.271 | 16.9 | 2.269 | 18.4 | 2.288 | 20.8 | 2.207 | 16.3 | -0.6% | -0.7% | +0.1% | -3.4% |

Positive percentages mean the variant did worse than def (slower or lower throughput).

## Reading of the narenas A/B (both VMs)

Against jemalloc in the same run (positive = challoc worse): on aarch64 the
default is larson1 -5.8%, larson10 -4.8%, larson16 -2.9%; `n4` is larson10
-9.2% and larson16 -7.6% (RSS: mstress 322 vs 292 MiB = +10.3%, xmallocN 93
vs 110, alloc-testN 16.9 vs 16.3); `n8` is larson10 -6.0%, larson16 -5.7%
(RSS: mstress +22%, xmallocN +15%, alloc-testN +13%). On x86_64 (file
`2026-09-18-x86_64-gce-arena-rss.md`) `n4` is larson16 -12.4% (286 vs 327 M
ops/s) and `n8` larson16 -7.8% with mstress +31% and alloc-testN +15% RSS.
No arena count passes every row on both VMs: the count trades larson16 time
against mstress and alloc-testN RSS, and the failing RSS rows are not the
arena count as such but what each arena multiplies (below). xmalloc-test
is faster with fewer arenas on both VMs (n4 -21.7% / -9.8% better than the
default on aarch64 / x86_64: fewer foreign caches per consumer thread to
flush into).

## RSS attribution by probe (aarch64, one run each, default = 64 automatic arenas)

`bench/probes/arenasample.c` is an `LD_PRELOAD` sidecar: every second it
calls `malloc_stats_print`, sums the per-arena `small:` and `large:` lines
(allocated = curregs x size, i.e. live plus thread-cached slots; active =
bytes of non-Empty slabs; dirty = Empty slabs waiting for decay) and, with
`ARENASAMPLE_SMAPS=1`, the `Rss` of `/proc/self/smaps` by mapping name
(`challoc:classes`, `challoc:meta`, `challoc:huge`). The Phase 0 command
lines at 16 threads (`xmalloc-test -w 16` has 32 threads: 16 producers and
16 consumers, a shared queue bounded at 100 batches of 4096 objects, 26 MiB
in flight at most). Library: c4a0772/523ad84 (before any change). MiB.

| bench | conf | VmRSS | small allocated | small active | small dirty | large allocated | large active | large dirty | smaps classes | smaps meta |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| xmalloc -w 16 | default (18 arenas) | 170..185 | 45.5 | 49.1 | 37..51 | 71.2 | 86.0 | 10..12 | 164..179 | 4.7 |
| xmalloc -w 16 | narenas:4 | 89..97 | 34.6 | 34.9 | 26..32 | 22.0 | 26.0 | 6.0 | | |
| xmalloc -w 16 | narenas:1 | 44.2 | 11.1 | 11.4 | 20.5 | 7.0 | 8.0 | 4.0 | 41.4 | 0.6 |
| mstress 16 (peak sample) | default (16 arenas) | 368..380 | 32.1 | 39.9 | 17.2 | 226.1 | 359.8 | 123.2 | 197..233 | 13.4..14.1 |
| mstress 16 (peak) | narenas:4 | 320 | 18.2 | 26.6 | 14.0 | 118.2 | 184.5 | 132.7 | | |
| mstress 16 (peak) | narenas:1 | 302 | 9.9 | 21.4 | 13.6 | 70.9 | 135.6 | 151.6 | 143..145 | 1.4 |
| alloc-test 16 | default (18 arenas) | 19.2..19.4 | 16.5 | 21.1 | 0 | 4.1 (16.3 before `tcache_max`) | 66.0 | 0 | 10.5..10.8 | 4.7 (7.1 at exit) |
| alloc-test 16 | narenas:1 | 16.6..16.7 | 16.5 | 17.2 | 0 | 4.1 | 6.0 | 0 | 10.7..11.1 | 2.0 |

(`challoc:huge` is 158..162 MiB for mstress under every conf; the huge
allocator is shared by all arenas and is not part of the gap.)

What the arenas multiply:

1. xmalloc-test, 70 MiB: the batch structs (4096 pointers, a 40 KiB slab
   class) are freed by consumers into their cache for the producer's arena,
   8 slots each (`MIN_CAP`), 16 consumers x 16 producer arenas x 8 x 40 KiB
   = 80 MiB of "large allocated" that is neither live nor reachable by any
   producer. jemalloc caches nothing above 32 KiB (`tcache_max`). Another 40
   MiB is Empty 64 KiB slabs of the 64-byte class waiting for the 10 s decay
   in a 5 s run (each producer arena's peak in flight is not the global
   peak) and 16 MiB the 64-byte class's foreign caches (16 x 16 x 1024 x 64
   B); those two are left as they are (xmalloc's RSS is inside the band
   without them, below).
2. mstress, ~190 MiB: its 0.1% of 8 KiB..512 KiB objects (`items * 1000`)
   land in the 4..32 KiB classes, 8..16 cached slots per (thread, arena,
   class) in foreign caches, 16 threads x 16 arenas x 12 classes; the
   256 KiB and up classes the same at 8 slots. jemalloc's single per-thread
   cache holds one set of 20.
3. alloc-testN, 2.7 MiB of VmRSS (4.6 of maxrss, which also sees the exit
   flush): the class regions are equal at 1 and 18 arenas (10.5 vs 10.7
   MiB); the difference is `challoc:meta`: each `(arena, class, set)` had
   its own page-aligned table whose level-1 word 0 and level-0 word 0 are
   on two different pages (the level-1 block of a 64 KiB-slab class is 8
   KiB), so every class a thread touched cost its arena 2 pages (4 more at
   exit when the empty set is written), 17 arenas x 40 classes.

## The change (this tree) and its probe

Three slow-path changes; no line of the fast paths in `heap.rs` moved:

- `tcache_max` (new key, jemalloc's default 32 KiB): classes above it are
  not cached, alloc and free go straight to the class bitmaps.
- A cache for another arena (`Heap::others`) holds nothing above `BIN_MAX`
  (4 KiB); `Config::other_cache_cap`.
- One chunked, interleaved set table per (arena, slab size)
  (`class::SetTable`, `TwoLevelBitmap::from_raw_chunked`): rows of `3 x
  classes-of-that-slab-size` words, chunks of one level-1 word plus its 64
  level-0 words, committed as a prefix; a small heap touches row 0 and row 1
  of the groups it uses, one page per (arena, slab size) instead of 2..4
  per (arena, class).

Probe with the new library at the default conf (same method):

| bench | VmRSS before | VmRSS after | large allocated | small dirty | smaps meta | jemalloc maxrss (protocol) |
|---|---:|---:|---:|---:|---:|---:|
| xmalloc -w 16 | 170..185 | 104.5..106.6 | 4.9 | 36..39 | 4.7 (before this table's fix) | 110.7 |
| mstress 16 (peak) | 368..380 | 347.4 | 210.5 (`tcache_max` alone); see A/B for the foreign-cache cap | 11.1 | | 292..294 |
| alloc-test 16 | 19.2..19.4 | 17.2..17.4 | 4.1 | 0 | | 16.3 |

(The interim tree measured here committed a 20 MiB level-1 block per arena
for the 4 KiB-slab classes' 2^26 slabs; the chunked layout replaced it before
the A/B below, and creating a table now commits nothing:
`class::tests::set_table_of_many_classes_touches_one_page_per_slab_size`.)
The A/B of the final tree against 523ad84 and jemalloc is
`2026-09-18-{aarch64,x86_64}-gce-arena-rss-fix.md`; the deciding run is the
second acceptance file.

## The first A/B of the change, and an xmalloc-test regression it exposed

`2026-09-18-{aarch64,x86_64}-gce-arena-rss-fix.md` (old = 523ad84, new = the
three changes above, jemalloc, 3 rounds): larson unchanged on aarch64
(-0.2 / +0.4 / +0.2%) and within noise on x86_64; alloc-testN RSS 20.6 ->
16.4 MiB against jemalloc's 16.4 (aarch64) and 21.6 -> 17.2 against 16.7
(x86_64); xmalloc -w 16 RSS 180 -> 105 MiB (jemalloc 111) but its throughput
489 -> 413 M frees/s on aarch64 (-15%) and 328 -> 247 on x86_64 (-25%),
against jemalloc's 487 / 336; mstress RSS 376 -> 346 MiB against 292 on
aarch64 (+18.5%), 388 -> 377 against 285 on x86_64 (+32%): still outside.

xmalloc -w 16, counters (`quick-perf`, aarch64, per free): old 86.9 cycles,
387.8 instructions, 0.343 L1D misses; new 100.9 cycles, 388.8 instructions,
1.992 L1D misses. Equal work, +1.65 misses per operation: placement, not
the slow paths. Isolation A/Bs on xmallocN: `tcache_max:262144` on the new
tree (batch structs cached again at home) 432 vs new 422 vs old 489
(`-xmalloc-isolation.md`); the set table behind a pointer so `Arena` keeps
its size 427 vs old 487 on aarch64, 243 vs 325 on x86_64
(`-xmalloc-boxed.md`): neither. `perf record -e L1-dcache-load-misses -c
2000` by symbol (aarch64, host perf attached to the container):

| symbol | old | new |
|---|---:|---:|
| `Heap::other_caches` (the `others` list walk) | 3.3% | **58.5%** |
| `Heap::free_other` | 1.3% | 6.9% |
| `purge::decay` | 18.8% | 4.9% |
| `je_malloc` | 13.2% | 4.4% |
| `ClassState::claim` | 3.8% | 1.2% |

Mechanism: a consumer's cross-arena `free` walks its `others` list (15
nodes, one per producer arena) reading each node head. Nodes are 3344-byte
metadata objects; before the change every node was followed by the 64-byte
slot stack of the 40 KiB class's foreign cache, which put successive node
heads 64 bytes apart modulo the metadata bin stride; with that cache gone
(`other_cache_cap` = 0 above `BIN_MAX`) successive nodes and their 8 KiB
64-byte-class stacks sit exactly 12 KiB apart, a multiple of the L1D way
size on both CPUs (Xeon: 48 KiB 12-way, 4 KiB per way, so all 15 heads in
one set; V2: 64 KiB 4-way, 16 KiB per way, 4 sets for 15 heads plus 15
stack tops), and the walk misses on every free. Fix: each heap's `n`-th
node is placed `n % 16` cache lines into a block padded by 15 lines
(`heap::NODE_STAGGER_LINES`), so node heads spread over 15 distinct sets
by construction rather than by the accident the old layout had. Node
creation only; the walk is unchanged. Second A/B:
`2026-09-18-{aarch64,x86_64}-gce-arena-rss-fix2.md`.

For mstress the remaining excess is the large classes' partial slabs
multiplied by the arena count (probe on the first fix tree: `large active`
349 MiB at 16 arenas against 136 at one, `challoc:classes` 181..216 MiB
against 143..145, dirty large equal at 142 / 152): those classes are
uncached and served under the class lock in any case, so the default
allocation path now takes them from arena 0 (`Heap::alloc_slow`; explicit
`MALLOCX_ARENA` allocations keep their arena), one partial slab per class
for the process instead of one per thread arena.

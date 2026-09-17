# challoc thread-affine slab refills (review P2 form a) A/B, 2026-09-17, aarch64 (GCE reference VM, us-east4-a)

Phase 1.5 task 4a. `old` is df0c38d (the reference-run library), `new` is
39acac8: `ClassState::plan` takes the heap's hint slab first while it is
still partial and still this heap's, then the lowest partial slab that is
not another heap's hint (a stamp of the heap id in the descriptor's
`dirty_at` word while the slab is `Active`), then empty, purged and fresh
slabs, and another heap's hint slab only when nothing can be committed;
refills pass the affinity hint-first, compaction rebuilds lowest-first,
direct claims keep the pure first fit, a heap's exit clears its stamps.
`narenas16` is the `old` library with `MALLOC_CONF=narenas:16` (the
review's form b, measured in the same run as the plan's acceptance asks),
jemalloc is Wolfi's 5.3.1. The x86_64 rows are in
`2026-09-17-x86_64-gce-thread-affinity.md`. VM numbers are never compared
with Mac numbers.

- host: challoc-bench-arm64 (c4a-standard-16, Google Axion, Neoverse V2),
  16 vCPUs, Ubuntu 24.04 kernel 7.0.0-1011-gcp, 4 KiB pages,
  `guard_install=yes`; container: Wolfi (rustc 1.98.1, gcc 15.3.0). Load
  average 0.12 at the start of the A/B; the rounds raise it to about 9.
- method (A/B): `bench/build.sh` programs; `old` built from `git archive
  HEAD` at df0c38d into `target/old`, `new` from a `.tmp-new` copy of the
  working tree that became 39acac8 into `target/new`; 5 rounds plus one
  warm-up, variants interleaved per round, every run's score listed
  sorted; RSS is `rt`'s median maxrss in KiB. Driver `.tmp-it19-ab.sh` in
  the iteration log (the iteration-13 recipe).
- programs: `larson 5 8 1000 5000 100 4141 N` for N = 1, 10, 16
  (M ops/s, higher is better); `xmalloc-test -w 10 -t 5 -s 64` (M frees/s,
  higher is better); `alloc-test 1` and `alloc-test 16` (seconds, lower is
  better).

## Run 1: old, new, narenas16, jemalloc

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 43.3 43.4 43.4 43.5 43.5 | 13588 |
| larson1 | new | 43.6 43.6 43.6 43.7 43.7 | 13972 |
| larson1 | narenas16 | 42.1 42.2 42.3 42.3 42.3 | 67104 |
| larson1 | jemalloc | 45.0 45.0 45.1 45.1 45.2 | 18152 |
| larson10 | old | 345.0 345.3 345.9 347.2 349.2 | 87456 |
| larson10 | new | 325.9 326.1 328.0 328.2 328.4 | 91296 |
| larson10 | narenas16 | 359.9 361.4 362.9 366.4 367.3 | 127648 |
| larson10 | jemalloc | 383.8 384.5 385.3 389.3 390.2 | 134428 |
| larson16 | old | 541.8 543.6 543.8 544.9 545.6 | 135072 |
| larson16 | new | 504.1 506.6 507.1 507.5 509.7 | 136224 |
| larson16 | narenas16 | 568.1 571.2 571.8 572.5 574.2 | 226592 |
| larson16 | jemalloc | 611.5 612.6 612.9 615.5 617.0 | 211028 |
| xmalloc-test | old | 354.0 383.8 385.8 407.9 419.0 | 39200 |
| xmalloc-test | new | 368.4 372.9 378.4 401.9 422.2 | 43124 |
| xmalloc-test | narenas16 | 583.9 586.5 587.2 587.7 588.4 | 88820 |
| xmalloc-test | jemalloc | 469.8 479.7 481.7 483.9 484.4 | 83992 |
| alloc-test1 | old | 2.641 2.648 2.654 2.665 2.684 | 13292 |
| alloc-test1 | new | 2.641 2.644 2.649 2.652 2.671 | 13284 |
| alloc-test1 | narenas16 | 2.645 2.665 2.670 2.673 2.686 | 13416 |
| alloc-test1 | jemalloc | 2.531 2.547 2.553 2.556 2.559 | 12620 |
| alloc-testN | old | 2.232 2.233 2.241 2.241 2.242 | 16620 |
| alloc-testN | new | 2.696 2.696 2.700 2.701 2.701 | 20768 |
| alloc-testN | narenas16 | 2.241 2.244 2.251 2.256 2.268 | 21740 |
| alloc-testN | jemalloc | 2.197 2.198 2.198 2.200 2.206 | 16716 |

Reading (medians, new vs old): larson1 43.4 -> 43.6 (+0.5%), larson10
345.9 -> 328.0 (-5.2%), larson16 543.8 -> 507.1 (-6.7%), xmalloc-test
385.8 -> 378.4 (inside the row's spread), alloc-test1 2.654 -> 2.649 s,
alloc-testN 2.241 -> 2.700 s (+20%). RSS: larson10 +3.8 MiB, alloc-testN
+4.1 MiB (the "fresh slab before another heap's hint slab" rule commits
one more partial slab per thread and class). `narenas16` in the same run:
larson10 362.9 (+4.9% over old, 6.1% behind jemalloc), larson16 571.8
(+5.1%, 6.7% behind jemalloc), xmalloc-test 587.2 (+52%, ahead of
jemalloc's 481.7), larson1 42.3 (-2.5%: the `multi_arena` load on the
`free` path); RSS larson1 67.1 MiB against old's 13.6 and jemalloc's 18.2
(see the conclusion), larson10 127.6 against jemalloc's 134.4, larson16
226.6 against 211.0 (+7.4%), xmalloc-test 88.8 against 84.0.

Acceptance (plan 4b): the 10-thread larson row is 9.6% *below* the same
run's `narenas16` (criterion: within 3%) and 14.9% behind jemalloc
(criterion: within 10%). Form (a) misses.

## Run 2: where the cost is (old, new, new2, new3; 3 rounds)

Same VM, right after run 1 (load 9 at the start). `new2` is `new` with the
walk never skipping another heap's hint slab (stamps and hint-first kept);
`new3` is hint-first only, no stamps (the heap id passed as 0), which is
the plan's literal form (a). Driver `.tmp-it19-ab2.sh`.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson10 | old | 342.0 346.1 349.1 | 87200 |
| larson10 | new | 323.0 324.3 325.4 | 90784 |
| larson10 | new2 | 343.7 345.7 346.2 | 87064 |
| larson10 | new3 | 347.0 350.0 350.3 | 87708 |
| larson16 | old | 537.5 541.5 542.2 | 135052 |
| larson16 | new | 500.8 504.0 506.5 | 136096 |
| larson16 | new2 | 540.4 541.9 544.3 | 134816 |
| larson16 | new3 | 544.9 545.5 545.8 | 135328 |
| alloc-testN | old | 2.240 2.241 2.253 | 16364 |
| alloc-testN | new | 2.692 2.693 2.698 | 20716 |
| alloc-testN | new2 | 2.230 2.235 2.238 | 16492 |
| alloc-testN | new3 | 2.233 2.242 2.295 | 16492 |

Reading: `new2` and `new3` are `old` within noise on every row (new3's
larson rows are +1%, inside the round-to-round spread), so the hint alone
buys nothing and the whole cost of `new` is the walk that skips other
heaps' hint slabs: under 10..16 threads a refill that misses its hint (or
takes only part of its batch from it, which every refill of the 4..16 KiB
slab classes does since a batch spans several slabs) reads the descriptor
header line of every stamped slab below the first unstamped one, each of
them a line another core writes on every refill and flush (`live`, the
bitmap word), under the class lock.

## perf stat, larson 1 and 10 threads (old, new, narenas16, jemalloc)

Method of `2026-09-17-gce-review-counters.md` (`sudo perf stat` attached to
the container, six events, one multiplexed out on this core). Per
operation with ops = throughput x 5 s. Driver `.tmp-it19-perf.sh`.

| program | variant | round | M ops/s | cyc/op | ins/op | L1D/op |
|---|---|---|---:|---:|---:|---:|
| larson1 | old | r1 | 40.8 | 69.1 | 235.9 | 2.49 |
| larson1 | old | r2 | 40.7 | 69.3 | 235.8 | 2.50 |
| larson1 | new | r1 | 40.9 | 68.8 | 235.8 | 2.49 |
| larson1 | new | r2 | 40.9 | 68.7 | 235.9 | 2.49 |
| larson1 | narenas16 | r1 | 39.6 | 71.5 | 243.9 | 2.47 |
| larson1 | narenas16 | r2 | 39.4 | 71.5 | 243.9 | 2.47 |
| larson1 | jemalloc | r1 | 42.4 | 66.1 | 223.0 | 2.06 |
| larson1 | jemalloc | r2 | 42.2 | 66.5 | 223.1 | 2.06 |
| larson10 | old | r1 | 320.9 | 86.8 | 236.8 | 3.09 |
| larson10 | old | r2 | 326.2 | 85.5 | 236.8 | 3.10 |
| larson10 | new | r1 | 303.3 | 91.9 | 245.4 | 3.12 |
| larson10 | new | r2 | 303.2 | 91.8 | 245.5 | 3.11 |
| larson10 | narenas16 | r1 | 335.5 | 82.6 | 244.0 | 2.66 |
| larson10 | narenas16 | r2 | 335.5 | 82.8 | 244.0 | 2.66 |
| larson10 | jemalloc | r1 | 359.1 | 77.3 | 223.5 | 2.23 |
| larson10 | jemalloc | r2 | 358.1 | 77.6 | 223.4 | 2.23 |

Reading: `new` does not move the 10-thread L1D misses per operation at all
(3.12 against old's 3.09; `narenas16` 2.66, jemalloc 2.23; criterion:
within 10% of the `narenas16` figure) and adds 8.6 instructions and 6
cycles per operation. The affinity does not separate the threads' slabs
in larson: it is not a cost that hides a gain, there is no gain.

## Conclusion

Form (a) is rejected and the code reverted (kept in history as 39acac8).
Why a hint cannot do what an arena does: larson's threads live for
`num_rounds x chperthread` = 5 M operations (about 0.14 s) and then hand
their 1000 blocks to a successor thread, so a lane's working set is placed
over many generations, and every generation's first refill of a class, and
every refill that misses its hint or spans more than one slab, takes holes
from the shared lowest partial slab, holes left by every other lane's
flushes; a flush also reopens its slabs to every heap. Each such refill
interleaves the lanes' slots again at slot granularity, and since the
per-thread cache then recycles those slots LIFO for the rest of the
generation, the interleaving persists. `narenas` partitions the *sets*:
a thread's refills and its own flushes stay inside slabs that only its
arena-mates ever touched, which is why it moves the counters and a hint,
with or without stamps, does not.

For form (b) (plan 4c), two things this run shows must go with it:

- Arena assignment must be least-loaded, not round robin. `Arenas::assign`
  hands each new heap the next automatic index, and larson's thread
  churn (about 35 generations per lane in a 5 s run) therefore materialises
  every arena and leaves a partial slab per class in each: larson1 (two
  threads alive at a time) at 67.1 MiB against 13.6 today and jemalloc's
  18.2. jemalloc's `arena_choose` picks the arena with the fewest bound
  threads (decremented at thread exit), so two live threads use two
  arenas. With that, the expected RSS cost is the `narenas16` rows'
  minus the idle arenas: larson10 127.6 against jemalloc 134.4, larson16
  226.6 against 211.0 (+7.4%), xmalloc-test 88.8 against 84.0.
- The `free`-path cost of `multi_arena` (the descriptor's arena load and
  the cache routing) is real at one thread: larson1 -2.5% here and -5% on
  x86_64 (7..8 more instructions per operation), which the plan's 3%
  single-thread criterion for this task must absorb or the routing must
  get cheaper.

# challoc address-ordered (compacting) cache flush A/B, 2026-09-16, GCE reference VMs

Phase 1 acceptance step 3: the post-delete RSS floor of the 20-minute Valkey
soak (+41% vs jemalloc on the Mac, `2026-09-16-aarch64-valkey-soak.md`;
+22.7% on the arm64 VM with the rejected 16 KiB geometry,
`2026-09-16-aarch64-gce-min-slab.md`) is attacked on the cache side: the
flush of a full cache releases the `cap / 2` *highest-address* entries and
keeps the lowest, instead of releasing the oldest half of the LIFO stack.
This file is the throughput row before and after, per CLAUDE.md, on both
VMs, plus the soak that decides the criterion. VM numbers are never
compared with Mac numbers.

Why by address (the reading behind the change): after a refill the bottom
of the stack is the refill's own low-address residue, so an oldest-first
flush hands exactly those slots back to the lowest slab and keeps the
randomly placed freed slots for reuse in place. Under Valkey's balanced
DEL:SET churn every boundary event (refill at empty, flush at full) then
moves nothing: the slab layout the defragmenter left is frozen at 43..48%
occupancy (step 2's tables). jemalloc's tcache has the same structure
(flush the oldest, fill from the lowest non-full slab), so its drain comes
from its 4..16-region slabs emptying by chance, not from migration.
Releasing the highest addresses instead sends every flushed hole to the
high slabs while refills keep taking the lowest partial slab, so each
boundary event compacts by `cap / 2` slots and the high slabs drain, empty
and are purged on the decay window. No fast-path instruction changes; no
geometry change.

- hosts: challoc-bench-arm64 (c4a-standard-16, Google Axion, Neoverse V2)
  and challoc-bench-amd64 (c4-standard-16, Intel Xeon Platinum 8581C),
  16 vCPUs, Ubuntu 24.04 kernel 7.0.0-1011-gcp, 4 KiB pages,
  `guard_install=yes`; container: Wolfi (rustc 1.98.1, gcc 15.3.0)
- method: `bench/build.sh` programs, one `.so` per variant built from a copy
  of the tree (`old` = HEAD 8df225f, the others = the working tree with the
  named flush body), jemalloc = Wolfi's libjemalloc.so.2 (5.3). 5 rounds
  plus one warm-up, variants interleaved per round, every run's score
  listed sorted; RSS is `rt`'s median maxrss. Load average under 1 at the
  start of each run unless noted.
- programs: `larson 5 8 1000 5000 100 4141 16` (M ops/s, higher is better),
  `xmalloc-test -w 16 -t 5 -s 64` (M frees/s, higher is better),
  `alloc-test 1` and `alloc-test 16` (seconds, lower is better)

## Run 1: two implementations of the address-ordered flush

`new`: `sort_unstable` the whole cache, release the top `n`, reverse the
kept run so the lowest address is on top like a refill. `alt`:
`select_nth_unstable` the whole cache, sort and release only the top `n`,
kept entries left in partition order.

arm64:

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson | old | 550.2 551.3 554.2 554.7 556.0 | 135584 |
| larson | new | 554.1 555.3 557.0 560.3 561.5 | 136204 |
| larson | alt | 554.5 555.7 556.3 556.4 560.7 | 135972 |
| larson | jemalloc | 606.7 611.0 614.6 618.8 619.1 | 211436 |
| xmalloc-test | old | 360.7 382.8 389.3 432.8 443.2 | 44688 |
| xmalloc-test | new | 396.0 397.1 427.3 428.2 431.4 | 44900 |
| xmalloc-test | alt | 376.8 380.3 413.6 468.9 481.7 | 45288 |
| xmalloc-test | jemalloc | 480.9 481.4 481.7 486.6 491.6 | 112416 |
| alloc-test1 | old | 2.507 2.517 2.523 2.533 2.563 | 13288 |
| alloc-test1 | new | 2.502 2.506 2.534 2.536 2.553 | 13292 |
| alloc-test1 | alt | 2.539 2.556 2.557 2.564 2.593 | 13296 |
| alloc-test1 | jemalloc | 2.464 2.476 2.481 2.483 2.485 | 12632 |
| alloc-testN | old | 2.224 2.226 2.228 2.229 2.239 | 16360 |
| alloc-testN | new | 2.228 2.229 2.230 2.230 2.232 | 16364 |
| alloc-testN | alt | 2.256 2.256 2.257 2.264 2.297 | 16368 |
| alloc-testN | jemalloc | 2.199 2.199 2.203 2.206 2.207 | 16732 |

x86_64:

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson | old | 283.5 284.6 286.5 288.7 296.9 | 91540 |
| larson | new | 285.3 285.6 289.0 292.0 297.2 | 91900 |
| larson | alt | 282.7 285.2 285.6 286.7 288.3 | 91660 |
| larson | jemalloc | 356.1 356.2 359.7 360.5 362.7 | 133620 |
| xmalloc-test | old | 532.9 536.9 536.9 538.0 539.6 | 45080 |
| xmalloc-test | new | 314.9 316.4 317.6 318.6 319.4 | 45496 |
| xmalloc-test | alt | 377.2 379.5 381.1 383.5 385.4 | 45148 |
| xmalloc-test | jemalloc | 346.8 347.2 347.7 349.0 349.9 | 127040 |
| alloc-test1 | old | 2.851 2.854 2.854 2.876 2.890 | 14260 |
| alloc-test1 | new | 2.867 2.874 2.878 2.890 2.892 | 14272 |
| alloc-test1 | alt | 2.891 2.897 2.910 2.911 2.918 | 14216 |
| alloc-test1 | jemalloc | 2.751 2.751 2.758 2.759 2.776 | 13608 |
| alloc-testN | old | 4.039 4.058 4.073 4.097 4.103 | 17232 |
| alloc-testN | new | 4.043 4.090 4.092 4.147 4.197 | 17208 |
| alloc-testN | alt | 4.087 4.088 4.118 4.183 4.237 | 17304 |
| alloc-testN | jemalloc | 3.815 3.841 3.863 3.889 3.919 | 17288 |

Reading: on arm64 both forms are neutral on larson and alloc-test (`alt`
costs alloc-test 1.3%) and xmalloc-test sits inside `old`'s own 361..443
spread. On x86_64 larson and alloc-test are unchanged but xmalloc-test
loses 41% with `new` and 29% with `alt`.

### x86_64 xmalloc-test split: policy or compute?

Same VM, xmalloc-test only, 5 rounds interleaved (load 2.7 at the start,
the tail of the run above). `nr` = `new` without the `reverse` of the
kept run; `pc` = release the *newest* `n` (top of the stack) sorting only
them, i.e. the old compute with the released set xmalloc-test would get
from the address policy (its pushes arrive in ascending order).

| variant | sorted M frees/s |
|---|---|
| old | 536.5 537.1 538.1 539.9 542.2 |
| new | 313.1 318.7 320.4 321.2 322.5 |
| nr | 526.0 527.0 530.0 530.6 532.3 |
| pc | 537.3 537.8 538.0 538.2 543.5 |
| alt | 377.0 379.0 381.5 382.9 384.2 |

Reading: `pc` equals `old`, so releasing the newest/highest slots the
producer is still working near costs nothing; `nr` costs 1.5% (sorting
1024 pointers instead of 512); the whole 41% is the `reverse`: a
descending run of 512 kept entries under 512 ascending pushes is a
pathological pivot sequence for the next flush's `sort_unstable`
(median-of-3 picks a high pivot, partitions degenerate), and `alt`'s
`select_nth_unstable` on a partition-ordered run under ascending pushes
suffers the same way. Adopted form: `nr`, the whole cache sorted
ascending, the top `n` released, the kept run left ascending (highest on
top). The zero prefix is dropped on every flush (the kept entries are
reordered).

## Run 2: adopted form (`new` = `nr`) against `old`, both VMs

arm64 (load 0.6 at the start, 3.6 over the previous 5 minutes from run 1):

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson | old | 546.7 548.0 549.5 553.1 553.2 | 135200 |
| larson | new | 554.4 559.1 559.4 561.7 562.8 | 136348 |
| larson | jemalloc | 607.9 608.3 613.9 614.9 616.4 | 211464 |
| xmalloc-test | old | 377.4 378.1 379.3 379.3 430.0 | 44568 |
| xmalloc-test | new | 323.8 334.6 358.0 380.1 386.2 | 44172 |
| xmalloc-test | jemalloc | 479.0 486.0 486.2 491.2 492.9 | 112140 |
| alloc-test1 | old | 2.504 2.513 2.520 2.520 2.533 | 13292 |
| alloc-test1 | new | 2.508 2.517 2.519 2.522 2.526 | 13292 |
| alloc-test1 | jemalloc | 2.455 2.458 2.460 2.465 2.465 | 12636 |
| alloc-testN | old | 2.219 2.220 2.228 2.228 2.250 | 16360 |
| alloc-testN | new | 2.223 2.226 2.232 2.253 2.254 | 16488 |
| alloc-testN | jemalloc | 2.198 2.199 2.199 2.200 2.207 | 16728 |

x86_64 (load 3.2 at the start, from the split run just before):

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson | old | 282.8 286.5 288.9 289.0 289.4 | 91568 |
| larson | new | 282.5 284.6 286.3 286.8 291.1 | 91460 |
| larson | jemalloc | 353.0 354.6 355.4 360.3 363.2 | 133580 |
| xmalloc-test | old | 533.8 537.5 539.3 539.9 542.6 | 45200 |
| xmalloc-test | new | 523.4 524.5 527.3 527.4 532.5 | 45084 |
| xmalloc-test | jemalloc | 348.5 348.7 348.8 349.3 350.2 | 122364 |
| alloc-test1 | old | 2.840 2.849 2.861 2.866 2.866 | 14212 |
| alloc-test1 | new | 2.849 2.854 2.858 2.862 2.864 | 14252 |
| alloc-test1 | jemalloc | 2.748 2.756 2.757 2.757 2.760 | 13624 |
| alloc-testN | old | 4.040 4.046 4.048 4.089 4.148 | 17424 |
| alloc-testN | new | 4.051 4.071 4.077 4.087 4.155 | 17324 |
| alloc-testN | jemalloc | 3.822 3.831 3.844 3.881 3.933 | 17268 |

Reading (medians): arm64 larson 549.5 -> 559.4 M ops/s (+1.8%; the gap to
jemalloc 10.5% -> 8.9%), alloc-test1 2.520 -> 2.519 s and alloc-testN
2.228 -> 2.232 s (unchanged), xmalloc-test 379.3 -> 358.0 with the two
ranges overlapping (324..386 against 377..430; this row moved between 361
and 443 for `old` across today's four runs on this VM, so 5 rounds do not
resolve it; run 1's `new`, the same policy with the reverse, had 396..431
against `old`'s 361..443). x86_64: larson 288.9 -> 286.3 (-0.9%, ranges
overlap), xmalloc-test 539.3 -> 527.3 (-2.2%, the sort of the whole
cache), alloc-test1 2.861 -> 2.858 s and alloc-testN 4.048 -> 4.077 s
(unchanged). RSS unchanged everywhere. The Phase 0 set is otherwise as in
`2026-09-16-aarch64-gce-min-slab.md`: the larson gap on both VMs and
xmalloc-test on arm64 are Phase 1.5 items, not moved by this change beyond
the arm64 larson gain.

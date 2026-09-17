# challoc compacting cache (scheduled address-ordered pops) A/B, 2026-09-17, x86_64 (GCE reference VM)

The x86_64 half of `2026-09-17-aarch64-gce-compacting-cache.md`: see that
file for the change, the method and the reading of the first form.

- host: challoc-bench-amd64 (c4-standard-16, Intel Xeon Platinum 8581C,
  16 vCPUs = 8 cores x 2 hyperthreads), Ubuntu 24.04 kernel
  7.0.0-1011-gcp, `guard_install=yes`; container: Wolfi (rustc 1.98.1,
  gcc 15.3.0); jemalloc = Wolfi's libjemalloc.so.2 (5.3.1)
- method and programs as in the aarch64 file; `old` = 9d92094

## Run 2: second form (commit 32bb909), gate on holes only

Load 2.42 at the start (the native test suite had just finished).

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 50.8 51.1 51.1 51.7 51.8 | 13096 |
| larson1 | new | 45.1 50.1 51.1 51.4 51.9 | 13084 |
| larson1 | new0 | 49.2 51.4 51.6 51.6 52.5 | 13072 |
| larson1 | jemalloc | 46.7 46.8 47.7 47.8 48.4 | 16340 |
| larson10 | old | 262.2 265.8 267.7 271.3 277.1 | 67576 |
| larson10 | new | 260.4 262.7 263.5 265.1 273.8 | 66988 |
| larson10 | new0 | 265.2 265.4 266.6 268.1 271.6 | 67224 |
| larson10 | jemalloc | 295.2 301.7 303.7 309.1 311.9 | 96060 |
| xmalloc-test | old | 537.1 538.3 538.4 540.9 545.1 | 39560 |
| xmalloc-test | new | 519.3 519.5 520.6 521.0 529.7 | 39596 |
| xmalloc-test | new0 | 532.4 533.9 534.6 535.5 542.6 | 39596 |
| xmalloc-test | jemalloc | 338.5 338.8 338.8 341.7 344.9 | 92916 |
| alloc-test1 | old | 2.927 2.938 2.942 2.952 2.964 | 14080 |
| alloc-test1 | new | 2.935 2.969 2.981 2.988 2.994 | 14024 |
| alloc-test1 | new0 | 2.929 2.962 2.978 2.987 3.001 | 14108 |
| alloc-test1 | jemalloc | 2.773 2.840 2.845 2.850 2.851 | 13440 |
| alloc-testN | old | 4.166 4.203 4.203 4.250 4.304 | 17240 |
| alloc-testN | new | 4.232 4.262 4.281 4.287 4.295 | 17308 |
| alloc-testN | new0 | 4.200 4.242 4.250 4.280 4.312 | 17060 |
| alloc-testN | jemalloc | 3.945 4.029 4.030 4.044 4.069 | 17152 |

Reading (medians): larson1 0%, larson10 -1.6% (`new0` -0.4%),
xmalloc-test -3.3% against `new0` -0.7% (the schedule engaged on
xmalloc-test's transient holes; see the bin sampler evidence in the
aarch64 file), alloc-test1 +1.3%, alloc-testN +1.9% (`new0` +1.1%); RSS
unchanged.

## Run 3: final form (commit 97e0cf8): descending run, block rebuild, holes and refill-rate gate

Load 1.47 at the start.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 50.6 51.4 52.7 53.3 53.5 | 13044 |
| larson1 | new | 50.9 51.8 52.6 52.8 53.4 | 13252 |
| larson1 | new0 | 24.7 48.5 50.9 51.7 53.2 | 12944 |
| larson1 | jemalloc | 46.5 47.5 47.9 48.8 49.2 | 16600 |
| larson10 | old | 268.1 268.9 270.9 277.8 281.3 | 67628 |
| larson10 | new | 260.1 264.8 270.0 272.8 281.0 | 67324 |
| larson10 | new0 | 267.2 272.2 273.3 273.9 284.3 | 67708 |
| larson10 | jemalloc | 302.4 304.9 310.8 316.0 317.3 | 96344 |
| xmalloc-test | old | 545.6 552.0 552.3 554.1 554.3 | 39704 |
| xmalloc-test | new | 535.8 543.2 551.1 555.9 558.3 | 39552 |
| xmalloc-test | new0 | 535.7 545.9 555.6 555.6 556.2 | 39616 |
| xmalloc-test | jemalloc | 336.8 338.3 347.1 347.4 348.6 | 94328 |
| alloc-test1 | old | 2.836 2.862 2.863 2.872 2.953 | 13980 |
| alloc-test1 | new | 2.852 2.853 2.858 2.936 2.966 | 14044 |
| alloc-test1 | new0 | 2.845 2.854 2.859 2.947 3.004 | 13984 |
| alloc-test1 | jemalloc | 2.748 2.755 2.764 2.828 2.870 | 13332 |
| alloc-testN | old | 4.090 4.099 4.100 4.152 4.265 | 17176 |
| alloc-testN | new | 4.100 4.104 4.164 4.169 4.251 | 17756 |
| alloc-testN | new0 | 4.072 4.081 4.194 4.232 4.260 | 17124 |
| alloc-testN | jemalloc | 3.838 3.854 3.871 4.063 4.097 | 16964 |

Reading (medians): larson1 52.7 -> 52.6 (-0.2%), larson10 270.9 -> 270.0
(-0.3%), xmalloc-test 552.3 -> 551.1 (-0.2%; the refill-rate gate keeps
the schedule off xmalloc-test's transient holes, run 2 had -3.3%),
alloc-test1 2.863 -> 2.858 s (-0.2%), alloc-testN 4.100 -> 4.164 s (+1.6%,
`new0` 4.194 in the same run), RSS unchanged. Every row is within the
plan's 3% of `old` measured in the same run.

## xmalloc-test's 64-byte class under the second form (`bench/probes/binsample.c`, one row per second)

```
columns: size ind nregs slab_size curregs curslabs nonfull empty purged
xmalloc-test -w 10 -t 5 -s 64, default (compacting when the gate opens)
t=1   64   3   1024   65536   147968   150   17   315   0
t=2   64   3   1024   65536   465920   455    7    10   0
t=3   64   3   1024   65536   407552   401   10    65   0
t=4   64   3   1024   65536   411648   410   25    56   0
same, tcache_compact_shift:0
t=1   64   3   1024   65536   226304   235   33   231   0
t=2   64   3   1024   65536    86016   104   45   362   0
t=3   64   3   1024   65536   452096   442    4    24   0
t=4   64   3   1024   65536   459264   451   10    15   0
t=5   64   3   1024   65536   164352   186   51   280   0
alloc-test 1:   t=1  64 3 1024 65536   4608   5  1  0  0
larson 10:      t=1  64 3 1024 65536   6110   7  7  0  0   (holes: one slab's worth, below the 4-slab gate)
```

The producers run 150..455 slabs ahead of the consumers; the holes
(curslabs x 1024 - curregs) are 0..8 slabs' worth, 2..4% of the class,
in 7..51 partial slabs, and `claimed` advances by tens of thousands of
slots per second: ordinary refills consume them. The final form skips a
class that refilled a slab's worth or more between two probes.

## Churn probe (`bench/probes/churn-run.sh`, commit 97e0cf8), full log in `2026-09-17-x86_64-gce-churn-probe.log`

30000 blocks of one class, a random 56% freed, then a strict
free-one/allocate-one churn for 20 s; `curslabs` is the class's non-empty
slab count. Acceptance: 469 -> under 230 slabs within 5 s at `k = 3`.

| library | class | after delete | 3 s of churn | end (22 s) | resident MiB start -> end | M ops/s |
|---|---|---|---|---|---|---:|
| challoc default (`k = 3`) | 1024 | 469 slabs, 44.6% | 216 slabs, 96.9% | 216, 97.0% | 35.0 -> 19.2 | 22.4 |
| challoc default | 640 | 295, 44.5% | 135, 97.3% | 135, 97.0% | 24.1 -> 14.1 | 23.5 |
| challoc `tcache_compact_shift:5` | 1024 | 469 | 216, 96.9% | 216 | 35.0 -> 19.2 | 22.3 |
| challoc `tcache_compact_shift:0` (off) | 1024 | 469 | 469, 44.6% | 469 | 35.0 -> 35.0 | 21.2 |
| challoc `tcache:false` | 1024 | 469 | 208, 99.9% | 208 | 35.0 -> 18.7 | 7.3 |
| jemalloc 5.3.1 | 1024 (4 regions/slab) | 6798, 49.0% | 6769, 49.1% | 6489, 51.2% | 34.1 -> 33.9 | 20.3 |
| jemalloc 5.3.1 | 640 (32 regions/slab) | 938, 44.5% | 938, 44.3% | 938 | 22.3 -> 22.3 | 21.2 |

The compaction is complete inside the first two seconds (the per-second
report at 2 s already shows 216) and stops where the gate says the class
is dense (holes under 1/32 of the capacity: 216 x 64 - 13397 = 427); the
emptied slabs are purged on the 10 s decay window, which the probe pop's
heartbeat now triggers on a thread with no other slow path. Throughput
during the 20 s is not lower than with the schedule off (22.4 vs 21.2 M
ops/s: the compacted class has better locality).

# The size-returning entry points for Valkey (lane sized-path, task 1), aarch64

Date 2026-09-21. Lane `sized-path`, lane VM `challoc-sized-path-arm64`
(c4a-standard-16, Google Axion, Neoverse V2, PMU exposed, kernel
7.0.0-1011-gcp, perf 7.0.14). Code commit 63bd7ee. The x86_64 file
(`2026-09-21-x86_64-sized-path-usable.md`) has the change, the codegen
accounting and the write-phase attribution this rests on (the +117 cycles
per SET were measured on the C4); this file is the aarch64 guard: the
model rows and one soak.


## 1. `quick-perf`: the write phase per SET, old shape against the pair (C4A)

The same command as the x86_64 file's section 4 on `challoc-sized-path-arm64`
(`old` 2078f67, `new` 63bd7ee, jemalloc 5.3.1); the rows are described
there.

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1-write | old | 6.47 | 469.5 | 450.7 | 4.705 | 3.883 |
| valkey1-write | new | 6.51 | 466.5 | 450.5 | 4.706 | 3.861 |
| valkey1-write | jemalloc | 6.59 | 460.2 | 390.2 | 4.750 | 3.776 |
| valkey1-write-zmalloc | old | 5.47 | 556.4 | 749.7 | 6.802 | 6.465 |
| valkey1-write-zmalloc | new | 5.38 | 565.6 | 752.2 | 6.816 | 6.532 |
| valkey1-write-zmalloc | jemalloc | 4.83 | 628.7 | 974.1 | 7.883 | 8.446 |
| valkey1-write-pair | new | 6.01 | 506.4 | 489.3 | 6.600 | 5.853 |
| valkey1-usable | old | 10.08 | 300.9 | 388.9 | 4.177 | 3.986 |
| valkey1-usable | new | 10.00 | 303.4 | 390.5 | 4.192 | 4.027 |
| valkey1-usable | jemalloc | 9.92 | 305.0 | 377.7 | 4.413 | 4.690 |
| valkey1 | old | 10.87 | 279.2 | 306.9 | 3.182 | 2.878 |
| valkey1 | new | 10.98 | 276.3 | 305.4 | 3.179 | 2.889 |
| valkey1 | jemalloc | 12.30 | 246.2 | 218.1 | 3.009 | 2.740 |
| larson1 | old | 41.50 | 72.1 | 264.5 | 2.100 | 4.008 |
| larson1 | new | 41.47 | 72.2 | 264.5 | 2.097 | 3.985 |
| larson1 | jemalloc | 44.75 | 66.5 | 224.8 | 2.064 | 1.725 |

Per SET on the C4A: the old `zmalloc` shape 565.6 cycles, 752
instructions, 6.82 L1D misses; the pair 506.4, 489, 6.60 (**-59 cycles,
-10.5%; -263 instructions**); the sized path without accounting 466.5,
450.5, 4.71. The same picture as the C4: the second classification is
gone, the exact check kept in `challoc_free_usable` is the +40 cycles
that remain over the bare sized path. jemalloc's old shape reads 628.7
cycles per SET. `valkey1-usable`, `valkey1` and `larson1` are unchanged
between `old` and `new` within the run's band.

## 2. The soak (C4A): old patch, new patch and jemalloc in one run, with the perf sidecar

The same protocol and sidecar arrangement as the x86_64 file's section 5,
on `challoc-sized-path-arm64`, 04:31..05:36 UTC; the files
`2026-09-21-aarch64-sized-path-usable-soak-*` beside this one.

| measure | challoc-old | challoc (new patch) | jemalloc | new vs jemalloc | old vs jemalloc | new vs old |
|---|---:|---:|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 889409 | 878237 | 813997 | -7.3% | -8.5% | -1.3% |
| write: valkey-benchmark p99 ms | 1.103 | 1.111 | 1.215 | -8.6% | -9.2% | +0.7% |
| write: memtier req/s | 914343 | 930342 | 827987 | -11.0% | -9.4% | +1.7% |
| write: memtier p99 ms | 1.031 | 0.999 | 1.343 | -25.6% | -23.2% | -3.1% |
| delete: valkey-benchmark req/s | 1376540 | 1410508 | 1365400 | -3.2% | -0.8% | +2.5% |
| delete: valkey-benchmark p99 ms | 0.735 | 0.719 | 0.735 | -2.2% | +0.0% | -2.2% |
| delete: memtier req/s | 1195485 | 1230463 | 1100946 | -10.5% | -7.9% | +2.9% |
| delete: memtier p99 ms | 0.815 | 0.775 | 1.039 | -25.4% | -21.6% | -4.9% |

Per operation (the x86_64 file's method):

| phase | tool | server | runs | req/s | p99 ms | cyc/op | ins/op | IPC | L1D miss/op | dTLB miss/op | br-miss/op | GHz | ns/op |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| write | valkey-benchmark | challoc-old | 12 | 904711 | 1.103 | 3246 | 9469 | 2.92 | 43.0 | 64.9 | 4.55 | 2.930 | 1108 |
| write | valkey-benchmark | challoc (new) | 12 | 893496 | 1.111 | 3283 | 9028 | 2.75 | 39.6 | 57.9 | 5.02 | 2.925 | 1122 |
| write | valkey-benchmark | jemalloc | 9 | 825321 | 1.215 | 3555 | 9662 | 2.72 | 41.8 | 46.7 | 4.65 | 2.932 | 1213 |
| write | memtier | challoc-old | 12 | 930781 | 1.031 | 3101 | 9499 | 3.06 | 44.6 | 44.5 | 8.40 | 2.890 | 1078 |
| write | memtier | challoc (new) | 12 | 946232 | 1.003 | 3054 | 9033 | 2.96 | 41.1 | 38.7 | 8.32 | 2.892 | 1060 |
| write | memtier | jemalloc | 10 | 841280 | 1.343 | 3406 | 9728 | 2.85 | 41.3 | 37.8 | 8.31 | 2.866 | 1189 |
| delete | valkey-benchmark | challoc-old | 13 | 1395284 | 0.735 | 2108 | 6800 | 3.23 | 21.4 | 21.1 | 3.83 | 2.936 | 718 |
| delete | valkey-benchmark | challoc (new) | 13 | 1436988 | 0.719 | 2048 | 6486 | 3.17 | 20.2 | 20.6 | 3.80 | 2.933 | 698 |
| delete | valkey-benchmark | jemalloc | 14 | 1401150 | 0.735 | 2099 | 6886 | 3.27 | 19.9 | 22.5 | 3.90 | 2.943 | 714 |
| delete | memtier | challoc-old | 13 | 1218144 | 0.815 | 2370 | 7315 | 3.08 | 28.8 | 26.9 | 6.10 | 2.880 | 822 |
| delete | memtier | challoc (new) | 13 | 1257854 | 0.775 | 2300 | 6971 | 3.03 | 26.6 | 25.6 | 5.90 | 2.886 | 797 |
| delete | memtier | jemalloc | 14 | 1120310 | 1.043 | 2536 | 7470 | 2.94 | 26.5 | 25.2 | 5.84 | 2.838 | 893 |

Record windows: write 3291 (old) / 3242 (new) / 3577 (jemalloc) cycles
per command; delete 2308 / 2310 / 2406. Perf record shares: write,
`libchalloc.so` 12.61% -> 6.49% (415 -> 210 cycles per command),
`je_malloc_usable_size` 9.01% -> 0.00% (296 cycles per command under the
old patch; jemalloc's is 9.95% = 356), `challoc_malloc_usable` 2.52% =
82, `challoc_free_usable` 3.02% = 98, `je_sdallocx` 1.88% -> 0.83%;
delete, `libchalloc.so` 10.46% -> 6.37% (241 -> 147), `je_malloc_usable_size`
6.63% -> 0.00%, the two new entry points 2.00% + 3.07%.

The guard holds: the query is gone here too (0.00% in both records),
the allocator's cycles per command halve, and three of the four
generator rows gain 1.7..2.9% against the old patch (-441..-466
instructions per SET, -314..-344 per DEL, 1.7..3.4 fewer L1D misses per
operation); the valkey-benchmark write row reads -1.3% (3283 against
3246 cycles per SET, +1.1%, at the same 2.93 GHz), inside the run-to-run
band the C4A soaks of Phase 2.5 have shown for that generator (the
09-20 aarch64 task 11 runs moved it by 2..3% between identical
configurations). RSS (the soak's table): peak 576.3 / 577.9 / 562.8 MiB
(the new patch +2.7% against jemalloc, the old +2.4%; the criterion is
+5%), after the settle 128.4 / 127.3 / 126.9 (+0.3%). Both challoc
servers stay ahead of jemalloc on every throughput and p99 row of this
host.

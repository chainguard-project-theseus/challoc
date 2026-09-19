# Phase 2.5 task 8: per-CPU caches through rseq mm_cid, aarch64

Lane A, 2026-09-19, reference VM challoc-bench-arm64 (C4A (Google Axion, Neoverse V2), 16 vCPUs, kernel
7.0.0-1011-gcp, glibc 2.44, `__rseq_size` 33, `nr_cpu_ids` 16). Code:
commit 14c7fad plus the tail-call fix that followed (the commit hash is in
PROGRESS.md). `old` is 9351eeb (main before the task), `new` the task's
tree with the default `percpu_arena:auto`, which is on here, `new-off` the
same library with `MALLOC_CONF=percpu_arena:off` (thread caches, the
mode's fallback), jemalloc Wolfi's 5.3.1. Method: bench/README.md (the
fast tier `quick`, then the protocol tier `ab`, 5 rounds, same run).

## 1. What the change does to the fast paths (disassembly)

The first form put the per-CPU branch inside the inlined fast path as a
call: LLVM did not shrink-wrap it and `malloc` and `free` grew a frame
(`stp x29, x30, [sp, #-32]!` in the prologue) in the thread mode as well.
The landed form tests the `PUSH_PERCPU` bit in the entry point and
tail-calls the per-CPU wrapper (`b percpu_malloc`): `malloc` and `free`
are frameless again and the thread mode's instruction stream is what it
was (aarch64, Mac dev container, `objdump` of `target/quick/{old,new}`).
In the mode a pop is the thread pointer, the `mm_cid` load, the table
index and one critical section (one store of the descriptor address, the
loads, one commit store) inside a called function.

## 2. Fast tier, `quick` (one round, failing rows confirmed on the median of 3)

`make quick OLD=9351eeb` on the C4A, `new` = the tree with the mode ON
(`percpu_arena:auto` before the default decision below; identical code to
`percpu_arena:percpu` after it):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---|---|---|---|---|
| larson1 | M ops/s | 42.6 (16.0 MiB) | 37.0 (19.1 MiB) (median of 3) | 44.9 (19.2 MiB) | +15.1% | +21.4% |
| larson10 | M ops/s | 370.1 (112.1 MiB) | 311.8 (135.2 MiB) (median of 3) | 388.0 (132.5 MiB) | +18.7% | +24.4% |
| xmalloc | M frees/s | 586.772 (53.4 MiB) | 473.724 (74.6 MiB) (median of 3) | 481.180 (77.9 MiB) | +23.9% | +1.6% |
| alloc-test1 | s | 2.661 (12.9 MiB) | 2.933 (12.9 MiB) (median of 3) | 2.495 (12.3 MiB) | +10.2% | +17.6% |
| valkey1 | M ops/s | 7.272 (584.2 MiB) | 6.629 (584.2 MiB) (median of 3) | 7.306 (585.4 MiB) | +9.7% | +10.2% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3418 (97.5%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 22876795 ops/s |

Verdict: NO-GO for the mode as the default (every row beyond the 5% gate;
positive is worse). The churn probe compacts under the mode (the gate
without the run, design 15: a scheduled pop claims one slot directly).
## 3. Per-operation attribution (`challoc-bench quick-perf arm64`, perf on the VM host)

`old` 9351eeb; `new:percpu_arena:percpu` the mode on; `new:percpu_arena:off`
the same library on thread caches (the shipped default, `auto`); jemalloc
5.3.1. One run each, counters over the whole process divided by the
operation count (bench/README.md).

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 42.16 | 70.9 | 255.4 | 2.094 | 4.008 |
| larson1 | new:percpu_arena:percpu | 36.85 | 80.6 | 338.1 | 2.133 | 3.041 |
| larson1 | new:percpu_arena:off | 41.49 | 72.2 | 264.5 | 2.100 | 3.988 |
| larson1 | jemalloc | 44.74 | 66.6 | 224.8 | 2.070 | 1.697 |
| valkey1 | old | 12.95 | 234.1 | 241.8 | 3.087 | 2.784 |
| valkey1 | new:percpu_arena:percpu | 10.05 | 302.5 | 425.0 | 3.342 | 3.122 |
| valkey1 | new:percpu_arena:off | 12.68 | 239.3 | 250.1 | 3.098 | 2.814 |
| valkey1 | jemalloc | 14.04 | 215.4 | 170.9 | 2.970 | 2.713 |

Mechanism. The mode is instructions, not misses: +83 instructions and
+9.7 cycles per larson operation (the thread pointer, the `mm_cid` load,
the table index, the descriptor store and the section's bookkeeping in a
called function against an inline two-load pop), L1D misses unchanged;
on the Valkey model +183 instructions and +68 cycles per operation, of
which the extra +100 over larson's is the compact-mode scheduled pop
claiming one slot under the class lock every 8 pops on the fragmented
500 MiB heap (the thread run amortises one lock over `cap / 4` slots).
`percpu_arena:off` (the default) is +9 instructions and +1.3 cycles per
larson operation over `old`: the `PUSH_PERCPU` test in each entry point
and the `percpu` byte on the slow paths.

## 4. Protocol tier, `ab` (5 rounds, same run; the full file with the sorted scores is `2026-09-19-aarch64-task8-percpu-ab.md`)

`new` = the mode on (the tree before the default decision, `auto` on this
kernel; byte for byte `percpu_arena:percpu` after it), `new-off` = the same
library with `percpu_arena:off` (the shipped default's path). Positive is
worse than `old`.

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | new-off | new-off RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | new-off vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.6 | 16.0 | 37.0 | 19.4 | 41.7 | 19.4 | 45.1 | 18.6 | +15.1% | +2.2% | -5.5% |
| larson10 | M ops/s | 370.2 | 112.2 | 312.3 | 135.0 | 359.3 | 139.5 | 388.7 | 132.9 | +18.5% | +3.0% | -4.8% |
| larson16 | M ops/s | 598.9 | 167.0 | 492.1 | 189.0 | 585.8 | 211.0 | 616.3 | 206.5 | +21.7% | +2.2% | -2.8% |
| xmalloc | M frees/s | 586.510 | 53.8 | 468.965 | 76.8 | 578.211 | 54.5 | 483.308 | 79.3 | +25.1% | +1.4% | +21.4% |
| alloc-test1 | s | 2.665 | 12.9 | 2.935 | 12.9 | 2.671 | 12.9 | 2.519 | 12.3 | +10.1% | +0.2% | -5.5% |
| alloc-testN | s | 2.282 | 16.4 | 2.520 | 16.4 | 2.307 | 16.3 | 2.200 | 16.4 | +10.4% | +1.1% | -3.6% |
| mstress | s | 0.376 | 322.7 | 0.388 | 320.4 | 0.375 | 314.5 | 0.646 | 291.6 | +3.2% | -0.3% | +71.8% |
| cfrac | s | 4.463 | 2.8 | 4.976 | 2.8 | 4.476 | 2.8 | 4.425 | 4.8 | +11.5% | +0.3% | -0.9% |
| espresso | s | 3.650 | 3.6 | 3.845 | 3.6 | 3.659 | 3.6 | 3.745 | 6.2 | +5.3% | +0.2% | +2.6% |
| glibc-simple | s | 2.165 | 2.2 | 2.795 | 2.3 | 2.183 | 2.3 | 2.528 | 4.4 | +29.1% | +0.8% | +16.8% |
| valkey1 | M ops/s | 7.182 | 584.6 | 6.483 | 584.6 | 7.090 | 584.6 | 7.151 | 585.4 | +10.8% | +1.3% | +0.4% |
| valkey1-free | M ops/s | 7.010 | 584.6 | 6.080 | 584.6 | 6.986 | 584.6 | 6.893 | 585.8 | +15.3% | +0.3% | +1.7% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

Note on `new-off`'s RSS in the table above (larson 20 MiB against `old`'s
16): the `ab` run predates the last code change, which marks the per-CPU
slow paths' buffer holders `inline(never)` (an 8 KiB flush buffer had been
inlined through `gc` into `tick` and deepened every slow path's frame, so
every larson thread stack touched two more pages, mode on or off). Section
5's `quick` of the committed tree shows the RSS back at `old`'s.

## 5. Fast tier under the shipped default (`quick`, committed tree, `auto` = thread caches)

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---|---|---|---|---|
| larson1 | M ops/s | 42.4 (16.0 MiB) | 41.6 (16.1 MiB) | 44.5 (18.7 MiB) | +1.9% | +7.0% |
| larson10 | M ops/s | 371.1 (112.6 MiB) | 361.9 (111.1 MiB) | 390.0 (132.7 MiB) | +2.5% | +7.8% |
| xmalloc | M frees/s | 585.417 (52.6 MiB) | 579.870 (54.2 MiB) | 482.043 (78.1 MiB) | +1.0% | -16.9% |
| alloc-test1 | s | 2.650 (12.9 MiB) | 2.670 (12.9 MiB) | 2.495 (12.3 MiB) | +0.8% | +7.0% |
| valkey1 | M ops/s | 7.155 (584.2 MiB) | 7.229 (584.3 MiB) | 7.181 (585.4 MiB) | -1.0% | -0.7% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24113380 ops/s |

Verdict: GO (every row inside the 5% gate, one round; larson1 and
larson10 at +1.9% / +2.5% are inside the 5..9% one-round spread of those
rows on this VM, bench/README.md "Noise"; the attribution puts the
default's cost at +1.3 cycles per larson operation).

## 6. The calloc-zero campaign under the mode (`bench/probes/calloc-zero-campaign.sh`, C4A)

`ROUNDS=60 MAX_SECS=600 STANDALONE=10 SKIP_SUITE=1`, four configurations
with `check_zero:true`, each under a continuous `cargo test api::` load
(the scheduling the 2026-09-18 failure needed); every run exit 0, no
`check_zero` abort:

| configuration | `MALLOC_CONF` | runs of `stress 8 50 10` | failed |
|---|---|---:|---:|
| percpu | `check_zero:true,percpu_arena:percpu` | 60 | 0 |
| percpu-nocompact | `check_zero:true,percpu_arena:percpu,tcache_compact_shift:0` | 60 | 0 |
| percpu-narenas1 | `check_zero:true,percpu_arena:percpu,narenas:1` | 60 | 0 |
| off | `check_zero:true,percpu_arena:off` | 60 | 0 |

The load ran 6 rounds of `cargo test api::` (116 tests each, 0 failed);
the standalone `stress 16 100 20` (`check_zero:true`, the default mode) 10
runs, 0 failed.

## 7. The Valkey soak under the mode (`make valkey-soak ALLOCS='challoc jemalloc' CHALLOC_CONF=percpu_arena:percpu DURATION=600 SETTLE=60`, C4A)

The challoc server (Valkey 9.1.2, `jemalloc-5.3.0-challoc`) ran with
`MALLOC_CONF=percpu_arena:percpu`, the jemalloc server stock; 300 s write,
300 s delete, 60 s settle (the 20-minute acceptance protocol shortened to
10 minutes of load; commit 4864d4b). Summary as the soak wrote it
(`target/valkey/soak-percpu/2026-09-19-aarch64-valkey-soak.md` on the VM):

| measure | challoc (percpu) | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 940885 | 901636 | -4.2% |
| write: valkey-benchmark p99 ms | 1.039 | 1.095 | -5.1% |
| write: memtier req/s | 976112 | 970208 | -0.6% |
| write: memtier p99 ms | 0.94300 | 0.95900 | -1.7% |
| delete: valkey-benchmark req/s | 1445997 | 1465367 | +1.3% |
| delete: valkey-benchmark p99 ms | 0.703 | 0.695 | +1.2% |
| delete: memtier req/s | 1275719 | 1306233 | +2.4% |
| delete: memtier p99 ms | 0.75900 | 0.74300 | +2.2% |
| peak RSS MiB (VmHWM) | 579.7 | 576.8 | +0.5% |
| RSS MiB at end of write phase | 579.4 | 576.5 | +0.5% |
| RSS MiB at end of delete phase | 128.0 | 135.5 | -5.5% |
| RSS MiB after settle | 128.0 | 134.8 | -5.0% |
| allocator_frag_ratio after settle | 1.79 | 1.87 | -4.3% |
| active_defrag_hits | 232260 | 237492 | +2.3% |

Reading: the soak is green in the plan's sense on RSS (peak +0.5%, after
settle -5%) and on the write phase; the delete phase is 1.3..2.4% behind
jemalloc in throughput and p99, which is the mode's per-operation cost
showing on a server whose main thread migrates between CPUs (the model
row `valkey1` shows the same direction much larger because it has no
network between operations). Under thread caches the same soak shape is
the reference scorecards' soak columns.

## 8. Protocol tier of the committed code (`ab`, 5 rounds, same run; `2026-09-19-aarch64-task8-percpu-ab2.md`)

`new` = the committed tree with `percpu_arena:percpu` (the opt-in), `new-off`
= the committed tree as shipped (`auto`, thread caches), `old` = 9351eeb,
jemalloc 5.3.1. The default's RSS is back at `old`'s (larson1 15.9 MiB).

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | new-off | new-off RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | new-off vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.6 | 15.9 | 37.1 | 16.0 | 41.7 | 15.9 | 45.2 | 18.6 | +14.8% | +2.2% | -5.8% |
| larson10 | M ops/s | 366.8 | 112.2 | 310.4 | 118.4 | 362.4 | 112.4 | 385.9 | 132.3 | +18.2% | +1.2% | -4.9% |
| xmalloc | M frees/s | 586.686 | 53.0 | 469.201 | 73.8 | 578.143 | 53.3 | 484.482 | 80.2 | +25.0% | +1.5% | +21.1% |
| alloc-test1 | s | 2.661 | 12.9 | 2.944 | 12.9 | 2.676 | 12.9 | 2.509 | 12.3 | +10.6% | +0.6% | -5.7% |
| mstress | s | 0.377 | 321.4 | 0.389 | 320.5 | 0.376 | 324.9 | 0.646 | 291.4 | +3.2% | -0.3% | +71.4% |
| valkey1 | M ops/s | 7.170 | 584.6 | 6.515 | 584.6 | 7.121 | 584.6 | 7.262 | 585.4 | +10.1% | +0.7% | -1.3% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

## 9. RSS bounded by cores: the Ruby idle-thread soak and a 64-thread larson (C4A, 16 vCPUs)

`make ruby-soak ALLOCS='challoc challoc-percpu' THREADS=T DURATION=180
COMPACT_AT=90` (bench/ruby/soak.rb, the Sidekiq shape: T mostly idle worker
threads under the GVL, bursts of 1..4 KiB strings and arrays, a slowly
growing long-lived set, a full GC every 30 s, `GC.compact` at 90 s; the
`--with-jemalloc` Ruby 3.4.10 with challoc staged as `libjemalloc.so.2`);
`challoc` is the shipped default (thread caches), `challoc-percpu`
`MALLOC_CONF=percpu_arena:percpu`. Full tables and CSVs:
`2026-09-19-aarch64-task8-ruby-soak-t{16,64,256}*`.

| threads | measure | challoc (thread caches) MiB | challoc-percpu MiB | percpu vs thread caches |
|---:|---|---:|---:|---:|
| 16 | peak VmRSS | 305.4 | 89.7 | -70.6% |
| 16 | after the last full GC | 305.4 | 89.7 | -70.6% |
| 16 | lowest after a periodic full GC | 279.2 | 65.1 | -76.7% |
| 64 | peak VmRSS | 1648.8 | 105.7 | -93.6% |
| 64 | after the last full GC | 1615.7 | 105.5 | -93.5% |
| 64 | lowest after a periodic full GC | 1575.8 | 73.4 | -95.3% |
| 256 | peak VmRSS | 2426.0 | 115.0 | -95.3% |
| 256 | after the last full GC | 2388.1 | 108.6 | -95.5% |
| 256 | lowest after a periodic full GC | 2308.5 | 83.5 | -96.4% |

Reading: with thread caches the process's RSS grows with the thread count
(305 -> 1649 -> 2426 MiB for 16 -> 64 -> 256 threads of the same live set,
about 21 MiB of live objects): each idle worker's heap keeps its own
caches and, because Ruby's GC frees other threads' objects, one `others`
node per arena it has freed into (design 6.2's foreign-cache residue
multiplied by the arena count). Under the mode the RSS is bounded by the
16 ids (90 -> 106 -> 115 MiB), which is the acceptance the plan asked for;
the reference soak (`2026-09-19-aarch64-ruby-soak.md`) has jemalloc at
160 MiB after 10 minutes of the 16-thread shape against challoc's 394,
so the mode also closes that gap. On this 7.0 kernel the 16-thread run is
in the per-task id mode (one id per thread, no sharing), so its saving is
the single any-arena cache per thread replacing the per-arena `others`
nodes; the 64- and 256-thread runs share ids.

larson with 64 threads (`larson 5 8 1000 5000 100 4141 64`, four times
the vCPUs, so ids are shared and sections are aborted and retried), three
runs each, `bench/rt`'s maxrss:

| variant | M ops/s (3 runs) | maxrss MiB |
|---|---|---:|
| `percpu_arena:percpu` | 397.8 396.2 398.0 | 392..401 |
| `percpu_arena:off` (default) | 578.6 582.0 582.1 | 561..562 |
| jemalloc 5.3.1 | 610.0 609.4 610.4 | 523..528 |

The mode: -31% throughput (the sections plus, at 64 threads on 16 CPUs,
the retries) and -30% RSS (16 ids' caches instead of 64 threads').

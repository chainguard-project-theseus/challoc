# challoc Dense cache headers, 2026-09-18, aarch64

- commit: 078cc0d
- date: 2026-09-18T02:54:34Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.17 0.82 2.22; at end: 9.18 7.91 5.34; 566 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; others: wall seconds
- variants:
  - old: 5b8bf5e7bbfb built into target/ab/5b8bf5e7bbfb
  - new: 078cc0d35ebc built into target/ab/078cc0d35ebc
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.8 43.0 43.0 43.0 43.1 | 16816 |
| larson1 | new | 42.9 42.9 43.0 43.0 43.0 | 17072 |
| larson1 | jemalloc | 44.8 44.9 45.0 45.1 45.1 | 19088 |
| larson10 | old | 370.1 370.3 370.6 373.4 374.1 | 119728 |
| larson10 | new | 367.8 369.7 374.1 375.0 375.2 | 119472 |
| larson10 | jemalloc | 381.4 383.4 384.1 387.0 387.2 | 134800 |
| larson16 | old | 596.2 598.0 599.8 599.9 603.1 | 184880 |
| larson16 | new | 591.6 595.5 597.2 598.9 598.9 | 182688 |
| larson16 | jemalloc | 612.4 614.4 614.6 615.6 617.7 | 212180 |
| xmalloc | old | 564.658 565.505 566.681 566.850 567.619 | 88052 |
| xmalloc | new | 565.848 570.093 570.739 572.195 572.904 | 85812 |
| xmalloc | jemalloc | 476.669 477.721 477.879 481.796 481.837 | 83372 |
| alloc-test1 | old | 2.646 2.649 2.659 2.671 2.681 | 13424 |
| alloc-test1 | new | 2.661 2.663 2.674 2.680 2.686 | 13304 |
| alloc-test1 | jemalloc | 2.543 2.548 2.554 2.555 2.575 | 12636 |
| alloc-testN | old | 2.245 2.250 2.252 2.255 2.268 | 22012 |
| alloc-testN | new | 2.265 2.267 2.269 2.269 2.278 | 21112 |
| alloc-testN | jemalloc | 2.207 2.208 2.208 2.212 2.213 | 16860 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 43.0 | 16.4 | 43.0 | 16.7 | 45.0 | 18.6 | +0.0% | -4.4% |
| larson10 | M ops/s | 370.6 | 116.9 | 374.1 | 116.7 | 384.1 | 131.6 | -0.9% | -3.5% |
| larson16 | M ops/s | 599.8 | 180.5 | 597.2 | 178.4 | 614.6 | 207.2 | +0.4% | -2.4% |
| xmalloc | M frees/s | 566.681 | 86.0 | 570.739 | 83.8 | 477.879 | 81.4 | -0.7% | +18.6% |
| alloc-test1 | s | 2.659 | 13.1 | 2.674 | 13.0 | 2.554 | 12.3 | +0.6% | -3.9% |
| alloc-testN | s | 2.252 | 21.5 | 2.269 | 20.6 | 2.208 | 16.5 | +0.8% | -2.0% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

- zone: us-east4-a (challoc-bench-arm64, c4a-standard-16); Phase 1.5 task 5 (review P3); code commits 97b3d31 (inline headers, per-class stacks) and 078cc0d (heap built in place); rows above are HEAD = 078cc0d against HEAD~2 = 5b8bf5e.

## Hardware counters, larson 1 and 10 threads (`sudo perf stat --inherit -p <container init>`, method of `2026-09-17-gce-review-counters.md`)

Per operation = per malloc+free pair (throughput x 5 s). `dTLB-load-misses` is `L1D_TLB_REFILL` on this core. old = 5b8bf5e, new = 97b3d31 (the header layout; 078cc0d only changes heap creation), two rounds each, quick builds.

| bench | variant | round | cycles | instr | L1D-miss | dTLB-miss | cyc/op | ins/op | L1D/op | dTLB/op | maxrss MiB |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | old | r1 | 1.5e+10 | 5.18e+10 | 5.28e+08 | 7.61e+08 | 70.3 | 242.7 | 2.47 | 3.56 | 17.2 |
| larson1 | new | r1 | 1.5e+10 | 5.17e+10 | 5.24e+08 | 6.81e+08 | 70.5 | 242.7 | 2.46 | 3.20 | 18.4 |
| larson1 | jemalloc | r1 | 1.49e+10 | 5.03e+10 | 4.63e+08 | 3.8e+08 | 66.6 | 224.8 | 2.07 | 1.70 | 17.4 |
| larson1 | old | r2 | 1.5e+10 | 5.19e+10 | 5.29e+08 | 7.57e+08 | 70.2 | 242.7 | 2.47 | 3.54 | 16.7 |
| larson1 | new | r2 | 1.5e+10 | 5.19e+10 | 5.27e+08 | 6.75e+08 | 70.2 | 242.7 | 2.46 | 3.15 | 18.2 |
| larson1 | jemalloc | r2 | 1.49e+10 | 5.04e+10 | 4.64e+08 | 3.8e+08 | 66.5 | 224.8 | 2.07 | 1.69 | 17.9 |
| larson10 | old | r1 | 1.49e+11 | 4.45e+11 | 4.87e+09 | 7.14e+09 | 81.4 | 242.6 | 2.65 | 3.89 | 116.7 |
| larson10 | new | r1 | 1.49e+11 | 4.48e+11 | 4.85e+09 | 6.57e+09 | 80.9 | 242.6 | 2.63 | 3.56 | 132.4 |
| larson10 | jemalloc | r1 | 1.49e+11 | 4.37e+11 | 4.29e+09 | 3.62e+09 | 76.6 | 224.8 | 2.20 | 1.86 | 130.1 |
| larson10 | old | r2 | 1.49e+11 | 4.43e+11 | 4.85e+09 | 7.25e+09 | 81.7 | 242.6 | 2.65 | 3.97 | 116.3 |
| larson10 | new | r2 | 1.49e+11 | 4.49e+11 | 4.87e+09 | 6.59e+09 | 80.6 | 242.6 | 2.63 | 3.56 | 130.7 |
| larson10 | jemalloc | r2 | 1.49e+11 | 4.3e+11 | 4.24e+09 | 3.59e+09 | 77.9 | 224.8 | 2.22 | 1.88 | 128.6 |

Reading: instructions and L1D misses per operation are unchanged (the header load moved, it did not disappear: `slots` is now a load on the header line instead of `caches[c]` on the heap line); L1D TLB refills per operation fall 3.55 -> 3.17 at one thread (-11%) and 3.93 -> 3.56 at ten. jemalloc is at 1.70 / 1.87: the other half of the gap is the 52 per-class descriptor pages the `free` check touches (review 3.3, plan task 4f / review P4), not the cache headers. Cycles per operation are flat within noise (70.3 -> 70.4; the A/B medians above are the number that counts). The maxrss column here is the header-layout commit before 078cc0d; see "RSS" below.

Final code (078cc0d, the `ab` builds of the rows above), two rounds:

| bench | variant | round | cycles | instr | L1D-miss | dTLB-miss | cyc/op | ins/op | L1D/op | dTLB/op | maxrss MiB |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | old 5b8bf5e | r1 | 1.5e+10 | 5.18e+10 | 5.28e+08 | 7.76e+08 | 70.3 | 242.7 | 2.47 | 3.64 | 16.8 |
| larson1 | new 078cc0d | r1 | 1.5e+10 | 5.15e+10 | 5.23e+08 | 6.75e+08 | 70.8 | 242.7 | 2.47 | 3.18 | 16.5 |
| larson1 | jemalloc | r1 | 1.49e+10 | 5.02e+10 | 4.62e+08 | 3.87e+08 | 66.7 | 224.8 | 2.07 | 1.73 | 17.5 |
| larson1 | old 5b8bf5e | r2 | 1.5e+10 | 5.16e+10 | 5.27e+08 | 7.69e+08 | 70.5 | 242.7 | 2.48 | 3.62 | 16.7 |
| larson1 | new 078cc0d | r2 | 1.5e+10 | 5.17e+10 | 5.27e+08 | 6.73e+08 | 70.5 | 242.7 | 2.47 | 3.16 | 16.8 |
| larson1 | jemalloc | r2 | 1.49e+10 | 5.02e+10 | 4.62e+08 | 3.9e+08 | 66.8 | 224.8 | 2.07 | 1.75 | 17.4 |
| larson10 | old 5b8bf5e | r1 | 1.49e+11 | 4.52e+11 | 4.92e+09 | 7.29e+09 | 80.2 | 242.6 | 2.64 | 3.92 | 117.2 |
| larson10 | new 078cc0d | r1 | 1.49e+11 | 4.46e+11 | 4.85e+09 | 6.58e+09 | 81.1 | 242.6 | 2.64 | 3.58 | 116.5 |
| larson10 | jemalloc | r1 | 1.49e+11 | 4.32e+11 | 4.26e+09 | 3.58e+09 | 77.6 | 224.8 | 2.22 | 1.86 | 128.6 |
| larson10 | old 5b8bf5e | r2 | 1.49e+11 | 4.45e+11 | 4.85e+09 | 7.19e+09 | 81.4 | 242.6 | 2.65 | 3.92 | 116.8 |
| larson10 | new 078cc0d | r2 | 1.49e+11 | 4.47e+11 | 4.84e+09 | 6.61e+09 | 81.0 | 242.6 | 2.63 | 3.59 | 116.9 |
| larson10 | jemalloc | r2 | 1.49e+11 | 4.26e+11 | 4.21e+09 | 3.52e+09 | 78.7 | 224.8 | 2.22 | 1.86 | 127.5 |

Acceptance reading: L1D TLB refills per larson operation 3.63 -> 3.17 at one thread (-13%; the plan asked for "3.4 -> ~2"; the review's own estimate was that the cache headers were one of two page sets, the other being the 52 descriptor pages the `free` check touches, which is plan task 4f), 3.92 -> 3.58 at ten; jemalloc 1.74 / 1.86. Cycles, instructions and L1D misses per operation unchanged; maxrss back at old's.

## The single slot block (review P3's second half): built, measured, rejected

The first build followed the plan literally: inline headers plus one contiguous slot block per heap (99 KiB for the default ladder, in a 128 KiB metadata bin, per-class offsets from the capacity ladder, `cap == 0` sentinel). `quick` (one round, this VM): larson1 +0.2%, larson10 +1.4%, alloc-test1 +1.1%, xmalloc -6.8% (median of three confirming rounds: 568.6 -> 532.2 M frees/s), NO-GO; on x86_64 "go" but every row worse (larson1 +2.4%, larson10 +4.8%, xmalloc +2.4%). Counters, xmalloc (`-w 10 -t 5 -s 64`, one class), block build against old:

| arch | variant | cycles | instr | L1D-miss | dTLB-miss | M frees/s |
|---|---|---:|---:|---:|---:|---:|
| arm64 | old | 1.34e+11 | 6.91e+11 | 5.52e+08 | 1.74e+08 | 367.5 |
| arm64 | block | 1.33e+11 | 6.82e+11 | 1.21e+09 | 1.75e+08 | 349.6 |
| arm64 | block r2 | 1.33e+11 | 6.94e+11 | 7.99e+08 | 1.77e+08 | 356.1 |
| x86_64 | old | 2.21e+11 | 6.14e+11 | 4.16e+08 | 5.18e+06 | 323.2 |
| x86_64 | block | 2.17e+11 | 5.9e+11 | 1.96e+09 | 9.34e+06 | 295.5 |
| x86_64 | block r2 | 2.18e+11 | 6e+11 | 1.82e+09 | 9.02e+06 | 300.8 |

larson1 under the block build had the same L1D misses per operation and 20% fewer dTLB refills (7.58e8 -> 6.08e8; the per-class stacks of the shipped layout get 6.75e8), so the block was the better layout for the TLB and a bad one for the L1D. `perf record -e L1-dcache-load-misses` on x86_64 puts 49% of the block build's misses in `Heap::other_caches` (the `others` list walk on the cross-arena free path: the loads of `(*node).arena` / `.next` on the node's first line) against 1.4% under per-class stacks, with `operator delete[]` (the `free` entry) and `free_slow` carrying most of the rest; `perf stat --per-thread` shows the misses on the ten freeing threads only (30..76 M each, 0.7..6.3 per k instructions, an 8x spread between threads) while the ten allocating threads stay at 1.3 per k under both builds. Variants built on the VMs (`target/quick/<v>`, one sed each) to split the mechanism:

| variant | what | x86_64 xmalloc L1D-miss | M frees/s |
|---|---|---:|---:|
| old | 5b8bf5e | 4.19e+08 | 322..328 |
| block | headers inline, one 128 KiB-bin block per heap | 1.78e+09 | 299..305 |
| v1 | headers inline, one 8 KiB-bin stack object per class (shipped) | 4.12e+08 | 317..320 |
| v3 | block, stacks page-aligned inside it | 1.64e+09 | 307.6 |
| v11 | per-class stack objects, each in a 128 KiB bin | 1.79e+09 | 297..301 |
| v13 | v11 plus a per-heap random 0..7 x 8 KiB slide inside the object | 1.76e+09 | 267 |

So the L1D fills come from the stack living in a 128 KiB-strided metadata object (v11 reproduces it with one class per object), not from packing classes together (v11 has none), not from sub-page placement (v3) and not from cross-thread virtual alignment (v13). The lines and the page geometry mod 4 KiB are identical between v1 and v11, and the L2 is physically indexed, so the mechanism is not attributed; it reproduces on both microarchitectures (arm64 v1: 5.47e8 misses, 366.1 M frees/s, at old's level). The shipped layout is v1.

## RSS

The A/B of the header-layout commit alone (97b3d31) carried larson RSS +1.5 MiB at one thread and +15 MiB at ten on both VMs (`quick` and the first protocol run; xmalloc and alloc-test unchanged). `smaps` by mapping name on this VM, larson1 at t=5 s: `challoc:classes` 7148 = 7148 kB, `challoc:meta` 1592 -> 1556 kB, unnamed anonymous 5448 -> 7152 kB. The unnamed mapping is the 3.5 GiB `rw-p` VMA just below `challoc:huge`: the merged thread stacks of larson's churned threads (glibc places the guard at the low end, so top-down consecutive stacks coalesce; the 437 stacks stay mapped). +1.7 MiB / 437 threads is one 4 KiB page per thread: the 3456-byte `Heap { .. }` temporary in `create_detached` deepened every thread's first allocation by a page. 078cc0d writes the heap field by field: unnamed 5428 kB against old's 5452, VmHWM 17852 against 17900 kB. The protocol rows at the top of this file are for 078cc0d.

## Valkey, the row task 4d said to watch (dTLB refills per SET)

`make valkey-soak ALLOCS=challoc DURATION=600 SETTLE=60 OCCUPANCY=1` on this VM with `bench/probes/valkey-perf-sidecar.sh` attached to `valkey-server` (078cc0d, the quantum8 stage; `soak-quick` needs a cached jemalloc soak this VM never saved, so the jemalloc column is task 4d's, same VM, same sidecar, same method: counters summed between the two snapshots around each 60 s record window, divided by the `total_commands_processed` delta; `2026-09-18-aarch64-gce-valkey-throughput.md` section 2, whose snapshot-window figures are the ones quoted).

| phase | alloc | run | cycles/cmd | instr/cmd | IPC | L1D miss/cmd | dTLB refill/cmd | br-miss/cmd | cmd/s in window |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| write (SET) | challoc 078cc0d | this run, t=98..160 | 3611 | 9937 | 2.75 | 43.9 | 56.3 | 7.02 | 675935 |
| write (SET) | challoc c18979d | 4d run 1 | 3704 | 10240 | - | 42.6..43.1 | 62..64 | 4.7..8.0 | 759k..854k |
| write (SET) | jemalloc | 4d run 1 | 3431 | 9927 | - | 39.3..39.6 | 38..47 | 4.5..8.6 | 850k..900k |
| delete (DEL) | challoc 078cc0d | this run, t=398..460 | 2373 | 7462 | 3.15 | 24.9 | 26.7 | 5.52 | 1030299 |
| delete (DEL) | challoc c18979d | 4d run 1 | 2417 | 7681 | - | 20.0..26.3 | 21..28 | 3.8..5.9 | 1.15M..1.34M |
| delete (DEL) | jemalloc | 4d run 1 | 2268 | 7406 | - | 18.8..24.9 | 22..25 | 3.8..5.6 | 1.24M..1.42M |

Reading: the server's L1D TLB refills per SET fall from 62..64 to 56 (jemalloc 47: about 40% of the +15 the review attributed to the cache headers; the descriptor pages are the rest, as for larson), cycles per SET 3704 -> 3611 (jemalloc 3431), so the write-phase gap to jemalloc narrows from +8.0% to about +5%; the delete phase is unchanged within run-to-run spread (2373 against 2417 / 2268). This is one run against task 4d's, not a same-run A/B, and the generator mix per window differs (the cmd/s column is the window's own rate); it is the row to re-measure same-run when task 4e (sized frees) lands, which is where the remaining cycles per SET are.

Soak summary of this run (`target/valkey/soak-it23/2026-09-18-aarch64-valkey-soak.md` on the VM; 4d run 1's challoc / jemalloc in brackets): write valkey-benchmark 790706 req/s, p99 1.215 ms [758892 / 850492; 1.255 / 1.131], memtier 873039, 1.055 [854341 / 899876; 1.095 / 1.027]; delete valkey-benchmark 1374912, 0.711 [1335273 / 1419519; 0.735 / 0.695], memtier 1178661, 0.791 [1152770 / 1236664; 0.823 / 0.767]; peak RSS 576.9 MiB, 125.7 MiB after settle, `allocator_frag_ratio` 1.74, 233143 defrag hits. Against jemalloc's 4d figures the write gap is -7.0% (valkey-benchmark) / -3.0% (memtier) and the delete gap -3.1% / -4.7%, the same shape as before with the write side a few percent closer; the Phase 1 soak criterion stays open on task 4e.

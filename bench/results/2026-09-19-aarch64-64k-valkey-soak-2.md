# The Valkey soak at 64 KiB pages, second run: page-sized slabs (Phase 2.5 task 4e)

Date 2026-09-19, 15:09..15:52 UTC. Tree 2a1919b's sources (the code commit
of task 4e; the working tree at the time was that code uncommitted on
dd5f5c8, no other change). Host: the same throwaway cell as task 4d,
`challoc-matrix-64k-arm64` from `MATRIX_MACHINE=c4a-standard-16
MATRIX_PMU=1 bench/gce/challoc-bench matrix lab 64k-arm64`: Ubuntu 24.04.5
with `linux-generic-64k`, kernel `6.8.0-139-generic-64k`, `getconf
PAGESIZE` 65536 on the host and in the container, Neoverse-V2, 16 vCPU,
`us-east4-a`; Docker with the dev image built on the VM, perf 6.8.12.
Feature line of the quantum8 library the server links, under `LD_PRELOAD`
in the container: `page_size=65536 guard_install=no mseal=no
process_madvise=no anon_name=yes mte=no mte3=no pac=yes lse=yes lse2=yes
bti=yes sve=yes cacheline=64 l1d=64K l2=2M`, and the report's new line:
`Page-sized slabs: classes 19..23 (512..1024 bytes) at 65536 bytes
(4096..4096 in the table)` (the run's text; the committed report prints
`4096..16384`, the range over the five classes). `make test` on this kernel:
474 passed, 0 failed, the cgroup churn tests ok, before the soak at the
run's sources and again after it at 2a1919b.

Protocol: exactly task 4d's. `make valkey STAGE=build`, then one `make
valkey-soak OCCUPANCY=1 MS_INTERVAL=60` running both halves (600 s write,
600 s delete, 60 s settle, `activedefrag yes`, stock memtier key stream,
Valkey 9.1.2, bundled jemalloc 5.3.0 built by the soak script on this
kernel with `LG_PAGE 16`; the 4d fix to `valkey-soak.sh` is in, so the
jemalloc half ran in the same invocation this time, 15:31..15:52, after
challoc's 15:09..15:31) with `bench/probes/valkey-perf-sidecar.sh
~/logs/4e/sidecar 700 1205` attached from the VM host. Nothing else on the
VM. The summary is the soak script's own (`runs.csv`, `samples.csv`,
VmHWM, `used_memory_rss` at the last sample of each phase). Samples:
`2026-09-19-aarch64-64k-valkey-soak-2-{challoc,jemalloc}.csv`; `MEMORY
MALLOC-STATS` after the settle: `...-2-challoc-mstats-t1260.txt`,
`...-2-jemalloc-mstats-t1261.txt`. Positive = challoc worse.

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc | 4d (sub-page slabs, same kernel) | 4 KiB (Phase 2 acceptance) |
|---|---:|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 954473 | 963665 | +1.0% | -0.4% | -4.5% |
| write: valkey-benchmark p99 ms | 0.999 | 0.991 | +0.8% | -0.4% | -4.4% |
| write: memtier req/s | 976179 | 996619 | +2.1% | +0.8% | -1.9% |
| write: memtier p99 ms | 0.911 | 0.879 | +3.6% | +0.5% | -3.3% |
| delete: valkey-benchmark req/s | 1434392 | 1483033 | +3.4% | +1.7% | +1.4% |
| delete: valkey-benchmark p99 ms | 0.695 | 0.663 | +4.8% | +1.2% | +1.1% |
| delete: memtier req/s | 1304583 | 1348515 | +3.4% | +2.0% | +1.2% |
| delete: memtier p99 ms | 0.711 | 0.671 | +6.0% | +2.4% | +0.0% |
| peak RSS MiB (VmHWM) | 589.7 | 585.4 | +0.7% (band 5%) | +2.2% | +1.0% |
| RSS MiB at end of write phase | 589.7 | 583.1 | +1.1% | +1.9% | |
| RSS MiB at end of delete phase | 174.6 | 145.5 | **+20.0%** | +53.9% | +0.2% |
| RSS MiB after settle | 174.6 | 145.5 | **+20.0%** (band 10%) | +51.9% | +0.3% |
| allocator_frag_ratio after settle | 2.32 | 2.07 | +12.1% | -11.5% | +0.6% |
| active_defrag_hits | 277987 | 360628 | +29.7% | +48.3% | +1.1% |
| keys after settle | 104604 | 103726 | | | |

`INFO memory` at the last sample of each phase (MiB):

| alloc | phase end | used_memory_rss | used_memory | allocator_allocated | allocator_active | allocator_resident | resident - active |
|---|---|---:|---:|---:|---:|---:|---:|
| challoc | write | 589.7 | 560.3 | 561.4 | 572.9 | 582.3 | 9.4 |
| challoc | delete | 174.6 | 63.0 | 64.8 | 157.2 | 167.1 | **9.9** |
| challoc | settle | 174.6 | 63.0 | 64.4 | 157.4 | 166.8 | 9.4 |
| jemalloc | write | 583.1 | 562.1 | 562.2 | 568.2 | 576.1 | 7.9 |
| jemalloc | delete | 145.5 | 62.5 | 62.6 | 128.7 | 136.6 | 7.9 |
| jemalloc | settle | 145.5 | 62.5 | 62.7 | 129.6 | 136.6 | 7.0 |
| challoc, 4d (sub-page slabs) | delete | 224.9 | 64.3 | 66.1 | 130.1 | 219.6 | 89.5 |
| jemalloc, 4d | delete | 146.1 | 63.5 | 63.6 | 131.4 | 138.8 | 7.4 |

(`MALLOC-STATS` after the settle agrees: challoc allocated 64.8 MiB, active
157.2, metadata 9.4, resident 166.6, retained 431.9; jemalloc allocated
62.8, active 129.7, metadata 7.0, resident 136.6.)

## Cycles per command (the sidecar)

`perf stat -I 1000` on the server divided by `total_commands_processed`
between the sidecar's snapshots (the acceptance's method; `.tmp-4e-*`
computed it from `stat-<alloc>.csv` and the `snap-*` files). Both servers
are main-thread bound; the `perf record` windows are t=100..162 and
700..761. The DSO-share files are empty again (the chain's `perf report`
ran after the `chown`, as in 4d; not needed here).

| window | alloc | commands/s | cyc/cmd | ins/cmd | CPI | L1D miss/cmd | dTLB refill/cmd | br-miss/cmd |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| write, unrecorded, t=30..100 | challoc | 977k | 2983 | 9398 | 0.32 | 44.2 | 23.3 | 7.3 |
| write, unrecorded, t=30..100 | jemalloc | 998k | 2919 | 9634 | 0.30 | 40.9 | 21.1 | 7.6 |
| write, under `perf record`, t=100..162 | challoc | 740k | 3118 | 9400 | 0.33 | 45.8 | 26.2 | 6.6 |
| write, under `perf record`, t=100..161 | jemalloc | 766k | 3017 | 9538 | 0.32 | 41.8 | 24.0 | 6.6 |
| write, unrecorded, t=162..700 | challoc | 1049k | **2781** | 8803 | 0.32 | 40.0 | 22.8 | 6.2 |
| write, unrecorded, t=161..700 | jemalloc | 1071k | 2724 | 8996 | 0.30 | 37.5 | 20.9 | 6.6 |
| delete, under `perf record`, t=700..761 | challoc | 1087k | **2131** | 6889 | 0.31 | 24.7 | 14.3 | 4.5 |
| delete, under `perf record`, t=700..761 | jemalloc | 1131k | 2063 | 7010 | 0.29 | 23.4 | 13.6 | 4.6 |
| delete, unrecorded, t=761..1205 (into the settle) | challoc | 1375k | 2114 | 6939 | 0.30 | 25.1 | 14.3 | 4.9 |
| delete, unrecorded, t=761..1205 | jemalloc | 1419k | 2049 | 7099 | 0.29 | 23.5 | 13.5 | 4.9 |
| whole life to t=1205 | challoc | | 2529 | 8108 | | 33.7 | 19.1 | 5.8 (task-clock 1231 s) |
| whole life to t=1205 | jemalloc | | 2445 | 8218 | | 31.3 | 17.5 | 5.9 (task-clock 1219 s) |

Against 4d on the same kernel (challoc / jemalloc: write long window 2676 /
2661, long delete 2084 / 2043): both servers are 2..4% slower per command
in this run (a different VM instance of the same shape), challoc's write
phase is +2.1% cycles per command against jemalloc (+0.6% in 4d) with fewer
instructions as always and 2.5 more L1D misses per command (40.0 against
37.5; 4d: 40.7 against 36.1), and the delete phase is +3.2% (+2.0% in 4d):
the standing residue, a point wider. The geometry change itself does not
show in the counters (the sized free is the same instruction stream, plan
task 4e's `quick-perf`), and the write-phase throughput (+1.0% / +2.1%) is
inside the generators' 3..5% spread; the delete-phase p99 (+4.8% / +6.0%)
is the widest reading of that residue so far and is recorded as such.

## The five classes after the settle

challoc at 64 KiB slabs (`MALLOC-STATS` t=1260: `nregs`, `curregs`,
`curslabs`, `nonfull`, `empty`, `purged`) against jemalloc at `LG_PAGE 16`
(t=1261: `regs`, `pgs`, `curregs`, `curslabs`, `util`). Active = curslabs x
slab; allocated = curregs x size.

| class | challoc slab | nregs | curregs | curslabs | active MiB | alloc MiB | util | Empty | purged | jemalloc slab (regs) | curregs | curslabs | active MiB | util |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|
| 512 | 64 KiB | 128 | 6936 | 119 | 7.4 | 3.4 | 45.5% | 0 | 78 | 64 KiB (128) | 6650 | 94 | 5.9 | 55.2% |
| 640 | 64 KiB | 102 | 13336 | 361 | 22.6 | 8.1 | 36.2% | 0 | 5031 | 320 KiB (512) | 13178 | 48 | 15.0 | 53.6% |
| 768 | 64 KiB | 85 | 13437 | 356 | 22.3 | 9.8 | 44.4% | 0 | 232 | 192 KiB (256) | 13211 | 107 | 20.1 | 48.2% |
| 896 | 64 KiB | 73 | 13258 | 418 | 26.1 | 11.3 | 43.4% | 0 | 263 | 448 KiB (512) | 13098 | 52 | 22.8 | 49.1% |
| 1024 | 64 KiB | 64 | 13496 | 489 | 30.6 | 13.2 | 43.1% | 0 | 292 | 64 KiB (64) | 13288 | 373 | 23.3 | 55.6% |
| five classes | | | | 1743 | **108.9** | 45.9 | 42% | **0** | | | | | **87.0** | 52% |
| 448 (64 KiB slabs in both geometries, the control) | 64 KiB | 146 | 6683 | 108 | 6.8 | 2.9 | 42.2% | 0 | 66 | 448 KiB (1024) | 6488 | 15 | 6.6 | 42.2% |

What changed against 4d: `empty` is 0 in every bin (4d: 1655 / 986 / 681 /
2562 / 6901 sub-page slabs, 79.5 MiB, that no page group let go), every
slab that emptied was purged (`purged` 78 / 5031 / 232 / 263 / 292; the
640 class turned over 5031 slabs during the delete phase), and `resident -
active` is 9.9 MiB, the page-rounded metadata (jemalloc's is 7.9). The
mechanism task 4d found is gone. What remains is `active`: 157.2 MiB
against jemalloc's 128.7 at the end of the delete phase (164.8 against
136.0 in the stats snapshots after the settle), 28.8 MiB, of which 21.9 are
the five classes above and the rest the same shape in the neighbouring
classes. For 512 and 1024 bytes the two allocators have exactly the same
slab (64 KiB, 128 and 64 regions) and challoc ends with 27% and 31% more
slabs at 45% and 43% utilisation against 55% and 56%: the difference is not
geometry but how far the survivors get compacted. Valkey's defragmenter
made 278k hits against 361k under jemalloc (`active_defrag_running` 0 at
the end for both: it stops when `frag_smallbins` falls under
`active-defrag-ignore-bytes`, 100 MiB; challoc's five classes sit at 63 MiB
of small-bin fragmentation after the settle, jemalloc's at 42), and
challoc's compacting cache (design 6.4) had 0.9..8.7 M compacted claims per
class, so both mechanisms ran; which decision differs (the utilization
queries design 13.4 answers, the direct-claim order under
`MALLOCX_TCACHE_NONE`, or the point the pass stops at) is the next task's
attribution, not this one's. The 4 KiB acceptance is unaffected: on 4 KiB
pages the table is byte for byte the same (`2026-09-19-page-sized-slabs.md`).

## The oscillation at the 4 KiB target

`scripts/cgroup-tests.sh oscillate challoc 300 200 480` in the 512 MiB
container on this kernel (HIGH given explicitly, so the task-3 envelope of
8 MiB is not applied): OOM-killed (`memory.events` `max 40 oom 1 oom_kill
1`, `memory.peak` 512 MiB) after at least 50 cycles. The `composition`
lines at the tops: `current 492..509, anon 479..503, kernel 4 (pagetables
3)` MiB with challoc `allocated 480, active 481..496, resident 483..498,
metadata 2, retained 0..25, empty 0` MiB. The stuck sub-page slabs of task
3's measurement (`empty` 0..2 MiB at the top) are gone, as expected; the
rest of the envelope (the page-rounded tables, the program's own data
segments, 2 MiB more of page tables than at 4 KiB) is what a 64 KiB kernel
adds, and 480 of 512 MiB still crosses the limit. The 8 MiB envelope stays
(design 11).

## Verdict (the Phase 1 soak criteria)

| criterion | result | verdict |
|---|---|---|
| throughput and p99 at least equal, write phase | +1.0% / +2.1% req/s, +0.8% / +3.6% p99 (valkey-benchmark / memtier); +2.1% cycles per command | inside the generators' 3..5% spread; a point behind 4d's reading |
| throughput and p99 at least equal, delete phase | +3.4% / +3.4% req/s, +4.8% / +6.0% p99; cycles per command +3.3% (recorded) / +3.2% (long) | behind by the standing delete residue, a point wider than 4d's; not a page-size term |
| peak RSS within 5% | 589.7 vs 585.4 MiB, +0.7% | PASS |
| post-delete RSS within 10% | 174.6 vs 145.5 MiB at the end of the delete phase and after the settle (+20.0%) | **FAIL**, from +53.9%; the failing term is now `active` (+28.8 MiB), not `Empty` slabs (0) |

Task 4e's mechanism is closed: no slab is smaller than the page, every
`Empty` slab purges, and the 79.5 MiB are gone. The acceptance as written
(post-delete RSS within 10%) is not met because the survivors compact less
under challoc than under jemalloc at the same slab geometry; that is plan
task 4f, with this soak's protocol as its acceptance.

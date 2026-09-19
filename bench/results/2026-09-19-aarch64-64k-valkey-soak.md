# The Valkey soak at 64 KiB pages (Phase 2.5 task 4d)

Date 2026-09-19, 13:50..14:41 UTC. Tree f0c76bc plus this iteration's two
harness edits (`MATRIX_MACHINE` in `bench/gce/challoc-bench`, the stderr
fix in `bench/valkey-soak.sh`, both below; no `src/` change). Host: a
throwaway `challoc-matrix-64k-arm64` from `MATRIX_MACHINE=c4a-standard-16
MATRIX_PMU=1 bench/gce/challoc-bench matrix lab 64k-arm64`: Ubuntu 24.04.5
(`ubuntu-2404-noble-arm64-v20260918`) with `linux-generic-64k`, kernel
`6.8.0-139-generic-64k`, `getconf PAGESIZE` 65536 on the host and in the
container, Neoverse-V2, 16 vCPU, `us-east4-a`, the reference C4A's shape;
Docker 27 with the dev image built on the VM (rustc 1.98.1), perf 6.8.12
from `linux-tools-6.8.0-139-generic-64k` (it exists; the PMU counts).
Feature line of the quantum8 library the server links, under
`LD_PRELOAD` in the container: `page_size=65536 guard_install=no mseal=no
process_madvise=no anon_name=yes mte=no mte3=no pac=yes lse=yes lse2=yes
bti=yes sve=yes cacheline=64 l1d=64K l2=2M` (task 3's line for this cell).
`make test` at f0c76bc on this kernel: 340 + 107 + 23 = 470 passed, 0
failed (task 3 ran it at the pre-4c library).

Protocol: `make valkey STAGE=build`, then `make valkey-soak OCCUPANCY=1
MS_INTERVAL=60` with the default protocol (600 s write, 600 s delete, 60 s
settle, `activedefrag yes`, stock memtier key stream, Valkey 9.1.2, bundled
jemalloc 5.3.0 built by the soak script on this kernel: `LG_PAGE 16` in its
`jemalloc_internal_defs.h`) and `bench/probes/valkey-perf-sidecar.sh
~/logs/4d/sidecar 700 1205` attached from the VM host. The two halves ran
as two invocations, not one: the challoc half 13:50:05..14:12:05, then the
first-use build of the jemalloc baseline hit a bug in the soak script (the
`step "building the jemalloc baseline"` line went to stdout inside
`server_for`'s `$(...)`, so the server path became a three-line string and
`valkey-server: not found`; fixed by sending it to stderr, the reference
VMs never saw it because their jemalloc tree is cached in the target
volume), and the jemalloc half ran 14:18:53..14:40:53 as `make valkey-soak
ALLOCS=jemalloc OCCUPANCY=1 MS_INTERVAL=60` with a jemalloc-only copy of
the sidecar, `make test` (14:17..14:18) between them, nothing else on the
VM. The table below is the soak script's summary computed over the two
halves' `runs.csv`, `samples.csv`, `vm.txt` and `dbsize.txt` with its own
definitions (mean req/s over a tool's runs in a phase, median of the
per-run client p99, VmHWM, `used_memory_rss` at the last sample of each
phase). Samples: `2026-09-19-aarch64-64k-valkey-soak-{challoc,jemalloc}.csv`
(133 rows each); challoc's `MEMORY MALLOC-STATS` at t=1200 (the end of the
delete phase): `2026-09-19-aarch64-64k-valkey-soak-challoc-mstats-t1200.txt`.
Positive = challoc worse.

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc | 4 KiB (Phase 2 acceptance, 4c7ced6) |
|---|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 985585 | 981199 | -0.4% | -4.5% |
| write: valkey-benchmark p99 ms | 0.967 | 0.971 | -0.4% | -4.4% |
| write: memtier req/s | 1017064 | 1024753 | +0.8% | -1.9% |
| write: memtier p99 ms | 0.867 | 0.863 | +0.5% | -3.3% |
| delete: valkey-benchmark req/s | 1470712 | 1496044 | +1.7% | +1.4% |
| delete: valkey-benchmark p99 ms | 0.671 | 0.663 | +1.2% | +1.1% |
| delete: memtier req/s | 1323796 | 1350605 | +2.0% | +1.2% |
| delete: memtier p99 ms | 0.687 | 0.671 | +2.4% | +0.0% |
| peak RSS MiB (VmHWM) | 599.9 | 587.2 | +2.2% (band 5%) | +1.0% |
| RSS MiB at end of write phase | 597.4 | 586.5 | +1.9% | |
| RSS MiB at end of delete phase | 224.9 | 146.1 | **+53.9%** | +0.2% |
| RSS MiB after settle | 224.9 | 148.1 | **+51.9%** (band 10%) | +0.3% |
| allocator_frag_ratio after settle | 1.84 | 2.08 | -11.5% | +0.6% |
| active_defrag_hits | 247549 | 367078 | +48.3% | +1.1% |
| keys after settle | 106364 | 105239 | | |

`INFO memory` at the last sample of each phase (MiB):

| alloc | phase end | used_memory_rss | used_memory | allocator_allocated | allocator_active | allocator_resident | resident - active |
|---|---|---:|---:|---:|---:|---:|---:|
| challoc | write | 597.4 | 564.6 | 565.9 | 584.9 | 595.1 | 10.2 |
| challoc | delete | 224.9 | 64.3 | 66.1 | 130.1 | 219.6 | **89.5** |
| challoc | settle | 224.9 | 64.1 | 65.9 | 130.0 | 219.5 | 89.5 |
| jemalloc | write | 586.5 | 564.8 | 564.9 | 571.0 | 578.9 | 7.9 |
| jemalloc | delete | 146.1 | 63.5 | 63.6 | 131.4 | 138.8 | 7.4 |
| jemalloc | settle | 148.1 | 63.4 | 63.6 | 132.3 | 138.8 | 6.5 |
| challoc, 4 KiB (acceptance) | delete | 129.8 | 63.4 | 65.1 | 126.7 | 130.1 | 3.4 |
| jemalloc, 4 KiB (acceptance) | delete | 129.5 | 62.3 | 62.3 | 112.3 | 124.5 | 12.2 |

## Cycles per command (the sidecar)

`perf stat -I 1000` on the server (task-clock plus five PMU events) divided
by `total_commands_processed` between the sidecar's snapshots
(`.tmp-percmd.py`, the acceptance's method). Both servers are main-thread
bound (100% of one CPU; `challoc-bg` idle), so this is the throughput.

| window | alloc | commands/s | cyc/cmd | ins/cmd | CPI | L1D miss/cmd | dTLB refill/cmd | br-miss/cmd |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| write, unrecorded, t=30..100 | challoc | 1035k | 2821 | 9261 | 0.30 | 45.0 | 22.6 | 7.4 |
| write, unrecorded, t=30..100 | jemalloc | 1039k | 2802 | 9506 | 0.29 | 39.1 | 20.0 | 7.6 |
| write, under `perf record`, t=100..162 | challoc | 784k | 2971 | 9376 | 0.32 | 48.3 | 25.6 | 6.2 |
| write, under `perf record`, t=100..162 | jemalloc | 773k | 2997 | 9751 | 0.31 | 41.8 | 21.8 | 6.6 |
| write, unrecorded, t=162..700 | challoc | 1090k | **2676** | 8748 | 0.31 | 40.7 | 22.2 | 6.3 |
| write, unrecorded, t=162..700 | jemalloc | 1095k | 2661 | 8965 | 0.30 | 36.1 | 19.8 | 6.7 |
| delete, under `perf record`, t=700..762 | challoc | 1059k | **2198** | 7138 | 0.31 | 26.2 | 14.3 | 4.9 |
| delete, under `perf record`, t=700..761 | jemalloc | 1124k | 2071 | 7074 | 0.29 | 23.8 | 12.8 | 4.6 |
| delete, unrecorded, t=762..1205 (into the settle) | challoc | 1405k | 2084 | 6997 | 0.30 | 25.6 | 13.9 | 5.1 |
| delete, unrecorded, t=761..1205 | jemalloc | 1426k | 2043 | 7120 | 0.29 | 23.7 | 12.8 | 5.0 |
| whole life to t=1205 | challoc | | 2438 | 8044 | | 34.2 | 18.5 | 5.8 (task-clock 1221 s) |
| whole life to t=1205 | jemalloc | | 2400 | 8185 | | 30.6 | 16.5 | 5.9 (task-clock 1212 s) |

Against the 4 KiB acceptance (challoc / jemalloc: write long window 2880 /
2970, recorded delete 2238 / 2272, long delete 2207 / 2178, write dTLB
refills 50.6 / 38.8 per command): the 64 KiB page halves the dTLB refills
per command for both allocators (22.2 / 19.8) and takes about 7% off the
write-phase cycles per command of both (2676 / 2661). The write phase is
at parity (+0.6% cycles per command, fewer instructions than jemalloc as
in every soak since 4e); the delete phase is +6.1% cycles per command in
the recorded window and +2.0% in the long one (+1.7..2.0% throughput,
+1.2..2.4% p99), the same residue the Phase 1.5 and Phase 2 acceptances
attributed to the usable-size query before `je_sdallocx` and the plain
free's chain, not a page-size term. DSO shares under `perf record`
(jemalloc: `valkey-server` 55.4% / 57.8%, kernel 37.9% / 34.4%, libc 6.7% /
7.8% in the write / delete windows); challoc's two record files were lost
to the chain script (it `chown`ed them before `perf report`, which perf
refuses without `-f`; the sidecar's `stat` CSV and snapshots are complete).

## The sub-page classes at the end of the delete phase (t=1200)

challoc, `MEMORY MALLOC-STATS` bins (design 5: 4 KiB slabs for 512 and
1024, 8 KiB for 896, 16 KiB for 640 and 768; committed, guarded and purged
in 64 KiB page groups on this kernel, a group only when every slab in it is
idle). `groups >=` is `(curslabs + empty) / slabs per group`, the fewest
resident groups these slabs can occupy (a group is resident when any slab
in it is); `live / group` and `Empty / group` divide by it.

| class | slab | slabs/group | curregs | allocated MiB | curslabs | active MiB | nonfull | Empty (not purgeable) | Empty MiB | purged | groups >= | live/group | Empty/group |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 512 | 4 KiB | 16 | 6989 | 3.4 | 1401 | 5.5 | 700 | 1655 | 6.5 | 131 | 191 | 7.3 | 8.7 |
| 640 | 16 KiB | 4 | 13615 | 8.3 | 1186 | 18.5 | 740 | 986 | 15.4 | 19861 | 543 | 2.2 | 1.8 |
| 768 | 16 KiB | 4 | 13637 | 10.0 | 1375 | 21.5 | 828 | 681 | 10.6 | 370 | 514 | 2.7 | 1.3 |
| 896 | 8 KiB | 8 | 13682 | 11.7 | 2526 | 19.7 | 1194 | 2562 | 20.0 | 539 | 636 | 4.0 | 4.0 |
| 1024 | 4 KiB | 16 | 13771 | 13.4 | 4331 | 16.9 | 1320 | 6901 | 27.0 | 1493 | 702 | 6.2 | 9.8 |
| five classes | | | | 46.8 | | 82.1 | | 12785 slabs | **79.5** | | >= 2586 | | |
| 1280 (64 KiB slabs, the control) | 64 KiB | 1 | 686 | 0.8 | 29 | 1.8 | 28 | 0 | 0 | 20 | 29 | | |

At t=600 (the end of the write phase) the same five classes have `empty`
5, 0, 1, 1, 5 and `purged` 3, 1, 0, 3, 5: nothing is stuck while the heap
grows. At t=1260 (after the settle) the row is unchanged from t=1200
(Empty 1655 / 986 / 682 / 2562 / 6893): the decay window has run out and
`purge_empty` has nothing it may take, exactly the F1 mechanism of
`2026-09-19-matrix.md` at Valkey's scale. The per-slab occupancy
histograms (`hist` lines of the snapshot file) show every class's slabs
split between full and nearly empty, the shape the compacting cache
(design 6.4) leaves on a random delete stream.

The arithmetic: challoc's `allocator_resident - allocator_active` is
89.5 MiB = 79.5 MiB of these `Empty` slabs + 10.0 MiB of metadata (page-
rounded tables at 64 KiB; 3.4 MiB in total at 4 KiB). `allocator_active`
is 130.1 MiB against jemalloc's 131.4 and the 4 KiB run's 126.7: the
partial-slab slack is the same on both kernels and both allocators; only
the unpurgeable `Empty` slabs are new, and they are the whole +78.8 MiB of
the post-delete criterion.

jemalloc 5.3.0 at `LG_PAGE 16`, same classes, same instant (its bins:
`regs`, `pgs`, `curregs`, `curslabs`, `util`):

| class | slab | regs | curregs | allocated MiB | curslabs | active MiB | util |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 512 | 64 KiB | 128 | 6722 | 3.3 | 100 | 6.2 | 0.525 |
| 640 | 320 KiB | 512 | 13264 | 8.1 | 46 | 14.4 | 0.563 |
| 768 | 192 KiB | 256 | 13308 | 9.7 | 105 | 19.7 | 0.495 |
| 896 | 448 KiB | 512 | 13401 | 11.5 | 60 | 26.2 | 0.436 |
| 1024 | 64 KiB | 64 | 13491 | 13.2 | 390 | 24.4 | 0.540 |
| five classes | | | | 45.8 | | 90.9 | |

jemalloc's slab is one page or more at every size (its `pgs` column), so
every slab it empties is a purge unit and its `resident - active` stays at
its metadata (6.8 MiB); its `active` for the five classes (90.9 MiB) is
above challoc's (82.1), and Valkey's defrag works harder for it (367k hits
against 247k; `allocator_frag_ratio` 2.08 against 1.84, the ratio Valkey
steers by, which does not see challoc's stuck slabs because they are
outside `active`). The 640 and 896 B classes (320 and 448 KiB slabs, 512
regions) show the design-5 trade-off does not bite jemalloc here: with
defrag on, survivors are moved out of sparse slabs whatever the slab size.

## Verdict (the Phase 1 soak criteria)

| criterion | result | verdict |
|---|---|---|
| throughput and p99 at least equal, write phase | -0.4% / +0.8% req/s, -0.4% / +0.5% p99 (valkey-benchmark / memtier); +0.6% cycles per command | parity, inside the generators' 3..5% spread, as at 4 KiB |
| throughput and p99 at least equal, delete phase | +1.7% / +2.0% req/s, +1.2% / +2.4% p99; cycles per command +6.1% (recorded) / +2.0% (long) | behind by the standing delete residue (+1.4% / +1.2% at 4 KiB, read the same way in the Phase 2 acceptance); not a page-size term |
| peak RSS within 5% | 599.9 vs 587.2 MiB, +2.2% | PASS |
| post-delete RSS within 10% | 224.9 vs 146.1 MiB at the end of the delete phase (+53.9%), 224.9 vs 148.1 after the settle (+51.9%) | **FAIL** |

The failing criterion is the allocator's, not the test's: `active` equal,
79.5 MiB of `Empty` sub-page slabs pinned by mixed 64 KiB page groups. The
container test's 8 MiB envelope (task 3) was the right size for a 480 MiB
oscillation with ~2 MiB stuck; at Valkey's delete phase the same mechanism
is 80 MiB, so the sub-page slab geometry is not fine on large pages and the
allocator-side fix is the next task (plan Phase 2.5 task 4e: page-sized
slabs for the 512 B..1 KiB classes when the page is larger than the slab,
refill steering by page-group density the fallback), with this soak as its
acceptance. Task 4d is done as a measurement.

## Harness notes

- `MATRIX_MACHINE=<type>` overrides a matrix cell's machine type (lab or
  not); the cell ran at 16 vCPU with the PMU (`MATRIX_PMU=1`).
- `bench/valkey-soak.sh`: the jemalloc baseline's `step` line goes to
  stderr (above). Any fresh host's first `make valkey-soak` with jemalloc
  in `ALLOCS` failed before this.
- The VM-side chains are `.tmp-4d-chain.sh` / `.tmp-4d-chain2.sh` (not
  committed): perf install, `make image`, `make valkey STAGE=build`, the
  probe, the soak with the sidecar, `make test`; the second the jemalloc
  half. Their `perf report` step must run before the `chown` (or with
  `-f`), which the first did not; the DSO shares above are from a manual
  report of the jemalloc records before the chain removed them.
- `linux-tools-6.8.0-139-generic-64k` is in Ubuntu 24.04's archive
  (installed with the kernel), so the sidecar runs on the 64k cell.

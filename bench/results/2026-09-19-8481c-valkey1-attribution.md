# The valkey1 gap on Sapphire Rapids: attribution (Phase 2.5 task 4b)

Date 2026-09-19 (11:35..12:37 UTC). Tree a50687f (library `target/quick/new`,
byte-identical to HEAD; jemalloc 5.3.1 from the dev image). Two hosts, the
same CPU:

- **`challoc-m4b-c3metal`**, `c3-standard-192-metal` in `us-east4-c` (a
  stockout in `us-east4-a`), 2 x Xeon Platinum 8481C (Sapphire Rapids, 48
  cores per socket, 2 MiB L2 per core, 105 MiB L3 per socket, SNC: 4 NUMA
  nodes), 755 GiB, Ubuntu 24.04 `7.0.0-1011-gcp`, `perf 7.0.14`. Bare metal
  exposes the core PMU (`cpu`, plus uncore) with every event used below.
  Created 11:35, deleted 12:36 (61 minutes). Direct ssh works as the local
  user name (`dlorenc@`), not the OS Login name the matrix VMs take.
- **`challoc-matrix-cpu-c3`**, `c3-standard-8` lab VM in `us-east4-a`
  (`challoc-bench matrix lab cpu-c3`), 8481C at 2.70 GHz, 8 vCPU, the same
  image and kernel, no PMU (`--performance-monitoring-unit` is refused for
  `c3-standard-8`, `c3d-standard-8`, `n2d-standard-8` and `c2-standard-8`
  at every level, in every zone tried and on the v1 and beta APIs:
  "PerformanceMonitoringUnit is not supported for c3-standard-8 on API
  version v1"). Software events, tracepoints and `perf record -e cpu-clock`
  work without a PMU and are what the VM side uses.

Also used: a `t2a-standard-8` lab VM for the Altra reading
(`2026-09-19-altra-larson-attribution.md`). Every VM of this iteration was
deleted; the reference VMs stayed TERMINATED.

Workload: `bench/probes/valkey_model` (the `valkey1` row is scored on its
write phase: 850,000 keys, SET overwrites over random keys, sized frees),
run as the standard `-t 10` row through `quick-perf.sh` and as the write
phase alone (`-w 8 -d 0`, "W"), the write phase with plain `free` ("WF",
the `valkey1-free` row) and the delete phase alone (`-w 0 -d 8`, "D").
Two new probe flags (this task): `-A FILE` dumps 4096 live pointers per
kind for the placement histograms; `-x K` adds K rounds of independent
integer work (about 80 instructions per SET per round) to the loop, the
instruction-window experiment of section 6. Raw logs:
`bench/results/matrix/2026-09-19-4b-*`.

## 1. The answer

**The mechanism is the instruction window, not the cache or the memory.**
Each SET of the write phase does one dependent DRAM miss, the load of the
random dict entry (`keys[rnd() % n]`, a 32 MiB table; 50% of the
program's own samples sit on that load's consumer under both allocators),
plus about three RFO misses on the recycled value and object slots. The
out-of-order core overlaps consecutive SETs' table misses only while both
SETs fit in its window (Golden Cove: 512-entry ROB, 192 loads). jemalloc's
SET is 266 instructions (77 loads); challoc's is 391 (95 loads), +125,
all in the two `malloc` and the two `sdallocx` calls, with the same miss
counts at every level (section 3). Below the window's limit the extra
instructions cost about 20 cycles; past it the next table miss cannot
issue and a whole DRAM latency is paid per SET. The `-x` sweep places the
cliff at about 350..430 instructions per SET on this workload, with
jemalloc's 266 under it and challoc's 391 at or over it (section 6): on the
C3 VM adding 160 dummy instructions to jemalloc's SET (426 total) drops it
from 10.2 to 6.5 M SET/s, below challoc's 7.3.

Where the miss goes decides the price: on the bare-metal 8481C with an
idle 105 MiB L3 the table mostly hits L3 and challoc is at **parity**
(290 vs 283 cycles per SET, 13.0 vs 13.1 M SET/s); on the 8-vCPU C3 VM
the table misses to DRAM (a 4-core slice of a shared L3) and challoc is
**-28%** (7.2 vs 10.0 M SET/s, 141 vs 101 ns per SET). The cells of the
CPU matrix line up with their L3 per core: Emerald Rapids C4 (5 MiB per
core, the reference) +4..5%, Sapphire Rapids C3 (1.9 MiB) +42%, Genoa
C3D and Milan N2D (32 MiB per 8-core CCD, half a CCD per 8-vCPU VM) +43%
and +21%, Cascade Lake C2 (1.4 MiB) +34%; the smaller ROBs of Zen 3/4
(256/320) and Cascade Lake (224) lower the cliff too.

**The single largest term is one instruction.** `Shared::sized_slot`, the
`sdallocx` fast path, calls `SlabTable::is_guard_slab`, whose
`page_group.wrapping_add(1) % self.guard_interval` is a 32-bit `divl` by a
run-time divisor: 12.5% of `je_sdallocx`'s samples on the metal, the
hottest instruction of the function, twice per SET, 12 `arith.div_active`
cycles per SET (32.5 vs 20.4 with `guards:false`; the other 20 are the
program's own `% n`), 16 instructions per SET, and about 12 cycles of
latency in the dependent chain that follows the table miss (address ->
slab -> page group -> `div` -> compare -> push). `guards:false` takes the
C3 VM's write phase from 7.2 to **9.7 M SET/s** (jemalloc 10.1), the whole
gap but 4 points; `guard_slab_interval:4194304`, which keeps the division
and removes nearly every guard slab, changes nothing (7.3), so it is the
division, not the guards. The plain `free` path has no such test (it reads
the slot's bit) and the `valkey1-free` row is unaffected (9.08 vs 9.10),
which is why the matrix showed `valkey1` far worse than `valkey1-free` on
the same CPUs. On the metal the division is worth +8..11%
(12.3..13.0 -> 13.6 M SET/s; 139.8 -> 127.8 cycles at n = 100k).

Refuted on the way: placement (section 4: every object kind is uniform
modulo the L1D, L2 and 2 MiB strides under both allocators, no
co-location), the background thread, decay, compaction, the cgroup sample,
arena count, cache size, vCPU migration (section 5: none moves the VM
number), kernel time and syscalls (section 5: user time equal, sys time
0.26 vs 0.22 s of 7.7 s, the 60k `mprotect` calls are page-group commits
in the 0.35 s fill and equal fill time), page faults (equal), TLB (equal
walks per SET), and the T2A's LSE helpers (the Altra file).

## 2. quick-perf on the metal (the standard rows, `-t 10`, both phases)

`bash bench/gce/quick-perf.sh amd64 ROWS="valkey1 valkey1-free larson1" VARIANTS="new jemalloc"`, one run each:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1 | challoc | 20.16 | 176.6 | 259.7 | 3.346 | 0.905 |
| valkey1 | jemalloc | 23.48 | 160.4 | 154.4 | 3.391 | 0.956 |
| valkey1-free | challoc | 21.01 | 178.6 | 230.0 | 4.586 | 0.924 |
| valkey1-free | jemalloc | 21.12 | 180.2 | 160.3 | 4.735 | 1.216 |
| larson1 | challoc | 27.90 | 70.1 | 232.7 | 1.125 | 0.001 |
| larson1 | jemalloc | 27.45 | 71.1 | 201.2 | 1.892 | 0.001 |

The reference C4 read the same shape on 2026-09-18 (valkey1 challoc
250..264 instr/op and 153..169 cycles against jemalloc 149 and 130;
`2026-09-18-x86_64-gce-tunables.md`): the instruction excess is a constant
of the code, the cycle cost of it is the host's.

## 3. Per-operation counters on the metal, by phase

`perf stat` attached to the container's process (seven event groups, each
its own run; cycles are the mean over the groups; per SET or per delete-phase
operation). Full table in `matrix/2026-09-19-4b-c3metal-ext-counters.txt`.

Write phase, sized frees (the scored phase; challoc 12.93..13.18 M SET/s
over seven runs, jemalloc 12.99..13.27):

| event | challoc /SET | jemalloc /SET | ratio |
|---|---:|---:|---:|
| cycles | 290.4 | 282.9 | 1.03 |
| instructions | 390.8 | 266.3 | **1.47** |
| loads (mem_inst_retired.all_loads) | 94.8 | 76.9 | 1.23 |
| stores | 37.6 | 39.0 | 0.96 |
| L1D load misses | 4.68 | 4.83 | 0.97 |
| mem_load_retired.l2_miss | 1.436 | 1.436 | 1.00 |
| mem_load_retired.l3_hit | 0.929 | 0.919 | 1.01 |
| mem_load_retired.l3_miss | 0.508 | 0.516 | 0.98 |
| ocr.demand_rfo.l3_miss (store misses to DRAM) | 1.893 | 1.919 | 0.99 |
| l2_rqsts.rfo_miss | 2.963 | 2.986 | 0.99 |
| dTLB load walks / store walks | 0.948 / 1.896 | 1.025 / 1.954 | 0.92 / 0.97 |
| walk_active cycles, load / store | 32.1 / 110.7 | 35.2 / 96.6 | 0.91 / 1.15 |
| memory_activity.stalls_l3_miss | 105.4 | 101.1 | 1.04 |
| cycle_activity.stalls_total | 164.7 | 182.3 | 0.90 |
| exe_activity.bound_on_stores | 22.3 | 47.3 | 0.47 |
| ld_blocks.address_alias (4K aliasing) | 0.172 | 0.233 | 0.74 |
| machine_clears.memory_ordering | 0.006 | 0.015 | 0.43 |
| br_misp_retired.all_branches | 0.044 | 0.185 | 0.24 |
| arith.div_active (section 7 runs) | 32.5 | (20.4 with `guards:false`) | |

Every miss column is equal within 5%; the two differ in instructions
(+125), loads (+18) and nothing else. Write phase, plain `free`
(`valkey1-free`): challoc **282.7** vs jemalloc 323.9 cycles (challoc 13%
ahead: 13.2..13.4 vs 11.3..11.6 M SET/s), 338.5 vs 278.4 instructions,
challoc fewer L2 misses (1.55 vs 2.46; jemalloc's `free` takes the rtree
miss that its `sdallocx` avoids). Delete phase, sized: 128.8 vs 113.6
cycles (+13%), 183.3 vs 111.7 instructions (+64%), misses equal or lower
(l3_miss 0.130 vs 0.148), 28.5..30.0 vs 32.9..33.4 M ops/s.

Software counters and kernel share (write phase, 8 s): page faults
149,900 vs 153,718 (all in the fill), context switches 64 vs 53,
migrations 0, `cycles:k` 7.4 vs 6.2 per SET, `instructions:k` 10.1 vs 7.5.
Syscalls over fill plus write: `mprotect` 60,097 vs 12 (59,191 of them in
the 2-second fill: 34,265 x 4 KiB, 12,138 x 8 KiB, 9,694 x 16 KiB and
3,083 x 64 KiB page-group commits of the small classes; about 170 per
second in the write phase), `madvise` 1,169 vs 27 (1,137 x 4 KiB
`MADV_GUARD_INSTALL`), `openat` 1,258 and `read` 2,508 vs 14 and 23 (the
cgroup sample; `perf trace` on the VM: 1,064 opens, 8.6 ms in 9 s). Fill
time 0.35 vs 0.34 s. None of it is in the write phase's budget.

## 4. Placement: the address histograms

`valkey_model -A` after the fill and after a 1 s write phase, 4096 live
keys per dump, each pointer's offset histogrammed at line granularity
modulo 4 KiB (48 KiB / 12 ways), 128 KiB (2 MiB / 16 ways) and 2 MiB.
`sd/poisson` is the spread of bucket counts over a uniform draw's:

| allocator | kind | span | mod 4K max/exp, sd/poisson | mod 128K | mod 2M | co-location |
|---|---|---:|---|---|---|---|
| challoc | ent / key / obj | 45 / 30 / 15 MiB (per-class regions) | 1.5x, 1.0 | 4.5x, 1.2 | 24x, 1.0 | 0 of 4096 pairs share a page |
| challoc | val | 34 classes' regions | 5.5x, 9.0 | 11.5x, 2.1 | 40x, 1.1 | |
| jemalloc | ent / key / obj | 636 MiB (one heap) | 1.3x, 1.0 | 4.5x, 1.0 | 24x, 1.0 | 0 |
| jemalloc | val | 636 MiB | 4.3x, 8.1 | 8.0x, 1.7 | 40x, 1.1 | |

The same numbers under both allocators: uniform for the fixed-size kinds,
the value's mod-4K clustering is the class rounding of its start address
and is identical under jemalloc. No sharp peak under challoc, none under
jemalloc: the placement hypothesis is refuted. The write-phase dump equals
the fill dump for `ent`/`key` (never reallocated) and stays uniform for
`obj`/`val` after 13 M overwrites.

## 5. The VM side: what does not move the number, and what does

C3 lab VM, write phase, `perf stat -e task-clock,page-faults,...`
(software events) plus `/proc/PID/stat`:

| run | write M SET/s | user s | sys s | page faults |
|---|---:|---:|---:|---:|
| challoc, default (x2) | 7.22, 7.10 | 7.7 | 0.26 | 149.8k |
| jemalloc (x2) | 10.05, 9.93 | 7.7 | 0.22 | 152.9k |
| challoc `background_thread:false` | 7.22 | 7.7 | 0.28 | |
| challoc `dirty_decay_ms:-1` | 7.21 | | | |
| challoc `tcache_compact_shift:0` | 7.18 | | | |
| challoc `cgroup:false` | 7.15 | | | |
| challoc all four | 7.18 | | | |
| challoc `narenas:1` / `tcache_max:1024` | 7.25 / 7.27 | | | |
| challoc / jemalloc pinned to one vCPU | 7.21 / 10.14 | | | |
| jemalloc `thp:always` | 10.61 | 7.9 | 0.08 | 7.3k |
| n = 300k: challoc / jemalloc | 11.42 / 12.64 (-10%) | | | |
| n = 100k: challoc / jemalloc | 17.73 / 19.61 (-10%) | | | |
| **challoc `guards:false` (x3)** | **9.69, 9.63, 9.74** | | | |
| challoc `guard_slab_interval:4194304` (x2) | 7.25, 7.33 | | | |
| challoc `guard_slab_interval:64` (x2) | 7.12, 7.12 | | | |
| challoc plain free, default / `guards:false` | 9.08 / 9.10 | | | |

The gap is user time; no background mechanism, arena or cache setting,
pinning or page size touches it; a smaller key table (n = 100k, a 4 MiB
table that stays cached) leaves the pure instruction cost (-10%, section
6's slope); and `guards:false` removes the division of section 7 and with
it most of the gap: from -28% to -4%.

`perf record -e cpu-clock` (write phase): challoc `set_overwrite` 78.6%,
`je_sdallocx` 13.0%, `je_malloc` 1.6%, kernel 3.3%; jemalloc
`set_overwrite` 71.9%, `sdallocx` 8.5%, `je_edata_heap_remove_first` 3.4%,
`malloc` 3.2%, `je_te_event_trigger` 2.3%, kernel 2.6%. Per SET: the
program's own code 111 ns under challoc against 72 under jemalloc, the
allocator 22 against 21. On the metal the same record reads `set_overwrite`
60.6% vs 59.1%, `sdallocx` 20.1% vs 14.3%, and `perf annotate` of
`set_overwrite` is the same picture under both: 49.9% / 47.8% on
`movl 0x20(%r12),%esi` (the entry's `vsize`, the table miss), 21.5% / 21.0%
on `movb $0x1,(%rax)` (the first store into the new value), 15.5% / 14.4%
on `movq (%r12),%rdi` (the entry's `val`). The program's time is the table
miss; what changes between allocators is how much of the next SET's miss
overlaps it.

## 6. The instruction-window experiment (`-x`)

One `-x` round adds about 80 independent integer instructions to every
SET (four xorshift chains, no memory, split around the two frees). Write
phase, 6 s, M SET/s (metal pinned to one core, with counters per SET):

| host | allocator | +0 | +1 (~80) | +2 (~160) | +3 | +4 | +6 |
|---|---|---:|---:|---:|---:|---:|---:|
| C3 VM | jemalloc | 10.20, 10.24 | 9.08 | **6.49, 6.52** | 6.44 | 6.21 | 5.84 |
| C3 VM | challoc | 7.27, 7.35 | 6.59 | 6.30 | 6.19 | | |
| C3 VM, n=100k | jemalloc | 20.28 | | 15.65 | | 14.08 | |
| C3 VM, n=100k | challoc | 17.73 | | 13.88 | | | |
| metal | jemalloc (cycles / instr / stalls_l3_miss per SET) | 13.26 (297 / 275 / 107) | 12.15 (324 / 379 / 115) | **10.38 (379 / 460 / 145)** | 10.01 (394 / 538 / 142) | 9.66 (408 / 617 / 141) | 8.87 (444 / 775 / 138) |
| metal | challoc | 13.01 (303 / 400 / 111) | 10.44 (377 / 506 / 134) | 9.83 (401 / 586 / 133) | 9.48 (416 / 664 / 130) | | |
| metal, n=100k | jemalloc | 32.93 (115.6 / 259) | | 23.04 (165 / 439) | | 19.16 (199 / 596) | |
| metal, n=100k | challoc | 27.25 (139.8 / 386) | | 19.38 (197 / 567) | | | |

Readings. (1) The cliff: on the VM jemalloc loses 11% for the first 80
instructions and 36% for 160, then only 1..4% per further 80; on the metal
the same step raises `memory_activity.stalls_l3_miss` from 115 to 145
cycles per SET (the second miss no longer overlaps the first) between 379
and 460 instructions per SET. challoc's 391..400 sit at the knee: under it
on the metal (111 stall cycles), over it on the VM, whose slower miss
holds the window longer. (2) The price per instruction beyond the knee:
jemalloc +160 instructions cost 55 ns per SET on the VM (0.30 ns per
instruction) but 14.6 ns at n = 100k (0.08, plain issue cost at IPC 3.6)
and 22 ns on the metal (0.12); a longer stream is four times as expensive
when the table misses to DRAM. (3) jemalloc plus 160 instructions (426 per
SET) lands at 6.5 M SET/s, below challoc's 7.3 with 391: the count alone
reproduces the gap on the VM, with jemalloc's own layout and paths.

## 7. The division (`is_guard_slab` on the sized path)

`perf annotate` of `je_sdallocx` on the metal (220 instruction lines, 24
of them at 1.5% or more): `divl %r11d` at `+0x28e` holds
**12.54%** of the function's samples, every other line 2.7..3.9%. It is
`SlabTable::is_guard_slab`'s `page_group.wrapping_add(1) % self.guard_interval`
inlined into `Shared::sized_slot` (`src/heap.rs`), evaluated for every
sized free that names a handed-out slab; the divisor is a field, so the
compiler cannot strength-reduce it although the default interval is 8.
Metal write phase, pinned, counters per SET:

| variant | M SET/s | cycles | instructions | l3_miss | arith.div_active |
|---|---:|---:|---:|---:|---:|
| default | 12.29 | 281.9 | 401.5 | 0.52 | 32.5 |
| `guards:false` | 13.62 | 289.5 | 385.5 | 0.51 | 20.4 |
| `guard_slab_interval:4194304` | 13.05 | 302.5 | 400.1 | 0.51 | 32.5 |
| n = 100k, default | 27.26 | 139.8 | 386.0 | 0 | 32.4 |
| n = 100k, `guards:false` | 29.79 | 127.8 | 371.8 | 0 | 20.3 |

Two `divl` per SET are 12 divider-active cycles and 16 instructions;
where nothing else stalls (n = 100k) they are 12 of the 140 cycles per
SET (8.6%); in the VM's window-bound regime their latency in the post-miss
chain is what tips the SET over the cliff (section 5: 7.2 -> 9.7 M SET/s).
The plain `free` path (`checked_slot`) has no division (the slot bit
decides) and is unaffected; the Altra shows no `guards:false` effect either
(its gap is elsewhere, the other file).

## 8. Proposed fix (the next task, not implemented here)

1. **Division-free guard test on the sized path.** Make
   `guard_slab_interval` a power of two (the default 8 is; round a
   configured value up and record the rounding in `opt.guard_slab_interval`)
   and test `(page_group + 1) & (interval - 1) == 0`, or keep `lg_interval`
   in the table. A few lines in `src/class.rs` (and `config.rs`), semantics
   unchanged (`sized_slot_rejects_guard_slabs`, `tests/c/guard_slab.c`).
   Expected: the C3-class `valkey1` row from -28% to about -4% (the
   `guards:false` measurement is the upper bound; the interval-4M run says
   the guards themselves cost nothing), the C4 row a few points, the metal
   +8..10%; `arith.div_active` per SET 32 -> 20 on the C4 `quick-perf`.
   Proof: `quick` on a `matrix lab cpu-c3` VM (`valkey1` within 5% of
   jemalloc, `valkey1-free` unchanged) and on both reference VMs; the
   `arith.div_active` row on the C4.
2. **The instruction budget of the sized path.** After (1) challoc's SET is
   about 375 instructions, still at the knee. Each further 80 instructions
   removed is worth 0.3 ns per SET on small-L3 hosts and about nothing on
   the reference; the `-x` sweep (`valkey_model -x K` under jemalloc on a
   C3 lab VM) is the yardstick for where the knee is, and
   `quick-perf`'s instr/op column the budget. Candidates in `sized_slot`
   and `push_fast`: the `is_handed_out` load and compare (the flush's slot
   bit test already rejects a slot of a slab that is not handed out), the
   second region classification in the slow-path fallthrough, the
   `push_flags` load. Target: under 330 instructions per SET, jemalloc's
   knee-side.
3. Not a fix, a note: the fill's 60k page-group `mprotect` commits (one per
   4..16 KiB page group of the small classes) cost nothing measurable here
   (fill 0.35 vs 0.34 s) but are 5,000 syscalls per second of fill; a
   commit granularity of one slab group for small classes would remove
   them.

## 9. Method notes

- PMU on GCE for these CPUs: only bare metal. `c3-standard-192-metal`
  (`--maintenance-policy=TERMINATE`, hyperdisk-balanced, the standard
  Ubuntu image) took 11 minutes from create to ssh in `us-east4-c`
  (`us-east4-a` was stocked out; the API names the zone that has capacity)
  and exposes `cpu`, `intel_pt`, `dsa`, `iax` and the uncore PMUs; direct
  ssh as the local user name, `gcloud compute ssh` for the first login.
- The `perf stat -x, --inherit -p <container init pid>` recipe of
  `quick-perf.sh` works unchanged; `perf record --call-graph dwarf` on
  syscall tracepoints recorded no user chains (the tracepoint samples carry
  no user stack here), the syscall arguments (`len`, `prot`, the per-second
  timeline) told the story instead.
- A `pkill -f`/`pgrep -f` pattern that appears in the ssh command line
  kills the session (twice this iteration); relaunch scripts live in files.

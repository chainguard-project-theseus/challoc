# The compacting cache at 64..128-region slabs (Phase 2.5 task 4g)

Date 2026-09-19, 17:19..17:58 UTC. Tree a3ddd6d plus the probe change of
this task (`bench/probes/churn.c` takes a pinned fraction; no `src/`
change). Host: a fresh throwaway `challoc-matrix-64k-arm64` from
`MATRIX_MACHINE=c4a-standard-16 MATRIX_PMU=1 bench/gce/challoc-bench matrix
lab 64k-arm64`: Ubuntu 24.04.5, `6.8.0-139-generic-64k`, page 65536,
Neoverse-V2, 16 vCPU, `us-east4-a`, the dev image built on the VM, Valkey
9.1.2 from `make valkey STAGE=build`. Deleted at the end (`matrix clean`).

The question (plan task 4g, the second-order term of 4f): after Valkey's
last defrag cycle, 210 s of balanced DEL:SET churn drain jemalloc's
`active` by 12 MiB and challoc's by 4, while challoc's 512 B class reports
`compacted` +356k scheduled pops and `curslabs` 128 -> 127. Is the
compacting cache (design 6.4), tuned on 4 KiB slabs of 4..25 regions,
failing at 64 KiB slabs of 64..128 regions, and if so where: the gate's
units (i), the rebuild and refills fighting over the low slabs (ii), the
flush's "highest half" spanning the same two slabs as the run (iii), the
defragmenter's `MALLOCX_TCACHE_NONE` traffic counted against the
refill-rate gate (iv)?

## 1. What 4f's own prints already say

The 5 s `MEMORY MALLOC-STATS` prints kept from the 4f soak
(`2026-09-19-aarch64-64k-defrag-attribution-challoc-mstats-t{396,666}.txt`,
the plateau after the last cycle and the end of the churn, 270 s apart),
challoc's classes with 64 KiB page-sized slabs:

| class | regions | `curslabs` | `curregs` | `refilled` | `compacted` | `c_on`/`c_off`/`gated` | hist t=396 (eighths, last = full) | hist t=666 |
|---:|---:|---|---|---|---|---|---|---|
| 512 | 128 | 128 -> 127 | 6991 -> 7116 | 28160 -> 28160 | 65088 -> 427712 | 1 / 0 / 0 | [20, 64, 1, 1, 0, 0, 0, 30, 12] | [64, 13, 1, 0, 0, 0, 1, 4, 44] |
| 640 | 102 | 336 -> 328 | 13670 -> 13663 | 556864 -> 556864 | 137664 -> 861056 | 1 / 0 / 0 | [94, 137, 1, 1, 0, 0, 0, 21, 82] | [183, 26, 1, 0, 0, 0, 0, 43, 75] |
| 768 | 85 | 376 -> 373 | 13724 -> 13679 | 59136 -> 59136 | 577568 -> 3780544 | 1 / 0 / 0 | [98, 146, 3, 0, 0, 0, 1, 0, 128] | [193, 39, 0, 0, 0, 1, 1, 18, 121] |
| 896 | 73 | 441 -> 434 | 13636 -> 13667 | 57600 -> 57600 | 133312 -> 854560 | 1 / 0 / 0 | [113, 176, 9, 0, 0, 0, 1, 22, 120] | [236, 33, 0, 2, 0, 0, 1, 0, 162] |
| 1024 | 64 | 502 -> 495 | 13751 -> 13742 | 58176 -> 58176 | 135232 -> 860256 | 1 / 0 / 0 | [108, 204, 25, 0, 0, 0, 0, 60, 105] | [234, 69, 0, 0, 1, 0, 5, 68, 118] |

- (i) the gate: every class entered compact mode once (`c_on` 1) and never
  left it (`c_off` 0); the refill-rate gate never declined a probe
  (`gated` 0). The 512 B class has 16.3k capacity and 9.3k holes at t=396,
  against a threshold of 4 x 128 = 512 holes and 1/32 of capacity = 508.
  The gate is open; its units are not the block.
- (ii) refills: `refilled` did not move in any class in 270 s. The
  balanced churn never empties a cache; the only slow path is the
  rebuild. Nothing fights the schedule for the low slabs.
- (iv) `MALLOCX_TCACHE_NONE`: no defrag cycle ran after t=368 and
  `gated` is 0, so the direct claims counted against nothing;
  `ClassState::refilled` still counts refills only (`note_refill` is
  called from `Heap::refill` alone; `note_compact` from the rebuild;
  direct claims go through `Shared::alloc_direct` -> `ClassState::claim`
  and touch neither).
- The schedule ran at 1.3..1.7k claims per second per class (768 B: 12k/s,
  its cache is the 128-slot one and rebuilds more often) and the
  histograms moved the way design 6.4 says they should: the 7/8 bucket
  emptied into "full" (512 B: 30 + 12 -> 4 + 44; 1024 B: 60 + 105 -> 68
  + 118) and the 1/8 bucket into the 0/8 one (64 -> 13 and 20 -> 64;
  204 -> 69 and 108 -> 234). The churned values were packed into the low
  slabs. What did not happen is the last step: the 0/8 slabs did not
  empty. 1024 B ends with 303 slabs under 25% holding about 1830 values,
  six per slab; 512 B with 78 slabs holding about 930, twelve per slab.

So the machinery is running as designed and the residue is a handful of
values per slab that 270 s of a churn turning the live set over every
0.4 s (14k allocations per second per class against 7..14k live values)
did not free. The 2026-09-17 soak-3 tracer already identified those
values: keys memtier's fixed-seed key stream never selects again in the
delete phase, about 2200 per class, scattered over the address range
(`2026-09-17-aarch64-gce-compacting-cache-soak3.md`). At 4 KiB pages the
answer was geometry (4 KiB slabs for 512..1024 B, a pinned value holds 4
KiB); at 64 KiB pages the slab is the page again (task 4e) and a pinned
value holds 64 KiB, under both allocators.

## 2. The churn probe at the 64 KiB geometry

`bench/probes/churn.c` in the dev container on the lab (`page 65536`;
Wolfi's `libjemalloc.so.2` runs at this page size). N blocks so that the
class has 469 slabs at 64 KiB: 512 B (128 regions) N = 60000, 1024 B (64)
N = 30000, 640 B (102) N = 48000; 56% freed at random, then a balanced
churn (free a random survivor, allocate a replacement) for T seconds. New
in this task: `PIN`, a fraction of the survivors picked at random that the
churn never frees, with the count of distinct 64 KiB slabs holding one
(the floor: a slab with a pinned block cannot empty). One run each.

| class | N | PIN | pinned (floor slabs) | challoc slabs after delete -> end (occ) | jemalloc | challoc M ops/s |
|---:|---:|---:|---|---|---|---:|
| 512 | 60000 | 0 | | 469 -> **214** (96.9%) | 469 -> 469 (43.8%) | 24.5 |
| 512 | 60000 | 0.018 | 473 in 291 | 469 -> 361 (57.5%) | 469 -> 469 | 19.5 |
| 512 | 60000 | 0.072 | 1893 in 465 | 469 -> 468 (44.2%) | 469 -> 469 | 19.4 |
| 1024 | 30000 | 0 | | 469 -> **216** (97.0%) | 469 -> 469 (44.3%) | 24.1 |
| 1024 | 30000 | 0.035 | 465 in 297 | 469 -> 371 (56.4%) | 469 -> 469 | 18.9 |
| 1024 | 30000 | 0.14 | 1862 in 465 | 469 -> 468 (44.8%) | 469 -> 469 | 19.1 |
| 640 | 48000 | 0 | | 471 -> **214** (97.0%) | 94 -> 94 (43.7%, 320 KiB slabs) | 24.0 |
| 640 | 48000 | 0.022 | 462 in 301 | 471 -> 367 (56.5%) | 94 -> 94 | 19.0 |
| 640 | 48000 | 0.09 | 1893 in 464 | 471 -> 469 (44.2%) | 94 -> 94 | 19.0 |
| 512, `tcache_compact_shift:0` | 60000 | 0 | | 469 -> 469 (44.1%) | | 25.1 |
| 512, `tcache_compact_shift:5` | 60000 | 0 | | 469 -> 214 (96.9%) | | 25.0 |
| 512, `tcache_nslots_small_max:64` | 60000 | 0 | | 469 -> 212 (96.9%) | | 24.9 |

- Without pinned blocks the schedule drains the class at 128, 102 and 64
  regions per slab exactly as it did at 64 regions in Phase 1 (469 -> 216
  then, 214..216 now, within 2 s, 97% end occupancy against the quick
  tier's 90% criterion), and the control with the schedule off stays at
  469, as does jemalloc (its LIFO tcache reuses the slot just freed; it
  neither fills nor flushes under a balanced churn of one size). (iii) is
  ruled out with the rest: at 128 regions the flush's highest half and the
  run are not the same two slabs, because the cache holds a sample of the
  live set's addresses, not a refill batch.
- With pinned blocks the end state is what an address gradient with
  immovable objects gives, and it is predictable: the churned values fill
  the slabs from the bottom of the class until they fit (25.8k values at
  127 free regions per slab: 204 slabs), and above that boundary only the
  slabs holding a pinned block stay (265 slabs x the pinned fraction
  291/469: 164), 368 predicted against 361 measured for 512 B; 372 against
  371 for 1024 B; 375 against 367 for 640 B. At four pinned blocks per
  slab (the soak's density, section 3) the floor is 464..465 of 469 and
  challoc ends at 468..469: at the floor. The optimum a policy could reach
  at one pinned block per slab is the floor itself (the pinned slabs'
  holes, 291 x 127, hold the churned set), 70..74 slabs or 19..20% below
  the gradient's end state; at four per slab the two coincide within 1%.
  jemalloc does not move under either pinning (469 -> 469 in all six
  runs).

## 3. The shortened soak: where the survivors are and whose they are

Two runs of the shortened protocol, `DURATION=300 INTERVAL=1 OCCUPANCY=1
MS_INTERVAL=5 make valkey-soak` (150 s write, 150 s delete, 60 s settle;
`MEMORY MALLOC-STATS` every 5 s), with `bench/probes/valkey-defrag-sidecar.sh`
attached from the VM host for the server's own cycle lines. Soak 1 ran both
allocators (`...-compaction-soak-{valkey-soak.md,challoc.csv,jemalloc.csv,
{challoc,jemalloc}-defrag.log,{challoc,jemalloc}-mstats-t{245,325,385}.txt}`);
soak 2 ran challoc alone with one extra sample (`...-soak2-*`): at the end of
the delete phase (process second 330, the soak's t = 325..330, the last
memtier chunk still running) one `EVAL` scanned every key inside the server
and bucketed it by `OBJECT IDLETIME` and by the allocation class of its
value's sds (`len + 4` below 256 bytes, `+ 6` above), `...-soak2-challoc-idle-t330.txt`.
(A first sampler that issued the two commands per key through `docker exec`
took 250 s per pass and touched every key's LRU clock on the way; its
samples are discarded and soak 1 has no idle sample.) Positive = challoc
worse.

Soak 1, the protocol rows and the cycles:

| measure | challoc | jemalloc | vs | 4f (600 s) | 4e (1200 s) |
|---|---:|---:|---:|---:|---:|
| write: valkey-benchmark / memtier req/s | 1026472 / 1060692 | 1008642 / 1060695 | -1.7% / +0.0% | | |
| delete: valkey-benchmark / memtier req/s | 1513651 / 1363478 | 1531395 / 1381015 | +1.2% / +1.3% | | |
| peak RSS MiB (VmHWM) | 592.0 | 588.0 | +0.7% | +1.7% | +0.7% |
| RSS MiB after settle | 180.9 | 153.8 | **+17.6%** | +20.5% | +20.0% |
| `allocator_active` MiB after settle | 156.4 | 140.8 | +15.6 MiB | +21.7 | +28.8 |
| defrag cycles (moves) | 2 (145911, 143898) | 3 (155253, 135336, **77938**) | | 2 vs 3 | |
| active_defrag_hits | 289809 | 368527 | +27.2% | +25.9% | |
| keys after settle | 106997 | 106590 | | | |

Soak 2 (challoc only): peak 590.0, RSS after settle 176.8 MiB, 2 cycles
(145023, 142487 moves), 105855 keys: the same run.

The per-class series (soak 1; the soak's clock, cycle 2 at t = 221 under
challoc, cycle 3 at t = 228 under jemalloc; `curslabs`, and for challoc
`compacted` and the 0/8 + 1/8 histogram buckets, the slabs under 25%):

| class | challoc t=245 | t=325 (delete end) | t=385 (settle end) | jemalloc t=245 | t=325 | t=385 |
|---:|---|---|---|---:|---:|---:|
| 512 | 124 slabs, 64.7k, 79 sparse | 123, 204.0k, 74 | 123, 209.5k, 74 | 101 | 100 | 100 |
| 640 | 317, 136.0k, 208 | 309, 413.1k, 190 | 309, 424.0k, 190 | 77 (320 KiB slabs) | 73 | 62 |
| 768 | 362, 530.1k, 229 | 362, 1763.1k, 220 | 362, 1799.6k, 220 | 118 (192 KiB) | 117 | 117 |
| 896 | 428, 132.0k, 269 | 428, 409.4k, 264 | 427, 420.4k, 263 | 63 (448 KiB) | 63 | 62 |
| 1024 | 503, 133.5k, 315 | 497, 411.6k, 305 | 497, 422.1k, 305 | 392 | 389 | 388 |

- The plateau is reached 20..45 s after the last cycle: 512 B `curslabs`
  125 (t=225) -> 123 (t=265) and flat for 120 s while `compacted` runs
  from 27.8k to 209.5k; 1024 B 511 (t=230) -> 497 (t=275) and flat while
  `compacted` runs 80k -> 422k. The histograms keep moving inside the
  plateau (512 B: the 1/8 bucket 64 -> 16, 7/8 into full), so the
  schedule is still migrating churned values downward; the slab count
  does not follow because every slab keeps a few values the churn does
  not free.
- The "second-order term" of 4f in this run: `active` after the last
  cycle to the end, challoc 159.8 -> 156.4 MiB (-3.4), jemalloc 148.5 ->
  140.8 (-7.7). In the classes with the same 64 KiB slabs the two drift
  alike (512 B: -1 slab under both; 1024 B: -6 vs -4); jemalloc's extra
  is its 640 B class of 320 KiB slabs emptying at the settle (77 -> 62
  slabs, 4.7 MiB, its tcache GC flushing a cache that holds a slab's
  last regions) plus a few slabs elsewhere. There is no 64 KiB-slab class
  where jemalloc's churn drains and challoc's does not.

Whose values sit in the sparse slabs (soak 2, the idle sample at the end
of the delete phase; the churn replaces a key in about 0.4 s when the
stream selects it, so a value idle for 60 s is one the stream has not
selected in 150 chunks' worth of turnover):

| class | live | idle 1..10 s | 10..60 s | 60..150 s | > 150 s (older than the delete phase) | sparse slabs at t=325 (values in them) |
|---:|---:|---:|---:|---:|---:|---|
| 512 | 6806 | 2222 | 3381 | 1146 | 57 | 73 of 122 (about 970) |
| 640 | 13357 | 4429 | 6670 | 2136 | 122 | 185 of 303 |
| 768 | 13439 | 4399 | 6687 | 2250 | 103 | 223 of 364 |
| 896 | 13420 | 4456 | 6636 | 2210 | 118 | 264 of 429 |
| 1024 | 13543 | 4378 | 6848 | 2214 | 103 | 301 of 492 (about 1880) |

Every class carries an age tail: 16..18% of its live values (2.1..2.3k in
the 640..1024 B classes, 1.2k at 512 B) have not been selected for 60 s
or more at the end of a churn that turns the rest over in under a second,
and 57..122 per class predate the delete phase altogether. The tail's
size equals the residue in the sparse slabs (1024 B: 2317 old values,
about 1880 in the 301 sparse slabs plus the rest inside dense ones; 512
B: 1203 against about 970), and its density is 4.7 old values per slab
of the 1024 B class (2317 / 492): a slab without one is a 1% event
(Poisson), which is the 4..6 slabs per class that did drain after the
plateau. These values were placed by the LIFO pop into whatever slot had
just been freed, i.e. into a random slab, and the compacting cache
cannot move a value that is not freed. The stream's shape is the
2026-09-17 finding (`2026-09-17-aarch64-gce-compacting-cache-soak3.md`,
"about 2200 of the
12921 live values predate the delete phase"); at 300 s the pre-delete
part is 100 per class and the rest of the tail is delete-phase values the
stream created and left, so the count is a property of the protocol, not
of the allocator (jemalloc's slabs hold the same keys). What differs
between the allocators is what the third defrag cycle did to them:
jemalloc's 77,938 moves at t=228 took the below-average slabs' values,
old ones included, into dense slabs, after which its sparse slabs held
young values only and could drain; challoc's got no third cycle (4f) and
its old values stayed at 4.7 per slab. 4f's diagnostic with the constant
at 80 MB gave challoc the third cycle and ended the same churn at 100 and
385 slabs in the 512 and 1024 B classes against jemalloc's stock 103 and
389.

## 4. Verdict

- The compacting cache is not failing at 64..128-region slabs. Its gate
  is open and never declined (i, iv), no refill runs to fight it (ii),
  and the probe drains a 469-slab class at 128, 102 and 64 regions to
  214..216 slabs at 97% occupancy within 2 s, the same figure as at 64
  regions in Phase 1 (iii). No mechanism of design 6.4 needs a change of
  units, origin or destination at this geometry, and no change is made:
  `src/` is untouched, the fast paths are untouched, so `quick`,
  `quick-perf` and the scorecard are not run and the 4 KiB soak is
  unchanged by construction.
- The mechanism, named: the residue after the last defrag cycle is the
  age tail of memtier's fixed-seed key stream, 16..18% of a class's live
  values that the churn does not free for the remaining 60..150 s (2.1k
  to 2.3k per 640..1024 B class, 1.2k at 512 B), placed LIFO into random
  slabs at 4..5 per 64 KiB slab so that no slab of the class is free of
  them. The schedule packs the other 82..84% into the lowest slabs within
  30..45 s (the 2/8..7/8 buckets empty into full, the 1/8 into 0/8) and
  `curslabs` sits at the floor an address gradient with immovable
  values reaches; the probe with the same density ends within 1% of the
  floor (468..469 against 464..465 of 469). Only a defragmenter cycle
  moves those values, and the missing cycle is 4f's finding.
- A policy that would beat the gradient, claiming the rebuild's block
  from the fullest partial slab so the churned values fill the old
  values' slabs instead of their own, gains 19..20% of a class's slabs
  only when the old values are sparse enough to leave slabs without one
  (about one per slab: probe 361 against a floor of 291). Neither soak
  is in that regime: at 64 KiB pages the density is 4..5 per slab and
  the floor is 99% of the slabs; at 4 KiB pages the 512..1024 B classes
  have 4..25-region slabs and the soak already passes at -1.5% (Phase 1
  acceptance), so there is no headroom to buy there. It would also cost a
  scan of the partial set per rebuild (every 512 pops) and is not built.
- Acceptance of task 4g: the 64 KiB post-delete RSS is **not** within 10%
  of bundled jemalloc (+17.6% at 300 s, +20.5% at 600 s, +20.0% at 1200
  s), and the residue is attributed to Valkey's
  `active-defrag-ignore-bytes` constant alone: the third cycle it grants
  jemalloc's geometry (4f) is the only mechanism that moves the values
  the churn keeps, and with it challoc lands at +1.9% (4f's diagnostic).
  The 4 KiB soak is unchanged (no library change). `design.md` 13.3 is
  corrected on where direct claims land (the lowest partial slab, as
  jemalloc's `slabcur`, not the fullest). The probe's `PIN` parameter
  stays for the quick tier's future use (its default 0 is the existing
  row).

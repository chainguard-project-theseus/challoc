# The 64 KiB post-delete `active` gap: what Valkey's defragmenter sees and where it stops (Phase 2.5 task 4f)

Date 2026-09-19, 16:26..17:10 UTC. Tree e7c8551 (no `src/` change in this
task). Host: a fresh throwaway `challoc-matrix-64k-arm64` from
`MATRIX_MACHINE=c4a-standard-16 MATRIX_PMU=1 bench/gce/challoc-bench matrix
lab 64k-arm64`: Ubuntu 24.04.5, `6.8.0-139-generic-64k`, page 65536,
Neoverse-V2, 16 vCPU, `us-east4-a`, the dev image built on the VM, Valkey
9.1.2 from `make valkey STAGE=build` (bundled jemalloc 5.3.0 at `LG_PAGE
16` built by the soak script). Deleted at the end (`matrix clean`).

The question (plan task 4f): at the same 64 KiB slab geometry (512 B at
128 regions, 1024 B at 64), 4e's soak ends the delete phase with challoc at
27..31% more slabs than bundled jemalloc at 45% utilisation against 55%,
278k defragmenter hits against 361k, and `active` 157 against 129 MiB
(post-delete RSS +20.0%, the criterion is 10%). Which decision differs: the
inputs challoc gives Valkey's decision rule, where the defragmenter's
`MALLOCX_TCACHE_NONE` copies land, or where the pass stops.

## Method

A shortened soak, `DURATION=600 INTERVAL=1 OCCUPANCY=1 MS_INTERVAL=5 make
valkey-soak` (300 s write, 300 s delete, 60 s settle, both allocators in one
invocation, INFO sampled every second, `MEMORY MALLOC-STATS` every 5 s),
with `bench/probes/valkey-defrag-sidecar.sh` attached from the VM host: it
switches the server to `loglevel verbose` so `defrag.c` writes its own
decision lines ("Starting active defrag, frag=X%, frag_bytes=N, cpu=P%" when
a cycle begins, "Active defrag done in Nms, reallocated=N, frag=X%,
frag_bytes=N" when it ends) and samples INFO once a second with
`allocator_frag_bytes` and the key count. The first attempt of the sidecar
waited on `kill -0` against the root-owned server and never attached, so
the challoc half has the soak's own 1 s samples and 5 s stats prints but no
log lines; the jemalloc half has all three; the diagnostic run at the end
has all three under challoc. Files: `2026-09-19-aarch64-64k-defrag-attribution-{challoc,jemalloc}.csv`
(the 1 s samples), `...-jemalloc-sidecar.csv`, `...-jemalloc-defrag.log`
(the server's defrag lines), `...-{challoc,jemalloc}-mstats-t<sec>.txt`
(stats prints after cycle 2, at the plateau, at the end). `.tmp-4f-frag.py`
computed `frag_smallbins` from the prints exactly as `allocator_defrag.c`
does. Positive = challoc worse.

The soak itself, for the record (the acceptance protocol is the 20-minute
one; this is the 10-minute attribution run):

| measure | challoc | jemalloc | challoc vs jemalloc | 4e (20 min) |
|---|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 1040712 | 1009772 | -3.0% | +1.0% |
| write: memtier req/s | 1066918 | 1056885 | -0.9% | +2.1% |
| delete: valkey-benchmark req/s | 1517644 | 1521358 | +0.2% | +3.4% |
| delete: memtier req/s | 1369850 | 1369746 | -0.0% | +3.4% |
| peak RSS MiB (VmHWM) | 594.0 | 584.0 | +1.7% | +0.7% |
| RSS MiB after settle | 172.2 | 142.9 | **+20.5%** | +20.0% |
| `allocator_active` MiB after settle | 159.6 | 137.9 | +21.7 MiB | +28.8 MiB |
| `allocator_allocated` MiB after settle | 66.1 | 64.1 | | |
| `frag_smallbins` MiB after settle (Valkey's sum over the bins) | 85.3 | 73.7 | | 84.4 vs 66.9 |
| active_defrag_hits | 297006 | 373907 | +25.9% | +29.7% |
| defrag cycles | 2 | 3 | | |
| keys after settle | 106960 | 106353 | | |

## What Valkey 9.1.2 reads, and what challoc answers (question a)

`allocator_defrag.c` (the checkout in `target/valkey/src-9.1.2`):
`allocatorShouldDefrag(ptr)` calls `experimental.utilization.batch_query`
for one pointer and uses only `nfree`, `nregs` and `size` (`nalloced =
nregs - nfree`; `size / nregs` maps to the bin). The verbose query's
`bin_nfree`, `bin_nregs` and `slabcur_addr` are not read anywhere in
9.1.2 (that was Redis's `je_get_defrag_hint`). The bin-wide figures come
from `stats.arenas.4096.bins.<j>.{curregs,curslabs,nonfull_slabs}` after
an `epoch`, refreshed by `allocatorDefragGetFragSmallbins()` on every
`monitorActiveDefrag` tick (100 ms) and at each cycle end. The rule
(`makeDefragDecision`): with `full = curslabs - nonfull_slabs` and
`allocated_nonfull = curregs - full * nregs`, move the object unless the
slab is full, or `nonfull_slabs < 2`, or `nalloced * nonfull_slabs >
1.125 * allocated_nonfull` (the slab is at least 12.5% above the average
utilisation of the non-full slabs); a slab under 12.5% is always moved.
The stop: `updateDefragCpuPercent` starts a cycle only if `frag_pct =
frag_smallbins / allocated >= active-defrag-threshold-lower` (10%) and
`frag_smallbins >= active-defrag-ignore-bytes` (100 MB = 104,857,600); a
running cycle is never cut short, and `endDefragCycle` immediately asks
again. `frag_smallbins = sum over bins of (nregs * curslabs - curregs) *
size`.

challoc's answers, read against jemalloc's:

- `nfree` is `nslots - live` from the slab descriptor (`SlabDesc::nfree`),
  `nregs` the class's slot count, `size = nregs * size` (design 13.4, so
  the quotient is the bin size). `live` counts slots out of the bitmap,
  i.e. in the program or in a thread cache, exactly as jemalloc's slab
  `nfree` counts regions in the program or in a tcache. Cached slots of
  these classes are at most the cache cap (`ladder`: 256 for 512 B, 128
  for 1024 B, one heap: `Arenas: 1 (64 automatic)` in the print), against
  jemalloc's `tcache_nslots_small_max` 200 / 128: the same order, one to
  two slabs' worth per class.
- `curregs` is `claimed - released` (`ClassState::bin_stats`), the same
  quantity; `curslabs` is `committed - empty - purged`, slabs that can hold
  live slots, as jemalloc's `curslabs` (it frees a slab the moment it
  empties). `Empty` slabs are not counted (and are 0 throughout this run).
- `nonfull_slabs` is the partial set's size. jemalloc's excludes `slabcur`
  (`arena_bin_slabs_nonfull_tryget` decrements it when a slab becomes
  slabcur, `arena_bin_lower_slab` re-inserts the old one), so for the same
  heap jemalloc reports one fewer non-full slab and an `allocated_nonfull`
  smaller by `nregs - nalloced(slabcur)`: a lower average, so jemalloc's
  rule skips slightly more objects than challoc's would. With 60..560
  non-full slabs in these classes the difference is under 1% of the
  threshold and points the other way from the gap.
- The stats prints agree with the 1 s INFO: e.g. at t=371 (after cycle 2)
  challoc's 512 B bin has `curslabs 132, nonfull 105, curregs 13044`, the
  print's utilisation histogram `[6, 1, 6, 8, 0, 5, 67, 1, 38]` (eighths,
  the last bucket full), and `live != bits 0`, `nonfull not in partial set
  0`: the figures are exact.

So the decision rule sees the same picture for the same heap shape. The
one place challoc's inputs differ in kind, `NBINS`: challoc's bins end at
4096 B (31 bins in the print; design 13.4), jemalloc's at `LG_PAGE 16`
run to 163840 B (44 bins with activity), so jemalloc's `frag_smallbins`
counts waste in the 5..160 KiB classes that challoc reports as large; in
this workload that is 1.0..1.4 MiB of the totals below (`<=4096` column
of `.tmp-4f-frag.py`), not the term.

## Where the copies land (question b)

`allocatorDefragAlloc` is `je_mallocx(size, MALLOCX_TCACHE_NONE)`, which
challoc routes to `arena::alloc_direct` -> `Shared::alloc_direct` ->
`ClassState::claim(out[1])` -> `plan(want = 1)`: the lowest-address slab
of the arena's partial set (`partial.map.lowest_set()`), the same slab a
cache refill or a compaction rebuild takes from next. jemalloc's copy goes
to `bin->slabcur`, which `arena_bin_lower_slab` keeps at the
oldest/lowest non-full slab by `(sn, address)` and which
`arena_bin_slabs_nonfull_tryget` replaces with the next lowest when it
fills. Both fill the lowest non-full slab first; neither picks the
fullest, and Valkey's rule does not need them to (it moves out of
below-average slabs; the destination only has to be a slab that is not
one of those, which the lowest non-full slab is once the low slabs have
been packed). The data agrees: at t=371 both have their low slabs full
(challoc 38 full + 67 at 75..87%; jemalloc 2 full + 150 at avg 102/128)
and the same waste in the two classes with identical geometry (512 B:
1.9 vs 1.9 MiB; 1024 B: 7.3 vs 5.5 MiB). Placement is not the term
either, as the Phase 1 acceptance found at 4 KiB.

## Where the pass stops (question c): the mechanism

The delete phase is two bursts: valkey-benchmark's `DEL key:__rand_int__`
(its own zero-padded keyspace, the 640 B class) takes `used_memory` from
566 to 239 MiB in 3 s, then memtier's `DEL:SET 4:1` over its keys takes
it from 239 to 65 MiB in 30 s (t=358..388 challoc, t=363..391 jemalloc)
and then churns the ~107k survivors at 1.37M ops/s for the remaining 210
s. Every defrag cycle is 0.3..1 s long against that, so the number of
cycles is the number of times `frag_smallbins` crosses 100 MB from below
while deletions are still adding waste. `allocator_frag_bytes` (`active -
allocated`, 1 s INFO) and the cycle lines:

| | challoc | jemalloc (server log) |
|---|---|---|
| cycle 1 | t=344..346, 153,852 moves; `frag_bytes` 225 -> 31 MiB, used 566 -> 250 | t=349.1..350.1, 952 ms, 163,211 moves; 301 -> 10.6 MB, used 565 -> 252 |
| between | frag climbs 22 -> 97.6 MiB (t=365) -> 105.3 (t=366) | 17.5 -> 92.1 MiB (t=368) -> 101.0 (t=369) |
| cycle 2 | t=366..368, 143,154 moves; 111 -> **35.8 MiB** at used 137 MiB | t=369.1..369.9, 806 ms, 136,551 moves; 105.8 MB -> **59.0 MB** at used 152 MiB |
| after | frag climbs to 97.7 MiB at t=392 (used 65) and stays: **never reaches 100 MB again** | frag climbs 58.7 -> 100.1 MiB by t=378 (used 106 MiB) |
| cycle 3 | none | t=378.1..378.4, 294 ms, 74,145 moves; 104.9 -> 55.6 MB |
| after | | frag climbs to 83.7 MiB at t=391 (used 65), never reaches 100 MB again |
| `frag_smallbins` (5 s prints) at the plateau, t=396 | 89.6 MiB | 83.9 MiB |
| after 210 s of churn, t=606 | 85.3 MiB | 74.6 MiB |

Both defragmenters stop for the same reason, `frag_smallbins <
active-defrag-ignore-bytes`, and both second cycles start within 6% of
the threshold (challoc's at 105.3 MiB of `frag_bytes`, jemalloc's at 105.8
MB). The gap is cycle 3, which jemalloc gets because its heap re-crosses
100 MB during the last 40 MiB of deletion and challoc's does not. Why:
challoc's second cycle leaves less waste behind, 35.8 MiB of `frag_bytes`
(40.6 MiB of `frag_smallbins` at t=371) against jemalloc's 59.0 MB (62.4
MiB at t=371), and the rest of the deletion (137 -> 65 MiB under challoc,
152 -> 65 under jemalloc) adds 0.85..0.9 MiB of waste per MiB freed under
both, so challoc plateaus at 97.7 MiB (`frag_smallbins` 89.6), 2.3% under
the constant, while jemalloc passes it at used 106 MiB with 40 MiB of
deletion still to come. Where the 22 MiB of extra post-cycle-2 waste sits
(per-bin `frag_smallbins` at t=371, MiB):

| class | challoc slab | waste | jemalloc slab | waste |
|---:|---|---:|---|---:|
| 320 | 64 KiB (204) | 1.1 | 320 KiB (1024) | 2.6 |
| 384 | 64 KiB (170) | 1.3 | 192 KiB (512) | 2.1 |
| 448 | 64 KiB (146) | 1.7 | 448 KiB (1024) | 4.3 |
| 512 | 64 KiB (128) | 1.9 | 64 KiB (128) | 1.9 |
| 640 | 64 KiB (102) | 6.5 | 320 KiB (512) | 13.7 |
| 768 | 64 KiB (85) | 5.2 | 192 KiB (256) | 8.2 |
| 896 | 64 KiB (73) | 6.2 | 448 KiB (512) | 10.3 |
| 1024 | 64 KiB (64) | 7.3 | 64 KiB (64) | 5.5 |
| all bins | | 40.6 | | 62.4 |

Same waste where the slabs are the same (512, 1024), two to three times
jemalloc's waste where jemalloc's slab is 192..448 KiB: a slab of 512
regions with a few survivors keeps 320 KiB resident where challoc's
64 KiB slabs empty and purge (`purged` 5,067 slabs in the 640 B class by
t=396). The threshold is absolute and the cycles are short, so the
allocator that wastes less after the second pass is the one that does not
get a third, and the third pass is what compacts the survivors of the last
40 MiB of deletion: 74k moves that end with jemalloc's 512 B class at 103
slabs (84 non-full) against challoc's 127 (92 non-full), and the 1024 B
class at 389 against 495. That is the 4e reading (119 vs 94 and 489 vs
373 slabs) and the 28.8 / 21.7 MiB of `active`.

The mechanism, named: `active-defrag-ignore-bytes` (100 MB, stock) is an
absolute stop evaluated between sub-second cycles against a deletion that
takes 35 s; whether a further cycle runs depends on the heap's waste
re-crossing the constant, which jemalloc's 192..448 KiB slabs of the
320..896 B classes do and challoc's 64 KiB slabs do not. No input challoc
gives Valkey is wrong or unhelpful for the rule, and the copies land where
jemalloc's do.

### The second-order term: what happens after the last cycle

After the last cycle both heaps are shaped by memtier's balanced churn
(SET creates a key for every hit DEL, so allocations equal frees; ~14k
allocations per second in the 512 B class, each value living ~0.5 s).
jemalloc's `active` drifts down 149.5 -> 137.9 MiB between t=392 and the
end (its tcache random-walks between empty, refilled from the lowest
non-full slab, and full, flushing its oldest entries, so about 1% of the
churn migrates downward and slabs whose remaining regions are all cached
are freed: 512 B `curslabs` 107 -> 103, 1024 B 420 -> 389). challoc's
drifts 163.6 -> 159.6 MiB: the compacting cache (design 6.4) claims 1 in 8
allocations from the lowest partial slab (`compacted` +356k in the 512 B
class over t=396..601, 1.7k/s) and flushes the highest-address half of the
cache, yet `curslabs` stays 128 -> 127 and the histogram only moves slabs
from the 12.5..25% bucket to the under-12.5% one (50 -> 14 and 33 -> 64)
and from 87..100% to full (30 -> 48): the claims fill holes in the dense
low slabs and the releases come back out of the same slabs, while the
sparse slabs (78 of 127 under 25% at the end, holding ~860 objects) keep
their last objects. 4 against 12 MiB over 210 s, a term of the same sign
as the gap but a fifth of it, and a cache-policy question for a task of
its own (the change would be on the `pop`/`flush` paths of `heap.rs`,
which need their per-operation attribution first); it is not what task 4f
asked about.

## Diagnostic: what a third cycle does for challoc

The same shortened soak, challoc only, with the sidecar issuing `CONFIG
SET active-defrag-ignore-bytes 83886080` (80 MB) at attach, everything
else stock (`...-ib80-challoc-{defrag.log,sidecar.csv,mstats-t666.txt}`).
This is a diagnostic of the mechanism, not a configuration challoc ships
or the contrib patch sets. The server's own lines:

| cycle | start | `frag_bytes` at start | length | moves | `frag_bytes` at end |
|---|---|---:|---:|---:|---:|
| 1 | t=347.5 (the valkey-benchmark burst) | 96.8 MB | 1224 ms | 155,181 | 13.3 MB |
| 2 | t=367.3 (used 165 MiB) | 84.3 MB | 1270 ms | 150,548 | 23.4 MB |
| 3 | t=379.6 (used 96 MiB) | 84.2 MB | 352 ms | 74,080 | 31.8 MB |

Three cycles, 379,809 moves (jemalloc's stock run: 373,907), the third
again starting within 5% of the (lowered) constant. Result after the
settle: RSS 145.6 MiB (VmRSS 140.2), `active` 124.2, `allocated` 66.5,
`frag_smallbins` 50.2 MiB, `allocator_frag_ratio` 1.76; 512 B class 100
slabs (55 non-full, histogram `[37, 10, 2, 0, 1, 0, 0, 5, 45]`), 1024 B
class 385 (jemalloc's stock run: 103 and 389). Against jemalloc's stock
142.9 MiB that is **+1.9%**, inside the 10% band, with the same third
cycle jemalloc got for free from its geometry; the churn afterwards moved
`active` 128.1 -> 124.2 MiB (the second-order term again, -4 MiB). Peak
RSS 587.8 MiB, throughput unchanged. So the whole 4f gap is one
defragmenter cycle that Valkey's absolute constant grants to the
allocator with more waste after the previous one.

## Verdict

- (a) Valkey's decision reads `nfree`/`nregs`/`size` from `batch_query`
  and `curregs`/`curslabs`/`nonfull_slabs` from the bin stats; challoc's
  are exact and match jemalloc's semantics except that `nonfull_slabs`
  includes the current slab (jemalloc's excludes `slabcur`), a one-slab
  difference that errs toward moving more, not less. `bin_nfree`,
  `bin_nregs` and `slabcur_addr` are not read by 9.1.2.
- (b) `MALLOCX_TCACHE_NONE` copies land in the lowest-address partial slab
  under challoc and in `slabcur`, the lowest non-full slab by (sn,
  address), under jemalloc: the same placement, and the same waste after
  the second pass in the classes with the same slab.
- (c) Both passes stop on `active-defrag-ignore-bytes`; jemalloc gets a
  third sub-second cycle because the 192..448 KiB slabs of its 320..896 B
  classes carry 22 MiB more waste after the second one, which re-crosses
  the 100 MB constant as the last 40 MiB are deleted; challoc's 64 KiB
  slabs purge and its waste plateaus at 97.7 MiB of `frag_bytes` / 89.6
  MiB of `frag_smallbins`, under the constant. The 74k moves of that
  third cycle are the 27..31% more slabs and the +20% post-delete RSS;
  the diagnostic with the constant at 80 MB gives challoc the same three
  cycles and lands at +1.9%.
- No `src/` change follows from this: there is no challoc-side answer to
  correct, and the rule since 2026-09-17 forbids a `USE_CHALLOC` constant
  that exists to pass the soak RSS criterion. The plan's acceptance for 4f
  (post-delete RSS within 10% of bundled jemalloc at 64 KiB) is **not
  met**: +20.5% in this run, +20.0% in 4e's; peak RSS +1.7% (within 5%),
  throughput at parity. Recorded under Notes for the human in PROGRESS.md.

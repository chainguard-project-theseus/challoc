# challoc compacting cache, soak 3 diagnosis, 2026-09-17, aarch64 (GCE reference VM, us-east4-a)

Phase 1 acceptance step 3 (c): why the compacting cache that drains the
churn probe in two seconds (`2026-09-17-aarch64-gce-compacting-cache.md`)
left Valkey's 640..1024-byte classes at ~27% occupancy in 358 non-full
slabs in soak 2. This file records the instrumented diagnostic runs and
their reading. Zone `us-east4-a`; VM numbers are never compared with Mac
numbers.

- host: challoc-bench-arm64 (c4a-standard-16, Google Axion, Neoverse V2),
  16 vCPUs, Ubuntu 24.04 kernel 7.0.0-1011-gcp, 4 KiB pages; container:
  Wolfi (rustc 1.98.1, gcc 15.3.0); Valkey 9.1.2 built by
  `contrib/valkey/build.sh` against the working tree, dynamically linked
  against `libchalloc.so` (quantum8); defaults: `io-threads 1`,
  `lazyfree-lazy-user-del yes` (Valkey 9; strings have free effort 1 and
  are still freed synchronously on the main thread),
  `active-defrag-ignore-bytes 100mb`, `active-defrag-threshold-lower 10`.
- instrumentation (commit 472cfad): five per-class counters,
  all written on cold paths and printed as the last columns of the bins
  table of `malloc_stats_print` (so `MEMORY MALLOC-STATS` shows them):
  `refilled` (slots claimed by cache refills, `ClassState::refilled`),
  `compacted` (slots claimed by compaction rebuilds), `c_on` / `c_off`
  (caches that entered / left compact mode), `gated` (probes of a
  fragmented class that the refill-rate gate declined).
- method: `bench/probes/soak-occupancy.sh` (the soak of `bench/valkey-soak.sh`
  plus `MEMORY MALLOC-STATS` every 20 s), challoc only, `DURATION=600
  SETTLE=60` for the diagnostic runs; the trajectory rows below are
  `curregs/curslabs/nonfull` of arena 0 and the counters, `t` seconds from
  the server's start (write phase to 300 s, delete phase 300..600 s,
  settle to 660 s).

## Run 1: the counters (challoc only, DURATION=600), `soak-results-it14`

Per-class rows are `curregs/curslabs/nonfull` with `refilled`, `compacted`,
`c_on/c_off`, `gated` (cumulative). Load 0.0 at the start.

| t | RSS MiB | frag | 640 | 768 | 896 | 1024 |
|---:|---:|---:|---|---|---|---|
| 300 | 568.7 | 1.00 | 548340/5378/85 r553088 c0 | 48437/572/45 r53184 c0 | 48080/661/30 r52288 c0 | 48300/759/143 r52992 c0 |
| 320 | 568.6 | 1.00 | 524249/5378/5038 r553664 c0 | 48407/572/102 r53888 c0 | 47969/661/158 r52800 c0 | 48521/759/4 r53760 c0 |
| 340 | 249.4 | 1.37 | 35744/555/537 c21760 on1 | 35654/570/485 c68064 on1 | 35496/658/563 c19264 on1 | 35675/758/660 c18784 on1 |
| 360 | 163.6 | 1.89 | 15095/319/226 c102144 | 15343/348/243 c366144 | 15172/404/272 c99712 | 15457/472/318 c100544 |
| 401 | 150.9 | 2.13 | 13399/300/195 c216864 | 13596/345/231 c918016 | 13545/398/256 c212672 | 13580/460/314 c214688 |
| 461 | 150.1 | 2.14 | 13345/297/235 c386176 | 13425/345/213 c1742080 | 13276/397/274 c380064 | 13543/460/330 c384064 |
| 521 | 150.0 | 2.13 | 13369/296/214 c555168 | 13411/345/230 c2560928 | 13345/397/282 c549152 | 13487/460/308 c552896 |
| 581 | 149.8 | 2.14 | 13275/296/220 c753568 | 13436/345/230 c3405472 | 13329/397/264 c747936 | 13525/460/286 c752672 |
| 641 | 149.8 | 2.14 | 13331/296/242 c907840 | 13381/345/237 c4094016 | 13308/397/265 c899840 | 13523/460/339 c907584 |
| 681 (end) | 149.8 | 2.14 | 13363/296/234 c907872 | 13381/345/237 c4094016 | 13308/397/265 c899840 | 13523/460/339 c907584 |

`refilled` does not move after t = 320 in any of the four classes (no
cache refill in the whole delete phase), `c_on` = 1 and `c_off` = 0 for
each class from t = 340 to the end (one cache, the main thread's, entered
compact mode once and never left), `gated` = 0 (the refill-rate gate never
declined a probe of a fragmented class). VmHWM 582484 kB, VmRSS 153564 kB
at the end.

Reading: candidates (2), (3) and (4) of soak 2 are out. The schedule runs
for the whole delete phase at 2.5..3k rebuild-claimed slots per second per
class (907k slots for class 1024 in 320 s, 67 times its live count; class
768 sees 4.5x the traffic of the others) while the slab count of every
class is constant to the slab from t = 401 (1024: 460 slabs holding a
constant 29440 slots; 640: 296 x 102 = 30192; 768: 345 x 85 = 29325; 896:
397 x 73 = 28981: the same 2.2x the live count in every class). The
full-slab count of class 1024 oscillates (146, 130, 152, 174, 121 at the
sampled instants) so live data does move into full low slabs; the high
slabs do not empty.

## Miniature protocol, `activedefrag` yes and no (`.tmp-it14-phases.sh`)

One server per setting: valkey-benchmark SET -d 512 (10 M requests over
500k keys), memtier SET 16..1024 B for 20 s, valkey-benchmark DEL (10 M),
memtier DEL:SET 4:1 for 40 s, DEL again, DEL:SET again. Class 1024
`curregs/curslabs/nonfull`:

| step | activedefrag yes | activedefrag no |
|---|---|---|
| after write | 39324/615/2 | 38748/606/2 |
| after vb DEL | 39315/615/1 (defrag hits 145110) | 38748/606/2 |
| after churn 1 (40 s) | 11729/350/227 (c 162368) | 11710/514/399 (c 159392) |
| after churn 2 (40 s) | 11764/199/89 (c 335392), RSS 85.3 MiB | 11715/201/81 (c 328704), RSS 96.0 MiB |

Reading: the same generators, the same allocator, the same compaction rate,
and the class compacts to within 8% of its 184-slab ideal in 80 s of churn
with or without the defragmenter, so candidate (5) is out too and the
valkey-benchmark runs are not the trigger. What the miniature lacks is
the soak's 300 s write phase.

## Run 2: per-slab occupancy histogram (challoc only, DURATION=600), `soak-results-it14b`

Same protocol as run 1 with the `slab occupancy` section of the stats
report (slabs per eighth of `nslots`, last bucket full; `live!=bits` and
`nonfull not in partial set` were 0 in every sample). Class 1024:

| t | curregs/curslabs/nonfull | compacted | histogram 0/8 .. 7/8, full |
|---:|---|---:|---|
| 300 | 47627/747/79 | 0 | [1, 0, 0, 0, 0, 0, 0, 71, 675] |
| 360 | 15762/458/317 | 92800 | [10, 78, 108, 105, 5, 0, 0, 3, 149] |
| 380 | 13489/456/356 | 118080 | [64, 167, 64, 1, 1, 0, 1, 57, 101] |
| 420 | 13495/450/344 | 244928 | [153, 110, 2, 1, 0, 0, 1, 72, 111] |
| 440 | 13382/450/296 | 310784 | [156, 107, 2, 3, 1, 1, 1, 24, 155] |
| 600 | 13400/448/339 | 758112 | [165, 96, 1, 1, 0, 0, 1, 75, 109] |
| 640 | 13433/448/314 | 885856 | [166, 95, 1, 1, 0, 0, 0, 51, 134] |
| 700 (end) | 13433/448/314 | 885856 | [166, 95, 1, 1, 0, 0, 0, 51, 134] |

RSS after settle 146.8 MiB, VmHWM 565.2 MiB, allocator_frag_ratio 2.11,
active_defrag_hits 272941, 103600 keys.

Reading: the compaction does what it is designed to do to the bulk of the
residue: by t = 420 the 2/8..6/8 buckets are empty and the low slabs are
full or nearly so. What is left is bimodal: ~260 slabs holding 1..15 live
regions each (the 0/8 and 1/8 buckets, 166 + 95 at the end) that drain
into the 0/8 bucket and then stop. A slab with a handful of survivors that
does not empty in 280 s of a churn that replaces every value in under a
second holds objects the churn never frees: the residue is pinned
objects, not a cache effect.

## Run 3: lifetime tracer (challoc only, DURATION=600), `soak-results-it14c`

Same protocol with `.tmp-it14-trace.c` preloaded into valkey-server: an
`LD_PRELOAD` shim in front of `libchalloc.so` that interposes both the
libc and the `je_*` entry points, records every allocation of 897..1024
bytes made by the first thread that makes one (the main thread) with its
ordinal in that class's allocation stream, and reports every 5 s the
lifetime distribution (in class allocations) and the 12 oldest live
allocations with 48 bytes of their contents. Summary: RSS after settle
139.9 MiB (VmHWM 557.7), allocator_frag_ratio 2.07, active_defrag_hits
264757, 97638 keys; class 1024 at t = 480: 12888/421/264, histogram
[174, 66, 0, 1, 0, 0, 0, 19, 161].

Tracer at the end (23.8 M class allocations, 23.79 M frees on the same
thread, no free from another thread, nothing untracked):

- lifetime distribution: 97.5% of the class's allocations are freed
  within 4096 later allocations, and the same 97.4..97.5% at every
  ordinal residue mod 8, i.e. the scheduled pops (one ordinal residue) do
  not receive a different population; the tail is 2^21..2^22 allocations
  for the write-phase overwrites, 375 allocations beyond that.
- the 12 oldest live allocations are all memtier values (`sds` type-16
  header, then `xxxx...`) with ordinals 12389623..12390740: allocated in
  one burst of about a thousand class allocations (about 8000 SETs), the
  first allocations of the fifth memtier write run (t = 253; four runs of
  about 3.1 M class allocations precede it), and never freed through the
  11.4 M class allocations that followed, including four DEL:SET 4:1 runs
  that turn every other value over in well under a second (the delete rate
  is 1.9 per key per second over the 500k keyspace).
- the keyspace at t = 540: 97964 keys, all `key:<n>` with n in
  1..499995 (memtier's format), no valkey-benchmark key left, expired 0,
  evicted 0, lazyfree pending 0.

Reading: the residue that the compacting cache cannot remove is a
population of long-lived values allocated in one burst during the write
phase, one or a few per slab across the address range that the write
phase covered. Whether the churn leaves them because their keys are never
selected or because nothing references them is settled by the
reproduction run below.

## Reproduction attempt (`.tmp-it14-repro.sh`) and run 4 with the keyspace observer

The soak's write phase in miniature (5 x [valkey-benchmark SET -d 512 20 M,
memtier SET 30 s], then vb DEL + DEL:SET 30 s twice, then a third DEL:SET
run during which the keyspace was scanned twice 30 s apart) did *not*
reproduce the frozen residue: class 1024 went 709 -> 425 -> 422 -> 273
slabs over the three churn runs and every live value at the end was
younger than 1.3 M class allocations. Two facts from it stand anyway: the
memtier key count is constant at 354899 from the second write run on
(memtier's fixed-seed key stream covers 71% of the 500k range and the same
keys every run), and 44173 of the ~100k live keys survived 30 s of the
DEL:SET run (a uniform 900k DEL/s over 500k keys would leave none).

Run 4 (`soak-results-it14d`, same protocol as run 3, tracer preloaded)
froze again: class 1024 at t = 500 is 12936/432/291 with histogram
[181, 67, 2, 0, 0, 6, 1, 28, 147]. An observer on the VM host scanned the
keyspace through the container 150 s into the delete phase and again 30 s
later (the window spans a memtier DEL:SET run at 1.1 M ops/s and part of a
valkey-benchmark DEL run):

- 100142 and 99727 keys, 81809 present in both scans, spread evenly over
  the 500k range (8038..8342 per 50k);
- of the first 2000 common keys, `OBJECT IDLETIME` is under 60 s for 1542,
  60..200 s for 58 and 200 s or more for 400; the oldest are idle 493..494 s
  (`key:110463`, `key:110624`, `key:110661`, `key:110701`, `key:110832`,
  with STRLEN 654..998), i.e. last touched during the first memtier write
  run, about 36 s after the server started; 250 of the 2000 have values in
  the 1024 class;
- the tracer's live class-1024 values by allocation ordinal (buckets of
  500k) at t = 500: `0:4 6:242 12:62 30:108 36:786 37:1026 40:57 42:47
  43:53 44:4029 45:6507`: about 2200 of the 12921 live values predate the
  delete phase's churn and come from four distinct bursts in the write
  phase, the rest are the churn's own recent values.

Run 4 summary: RSS after settle 145.6 MiB (VmHWM 560.9, VmRSS 149144 kB
at the end), allocator_frag_ratio 2.10, active_defrag_hits 260577, 99866
keys; final bins `640: 12823/283/212 c719040`, `768: 12776/320/194
c3291392`, `896: 12616/371/251 c713504`, `1024: 12963/432/323 c717952`,
class 1024 histogram [181, 68, 2, 0, 0, 0, 1, 70, 110], `c_on` 1 and
`c_off` 0 and `gated` 0 in every class.

## Conclusion

The compacting cache is not the cause. Under Valkey it does everything the
design asks: the gate opens once per class as soon as the delete phase
leaves holes, never closes and is never declined, no refill runs, rebuilds
claim 3k low slots per second per class, the 2/8..6/8 occupancy buckets
are emptied within 60 s and the low slabs are full. The residue that
remains is 1..15 live values per slab in some 260 slabs per class, and
those values are live keys that the load generators never select again:
memtier's random key pattern replays a fixed sequence per run whose
coverage of the keyspace is partial and time-structured, so a few percent
of the keys written during the write phase are never deleted or
overwritten during the delete phase (idle times of 200..494 s in a churn
that turns the other keys over in under a second). Every one of the five
candidate causes listed after soak 2 is ruled out by the counters; the
candidate not on that list, pinned objects, is confirmed by the
tracer and the observer.

What this costs challoc and not jemalloc is slab geometry, not cache
policy: a pinned 1 KiB value holds a 64 KiB slab (64 regions) resident,
against jemalloc's 4..32-region slabs for the 512..1024-byte bins, and
the pinned values are scattered over the address range because the cache
served the bursts that created them from recently freed slots. In run 4
the four classes hold about 1400 slabs (90 MiB) for 51.4k live values
(37 MiB) whose ideal is about 650 slabs; with the histogram's 0/8 and 1/8
buckets counted as pinned that is roughly 45 MiB of slabs pinned by under
8 MiB of values. The RSS after settle in the four 600 s runs was 149.8,
146.8, 139.9 and 145.6 MiB against jemalloc's 131.2 MiB in the 20-minute
protocol (+6.6% to +14%); the run-to-run spread is the pinned population
of that run, not the cache.

Next step (allocator side, docs/review-2026-09-17.md P6, prepared for in
step 2 by `layout::SMALL_SLAB_BELOW` / `LG_SMALL_SLAB` / `lg_slab_for`):
smaller slabs for the 512..1024-byte classes so a pinned value holds 16 KiB
rather than 64 KiB, with the larson/xmalloc-test/alloc-test rows on both
VMs (step 2 measured the two-regime form at larson -2.2% on arm64 and
-3.8% on x86_64 before the compacting cache existed) and then the
20-minute soak with both allocators. The diagnostics stay in the stats
report so the soak's `MEMORY MALLOC-STATS` keeps showing the schedule and
the occupancy histogram.

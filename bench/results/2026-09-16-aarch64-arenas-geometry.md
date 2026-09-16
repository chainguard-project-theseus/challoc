# challoc Phase 1 task 3 follow-up (3): descriptor tables aliased in L2, 2026-09-16, aarch64

Fourth file on the larson regression of the arena tree, after
`2026-09-16-aarch64-arenas.md`, `-arenas-lines.md` and `-arenas-threads.md`.
Same host (Apple M4, Docker Desktop, kernel 6.10.14-linuxkit, 10 CPUs, one
container, host 1-minute load 2 to 5 during the timed sections, no foreign
containers). Every table interleaves the variants round by round; read the
sorted lists by position.

- old: 8449c3a (pre-arena tree)
- cur: d4bb3d4 (HEAD at the start of this iteration)
- newa: cur plus variant (a) of the previous file: `free` reads the slot
  geometry from `Regions::class_base[c]` / `lg_region` / `CLASSES[c]`
  again and only `desc_stride`/`desc_base`/`committed_slabs` from
  `tables[c]`; `SlabTable` loses its `geom` and `region_len` copies
- new: newa plus a per-class stagger of the descriptor array inside its
  reservation (`class::desc_stagger`, 384 bytes per class); this is what
  landed
- larson `5 8 1000 5000 100 4141 10` (M ops/s) and `4 8 1000 5000 100 4141 1`
  for the 1-thread rows; xmalloc-test `-w 10 -t 5 -s 64` (M frees/s);
  alloc-test1 wall seconds from `bench/rt`

## 1. Variant (a) is neutral: the fast-path code was never the cause

With (a) the exported `free` is instruction for instruction old's stream
(75 instructions against old's 67): the only additions are the
`multi_arena` load and its `cbz` (shown free by variant vc of the previous
file) and the arena compare behind it. Interleaved rounds:

| variant | larson 1 thread, sorted | larson 10 threads, sorted |
|---|---|---|
| old | 55.4 56.6 57.9 57.9 58.6 | 202.8 211.6 216.7 217.2 237.7 |
| cur | 51.7 52.9 52.9 53.3 54.5 | 174.0 187.2 187.8 198.4 200.1 |
| newa | 47.6 52.1 53.6 54.3 54.8 | 175.8 189.7 196.5 198.3 202.5 |

Same code, same lines, old still 7% ahead with one thread. So the
regression is in the data, not the instructions.

## 2. Where the time goes with one thread (sampler, 8 s larson)

Symbolised `bench/sampler.so` samples, shares of all samples in the
process, two runs per variant (4125/780 samples for old, 962/3776 for newa):

| | old | newa |
|---|---|---|
| M ops/s | 58.2 / 56.9 | 52.9 / 54.2 |
| `main` (larson's own code) | 62.5% / 64.4% | 64.0% / 67.2% |
| `free` | 28.4% / 27.7% | 27.9% / 26.1% |
| `operator new` | 8.1% / 5.8% | 6.2% / 5.7% |
| everything else in challoc | 0.5% / 1.0% | 1.0% / 0.7% |

Times the CPU time per op (17.2 ns old, 18.7 ns newa): `free` costs 4.8 ns
in both, `operator new` 1.0 to 1.4 ns in both, the slow paths under 0.2 ns
in both, and `main` 10.9 ns under old against 12.3 ns under newa. The
whole gap sits on larson's own instructions, which are the same binary.
That is the signature of an out-of-order core reporting a stall a few
instructions after the load that caused it: the load is `free`'s
descriptor bitmap word, the last dependent load before `ret`.

Not the slow paths, measured directly (`wait4` rusage and `strace -c -f`,
1-thread runs): old 3001-3016 minor faults per 4 s run, newa 2872-2884;
per 2 s run old/newa `clone` 219/204, `madvise` 221/206, `mprotect`
577/562, `mmap` 253/238, every count proportional to the ops done (larson
starts a thread per round) and none higher under newa. No purge, no extra
commit, no extra fault.

## 3. The cause: every descriptor table on the same 1 MiB boundary

`/proc/self/maps` of a probe that touches a few classes, committed
metadata pages only. Under new (before the stagger) every committed
descriptor page sat at offset 0 modulo 1 MiB (and modulo 16 MiB):

```
rw 0xcafbe4000000 size=4096 mod16384=0 mod1M=0
rw 0xcafbeb000000 size=4096 mod16384=0 mod1M=0
rw 0xcafc1d000000 size=4096 mod16384=0 mod1M=0
rw 0xcafc9d000000 size=4096 mod16384=0 mod1M=0
rw 0xcafcdd000000 size=4096 mod16384=0 mod1M=0
```

Why: `Shared::init` now builds the 52 `SlabTable`s first, and each one
reserves its descriptor array (`nslabs * desc_stride`, a power of two of
at least 1 MiB) from the top of the metadata region, back to back. Old's
`ClassState::new` reserved each class's descriptor array and then its
three set bitmaps (0x82000 bytes each for the 64 KiB-slab classes), so
consecutive descriptor arrays were displaced by 0x186000 and their
offsets modulo 1 MiB spread out by accident.

A 16 MiB L2 with 1 MiB per way indexes by address bits 7 to 19: slab `k`'s
descriptor line of all 52 classes maps to one L2 set (and one L1 set,
16 KiB per way). larson keeps one or two slabs per class busy, so the 52
hottest descriptor lines of the process compete for one set's ways and
the `free` bitmap-word load misses L2 for a share of calls. Old had a
26-way L1 conflict on the same lines (its stagger was 0x2000 modulo
16 KiB), which measured as harmless: an L2 hit is 15 cycles and mostly
hidden behind the slot arithmetic; a DRAM access is not.

Fix: `class::desc_stagger(c) = c * 384` bytes, added to the reservation
length and to `desc_base`; `commit_next` commits `stagger + (s + 1) *
stride`. Three 128-byte lines per class, `gcd(3, 128) == 1`, so all 52
classes take distinct L1 and L2 sets; a multiple of `DESC_ALIGN`. After
the stagger the histogram of committed metadata pages by offset modulo
1 MiB under a larson-like probe (5000 live blocks, 200k ops) has no offset
shared by more than 5 pages under new against 4 under old (350 and 356
pages).

## 4. Measurements with the stagger

Interleaved rounds, one container, four variants:

| variant | larson 1 thread, sorted | larson 10 threads, sorted |
|---|---|---|
| old | 57.6 59.7 60.1 60.1 60.8 | 241.5 242.9 254.2 255.0 257.3 |
| cur | 55.1 55.1 55.2 55.8 56.5 | 224.7 225.1 228.4 229.9 230.6 |
| newa | 53.4 54.2 54.4 55.2 55.4 | 214.3 218.4 225.4 226.5 230.1 |
| new | 56.9 56.9 57.4 57.7 59.3 | 219.2 239.9 240.1 243.5 248.6 |

Seven more 10-thread rounds of old, cur and new, plus xmalloc-test and
alloc-test1 in the same session:

| variant | larson 10 threads, sorted (median) | larson RSS MiB | xmalloc-test, sorted | alloc-test1 s, sorted |
|---|---|---|---|---|
| old | 234.6 244.0 244.6 **250.4** 252.9 254.1 255.0 | 74.8-78.0 | 505.8 521.8 522.2 563.7 | 1.988 2.079 2.109 |
| cur | 212.8 219.0 223.0 **223.1** 223.2 223.2 233.2 | 71.7-75.5 | 506.6 507.1 520.8 521.6 | 1.988 1.989 1.999 |
| new | 238.2 239.6 240.1 **240.1** 245.0 246.4 254.6 | 74.4-76.3 | 489.6 492.0 512.8 533.0 | 1.976 1.979 1.990 |

The stagger takes larson from 11% behind old (median) to 4% behind, with
the ranges overlapping (new 238-255 against old 235-255); with one thread
the medians are 57.4 against 60.1 (4.5%; cur was 8% behind). xmalloc-test
and alloc-test1 are within noise, alloc-test1 if anything better. The
1-thread sampler split after the stagger (2567 and 569 samples): old
59.4 M ops/s, `main` 62.2%, `free` 28.8%, `operator new` 7.4%; new 58.3
M ops/s, `main` 59.9%, `free` 32.9%, `operator new` 6.2%: `main` is back
to the same cost under both and what is left, about 0.5 ns per op, is in
`free`.

## 5. Against jemalloc, same run

Five interleaved 10-thread rounds of old, new and jemalloc 5.3.1 (host
1-minute load 6.7 at the start, so noisier than the tables above; old's
205 and jemalloc's 216 are the load):

| variant | larson 10 threads, sorted (median in bold) |
|---|---|
| old | 205.4 236.5 **240.9** 251.8 253.3 |
| new | 221.1 221.1 **232.8** 240.2 248.1 |
| jemalloc (RSS 158-171 MiB) | 215.8 257.8 **258.3** 259.3 269.1 |

By medians jemalloc is 7.2% ahead of old (Phase 0 acceptance measured
7.7%) and 11% ahead of new. Phase 0's "larson within 10% of jemalloc" is
therefore marginal on this tree: about one point outside, at the
resolution this host gives (old's own rounds span 8%).

## 6. Open

New is 3 to 4% behind old on larson by medians (1 and 10 threads alike),
with overlapping ranges; the remainder is inside the exported `free` and
worth about 0.5 ns per call. `free`'s instruction stream is old's plus
the `multi_arena` load, `cbz` and the arena compare behind it (measured
neutral to +2% for its removal in the previous file, inside noise), and
every line it reads is now placed as well as or better than old's (the
descriptor lines have no set conflicts at all, where old had a 26-way L1
conflict). The metadata page histogram shows no other systematic
aliasing. Candidates that remain: the `multi_arena` check itself, which
the design needs; L1-set placement of the slow paths' lines (arena 0's
class locks and set bitmaps sit at different addresses than old's and
evict different fast-path lines); and hardware counters on a bare-metal
box, still the one measurement that would settle it. Not worth more
iterations of this loop unless the Phase 1 acceptance run shows larson
outside 10% again.

Lessons recorded in PROGRESS.md: identical instruction streams plus a
sampler that blames the caller's code point at data placement, and the
first thing to compare is `/proc/self/maps` offsets modulo the L1 and L2
way sizes; reservations of power-of-two size carved back to back all
start on the same boundary.

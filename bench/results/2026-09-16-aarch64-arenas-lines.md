# challoc Phase 1 task 3 follow-up: larson regression, 2026-09-16, aarch64

Follow-up to `2026-09-16-aarch64-arenas.md`, which left the arena tree 11%
behind the pre-arena tree on larson (and 5% on xmalloc-test) with no
attribution. This file records the attribution and the fix.

- old: 8449c3a (pre-arena tree), built from `git archive` into `.tmp-old`
- cur: c1f2097 (arena tree as committed; bd8fbbb differs only in a decay
  wake-up, off the hot paths)
- fix: cur plus `multi_arena` moved to the `Regions` line and the heap's
  home arena read only under the flag (`.tmp-fix`, first attempt)
- fix2: fix plus the `free` fast path classifying from the `SlabTable` line
  alone (the change that landed)
- host: Apple M4, 10 CPUs visible to the container, kernel 6.10.14-linuxkit;
  the host carried other sessions' load all morning (load average 5 to 28),
  so every table below interleaves the variants round by round and the
  spread within a variant is the noise floor; compare positions in the
  sorted lists, not single runs
- larson `5 8 1000 5000 100 4141 10` (M ops/s, higher is better);
  xmalloc-test `-w 10 -t 5 -s 64` (M frees/s); alloc-test1 wall seconds
- sampler: `bench/sampler.so` at 1000 Hz of CPU time, 10 s larson runs;
  shares are of all samples in the process

## 1. Where the time went (sampler, old vs cur)

Module and symbol shares, 10 s larson under each library (same run as the
6-round table in section 3):

| | old | cur | fix |
|---|---|---|---|
| samples | 2648 | 55859 | 2206 |
| `main` | 58.4% | 54.8% | 56.0% |
| `free` | 24.1% | 30.5% | 29.5% |
| `operator new` | 16.0% | 13.4% | 13.3% |
| `ClassState::claim` + `release` + sort | 0.6% | 0.7% | 0.7% |

Multiplying by CPU time per op (10 threads / throughput): `free` costs
about 9.6 ns per call in old and 14.6 ns in cur; `operator new` is
unchanged at 6.4 ns; the slow paths are under 1% in both. So the whole
regression is inside the exported `free`, not in `claim`, `release` or the
`SlabTable` indirection that `2026-09-16-aarch64-arenas.md` suspected.

Per-instruction shares inside `free` (samples on each instruction as a
share of all samples in `free`; an out-of-order core reports the oldest
unretired instruction, so a stalled load shows up on itself or the next
few instructions):

| instruction window | old | cur | fix |
|---|---|---|---|
| TLS slot load and `cmp x8, #3` | 4.6% | 3.6% | 3.5% |
| `Regions`: `superblock`, `lg_stride`, class index | 3.3% | 1.8% | 1.5% |
| `Regions::class_base[c]`, `lg_region`, `cbnz`, then `CLASSES[c]` loads (`adrp`, `ldrb`, `ldp`, `lsl`) | 16.7% | **40.1%** | **41.6%** |
| slot arithmetic (`umull`, `cmp`, `b.ne`) | 2.4% | 3.0% | 3.7% |
| `SlabTable`/`ClassState`: `committed_slabs`, `desc_stride`, `desc_base` | 18.9% | 10.9% | 7.9% |
| bitmap word load and test | 15.5% | 14.2% | 14.6% |
| `multi_arena` flag, `cbz` | n/a | 5.3% | 7.2% |
| `heap.caches[c]`, cache header, slot store, `len` store, `ret` | 38.6% | 20.2% | 19.9% |

Old spends its `free` time on the lines that are meant to miss: the
bitmap word (written by other threads' flushes and refills) and the
thread's own cache (evicted by the block traffic). Cur and fix move
about a quarter of `free` onto the loads of `Regions::class_base[c]`,
`lg_region` and the `CLASSES[c]` record, which are read-only and were
cheap in old. At 14.6 ns per `free`, the window is worth about 5.9 ns in
cur against 1.6 ns in old: one extra L2 access per `free`.

The `free` disassembly differs from old only by the flag test, the home
arena load, the table stride (384 instead of 640) and the addresses of
`CLASSES` and `Regions`. Larson's working set (5000 live blocks per
thread, 10 to 1000 bytes) turns over the whole L1 every few thousand
operations, so a fast path's cost is the number of distinct lines it
touches and how those lines fall into L1 sets, not its instruction count.
Cur's `free` reads the `Regions` header line, the `class_base` line for
the class, the `CLASSES` line for the class, the `SlabTable` line, the
descriptor line, the heap's cache-pointer line, the cache header and the
slot: eight lines before the flag and home arena added a ninth and tenth.

## 2. What did not work: the flag and the home arena alone

fix moved `multi_arena` in front of `regions` (same 128-byte line as
`superblock` and `lg_stride`, checked by a compile-time assert) and made
the home arena a volatile read under the flag, so single-arena `free`
touches neither the flag's old line nor the heap header. The disassembly
confirms both loads moved. It did not help (section 3): the extra stall
sits on the geometry loads, which fix still makes from three lines.

## 3. Measurements before the fix that landed

Six interleaved rounds (old, cur, fix in turn), then the same numbers
sorted:

| variant | rounds | sorted |
|---|---|---|
| old | 264.9 249.6 247.5 234.7 234.3 242.3 | 234.3 234.7 242.3 247.5 249.6 264.9 |
| cur | 201.2 217.3 218.7 211.1 206.6 212.7 | 201.2 206.6 211.1 212.7 217.3 218.7 |
| fix | 216.3 216.6 203.7 194.9 206.5 203.3 | 194.9 203.3 203.7 206.5 216.3 216.6 |

Old beats cur and fix in every round (median 245 against 212 and 205);
fix is not distinguishable from cur. An earlier 4-round set under heavier
load read old 208-233, cur 209-217, fix 191-218, and xmalloc-test old
490-563, cur 451-533, fix 462-506: same ordering, same conclusion.

## 4. The fix that landed (fix2)

`SlabTable` carries a copy of `CLASSES[class]` and the region length on
its first line, and `Shared::checked_slot` classifies a pointer from the
`Regions` header line (class index) plus that one table line:
`SlabTable::slab_slot_fast`. `free` no longer reads `class_base[c]` or
`CLASSES[c]`, so it touches six lines where old touched eight (71
instructions against old's 67 and cur's 74). The `multi_arena`/volatile
change from fix stays: it is correct on its own terms (it keeps the
single-arena path at those six lines) and costs nothing.

Results on a quiet host are in section 5.

## 5. Final A/B on a quiet host (1-minute load 3.0 at start)

Five interleaved larson rounds, three of xmalloc-test and alloc-test1:

| variant | larson rounds | sorted | RSS KiB |
|---|---|---|---|
| old | 254.9 250.7 249.9 247.0 250.2 | 247.0 249.9 250.2 250.7 254.9 | 77232-80072 |
| cur | 227.5 223.3 222.9 216.4 213.5 | 213.5 216.4 222.9 223.3 227.5 | 72700-74480 |
| fix2 | 226.0 206.6 222.5 214.9 218.2 | 206.6 214.9 218.2 222.5 226.0 | 71988-74640 |

| variant | xmalloc-test M frees/s | alloc-test1 s | alloc-test1 RSS KiB |
|---|---|---|---|
| old | 541.4 516.9 535.3 | 2.084 2.132 2.196 | 15184 |
| cur | 534.9 541.6 509.7 | 2.119 2.194 2.191 | 15248 |
| fix2 | 519.4 527.2 514.1 | 2.138 2.185 2.142 | 15240-15248 |

fix2 is neutral: within noise of cur on all three, and larson stays
about 12% behind old. xmalloc-test's 5% gap from the earlier file is not
reproducible on the quiet host (cur and old overlap). fix2 lands anyway:
it is the natural shape for the fast path (one per-class line, the flag
on the line already read), it is correct on both architectures (204 + 53
tests on aarch64 and emulated x86_64, 152 on the host), and it is what a
multi-arena `free` should read; but it does not close the gap.

## 6. Two more hypotheses rejected

Page mappings (`/proc/<pid>/smaps_rollup` and `maps` 5 s into an 8 s
larson, THP `always`): old RSS 77.4 MiB, cur 74.5 MiB, fix2 76.2 MiB;
`AnonHugePages` 20 MiB, 20 MiB, 22 MiB; VMAs 5389, 4913, 4901 (about
2700 `rw-p` anonymous VMAs each: with `PROT_NONE` guard slabs on this
kernel every slab is its own VMA); `thp_collapse_alloc` unchanged. Nothing
there explains 12%.

Code placement: cur's `free` spans 0xffc8-0x100f0 and straddles the 4 KiB
page at 0x10000, exactly where the hot instructions are; old's fits in one
page. A variant with every exported entry point in `.text.hot` (`free` at
0x5d48, `operator new` at 0x62d4, no page crossing) measured 219.1 220.9
230.3 230.7 against cur 199.7 219.2 225.2 229.2 and old 247.7 255.2 257.2
258.8 in the same interleaved run: within noise of cur. Not landed.

## 7. Open

Twelve percent on larson, all of it inside the exported `free` (about
5 ns per call) and `main` (about 3 ns per op), none of it in the slow
paths, with the `free` samples parked on the first loads after the TLS
check. Not: the flag or home-arena loads, the number of lines `free`
touches, the `SlabTable` stride, THP, VMA count, code page placement.
Still untested: (a) allocation *placement*: cur uses about 9% fewer VMAs
and 4% less RSS on larson than old, so slots are packed into fewer slabs;
denser packing means more threads share each descriptor and bitmap line,
and a coherence miss on the bitmap word would surface a few instructions
later as a stall on independent loads only if the core reports the newest
rather than the oldest instruction, which the sampler under this VM may
well do; compare `plan`/`take`'s slab selection with 8449c3a's, in
particular which slabs a refill draws from when several arenas' states
share one table; (b) the `Heap` layout (`repr(Rust)` put `arena` at 472
and `caches` at 0; a `repr(C)` heap with the header on the `caches[0..8]`
line would make the multi-arena read free too); (c) a per-thread hardware
counter run on a bare-metal Linux box (`perf stat -e
L1-dcache-load-misses,dTLB-load-misses,iTLB-load-misses`), which this
VM cannot provide and which would settle it in one run.

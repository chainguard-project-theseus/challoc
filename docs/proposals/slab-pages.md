# Proposal: slab pages (smaller initial slabs for the small classes)

Status: proposal, not built. Nothing here describes the allocator as it
is; `docs/design.md` does, and its sections are cited as "design N.M".

## 1. Problem

A small class (quantum to 448 bytes) allocates 64 KiB slabs (design 3).
Whatever its live data, a class in use holds at least one whole slab, and
a slab returns memory only when every slot in it is free (`Empty`, then
`Purged`, design 8). Two patterns turn that into memory the program cannot
get back, and that Redis's active defragmenter cannot reduce:

1. **One slab per class in use.** A class with a handful of long-lived
   objects keeps one 64 KiB slab: 2048 B with 1 live slot of 32, 192 B
   with 7 of 341, 96 B with 12 of 682. The defrag hint correctly refuses
   every move (the slab is the class's only one, design 13.5).
2. **Survivors spread over many slabs.** A few hundred long-lived small
   objects allocated among bulk data that is later freed end up a dozen
   or two per slab, and the slab never empties: 550 to 1000 live 24- and
   32-byte objects held 21 to 38 slabs at 1 to 5% occupancy. jemalloc's
   bins have the same scatter, but its one-page slabs empty and are
   released as the survivors die; a 64 KiB slab with 20 to 30 survivors
   almost never does.

Redis counts the free slots of every slab as fragmentation (`(nregs *
curslabs - curregs) * size` per bin, `zmalloc_get_frag_smallbins`). The
floor therefore stays in its figure, and whenever
`active-defrag-ignore-bytes` is below it the defragmenter rescans the
keyspace forever without moving anything: 557 million hint misses in 40 s
in one run. Eight active-defrag tests in Redis 8.10.3's
`unit/memefficiency` fail on it, and `tests/consumers/redis-build.sh`
skips them.

### 1.1 Measurements

One-host diagnostic runs on x86_64, not cb numbers: Redis 8.10.3 with
`contrib/redis`, challoc main plus #576, quantum 8; stock Redis with its
bundled jemalloc as the reference. Each run dumped `MEMORY MALLOC-STATS`
after defrag settled. In a scratch build, the report also counted, per
class, the 4 KiB pages of the non-empty slabs that hold at least one set
slot bit (live or cached). "Closed" below means every page with no set
bit given back, and the slots inside it removed from Redis's figure.

| Point | challoc arena 0 floor | of which single-slab classes | with all-free pages closed | jemalloc |
|---|---|---|---|---|
| Fresh server, "eval scripts" pattern | 1379 KB | 908 KB (20 classes) | 175 KB | 791 KB (arena 0) |
| Whole test file, 22 of 33 settle points | 0.99 to 3.9 MB | 0.55 to 1.08 MB | 57 to 679 KB at 21 points, 1.86 MB at one | median 735 KB, 90th percentile 2.2 MB (Redis's figure, 45 points) |

The other 11 settle points followed tests that failed and left their data
(about 25 MB), so they say nothing about the floor. Two observations
decide the design:

- **The live slots are low in each slab.** In every single-slab class the
  live and cached slots sat in the first 1 to 4 of 16 pages, because
  claims take the lowest clear bits (design 4, 5). Closing the rest takes
  each such class from 50 to 62 KB to 0 to 4 KB.
- **Survivors leave most pages empty too.** In the multi-slab classes,
  closing all-free pages removed 74 to 95% of the free space at 20 of 22
  points (2.0 MB to 159 KB, 3.3 MB to 324 KB, ...). It removed 48% and
  26% at the other two; at the latter (2.4 MB to 1.8 MB), the survivors
  sat on most pages.

## 2. Why not smaller slabs

- **16 KiB slabs for every class were tried and dropped** (design 3).
  They doubled the per-slab refill and flush work and halved
  `xmalloc-test`.
- **A per-class or variable slab size changes everything keyed on it.**
  That covers the slab and slot arithmetic of the `free` and `sdallocx`
  fast paths, the bitmap's `slab_bits`, the dense descriptor array, the
  group hand-out (design 4) and the commit runs (design 5).
- **A smaller first slab alone would not hold.** A refill claims up to
  `cap / 2` slots at once (design 6.4), 500 for most small classes at
  Redis's cache size. The lone 96-byte class above had been refilled 500
  slots and kept 12. Its first refill would already have grown it to the
  full slab. The memory comes back only if pages are returned after the
  cache GC gives the slots back.

So the proposal keeps the 64 KiB slab, its address arithmetic, its bitmap
and its descriptor. It manages the slab's memory a page at a time:
- a fresh slab opens only the pages its first claim needs (the "smaller
  initial slab");
- a slab opens more pages as claims need them;
- a page whose slots are all free closes, and its memory is discarded.

## 3. Design

### 3.1 Scope

The scope is classes whose slab is larger than the page: the small tier,
quantum to 448 bytes, at 64 KiB slabs. That's 16 pages on 4 KiB-page
kernels and 4 pages on 16 KiB pages. Out of scope:
- 64 KiB-page kernels, where the slab is the page (design 3);
- the 512..1024 classes, already 4 to 16 KiB slabs;
- the medium tier, as a follow-up (8).

### 3.2 State

- **`open: u16` per slab**, the open pages, bit `p` for page `p`. It fits
  in the descriptor's spare bytes: these classes' descriptor stride is 32
  or 64 bytes around the 16-byte header (`slab::strides_of`). Next to it
  is `pages_dirty_at: u64`, when the slab last got a closable page.
- **A closed page's slots have their bitmap bits held set by the
  allocator**, as the padding bits are (design 4). A claim never sees
  them, so the lock-free claim (`fetch_or` of the clear bits) needs no
  new test. They are not live and not in `live`.
- **Slots that straddle a page boundary** (48, 80, 96, ..., 448 bytes):
  - a slot is open when every page it overlaps is open;
  - the class table gains, per page, the range of slots overlapping it,
    16 pairs of `u16`, computed at init with the rest of the geometry
    (`layout::adopt_page_geometry`);
  - closing page `p` holds every slot overlapping `p`;
  - opening `p` frees the ones whose other pages are open.
- **A new set per `(arena, class)`, `closed`:** the slabs that are
  `Active` with at least one closed page. It is a view of the same
  `SetTable` as `partial`, `empty` and `purged` (design 5).
- **`closed_slots` per class state:** the slots currently held by closed
  pages.

### 3.3 Opening (the smaller initial slab)

- **The selection order of design 5 gains one step.** It becomes:
  1. the lowest partial slab (an open, clear slot);
  2. else the lowest slab in `closed`, whose lowest closed pages are
     opened for the batch;
  3. else the lowest `Empty` slab, then `Purged`, then `Fresh`.

  Filling a slab's pages before taking a higher slab keeps address-ordered
  first fit, which is what makes defragmentation converge.
- **A `Fresh` or `Purged` slab opens only the pages its first claim
  covers.** That's `ceil(n * size / page)`, rounded to whole straddling
  slots. Its other pages start closed, so their bits are set when the
  slab's bits are initialised (`claim_prefix`, design 5). A closed page
  that was never touched costs no RSS and needs no discard later.
- **Opening is a class-lock step.** Under the lock it clears the page's
  slot bits (`fetch_and`), sets `open`, subtracts from `closed_slots`, and
  updates the `closed` and `partial` sets.
- **Zero tracking.** A page that was closed by an eager discard, or never
  touched, is zero. Its slots can be reported zero exactly as a `Purged`
  slab's are, under `purge::purged_is_zero` (design 8). First version:
  never report a reopened page zero, which is what design 5 does for
  every `Active` slab now.

### 3.4 Closing (giving pages back)

- **Candidates.** A page is closable when every slot overlapping it is
  clear: neither live nor cached. A release that leaves a slab with at
  least one closable page stamps `pages_dirty_at` (if unset) and adds the
  slab to the class's decay gate, as `dirty_at` does for an `Empty` slab
  (design 8).
- **The decay walk closes pages,** with the smootherstep curve over
  `dirty_decay_ms` applied to closable pages as it is to `Empty` slabs.
  For each page it closes, under the class lock:
  - `old = fetch_or(slots)` on each word the page's slots use;
  - if any of those bits was already set in `old` (a lock-free claim took
    a slot after the candidate test), it undoes its own bits with
    `fetch_and(!(slots & !old))` and skips the page;
  - otherwise the page is the walk's, and it joins the walk's
    `purge::Batch`, with adjacent pages of one slab merged;
  - after the batch's `madvise`, the walk clears the page's `open` bit,
    adds to `closed_slots` and puts the slab in `closed`;
  - a refused discard (`EINVAL`, `mlock`ed pages) reopens the page's slots
    and keeps its age, as an `Empty` slab stays `Empty` (design 8).
- **The advice is the band's,** `MADV_DONTNEED` or `MADV_FREE`. A lazy
  discard clears `purged_is_zero` as it does now.
- **Explicit purges close every closable page:** `malloc_trim(0)`,
  `arena.<i>.purge`, and the `high`-rise full purge. A defragmenter's
  hints also close the hinted class's pages, rate-limited (7.1).
- **A slab whose last live slot is freed goes `Empty` as now.** Its closed
  pages are already discarded, so the slab's purge discards only its open
  pages. A `Purged` slab reused through step 3 is opened like a `Fresh`
  one.

### 3.5 Checks

- **`free` fast path: unchanged.** It reads the slot's bit, and a closed
  page's bits are set, so a stale free into a closed page passes the
  check. The flush then catches it, as it does a stale free of a slot
  sitting in a cache (design 6.3).
- **Release (flush) checks each slot's page** against the slab's `open`,
  one load per slab in `group_by_slab`. A slot on a closed page aborts as
  an invalid free (`invalid or double free of <p>: slot is on a closed
  page`).
- **`live` is unchanged.** Closed slots are never live, so the
  `Empty`-at-zero rule stands.

### 3.6 Statistics

This is a deliberate difference from jemalloc's per-bin identity, and
design 13.4 must say so:

- **`stats.arenas.<i>.bins.<j>.curregs` counts closed slots.** It already
  means "slots out of the bitmaps" (live or cached). With them in it,
  Redis's and Valkey's `(nregs * curslabs - curregs) * size` counts only
  open free slots, and the floor in their figure falls with the memory.
- **`allocated` (`stats.allocated`, `small.allocated`) does not count
  them.** It is `(curregs - closed_slots) * size` per bin, so jemalloc's
  `curregs * size == small.allocated` no longer holds bin by bin.
- **`active` does not count closed pages**, and neither does `resident`.
  A never-touched page never was resident, and a discarded one no longer
  is. `resident >= active >= allocated` still holds.
- **`experimental.utilization`** (Valkey's defragmenter) reports a slab's
  `nregs` as its open slots and `nfree` as its open free slots.
- **The defrag hint's average** (`live * (curslabs - 1) <= curregs -
  live(slabcur)`, design 13.5) reads `curregs - closed_slots`: jemalloc's
  meaning of `curregs`, live plus cached. With closed slots left in, the
  right-hand side grows by every closed slot, and the hint also moves
  objects out of slabs that are above the class's real average: wasted
  moves, and `slabcur` filling faster than the rule intends.
- **Compaction's probe needs no change.** `ClassState::holes` is
  `curslabs * nslots - curregs` (`bin_stats`), so with closed slots in
  `curregs` it counts open free slots only, which is what its
  "worth four slabs" test means.
- **The report** (design 16) gains a closed-pages column in the bins
  table.

### 3.7 Concurrency and proofs

- **Claims stay lock-free.** A closer's `fetch_or` and a claimer's
  `fetch_or` on one word give each bit to exactly one of them, and the
  closer backs off unless it got the page's slots whole. Opening and
  closing run under the class lock, which already orders every set
  change.
- **Verus.**
  - The bitmap claim and release contracts gain the open mask.
  - Discarding part of an `Active` slab needs a proof that every slot of
    the discarded pages is held by the closer: its `fetch_or` found them
    clear. That is the analogue of the `live == 0` premise of an `Empty`
    slab's purge, discharged against the same discard seam.
  - Per-page zero tracking joins the slab state's ghost.
  - The page-to-slot ranges get a Kani harness over every class at every
    supported page size, as `slot_of_offset_is_exactly_the_reciprocal_check`
    does for the slot test (design 3).

## 4. Costs and risks

- **The hot set must not change.** The `malloc` and `free` fast paths
  never read `open`. `scripts/codegen-diff.sh` must show `changed 0` on
  the hot set in all four cells.
- **Refill:** one more set lookup when no partial slab exists, and a lock
  hold per page opened, at most 16 per slab.
- **Flush:** one load of `open` per released slab, and one mask test per
  word, on the slow path.
- **Decay:**
  - an `madvise` per run of closed pages, batched (design 8);
  - a TLB shootdown for each `MADV_DONTNEED` in a multithreaded process.
    The curve and the `pages_dirty_at` gate bound the rate, and a churning
    class whose pages reopen within the window pays it repeatedly.
  - `MADV_DONTNEED` and `MADV_FREE` split no VMA.
- **Under `thp:always`,** discarding pages inside a huge page splits it.
  Closing is off inside `thp:always` chunks, as guard slabs are
  (design 17.1).
- **Metadata:** none new for these classes; the descriptors' spare bytes
  hold `open` and `pages_dirty_at`.

## 5. Plan

1. **Closing, first.** This is the larger effect, on both patterns:
   - the `open` mask, the held bits and the `closed` set;
   - the decay walk and explicit purges;
   - the release check, the statistics and the report.

   With every slab opened whole, as now, this is closing alone.
2. **Opening on demand:** a `Fresh` or `Purged` slab opens only the pages
   its claim needs. This saves the pages a slab's first refill would touch
   and the cache GC would give back.
3. **The defrag hint and `experimental.utilization`,** checked against
   Redis's and Valkey's defragmenters.

## 6. Validation

- **The floor measurement above, as a harness.** Fill, delete and defrag,
  then dump per class and per page. It belongs in `bench/` as a row with
  the floor bytes as its score, against jemalloc.
- **Redis `unit/memefficiency`:** the eight skipped tests pass at their
  stock thresholds, and their skips leave `tests/consumers/redis-build.sh`.
  For Valkey, `contrib/valkey/0001`'s eval-scripts calibration goes.
- **Benchmarks:**
  - `make quick`, with `xmalloc-test`, the row the 16 KiB slabs lost, and
    the `churn1024` occupancy probe;
  - `kv-redis` and `kv-valkey`, whose soak `rss_settle_mib` should fall.
- **Proofs:** Verus, Kani and the seam ledger; codegen-diff on the hot
  set.

## 7. Decisions

The questions an earlier draft left open, answered.

### 7.1 When to close: on the decay curve, and on two prompt signals

Not at flush time. A close and its reopen cost one `madvise` and one fault
per page. This host (x86_64, 16 CPUs, one 64 KiB mapping, the other
threads spinning on their own memory in the same process) measured, per
cycle:

| Other threads | `MADV_DONTNEED`, 1 page | refault, per page | 1 page closed and reopened | 4 adjacent pages |
|---|---|---|---|---|
| 0 | 1.4 us | 1.9 us | 3.3 us | 9.0 us |
| 3 | 7.1 us | 2.1 us | 9.2 us | 14.7 us |
| 15 | 12.3 us | 3.1 us | 15.4 us | 23.7 us |

The discard's cost is the TLB shootdown: it grows with the threads, and
four adjacent pages cost about what one does. Under churn, a flush
releases the high half of a cache (design 6.4), and the next refill
claims those slots back. Closing at flush would pay one cycle in most
refill and flush pairs: 3 to 24 us per `cap / 2` (about 500) allocations,
6 to 47 ns per allocation, on paths that cost 10 to 20 ns. So:

- **By default, pages close on the decay curve,** exactly as `Empty` slabs
  are purged. Their age is `pages_dirty_at`, and the allowance is the
  smootherstep over `dirty_decay_ms`. A page that reopens within the
  window was never discarded.
- **Explicit purges close every closable page at once:**
  `malloc_trim(0)`, `arena.<i>.purge`, and the `high`-rise full purge.
- **A defragmenter's hints close the hinted class's closable pages,** at
  most once per `DEFRAG_GC_INTERVAL_NS` (100 ms) per class, and only pages
  closable for at least one decay step (design 8: `dirty_decay_ms /
  STEPS`, 50 ms by default). #576 already runs the cache GC from
  `get_defrag_hint` on the same interval. A defragmenter asking about a
  class is the signal that its fragmentation is unwanted now. It is what
  ends Redis's endless rescans: its figure drops on the next cycle, not a
  decay window later. A pass is one batched `madvise` per merged run, so
  it costs at most 10 such passes per class per second, and only while
  active defrag runs.

### 7.2 How many pages to open: the batch's, in one lock hold

Opening is a bit clear (`fetch_and`) and a mask update under the class
lock the refill already holds for slab selection (design 5): no syscall.
The fault happens at the first write either way. A claim that selects a
slab with closed pages opens the lowest closed pages until the open free
slots cover the batch or the slab is fully open, then claims. One lock
hold per refill, as now; never one per page.

### 7.3 Compaction: unchanged

The compacting cache's rebuild (design 6.4) claims from the lowest slab
with room. With step 2 of the selection order (3.3), that is a slab with
closed pages before any higher slab, which is the downward migration
compaction wants. Its probe reads `holes`, which already excludes closed
slots (3.6).

### 7.4 Zero tracking: not in the first version

A page closed by an eager discard reads as zero, but design 5 reports no
`Active` slab's slots as zero, and no zeroing entry point trusts the
report (design 6.3). The first version keeps that. Reporting reopened
pages zero under `purged_is_zero`, as `Purged` slabs are, is a later
optimisation for `calloc`.

### 7.5 Unchanged

Guard slabs and commit runs (design 5) stay as they are. A closed page is
still committed (`PROT_READ | PROT_WRITE`); it is only discarded.

## 8. Follow-up: the medium tier

The medium tier's slabs showed the same pattern: 1536, 2048, 3072 and
4096 B each held one 64 KiB slab for 1 to 3 live slots, about 240 KB on
the fresh server. Its descriptors have no spare bytes (16 slots or more
per slab pad to whole bitmap lines at 8 slabs a line, stride 16, design
4), so its state goes in a per-class side array of one `u32` per slab.
That array is reserved on the table side of the metadata region and
committed page by page as slab numbers grow, like the owner array.
Classes above 4096 bytes are 16 to 25 slots per slab, so this is one
32-bit word for every slab of the class, committed only where slabs are.

- **1280 to 4096 bytes:** the slab is 64 KiB, and a page holds parts of
  one to four slots. These classes use the small tier's mechanism
  unchanged, with the 16-bit page mask in the side array.
- **Above 4096 bytes:** the slab is the power of two at or above
  `16 * size`: 128 KiB at 5120 B (25 slots), 256 KiB at 14336 B (18
  slots). Every slot covers whole pages. The unit closed is the slot: a 32-bit
  closed-slot mask (at most 25 slots per slab), whose slot closes when it
  is free and opens when a claim wants it.

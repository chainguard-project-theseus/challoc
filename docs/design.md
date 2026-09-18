# challoc design

Status: draft for implementation. Where this document says "must", an
implementation that does otherwise is wrong. Where it says "should" or gives a
number, the value is a starting point to be validated by benchmarks in
[plan.md](plan.md).

## 1. Scope and constraints

- Linux only. glibc 2.44 is the floor. The kernel is probed at runtime; the
  reference kernel is 6.18 (Chainguard OS) and the oldest kernel that must
  work correctly, with reduced features, is 5.10.
- 64-bit only: x86_64 and aarch64. Page size is read from `AT_PAGESZ` at init
  and may be 4 KiB, 16 KiB, or 64 KiB.
- Built as a `no_std` Rust crate with `panic = "abort"`, producing a `cdylib`
  and a `staticlib`. The crate must never allocate through anything but its own
  machinery, must never call `fopen`, `opendir`, `dlopen`, or
  `pthread_setspecific` from an allocation path, and `free` must preserve
  `errno`. These are glibc's documented rules for a replacement malloc.
- Stable Rust. Thread-local storage uses a hand-written initial-exec accessor
  (section 6). Nightly `#[thread_local]` with `-Z tls-model=initial-exec` is
  an acceptable alternative behind a Cargo feature but must not be required.
- Consumers are C programs. There is no Rust `GlobalAlloc` facade in scope.

## 2. Address space layout

At init challoc reserves address space with `mmap(PROT_NONE, MAP_PRIVATE |
MAP_ANONYMOUS | MAP_NORESERVE)` and never unmaps it. All memory handed to
callers lives inside these reservations. Committing memory means `mprotect`
to `PROT_READ | PROT_WRITE` on a range of an already reserved region; the
kernel provides zero pages on first touch.

```
+------------------+  metadata region     (reserved, committed on demand)
| region table     |  sealed after init
| heap table       |
| slab descriptors |  per class, dense arrays indexed by slab number
| huge descriptors |
+------------------+
| gap              |
+------------------+  class region 0      (randomised offset inside its gap)
| slab | slab | .. |
+------------------+
| gap              |
+------------------+  class region 1
| ...              |
+------------------+
| ...              |
+------------------+  huge region
| run | guard | .. |
+------------------+
```

Sizes are runtime parameters chosen by probing how much virtual address space
the process can reserve, so the same binary works on 47-bit x86_64, 48-bit
aarch64, and 39-bit aarch64 kernels:

| Parameter          | Preferred     | Minimum      |
|--------------------|---------------|--------------|
| Class region size  | 2^38 (256 GiB)| 2^32 (4 GiB) |
| Gap per class      | 2 x region    | 1.25 x region|
| Huge region        | 2^40 (1 TiB)  | 2^34         |
| Metadata region    | 2^36          | 2^30         |

Region bases are randomised with `getrandom`. Reservation order is randomised
too, so the relative order of classes is not fixed across processes.

Because every region is a contiguous power-of-two-aligned range, classifying a
pointer is:

```
if   p in class_region[c] for some c  -> small or medium; slab = (p - base_c) >> lg_slab_c
elif p in huge_region                 -> huge; look up descriptor by page index
else                                  -> invalid; abort
```

The class lookup is a comparison against a sorted table of at most 64 entries,
or a direct index when regions are laid out at a fixed stride inside one
superblock. The implementation should start with the stride approach: one
superblock of `nclasses * (region + gap)` bytes, class `c` at
`superblock + c * stride + random_offset_c`, so `c = (p - superblock) / stride`
is a shift when stride is a power of two.

## 3. Size classes

Two quantum modes selected at build time by Cargo feature:

- `quantum16` (default): 16-byte minimum alignment as required by
  `max_align_t`. Classes: 16, 32, 48, 64, 80, 96, 112, 128, then four classes
  per doubling (160, 192, 224, 256, 320, ...) up to the largest medium class.
- `quantum8`: matches jemalloc built with `--with-lg-quantum=3`, which is what
  Redis and Valkey vendor and what Valkey's defragmenter asserts via the
  `arenas.quantum` control. Classes: 8, 16, 24, 32, 40, 48, 56, 64, 80, 96,
  112, 128, then four per doubling. Allocations of 8 bytes are 8-aligned.
  `aligned_alloc` and friends always honour their requested alignment.

Tiers:

| Tier   | Size range              | Slab size          | Slots per slab |
|--------|-------------------------|--------------------|----------------|
| Small  | quantum .. 448          | 64 KiB             | 146 .. 8192    |
| Small  | 512 .. 1024             | 4 .. 16 KiB        | 4 .. 25        |
| Medium | 1280 .. 256 KiB         | max(64 KiB, 16*sz) | >= 16          |
| Huge   | > 256 KiB               | page-granular runs | 1              |

The 512..1024-byte classes use the smallest power-of-two slab from 4 KiB up
whose internal waste is at most a thirty-second of the slab
(`layout::LG_MIN_SLAB`, `SLAB_WASTE_DIV`): 4 KiB for 512 and 1024 bytes,
8 KiB for 896, 16 KiB for 640 and 768 (whose 4 KiB and 8 KiB slabs would
waste a sixteenth: 21 MiB at the soak's peak for the 640 class alone,
against a 5% peak RSS criterion). Phase 1 acceptance found the Valkey soak's post-delete
residue to be live values the load never touches again, one to fifteen per
slab in exactly these classes, each pinning a whole 64 KiB slab where
jemalloc's 4..32-region slabs pin 4..28 KiB
(`bench/results/2026-09-17-aarch64-gce-compacting-cache-soak3.md`); the
compacting cache (6.4) moves everything else. Every other class keeps 64 KiB:
a 16 KiB slab for every class halved `xmalloc-test` through per-slab refill
and flush work, and a class with 64 or more regions per slab never empties by
chance anyway (`bench/results/2026-09-16-aarch64-gce-min-slab.md`). The
change is priced in `bench/results/2026-09-17-*-gce-slab-geometry.md`. Slabs
smaller than the kernel page are committed, guarded and purged in page
groups (a group only when every slab in it is idle and owned by the purging
arena); guard slabs keep their density per byte (one per
`guard_slab_interval` x 64 KiB); the compaction thresholds of 6.4 count
64 KiB slabs' worth of slots whatever the class's slab size; a refill or a
flush batch reopens up to 16 slabs per lock hold.

Slab size per class is a compile-time table. Each class region is divided into
slabs of that class's slab size, so slab number and slot index are:

```
slab   = (p - base_c) >> lg_slab_c
offset = p & (slab_size_c - 1)
slot   = offset * reciprocal_c >> 32       // precomputed fixed-point divide
valid  = slot * size_c == offset && slot < nslots_c
```

Size class of a request uses the same lookup shape as jemalloc and mimalloc:
a small table for requests up to 1024 and a leading-zero-count computation
above that. `nallocx` returns the class size.

## 4. Slab descriptors and bitmaps

Every slab has a descriptor in the metadata region. Descriptors for class `c`
form a dense array indexed by slab number, committed on demand as the region
grows. Descriptor layout, a fixed 16-byte header:

```
struct SlabDesc {
    state:     AtomicU8,     // Fresh, Active, Empty, Purged
    arena:     u16,
    live:      AtomicU32,    // slots currently marked allocated
    dirty_at:  AtomicU64,    // monotonic ns when it became Empty, 0 otherwise
}
```

The allocation bits are not in the descriptor. Each class has one contiguous
*slot bitmap* in the metadata region, committed on demand like the
descriptor array, in which slab `s` owns bits `s * slab_bits .. (s + 1) *
slab_bits`. `slab_bits` is `nslots` padded to a power of two below 512 or a
multiple of 512 from there (`Class::slab_bits`), so a slab's bits are either
whole words or one range inside a word, never straddling a word, and a
64-byte line holds a whole number of slabs. The bitmap is one bit per slot:
a 550 MiB heap of 1 KiB values is 70 KB of bitmap (a few hundred KB over
all the classes of a real Valkey heap), which stays in the cache hierarchy
where one 64-byte descriptor line per slab was 9 MB and a DRAM miss per
`free` (Phase 1.5 task 4f; on Valkey's write path the word still comes from
beyond L2, but the query dropped from 492 to jemalloc's 327 cycles per SET). The padding bits between `nslots` and `slab_bits` are
kept permanently set.

Slabs are handed to arenas in *groups* of `slabs_per_line` consecutive slabs
(the slabs whose bits fill one bitmap line; or one page group when the page
is larger than that), lowest group first to whichever arena needs a slab
next, and each arena commits its group's slabs one at a time as it uses
them. So a bitmap line, and a descriptor line, is written only by the
refills and flushes of one arena, and the `free` check never reads a line
another arena's threads write. A dense *owner array* of one `u16` per group
records each group's arena; `free` reads it (not the descriptor) to route
a slot back to a cache of its arena. A group spans at most 2 MiB of the
region (`LG_GROUP_SPAN`): classes with larger slabs pad each slab's bits to
a whole line instead.

Bitmap semantics: a set bit means the slot is not available to a slab refill.
It is either live in the program or sitting in some thread's cache. A clear
bit means the slot belongs to the slab and may be claimed.

Operations on the bitmap are always atomic. Slabs have no owning thread.

- **Claim** (cache refill): find the lowest word with any clear bit of the
  slab, compute `want = !word & slab_mask` masked to at most `n` bits,
  `fetch_or(want)`; the bits that were actually clear in the returned value
  are the claimed slots. Retry on the next word if fewer than requested. Add
  the count to `live`.
- **Release** (cache flush): group pointers by slab, `fetch_and(!mask)` per
  word, subtract from `live`. If `live` reaches zero, transition the slab to
  `Empty`, stamp `dirty_at`, and put it on the class's empty list.
- **Check** (free): load the bitmap word, verify the bit is set, else abort
  with "invalid or double free". No write.

Slot randomisation, when enabled by the `slot_randomize` tunable, claims from
a random word rather than the lowest one and shuffles the claimed batch before
it goes into the cache. It is off by default because it fights the
fragmentation policy in section 8.

## 5. Per-class state and slab selection

Each `(arena, class)` pair has:

```
struct ClassState {
    lock:        Mutex,          // futex-based, only taken on slab transitions
    partial:     TwoLevelBitmap, // slab numbers with at least one clear bit
    empty:       TwoLevelBitmap, // slabs with live == 0, not yet purged
    high_water:  u32,            // highest slab number ever committed
    stats:       ClassStats,     // curslabs, curregs, nonfull_slabs, allocated
}
```

The two-level bitmap over slab numbers gives "lowest slab with a free slot" in
two `tzcnt` instructions. An arena's sets (three per class) are views of one
interleaved table per arena, logical word `i` of set `s` at word
`i * 156 + s`, so the words a small heap touches (word 0 of each level of the
few sets it uses) share two pages per arena instead of one or two pages per
`(arena, class)`; measured as 2.7 MiB of alloc-testN's 16-thread RSS at
16 arenas (`bench/results/2026-09-18-*-gce-arena-rss.md`). Level 1 rows
are committed with the table, level 0 rows as a prefix while slab numbers
grow. Refill takes the lock, reads the lowest partial slab,
releases the lock, then claims from that slab's bitmap without the lock.
Contention on the same slab between two threads is resolved by the atomic
claim. When a slab's bitmap becomes completely set, the thread that observed
it clears the slab's partial bit under the lock. When a release clears bits in
a slab that was full, the releasing thread sets the partial bit under the lock.

The lock is taken roughly once per 64 allocations per thread in steady state.
That is the only lock on the allocation path.

When there is no partial slab, take the lowest `Empty` slab if one exists,
else the lowest `Purged` slab, else commit `high_water + 1`. Committing means
`mprotect` of the slab's bytes to read-write plus, when guards are enabled,
`MADV_GUARD_INSTALL` on the first page of the next slab-sized stride if the
class is configured with guard slabs. Fresh slabs are zero from the kernel.

Address-ordered first fit is deliberate. It is what jemalloc does and it is
what keeps long-lived heaps dense and makes defragmentation converge.

## 6. Thread heaps and caches

### 6.1 Thread-local access

One pointer-sized TLS slot, declared in `.tbss` via `global_asm!`, read and
written by two small `asm!` sequences:

- x86_64: `mov rax, [rip + challoc_tls@GOTTPOFF]; mov rax, fs:[rax]`
- aarch64: `adrp x0, :gottprel:challoc_tls; ldr x0, [x0, :gottprel_lo12:challoc_tls]; mrs x1, tpidr_el0; ldr x0, [x1, x0]`

This is the initial-exec model. glibc reserves static TLS for objects present
at program start, which covers both linking against `libchalloc.so` and
`LD_PRELOAD`. It does not cover `dlopen` of challoc after start; that is not a
supported configuration and init aborts with a message if static TLS cannot be
obtained, which glibc reports as a `dlopen` failure rather than a crash.

A Cargo feature `nightly-tls` may replace the asm with `#[thread_local]`.

### 6.2 Heap structure

```
struct Heap {                                // repr(C); the header array at a fixed offset
    tid:         u32,
    arena:       u16,                        // default arena for MALLOCX without ARENA flag
    rng:         u64,                        // xorshift state seeded from getrandom
    stats:       ThreadStats,                // allocated/deallocated counters for thread.allocatedp
    slow_ticks:  u32,                        // counts slow-path entries for periodic work
    others:      *mut ArenaCaches,           // same shape as below, one node per other arena touched
    caches:      [CacheHeader; NCLASSES],    // home arena, inline, one 64-byte line per class
}

struct CacheHeader {                         // one per (arena, class), 64 bytes, line aligned
    len:      u32,
    cap:      u32,                           // 0 until first use; then the configured capacity
    low_water: u32,                          // for GC
    zeroed:   u32,                           // slots[..zeroed] read as zero
    slots:    *mut *mut u8,                  // this class's stack, its own metadata object
    allocs:   u64,
    compact_mask: u32,                       // probe mask, (1 << tcache_compact_shift) - 1, or off
    run_end:  u32,                           // slots[..run_end] is the run of low addresses (6.4)
    ...                                      // moved, probe_refilled, arena, mode
}
```

Heaps and slot stacks live in the metadata region and are allocated by a
small internal bump-and-freelist allocator that is only ever used for
allocator metadata. Heaps are created on a thread's first allocation. The
headers are dense inside the heap (review 2026-09-17 P3, Phase 1.5 task 5):
`malloc` reaches `len` at `heap + 128 + 64 * c` with no cache pointer load,
and a thread's 52 headers share one page instead of one page each. Each
class's stack is its own metadata object, allocated on the class's first
slow path, so a thread that only ever touches five size classes pays for
five stacks. The review's single contiguous slot block per heap was
measured and rejected: a stack inside a 128 KiB metadata object costs
xmalloc-test's freeing threads three to four times the L1D fills
(`bench/results/2026-09-18-*-gce-dense-headers.md`).

Default cache capacity: 1024 slots for classes up to 128 bytes, then halving
every doubling of class size, minimum 8; classes above `tcache_max` (32 KiB,
jemalloc's default) are not cached and go straight to the slab bitmaps.
`tcache_nslots_small_max` and `lg_tcache_nslots_mul` from `malloc_conf`
override the ladder, because Redis sets them. A cache for another arena
(`others`) keeps the ladder for bin classes (up to 4 KiB) and holds nothing
above: a thread caches slots of every arena it frees into, so a foreign
cache's residue is multiplied by the arena count, and with 16 threads and 16
arenas the 8 cached 4..32 KiB slots per (thread, arena, class) were 190 MiB
of mstress's RSS and 8 cached 40 KiB slots 70 MiB of xmalloc-test's
(`bench/results/2026-09-18-*-gce-arena-rss.md`); jemalloc's single
per-thread cache holds one such set. The capacity is larger than jemalloc's 200 because a flush or refill
writes the descriptor lines of every slab it touches, and with shared slabs
(one arena) that invalidates the lines every other thread's `free` check
reads; halving the flush rate halves those misses (Phase 0 acceptance,
`bench/results/2026-09-16-aarch64-fast-path.md`). Redis itself asks for
`tcache_nslots_small_max:1000` (section 12).

### 6.3 Fast paths

`malloc(n)`:
1. `heap = tls()`; if null, slow path (create heap, may init the allocator).
2. `c = class_of(n)`; if huge, slow path.
3. `cache = heap.caches[heap.arena][c]`; if `len == 0`, slow path (refill).
4. `p = slots[--len]`; return `p`.

`free(p)`:
1. If `p` is null return. Classify `p` (section 2). Huge: slow path. Invalid:
   abort.
2. Compute slab and slot; verify `slot * size == offset`; load the class
   bitmap word; verify bit set. Otherwise abort.
3. Optional hardened check: compare `p` against the top 4 cache entries; a
   match is a double free. Full-array scan when `double_free_scan` is set.
4. `heap = tls()`; `cache = heap.caches[owner[slab / group]][c]`; if
   `len == cap`, slow path (flush half). `slots[len++] = p`.

`sdallocx(p, n, flags)`, `free_sized(p, n)`, `free_aligned_sized(p, a, n)`
(the sized path; Phase 1.5 task 4e):
1. If `p` is null return. `c_hint = class_for(n, align)` (`None` for a huge
   size). Compute class, slab and slot as in `free` step 2; if `p` is not a
   slot start in a committed slab, or its class is not `c_hint`, slow path.
2. `heap = tls()`; `cache = heap.caches[heap.arena][c]`; if `len == cap`,
   slow path. `slots[len++] = p`. Neither the bitmap word nor the owner
   word is loaded.
3. Slow path: a slab pointer whose class is not `c_hint` aborts (jemalloc's
   contract, `nallocx(n, flags) == sallocx(p)`); otherwise the exact `free`
   above, so a full cache, a huge run or an invalid pointer gets the exact
   check and its message.

Neither path performs an atomic read-modify-write or takes a lock. `free`
does one load from the class bitmap (and one from the owner array once a
second arena exists), which is the price of exact invalid-free detection
and is also what makes cross-thread frees free of coordination; neither
load touches the slab descriptor.
The sized path skips that load, and with it the check: an invalid or double
free through it is caught when the cache flushes the slot (the release
clears every bit it was handed and aborts on one that is already clear,
section 4), up to a cache's worth of frees later, and until then the slot
can be handed out twice, as under jemalloc. The size check stands in for
the bit test: a stale or foreign pointer that happens to be a slot start of
the right class is the case that slips to the flush. Because the arena is
not read either, the home cache holds slots of any arena on this path; the
flush (6.4) sorts the batch and releases each run of slabs through the
arena the owner array names (`release_owned`), which costs nothing extra
when every slab is the cache's own. Plain `free` keeps step 2's load, so a
double `free` of a slot that has left the cache aborts at the call.
`MALLOCX_ARENA(a)` with the default cache pops whatever the cache holds,
as jemalloc's does; `MALLOCX_TCACHE_NONE` releases straight to the slab
with the exact check.

`calloc` clears with `memset` unless the slots came fresh from the kernel,
which is tracked per claim batch with a flag in the cache when the whole batch
came from a `Fresh` or `Purged` slab.

`realloc` returns the same pointer if the new size maps to the same class,
otherwise allocates, copies `min(old, new)`, frees. `rallocx` and `xallocx`
follow jemalloc semantics; `xallocx` can only grow in place within the same
class or, for huge runs, into free following pages.

### 6.4 Slow paths

- **Refill**: claim up to `cap / 2` slots from the current or next partial
  slab (section 5), lowest address on top; the batch is the cache's *run*
  (below) and `low_water` restarts at its length. Count refills in
  `slow_ticks` and run periodic work every 64 slow paths: merge thread
  stats into arena stats, run cache GC if the interval has elapsed, run
  decay if the class's deadline has passed, sample the cgroup (section 11).
- **Flush**: release the `cap / 2` highest-address entries grouped by slab
  and keep the lowest in ascending order (not reversed into a run: a
  descending run under ascending pushes, or an ascending batch under
  descending pushes, is a pathological input for the next flush's sort;
  the run is dropped and rebuilt on demand). Update `live`; slabs reaching
  zero become `Empty`. The selection is by address, not by age, so that
  every flush sends holes to the high slabs while refills and rebuilds
  keep taking the lowest partial slab.
- **Scheduled pop** (the compacting cache): every `2^tcache_compact_shift`
  pops of a class (default 8) are served from the *run* of low addresses
  `slots[..run_end]` (descending, lowest at `run_end - 1`) instead of the
  top: the run's lowest entry is returned and the top entry moved into its
  place. The run is a refill's batch or a *rebuild*'s block: `cap / 4`
  slots claimed from the lowest partial slab and swapped into the bottom
  of the stack (no sort), after flushing the cache down to `cap / 2` if it
  has no room; the rebuild also runs the periodic work, which under a
  balanced churn is the only slow path a class takes. `run_end <=
  low_water <= len` keeps the run intact: a pop that dips below it clips
  both, a push never writes below `len`. The schedule is gated per cache:
  in *probe* mode one pop in 4096 asks the class for its holes
  (`ClassState::holes`, one lock hold) and switches to *compact* mode only
  if they are worth at least 4 slabs of 64 KiB and 1/32 of the class's
  capacity and cache refills claimed less than 2 such slabs' worth since the previous probe
  (`ClassState::refilled`: refills already take the lowest partial slab,
  so a class they are draining has only transient holes; direct
  `MALLOCX_TCACHE_NONE` claims such as Valkey's defragmenter's do not
  count); a rebuild re-checks the holes and drops back to probing once
  the class is dense. Why:
  a LIFO cache hands every allocation the slot just freed, so under a
  balanced alloc/free churn (Valkey's DEL:SET steady state) the cache never
  reaches either boundary, no refill or flush runs, and the slab layout is
  frozen at whatever occupancy the delete phase left; the flush selection
  alone cannot change that (measured on the 2026-09-17 review's churn probe
  and soak, `docs/review-2026-09-17.md` 3.2 and 5). The scheduled pop makes
  one allocation in `2^k` land in the lowest slab with room, so the live
  set migrates downward at `1/2^k` of the allocation rate and the high
  slabs drain, empty and are purged. The gate exists because compacting a
  class costs slab traffic (claims low, releases high) and cold slots that
  larson measured at -15..-22% when paid unconditionally at `k = 3`; a
  dense class pays one counter test per `malloc`. `tcache_compact_shift:0`
  turns the schedule off.
- **Cache GC**: every `tcache_gc_interval_ms` (default 10 s, shortened under
  cgroup pressure) shrink `cap` toward `low_water` and flush the excess, then
  reset `low_water = len`. This is jemalloc's incremental tcache GC in spirit.

### 6.5 Thread exit

A `pthread_key` created at init with a destructor that flushes all caches and
frees the heap back to the metadata allocator. `pthread_setspecific` is called
once at heap creation, guarded against recursion by a per-thread "creating"
flag stored in the TLS slot as a sentinel value; if `pthread_setspecific`
allocates during that window the request is served from a bootstrap slab with
no cache. glibc does not allocate for the first 32 keys, and challoc creates
its key during init, so the recursion path is expected never to run but must
be correct.

## 7. Huge allocations

Requests above the largest medium class are served from the huge region with
page granularity.

- A `TwoLevelBitmap` over pages tracks free pages; allocation is lowest-address
  first fit of `ceil(n / page) + guard_pages` contiguous pages, under the huge
  lock.
- A hash table keyed by start page holds `HugeDesc { pages, usable, arena,
  state, dirty_at }`; `free` of a pointer inside the huge region that is not a
  run start aborts.
- Runs are followed by a guard: `MADV_GUARD_INSTALL` on one page when
  available, `PROT_NONE` (the reserved default) otherwise. Since the region is
  reserved `PROT_NONE`, the fallback costs nothing beyond leaving the page
  unmapped; the guard install variant exists so that a later commit of the
  neighbouring run does not silently remove the guard.
- Alignment requests above page size are satisfied by over-allocating and
  releasing the head and tail pages back to the bitmap.
- Freed runs become `Empty` and decay like slabs. Runs above
  `huge_retain_max` (default 64 MiB) are purged immediately.
- Optional `MADV_HUGEPAGE` on runs of 2 MiB or more when the `thp` tunable is
  `always`; default `never`, because fork-based persistence in Redis and
  Valkey suffers from copy-on-write amplification with huge pages.

## 8. Decay and purge

Each `(arena, class)` and the huge allocator keep an `empty` set ordered by
address, with `dirty_at` per entry. Decay policy is jemalloc's in shape:

- `dirty_decay_ms` (default 10000): an `Empty` slab older than this is purged
  with `MADV_DONTNEED` and becomes `Purged`. Its bytes stop counting toward
  `resident` and start counting toward `retained`.
- `muzzy_decay_ms` is accepted and ignored. challoc does not use `MADV_FREE`
  by default: lazily freed pages remain charged to the cgroup and visible in
  RSS until reclaim, which misleads both autoscalers and Redis's fragmentation
  ratio. A `lazy_free` tunable enables `MADV_FREE` for callers who want it.
- Purges are batched: collect up to 64 ranges and issue one `process_madvise`
  with `PIDFD_SELF` (kernel 6.14) or a pidfd of self (5.10), falling back to
  a loop of `madvise`.
- Decay is driven from the slow path (section 6.4) with a per-class deadline,
  and from the background thread when enabled. Without the background thread
  an idle process does not purge, exactly like jemalloc; Valkey enables the
  background thread by default through `mallctl("background_thread")`.
- `arena.<i>.purge` and `malloc_trim(0)` purge everything immediately.

### 8.1 Background thread

Created only when `background_thread:true` is set via `malloc_conf` or
written through `mallctl`. It is a plain `clone`-based thread created through
`pthread_create` from the slow path that requested it, never from init. It
sleeps on a futex with a timeout equal to the next decay deadline, wakes on
PSI events when armed (section 11), and after `fork` is not recreated in the
child until the child asks for it again. Its stack is allocated from the
metadata region.

## 9. Fork

`pthread_atfork` handlers registered at init: the prepare handler blocks every
signal on the forking thread, then takes every class lock, the huge lock, the
metadata lock, and the background thread's state lock in a fixed order; parent
and child handlers release them and restore the signal mask. The blocking is
needed because glibc restores the caller's mask before the parent handlers
run, so a handler delivered there that forks (glibc's `tst-mallocfork`) would
otherwise re-enter prepare with the locks held by its own thread. The
child additionally marks the background thread as absent. Thread heaps other
than the caller's are left alone in the child; their cached slots stay marked
in bitmaps and are reclaimed when the child process exits, matching what every
other allocator does.

## 10. Initialisation and kernel feature detection

Init is lazy, triggered by the first call to any entry point, and serialised
with an atomic state machine: `Uninit -> Initialising -> Ready`. A second
thread arriving during `Initialising` spins. ld.so may call `malloc` before
constructors run, so there is no reliance on `.init_array`.

Init must not call anything that allocates. Steps:

1. Read `AT_PAGESZ` from `getauxval`. Read secrets from `getrandom`.
2. Probe reservation sizes (section 2) and reserve regions. Name them with
   `PR_SET_VMA_ANON_NAME` when the kernel accepts it.
3. Probe features by attempting them on a scratch page and recording
   `EINVAL`/`ENOSYS`: `MADV_GUARD_INSTALL`, `mseal`, `process_madvise` with
   `PIDFD_SELF`, `MADV_COLLAPSE`. Read `/sys/kernel/mm/transparent_hugepage/
   enabled` lazily only if the `thp` tunable is set.
4. Parse configuration (section 12).
5. Build the region table and `mseal` it and the reservation ranges when
   available. Sealing forbids `munmap`, `mremap`, and `mprotect` changes on
   ranges we never intend to change; committed slab memory is not sealed
   because it must be `mprotect`ed on commit and `madvise`d on purge.
6. Create the thread-exit key and register fork handlers.
7. Read cgroup limits (section 11).

Any pointer passed to `free` before init completes, or that does not classify,
is treated as invalid. ld.so's minimal startup allocator only frees its own
last block, so in practice nothing foreign reaches us; if it ever does, abort
with a clear message rather than corrupt.

## 11. cgroup awareness

Read once at init and re-read on demand:

- Cgroup path from `/proc/self/cgroup` (the `0::` line) and the cgroup2 mount
  from `/proc/self/mountinfo`. Inside a cgroup namespace the path is `/` and
  the mount is `/sys/fs/cgroup`.
- Effective limits: walk from the leaf up to the mount, reading `memory.max`
  and `memory.high` at each level and keeping the minimum of the numeric
  values. `max` means unlimited. `memory.max.effective` does not exist in
  mainline kernels.
- `cpu.max` and `cpuset.cpus.effective` for the effective CPU count, used to
  size per-CPU state in the optional per-CPU mode and to cap the number of
  background purge batches in flight.

All reads use raw `open`/`read`/`close` on fixed-size stack buffers. Absence
of any file means unlimited.

Policy, applied by the slow path and the background thread:

| Usage vs effective limit          | Behaviour                                         |
|-----------------------------------|---------------------------------------------------|
| No limit                          | Defaults                                          |
| < 70% of `min(high, max)`         | Defaults, retention capped at 5% of the limit     |
| 70% .. 90%                        | `dirty_decay_ms` scaled down linearly to 1000     |
| > 90%                             | Purge immediately on empty; caches GC every flush |
| `memory.events` `high` increased  | Full purge once                                   |

`memory.current` is sampled at most every 10 ms, measured with the vDSO
`clock_gettime(CLOCK_MONOTONIC_COARSE)`, and only from slow paths, so the hot
path never sees it. When the background thread exists and the kernel is 6.4 or
newer it arms a PSI trigger `some 150000 1000000` on `memory.pressure` and
polls it with `POLLPRI`; a trigger causes an immediate decay pass. Older
kernels poll `memory.current` on the decay timer.

THP is disabled regardless of the `thp` tunable when the effective limit is
under 1 GiB.

Statistics exposed through `mallctl`: `stats.cgroup.limit`,
`stats.cgroup.high`, `stats.cgroup.current`, `stats.cgroup.pressure_events`.

## 12. Configuration

Sources, in increasing priority:

1. Compiled defaults.
2. The `malloc_conf` global read as a weak symbol, jemalloc style. Redis
   defines `je_malloc_conf` with `lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000`.
3. `/etc/challoc.conf`, first line, if present. Read with raw syscalls.
4. `MALLOC_CONF` and `CHALLOC_CONF` environment variables, read from
   `environ` without `getenv` to avoid any locale or TLS interaction.

Format is jemalloc's `key:value,key:value`. Recognised keys and their
jemalloc names where they exist:

| Key                       | Default | Notes                                   |
|---------------------------|---------|-----------------------------------------|
| `background_thread`       | false   |                                         |
| `dirty_decay_ms`          | 10000   |                                         |
| `muzzy_decay_ms`          | ignored | accepted for compatibility              |
| `narenas`                 | 0       | automatic arenas; 0 = `min(4 * ncpus, 64)`; see 13.3 |
| `tcache`                  | true    | false disables caches                   |
| `tcache_max`              | 32768   | largest cached class size (bytes)       |
| `tcache_nslots_small_max` | 1024    |                                         |
| `lg_tcache_nslots_mul`    | 1       |                                         |
| `tcache_gc_interval_ms`   | 10000   | challoc only                            |
| `tcache_compact_shift`    | 3       | challoc only; one pop in 2^k compacts, 0 = off (6.4) |
| `thp`                     | never   | `never`, `always`                       |
| `lazy_free`               | false   | use `MADV_FREE` for purge               |
| `guards`                  | true    | guard slabs and huge runs               |
| `guard_slab_interval`     | 8       | one guard per N x 64 KiB of slabs       |
| `zero_on_free`            | false   |                                         |
| `slot_randomize`          | false   |                                         |
| `double_free_scan`        | false   | scan whole cache on free                |
| `abort_conf`              | false   | abort on unknown keys                   |
| `cgroup`                  | true    |                                         |
| `junk`, `zero`, `retain`, `oversize_threshold`, `percpu_arena`, `metadata_thp`, `stats_print`, `confirm_conf` | accepted | ignored or mapped where sensible |

Unknown keys are ignored unless `abort_conf` is set. `opt.<key>` is readable
through `mallctl` for every key above.

## 13. API surface

### 13.1 Standard and glibc

`malloc`, `free`, `calloc`, `realloc`, `aligned_alloc`, `posix_memalign`,
`memalign`, `valloc`, `pvalloc`, `free_sized`, `free_aligned_sized`,
`reallocarray`, `malloc_usable_size`, `malloc_trim`, `mallopt` (accepts and
ignores every parameter, returns 1), `mallinfo`, `mallinfo2`, `malloc_stats`,
`malloc_info` (emits a minimal XML document with totals).

All exported symbols are free functions marked `#[unsafe(no_mangle)]` with
`extern "C"` and `pub`, since rustc's version script only exports those.

The C++ operators are exported too, as jemalloc does: the eight
`operator new` forms (`new`, `new[]`, each plain, `nothrow_t`, `align_val_t`
and both) and the twelve `operator delete` forms (plain, `nothrow_t`, sized,
`align_val_t`, aligned `nothrow_t`, sized aligned), under their Itanium ABI
names via `#[unsafe(export_name)]`. Without them a C++ program under
`LD_PRELOAD` pays a second PLT hop through libstdc++'s `operator new` on
every allocation (13% of larson). `operator new` follows the standard
failure protocol: call `std::get_new_handler()` in a loop while one is
installed, then throw `std::bad_alloc` via libstdc++'s
`std::__throw_bad_alloc()`; the `nothrow_t` forms return null instead. Both
libstdc++ entry points are weak undefined references loaded from the GOT, so
a C process that never maps libstdc++ still loads the library under
`BIND_NOW`, and a C++ program with a static libstdc++ that hides them gets
an abort with a message instead of the throw. Sized and aligned `delete`
ignore their extra arguments in Phase 0.

### 13.2 jemalloc

Symbols: `mallocx`, `rallocx`, `xallocx`, `sallocx`, `dallocx`, `sdallocx`,
`nallocx`, `mallctl`, `mallctlnametomib`, `mallctlbymib`,
`malloc_stats_print`, `malloc_conf` (data symbol: defined null and exported so
a program that only references it links, as Ruby's configure probe does;
read through the GOT so a program's own definition interposes; `je_malloc_conf`
is only read, as a weak reference),
`malloc_message` (function pointer data symbol, default writes to stderr).
Each also exported with the `je_` prefix via `#[unsafe(export_name)]`
wrappers. The `jemalloc-soname` Cargo feature names the output
`libjemalloc.so.2` for link-only consumers.

`MALLOCX_*` flags decode as jemalloc's: bits 0-5 `lg_align`, bit 6 `ZERO`,
bits 8-19 `tcache` where 0 means default, 1 means none, `t+2` means explicit
cache `t`; bits 20-31 `arena` where 0 means default and `a+1` means arena `a`.
`MALLCTL_ARENAS_ALL` is 4096.

`sdallocx(p, size, flags)` requires `nallocx(size, flags) == sallocx(p)`,
as jemalloc documents: the class that `size` and `MALLOCX_LG_ALIGN` name is
checked against the pointer's class at the call and a mismatch aborts
(`challoc: sized free of ...: size hint N does not match the allocation`);
the slot bit is not read, so an invalid or double free through `sdallocx`
is detected at the cache flush, not at the call (6.3). A huge run passes
the size check and is judged by the huge allocator. `dallocx` has no size
and is `free` with flags. C23 `free_sized` and `free_aligned_sized` follow
the same rule with the size (and alignment) the allocation was requested
with; `free_sized` on memory from `aligned_alloc`/`memalign` is undefined
in C23 and aborts here when the alignment put the block in a larger class.

### 13.3 Arenas and explicit caches

An arena is a set of `ClassState`s plus a huge sub-allocator sharing the huge
region, plus statistics. Arena 0 exists at init. `arenas.create` allocates a
new one. Slab groups record their arena (section 4). A pointer freed with
plain `free` is returned to a cache keyed by its slab's arena, so memory never
migrates between arenas and per-arena statistics stay exact. This is what
lets Redis exclude its Lua heap from fragmentation accounting.

`narenas` shards automatic allocations across arenas by thread, jemalloc
style. The default resolves to `min(4 * ncpus, 64)` (`ncpus` from the
affinity mask, so cpusets count); a new thread heap is bound to the automatic
arena with the fewest bound heaps (ties: an already built arena, then the
lowest index; jemalloc's `arena_choose_hard`), the binding is released when
the heap is destroyed and moves with `thread.arena`. So `t` concurrently live
threads use `min(t, narenas)` arenas, a single-threaded program stays on
arena 0 and never pays the `free` routing (the `multi_arena` flag is only set
when a second arena is built), and thread churn reuses the arena an exited
thread left. Slab classes above `tcache_max` (32 KiB) are the exception: they
are uncached and served under the class lock anyway, their slabs are 1..4
MiB, and one partial slab per class per arena multiplied by 16 arenas was 60
MiB of mstress's RSS (`bench/results/2026-09-18-aarch64-gce-arena-rss.md`),
so the default path allocates them from arena 0 whatever the thread's arena;
`MALLOCX_ARENA` allocations keep their arena. Slabs are not thread-owned, so
arenas are not needed for correctness or lock scalability; they are needed
for cache locality: under one
shared arena every refill interleaves the threads' slots in the same slabs and
the 10-thread `larson` row paid about 1 L1D miss per operation for it
(`bench/results/2026-09-17-gce-review-counters.md`,
`2026-09-17-{aarch64,x86_64}-gce-thread-affinity.md`; a per-heap hint slab
did not move the counter, only partitioning the slab sets did). The cost is
one partial slab per class per live thread (as in jemalloc), plus the
owner-array load on `free` once a second arena exists.

`tcache.create` allocates a `ClassCache` set owned by no thread; `MALLOCX_TCACHE(t)`
uses it. Explicit caches are not thread safe, matching jemalloc. `MALLOCX_TCACHE_NONE`
bypasses caches and claims or releases directly against slab bitmaps; the
defragmenter relies on this to land allocations in the fullest slabs.

### 13.4 `mallctl` namespace

Implemented keys, chosen from what Redis, Valkey, Aerospike, Rspamd,
ClickHouse, MariaDB, RocksDB, and tikv-jemallocator actually call:

```
version, epoch, background_thread, max_background_threads
opt.<every key in section 12>
arenas.narenas, arenas.nbins, arenas.nlextents, arenas.page, arenas.quantum,
arenas.bin.<j>.size, arenas.bin.<j>.nregs, arenas.bin.<j>.slab_size,
arenas.create, arenas.lookup
arena.<i>.purge, arena.<i>.decay, arena.<i>.destroy, arena.<i>.dirty_decay_ms
stats.allocated, stats.active, stats.metadata, stats.resident, stats.mapped, stats.retained
stats.arenas.<i>.pactive, stats.arenas.<i>.pdirty, stats.arenas.<i>.pmuzzy,
stats.arenas.<i>.small.allocated, stats.arenas.<i>.small.resident,
stats.arenas.<i>.large.allocated, stats.arenas.<i>.resident,
stats.arenas.<i>.bins.<j>.curregs, stats.arenas.<i>.bins.<j>.curslabs,
stats.arenas.<i>.bins.<j>.nonfull_slabs
stats.cgroup.limit, stats.cgroup.high, stats.cgroup.current, stats.cgroup.pressure_events
thread.tcache.flush, thread.tcache.enabled, thread.allocated, thread.allocatedp,
thread.deallocated, thread.deallocatedp, thread.arena
tcache.create, tcache.flush, tcache.destroy
experimental.utilization.query, experimental.utilization.batch_query
config.malloc_conf, config.debug, config.stats
prof.* -> ENOENT
```

Everything else returns `ENOENT`. `arenas.nbins` counts the classes up to
4 KiB (32 with `quantum8`, 28 with `quantum16`), a fixed boundary that does
not move with the minimum slab; the larger classes are slab-allocated all
the same but appear the way jemalloc reports large extents (in `large.*`,
absent from `bins`,
and `(nfree 0, nregs 1, size)` from the utilization queries). Consumers such
as Valkey's defragmenter and its `frag_smallbins` metric are calibrated to
jemalloc's bins, whose slabs are a few pages; with `16 * size` slabs a
handful of live 10 KiB blocks pin a 256 KiB slab that would count as
fragmentation no defragmenter can recover, and one such class per size step
adds up to megabytes on an idle server. The utilization queries report
`size` as `nregs * size`, the region bytes of the slab, so that
`size / nregs` is exactly the class size as Valkey asserts. `epoch` merges every heap's pending
statistics into arena totals. `stats.allocated` is the sum over classes of
`live * size` plus huge usable bytes; `active` counts committed slabs and
huge runs that are not `Empty` or `Purged`; `resident` is `active` plus
`Empty` bytes plus committed metadata; `retained` is `Purged` bytes;
`mapped` is `resident + retained`.

`experimental.utilization.batch_query` takes an array of pointers and fills,
per pointer, `(nfree, nregs, size, bin_nfree, bin_nregs, slabcur_addr)` in
jemalloc's layout. For challoc `nfree` is `nslots - live` from the
descriptor, and the bin-wide figures come from `ClassStats`. Valkey's
defragmenter uses these to decide which allocations to move; the decision
rule is Valkey's, challoc only has to answer honestly and cheaply.

### 13.5 Redis-specific extensions

`get_defrag_hint(void *ptr)` and the `*_with_usize` family exist only in
Redis's patched bundled jemalloc. They are out of scope unless a Wolfi Redis
package appears; Valkey uses the standard `experimental.utilization` keys.

## 14. Hardening summary

Always on:

- Out-of-band metadata; no allocator pointers or sizes inside user memory.
- Size-class isolation in separate randomised regions with unmapped gaps.
- Invalid-free and stale double-free detection on every `free`.
- Region table and reservations sealed with `mseal` when available.
- Guard between huge runs; guard slab every `guard_slab_interval` x 64 KiB
  of slabs.
- Abort, never continue, on any inconsistency. Abort messages go to stderr
  via `write(2)` and then `abort()`.

Off by default, tunable:

- `zero_on_free`, `slot_randomize`, `double_free_scan`, `guard_slab_interval:1`.

Explicitly not attempted: quarantine (defeats defragmentation and costs
throughput), canaries (needs bytes inside the block), MTE (only AmpereOne
exposes it in servers and glibc 2.44 removed its own support). Type-isolated
heaps via Clang's `-fsanitize=alloc-token` are a plausible later addition
because Wolfi controls compiler flags, tracked in [plan.md](plan.md).

## 15. Optional per-CPU mode

Deferred. When `percpu_arena:percpu` is set and `__rseq_size` covers the
`mm_cid` field (glibc 2.41+, kernel 6.3+), thread caches are replaced by
per-concurrency-id caches indexed by `mm_cid` read from glibc's registered
rseq area. This bounds cache memory by the number of runnable threads rather
than the number of threads, which matters for services with hundreds of mostly
idle threads. The implementation must only read the rseq area and must use
proper `rseq_cs` critical sections for the pop and push; writing to
kernel-owned fields is what broke tcmalloc on Linux 6.19.

## 16. Observability

- `malloc_stats_print` emits a human-readable report in jemalloc's general
  shape: totals, per-arena, per-class table with `size nregs curslabs curregs`,
  cgroup section. The per-class table also carries the compaction
  diagnostics of section 6.4 (`refilled`, `compacted`, caches that entered
  and left compact mode, probes the refill-rate gate declined) and is
  followed by a per-class occupancy histogram of the non-empty slabs
  (slabs per eighth of `nslots`), so a soak's `MEMORY MALLOC-STATS` shows
  where the live data sits and whether the schedule is running.
- Mappings named `challoc:meta`, `challoc:class-<n>`, `challoc:huge`.
- `CHALLOC_CONF=stats_print:true` prints at exit via an `atexit` handler
  registered lazily.
- Abort messages include the faulting pointer, its classification, and the
  reason.

## 17. Source layout

```
src/lib.rs            crate root, entry-point re-exports
src/sys/mod.rs        raw syscalls (mmap, mprotect, madvise, process_madvise, mseal, prctl, getrandom, futex, clock_gettime, open/read/close, write, abort)
src/sys/features.rs   kernel feature probing
src/sys/cgroup.rs     cgroup discovery and sampling
src/tls.rs            initial-exec TLS accessor
src/layout.rs         size classes, regions, pointer classification
src/bitmap.rs         atomic bitmap and TwoLevelBitmap
src/slab.rs           SlabDesc, claim/release/check
src/class.rs          ClassState, slab selection, decay
src/heap.rs           Heap, ClassCache, fast and slow paths
src/huge.rs           huge allocator
src/meta.rs           metadata region allocator
src/purge.rs          decay driver, batching, background thread
src/arena.rs          arenas, explicit caches
src/config.rs         malloc_conf parsing
src/stats.rs          counters and epoch merge
src/api/libc.rs       standard entry points
src/api/glibc.rs      glibc extras
src/api/jemalloc.rs   *allocx, je_ aliases
src/api/mallctl.rs    control namespace
src/api/print.rs      malloc_stats_print, malloc_info
tests/                integration tests (Linux only, run against the cdylib)
bench/                benchmark drivers and scripts
fuzz/                 randomized alloc/free driver with seeds
docs/
```

Pure logic (`layout`, `bitmap`, size-class math, config parsing, cgroup
parsing) must be testable with `cargo test` on the host under a `std` dev
feature. Everything touching syscalls, TLS, or asm is tested inside a Linux
container.

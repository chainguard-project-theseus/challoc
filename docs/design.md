# challoc design

Status: draft for implementation. Where this document says "must", an
implementation that does otherwise is wrong. Where it says "should" or gives a
number, the value is a starting point to be validated by benchmarks
([benchmarking.md](benchmarking.md)).

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
  (section 6). Nightly `#[thread_local]` with `-Z tls-model=initial-exec`
  would be an acceptable alternative behind a Cargo feature; no such feature
  exists and none may be required.
- Consumers are C programs. There is no Rust `GlobalAlloc` facade in scope.
- Floor and ceiling (section 17). The floor is a compatibility promise:
  glibc 2.44, kernel 5.10, Wolfi's compiler baseline, proven on real
  kernels. The ceiling is the product: the same binary lights every kernel
  and CPU feature the host offers, found at runtime by probing (a syscall
  tried on a scratch page, `AT_HWCAP`, `cpuid`, a system register read
  through `asm!`), never by `-C target-cpu` or a header constant. A feature
  that is on by default when present must be within noise on the reference
  scorecard when absent, and may change only cost, memory and detection,
  never observable behaviour (allocation results, abort conditions). Every
  probe has a fallback and a line in the feature report
  (`challoc_features()`, section 16).

## 2. Address space layout

At init challoc reserves address space with `mmap(PROT_NONE, MAP_PRIVATE |
MAP_ANONYMOUS | MAP_NORESERVE)` and never unmaps it. All memory handed to
callers lives inside these reservations. Committing memory means `mprotect`
to `PROT_READ | PROT_WRITE` on a range of an already reserved region; the
kernel provides zero pages on first touch.

```
+------------------+  metadata region     (reserved, committed on demand)
| heap table       |
| slab descriptors |  per class, dense arrays indexed by slab number
| huge descriptors |
+------------------+
| gap              |  (sealed with mseal where the kernel has it)
+------------------+  class region 0      (randomised offset inside its gap)
| slab | slab | .. |
+------------------+
| gap              |  (sealed)
+------------------+  class region 1
| ...              |
+------------------+
| ...              |
+------------------+  huge region
| run | guard | .. |
+------------------+
```

The parts of the reservations that no code ever commits, purges or
reprotects (the gap on either side of every class region and the alignment
and slide slack around the huge and metadata regions,
`Regions::never_committed`) are sealed with `mseal` at init when the probe
of section 10 says the kernel has it: `munmap`, `mremap`, `mprotect` and
`mmap(MAP_FIXED)` over them fail with `EPERM` for the rest of the process,
so nothing can land a foreign mapping between two class regions or unmap
part of a reservation. The regions themselves cannot be sealed: sealing
forbids exactly the `mprotect` that commits a slab and the `madvise` that
purges it. Each sealed range splits its VMA, so the count grows by at most
two per range (57 ranges at quantum 16: 108 more lines in `/proc/self/maps`,
measured); on kernels without `mseal`, or under a seccomp profile that
filters it (Docker's default), nothing is sealed and nothing else changes.
The region table (`Regions`, inside the `Shared` static in the library's
own data segment) is not sealed either: it shares its pages with locks and
counters that are written at run time, the mapping belongs to `ld.so`, and
`mseal` does not stop writes, only remapping.

Inside the metadata region (`meta.rs`) two things grow towards each other:
objects (heaps, cache slot stacks, node blocks, huge descriptors) from a
bump pointer at the base, committed ahead in 64 KiB chunks, and the
never-freed tables (a class's descriptor array, slot bitmap and owner
array, an arena's set tables, the huge free-page bitmap, the background
thread's stack) reserved downward from the top and committed page by page
from each table's base as its slab numbers grow. Each committed prefix on
the reservation side is its own VMA: its table's uncommitted tail, up to
the next table's base, keeps the kernel from merging it, so a process that
touched every class carried 337 `[anon:challoc:meta]` lines in
`/proc/self/maps` (168 `rw-p`, 169 `---p`) and a forked child inherited
all of them (the PostgreSQL `connect20` row,
`bench/results/2026-09-20-*-postgres-vmas.md`; the probe is
`bench/probes/fork_maps.c`). Since 2026-09-20 a table of at most
`meta::EAGER_MAX` (8 MiB) is not reserved at the top at all: it is carved
from the object side, page aligned and writable from the start, inside the
chunked commit that covers the heaps and caches, so it joins the one
mapping that grows from the base (`Meta::reserve_table`; its owner's
`commit` calls are accounting only). At the preferred layout that takes 56
of the 167 tables (every owner array, every table of the medium classes,
the set tables) for 82 MiB of writable-but-untouched address space per
process, and the region's lines fall from 337 to 225 (a PostgreSQL
backend's from 251 to 173). The small classes'
64..256 MiB descriptor arrays and 128 MiB..2 GiB slot bitmaps stay on the
reservation side, committed page by page: making them writable whole would
put gigabytes of never-touched address space into every core dump (a
private writable mapping with one page in it is dumped whole), under
`mlockall` and `RLIMIT_DATA`, and under PMD pages on a `thp=always` host,
for a fork-cost gain the same probe measured at 10-15% of `fork()` on
aarch64 and nothing on x86_64 even with the whole region as one mapping:
what the child pays for is the page-table sparsity of the touched table
heads (VmPTE 1.3 MiB against glibc's 120 KiB at every cap), which only a
layout that packs the heads (a slab-major interleave of the tables, a
fast-path address formula change) would remove. Eager carving is off
under `vm.overcommit_memory=2` (the kernel ignores `MAP_NORESERVE` there
and would charge every backend the tables' commit) and under any `thp` key
but `never` (whose `MADV_NOHUGEPAGE` on the reservation keeps a table's
first touch to one page), both read on the first table.

Sizes are runtime parameters chosen by probing how much virtual address space
the process can reserve, so the same binary works on 47-bit x86_64, 48-bit
aarch64, and 39-bit aarch64 kernels:

| Parameter          | Preferred     | Minimum      |
|--------------------|---------------|--------------|
| Class region size  | 2^38 (256 GiB)| 2^32 (4 GiB) |
| Gap per class      | 3 x region (stride 4 x) | 1 x region (stride 2 x) |
| Huge region        | 2^40 (1 TiB)  | 2^34         |
| Metadata region    | 2^36          | 2^30         |

Region bases are randomised with `getrandom`. The three mappings (the class
superblock, the huge region, the metadata region) are reserved in a random
order, so their relative placement is not fixed across processes. Inside the
superblock the class regions sit at a fixed stride in class order; what is
random per class is the offset of its region inside its gap (about 17.6 bits
at the preferred layout).

Because every region is a contiguous power-of-two-aligned range, classifying a
pointer is:

```
if   p in class_region[c] for some c  -> small or medium; slab = (p - base_c) >> lg_slab_c
elif p in huge_region                 -> huge; look up descriptor by page index
else                                  -> invalid; abort
```

The class lookup is a comparison against a sorted table of at most 64 entries,
or a direct index when regions are laid out at a fixed stride inside one
superblock. The implementation uses the stride approach: one superblock of
`nclasses * (region + gap)` bytes, class `c` at
`superblock + c * stride + random_offset_c`, so `c = (p - superblock) / stride`
is a shift because the stride is a power of two. The reservation prefers a
stride of 4 x the region, shrinks the region from 2^38 towards 2^32 before it
gives up on that, and only then drops to a stride of 2 x the region
(`layout::reserve_superblock`).

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
| Small  | 512 .. 1024             | 4 .. 16 KiB, or the page when larger (up to 64 KiB) | 4 .. 25 (64 .. 128 at 64 KiB) |
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

On a kernel whose page is larger than a class's slab (16 and 64 KiB pages on
aarch64) the class takes the page as its slab size, up to 64 KiB, decided
once at init from the probed page size (`layout::adopt_page_geometry`, plan
Phase 2.5 task 4e). At 64 KiB the five classes have 64 KiB slabs of 128, 102,
85, 73 and 64 slots (jemalloc's `regs` for 512 and 1024 at `LG_PAGE 16`); at
16 KiB, 16 KiB slabs of 32, 25, 21, 18 and 16 slots (640 and 768 already had
them); at 4 KiB nothing changes. Every `Empty` slab is then one purge unit
and the page groups of section 5 have nothing to do: with sub-page slabs the
Valkey soak at 64 KiB pages ended its delete phase with 79.5 MiB of `Empty`
4..16 KiB slabs in mixed page groups that never became idle, `active` equal
to jemalloc's and RSS 224.9 MiB against 146.1
(`bench/results/2026-09-19-aarch64-64k-valkey-soak.md`); with page-sized
slabs the same soak is in `2026-09-19-aarch64-64k-valkey-soak-2.md`.
Everything keyed off the slab size follows from the table entry: `nslots`,
`slab_bits`, `slabs_per_line`, the descriptor stride (the header for these
classes on every page size, checked at compile time in `slab.rs`), the
compaction unit, the guard interval (unchanged, the group is already 64 KiB),
`arenas.bin.<j>.slab_size` and `nregs`, the utilization queries; `BIN_MAX`
and the cache-capacity ladder do not move. The form is the table itself,
rewritten in place at init (only `lg_slab`, `nslots` and `slab_bits` of the
raised classes; sizes and reciprocals are constant), not a pointer to one of
two tables on `Shared` and not a shift applied where `lg_slab` is read: the
`free` and `sdallocx` fast paths load the class line from a link-time
constant address exactly as they did from the compile-time table (the sized
path's class comes from the size hint, so a table pointer would have put a
dependent load in front of its only geometry load), and the disassembly of
the exported fast paths is instruction for instruction the same on 4 KiB
hosts (`bench/results/2026-09-19-page-sized-slabs.md`). The table lives in
writable memory as a result, like the `Regions` the same paths already
trust.

The 64 KiB slab is also a folio size on the kernels that enable multi-size
THP (plan Phase 2.5 task 9, section 17): under `thp:auto` a class whose slab
can hold an enabled folio size below the PMD (every class from 64 KiB slabs
up when `hugepages-64kB` is enabled) has its slab groups advised with
`MADV_HUGEPAGE` at hand-out, so the first touch of a slab maps one 64 KiB
folio (one TLB entry on arm64's contiguous-PTE hint, 16 fewer page faults
and one zeroing pass per slab) and nothing about the geometry changes; the
4..16 KiB slabs cannot hold one and are left alone.

Slab size per class is therefore a table fixed at init. Each class region is
divided into slabs of that class's slab size, so slab number and slot index
are:

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
form a dense array indexed by slab number at a per-class stride of 16 to 64
bytes (`max(16, 64 / slabs_per_line)`, so that the descriptors of the slabs
sharing one bitmap line fill whole descriptor lines), committed on demand as
the region grows (a table of at most `meta::EAGER_MAX` is carved writable
from the metadata region's object side instead and the commits are
accounting only, section 2). Descriptor layout, a fixed 16-byte header:

```
struct SlabDesc {
    state:     AtomicU8,     // Fresh, Active, Empty, Purged
    shadow:    AtomicU8,     // state + 1, written with every change; check_zero tests it
    arena:     AtomicU16,
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
a slot back to a cache of its arena. The word and the descriptors' arena
field are written together only when a group is handed out
(`take_group`), which first checks that the two agree (both zero for a
never-touched group) and aborts on a disagreement, and when an idle group
changes arena (13.3, `adopt_idle_group`, under both arenas' class locks).
Nothing else tracks idleness: a group is idle when every committed slab of
it is in its owner's `empty` or `purged` set, which the taker reads under
the owner's lock. Because ownership can change, a slab number carried from
a lock-free step into a lock hold (`reopen`, `on_live_zero`) is checked
against the owner word under the lock before the arena touches its sets;
live slots pin a slab, so nothing else crosses that gap. A group spans at most 2 MiB of the
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
  (`invalid or double free of <p> (class, slab, slot): slot is not
  allocated`; a slot start in a guard slab or in a slab never allocated from
  gets its own message). No write.

Slot randomisation (`slot_randomize`, opt-in, Phase 2 task 3): a cache
refill or run rebuild (`ClassState::claim_random`) plans the same slabs and
reservations as the ordered claim, but the bit scan of a slab starts at a
word drawn from the heap's xorshift instead of the lowest one, and the
claimed batch is shuffled with the same generator before it becomes the
cache's stack (or its run, 6.4). The shuffle changes the order of the
batch, not the set of slots, so a `Fresh` or `Purged` slab's prefix claim
still yields a batch known to be zero. The generator is per heap, seeded
from `getrandom` on the heap's first randomised claim (no syscall per heap
creation in the default configuration); direct claims (`MALLOCX_TCACHE_NONE`,
threads without a heap) stay address-ordered. Off by default because it
fights the fragmentation policy in section 8 and the compaction in 6.4:
with it on, the scheduled pop serves a random slot of the lowest slab with
room rather than its lowest slot, which still moves the live set into that
slab but no longer fills it from the bottom. Priced in
`bench/results/2026-09-18-*-gce-tunables.md`.

## 5. Per-class state and slab selection

Each class has one process-wide `SlabTable` (`class/`): the class's
descriptor array, slot bitmap and owner array, and, behind its own lock, the
commit watermark and the cursor that hands slab groups to arenas (section 4).
Each `(arena, class)` pair then has a `ClassState` over that table:

```
struct SlabTable {                    // one per class, shared by every arena
    class, lg_group, base, nslabs, desc_base,
    states,                           // every arena's ClassState over this table, a list (13.3)
    inner: Lock<{ descs, bits, owners, // metadata reservations, committed on demand
                  committed_slabs, next_group, guards }>,
}

struct ClassState {                   // one per (arena, class)
    class:        u16,
    arena:        u16,
    table:        *const SlabTable,
    gate:         purge::Decayer,     // dirty_at of the oldest Empty slab (read lock-free by decay) and the next walk's deadline
    inner: Lock<{                     // the class lock, taken on slab transitions only
        partial:    Set,              // slab numbers with at least one clear bit
        empty:      Set,              // slabs with live == 0, not yet purged
        purged:     Set,              // slabs purged and still owned by this arena
        stats:      ClassStats,       // committed, guards, empty, purged, nonfull
        group_next, group_end,        // this arena's current slab group (section 4)
    }>,
    claimed, released, refilled,      // slot counters: curregs = claimed - released
    compacted, compact_on, compact_off, gate_refill,   // compaction diagnostics (6.4, 16)
}
```

The lock (`lock.rs`, every `Lock` and `RawLock` in the allocator: the class
locks, the table locks, the huge, metadata and statistics locks, the
background thread's state) is a futex mutex in the shape of Drepper's
"mutex, take 3": one `u32` that is unlocked, locked or contended, one CAS to
acquire uncontended, one swap to release, `FUTEX_WAKE` only when the word
says a waiter may sleep. A contended locker spins 100 rounds before
`FUTEX_WAIT`; slow-path critical sections are a few dozen instructions, so
the holder is usually gone within them. One round is one poll of the word
and one `pause` (x86_64) or `isb` (aarch64).

There is no per-arena high-water mark: the class's commit watermark lives in
the `SlabTable` and each arena commits the slabs of its current group as it
uses them.

The two-level bitmap over slab numbers (`Set`) gives "lowest slab with a free
slot" in two `tzcnt` instructions. An arena's sets (three per class) are
views of one interleaved table per (arena, slab size) (`SetTable`; classes
with the same slab size have the same slab count): logical word `i` of set
`s` sits at word `i * 3n + s`, `n` the number of classes with that slab size,
so the words a small heap touches (word 0 of each level of the few sets it
uses) share one page per (arena, slab size) instead of one or two pages per
`(arena, class)`; measured as 2.7 MiB of alloc-testN's 16-thread RSS at
16 arenas (`bench/results/2026-09-18-*-gce-arena-rss.md`). Level 1 rows
are committed with the table, level 0 rows as a prefix while slab numbers
grow. Refill takes the lock, reads the lowest partial slab,
releases the lock, then claims from that slab's bitmap without the lock.
Contention on the same slab between two threads is resolved by the atomic
claim. The one exception is a slab that was `Fresh` or `Purged` at
selection: its slots (the lowest `n` bits; `live` is zero, so nothing
contends) are claimed in the same lock hold, before the slab enters the
partial set, and the refill reports them zero. Decided after the lock is
dropped, a bit could be one that another thread claimed from the
now-partial slab, handed to the program and got back dirty in between, and
`calloc` would skip its memset over it (the calloc race, Phase 1.5 task 7:
the zero guarantee is for slots nothing else has touched, not for the
slab's memory at selection time). Slots of an `Active` slab are never
reported zero, so their bits stay outside the lock. When a slab's bitmap becomes completely set, the thread that observed
it clears the slab's partial bit under the lock. When a release clears bits in
a slab that was full, the releasing thread sets the partial bit under the lock.

The lock is taken once per refill, i.e. once per `cap / 2` allocations per
thread in steady state (512 for the small classes at the default capacity).
That is the only lock on the allocation path.

When there is no partial slab, take the lowest `Empty` slab if one exists,
else the lowest `Purged` slab, else commit the next slab of the arena's
current group, else, with `group_handoff:true` (an opt-in, off by default,
13.3), take over an idle group of another arena (its `Empty` and `Purged`
slabs join this arena's sets and its uncommitted slabs become the current
group), else take the next group from the class table.
The handoff visits the other arenas' states in turn and `try_lock`s each
donor's class lock while holding the taker's, never waiting (a busy donor is
skipped, the fresh group is the fallback, so no thread blocks while holding
a class lock and the fork handlers' order is untouched); under that lock it
scans the donor's `empty` and `purged` sets group by group (at most
`MAX_ADOPT_TRIES` groups per donor) for one whose every committed slab is in
them. The slab transitions keep no extra state for
this: a first form that counted a group's `Active` slabs and kept a
class-wide idle set under the commit lock cost `xmalloc` 9% on the fast
tier, one commit-lock hold and one shared-line write per slab transition.
Committing means
`mprotect` of the slab's bytes (or of its page group, for slabs smaller than
a page) to read-write. Fresh slabs are zero from the kernel. With the `thp`
key (`thp.rs`, sections 7, 10 and 17) the commit path grows two branches,
both off under the default `never`: `take_group` issues `MADV_HUGEPAGE`
over the group's range (still `PROT_NONE`; the flag survives the split the
commit makes) when the class qualifies under `auto`, and `commit_slab`
under `always` with the PMD size enabled, and no larger than 2 MiB (the
PMD of a 4 KiB page kernel; at 16 and 64 KiB pages it is 32 and 512 MiB
and a chunk per class would be the RSS floor, so `always` there takes the
folio path over the sizes below the PMD or is inert, `thp.rs`
`LG_PMD_CHUNK_MAX`), commits and advises the
PMD-sized chunk around the slab, clipped to the region, so the first touch
maps a 2 MiB page (every slab of the chunk re-issues the two calls, no-ops
in the kernel after the first, since which arena reaches a chunk first is
not known; the counters take the one that starts it). A guard slab inside
such a chunk is left unused but unguarded: a guard marker (or a `PROT_NONE`
page) inside a 2 MiB page splits it back into small pages the moment the
arena reaches the guard slab, which is what the first form of the branch
did to every chunk (`AnonHugePages` 0 after 8 MiB touched, `thp_split_pmd`
one per chunk).

The cost of sub-page slabs on a large-page kernel, measured (Phase 2.5
task 3, `bench/results/2026-09-19-matrix.md` F1): a page group is purged
only when every slab in it is idle, and a workload that frees at random
leaves `Empty` 4 KiB slabs scattered through 64 KiB groups that are never
all idle. In the oscillation test on `6.8.0-139-generic-64k` the 1 KiB
class (4 KiB slabs, 4 slots) holds 571 such slabs at the top of the swing,
2.2 MiB of 480 MiB live, that a 4 KiB kernel has purged (`retained` 5 MiB
against 2 + `empty` 2); `dirty_decay_ms:0` does not remove them. It is a
granularity trade-off, not retention, and no policy timing changes it;
only page-sized slabs for those classes or allocation steering by
page-group density would. The Valkey soak on the same kernel (Phase 2.5
task 4d, `bench/results/2026-09-19-aarch64-64k-valkey-soak.md`) sizes it
for the real consumer: at the end of the delete phase 79.5 MiB of `Empty`
slabs of the five classes sit in mixed page groups (`active` 130.1 MiB,
equal to jemalloc's 131.4; RSS 224.9 MiB against jemalloc's 146.1, the
post-delete criterion failed by that amount alone), so page-sized slabs
when the page is larger than the slab became plan Phase 2.5 task 4e, done
(section 3): with the five classes at 64 KiB slabs the same soak
(`bench/results/2026-09-19-aarch64-64k-valkey-soak-2.md`) ends its delete
phase with `empty` 0 in every bin, every emptied slab purged and `resident
- active` at the metadata (9.9 MiB), RSS 174.6 MiB against jemalloc's
145.5 (from 224.9). The gap left is `active` (157.2 against 128.7 MiB):
survivors in 64 KiB slabs at 36..45% utilisation where jemalloc's, at the
same slab size for 512 and 1024 bytes, are at 55%, so it is compaction,
not geometry (plan task 4f). Page groups remain for a faked page in the
tests and for a page above 64 KiB, which no supported kernel has.

Guard slabs: with `guards` on, every `guard_slab_interval`-th 64 KiB of slabs
is a *guard slab* that is never handed out (`SlabTable::is_guard_slab`,
arithmetic on the slab number: a shift to the page group, an add and a mask).
The interval is a power of two (the default 8 is; a configured value that is
not rounds up to the next one, is reported as a configuration issue, which
`abort_conf` turns into an abort, and `opt.guard_slab_interval` returns the
rounded value), so the test costs no division: the runtime `%` it used to be
was a 32-bit `divl` twice per Valkey SET on the `sdallocx` fast path and the
largest single term of that path's instruction window on small-L3 hosts
(`bench/results/2026-09-19-8481c-valkey1-attribution.md` section 7). The
guard density per byte is unchanged. When its page group is committed it is made
read-write like its neighbours, so the VMA merges, and `MADV_GUARD_INSTALL`
is applied to its first page so a write running off the previous slab
faults; if the install fails the whole slab is left `PROT_NONE` instead. This
only exists when the kernel has `MADV_GUARD_INSTALL` (6.13+): on an older
kernel `Shared::init` forces the interval to 0 and there are no guard slabs
at all, because a `PROT_NONE` slab every eighth slab would split each class
region into thousands of VMAs. Guard slabs are separate slabs, not the first
page of the next stride.

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

A Cargo feature could replace the asm with nightly `#[thread_local]`; none is
defined today.

Signed heap pointer (plan Phase 2.5 task 7, `sys::pac`). With
`pac_pointers:true` on an arm64 host whose kernel exposes pointer
authentication (`HWCAP_PACA` and `HWCAP_PACG`, Armv8.3 FEAT_PAuth with
per-process keys; `Shared::pac()`, the `PUSH_PAC` bit of `push_flags`,
section 6.3 step 3; off by default, section 14), the slot holds the heap pointer
signed with the data key A, `pacda heap, tpidr_el0`: the thread pointer is
the modifier, so a heap pointer copied into another thread's slot fails,
and the accessor loads `tpidr_el0` anyway to reach the slot, so the
`autda` on the way out costs no extra load (`tls::get_tp` returns both).
The sentinels (`TLS_CREATING = 1`, `TLS_EXITED = 2`, and null) are never
signed: the compare against them comes first, and a signed pointer keeps
its low 48 bits, so it stays above them. A corrupted slot value faults at
the `autda` (FEAT_FPAC: `SIGILL`) or comes out non-canonical and faults on
the first header load (original PAuth: `SIGSEGV`). x86_64 has no PAC and
the code compiles to the plain accessor there; a CPU without the extension
(Altra) takes the same path through the predicted branch on the flag.

### 6.2 Heap structure

```
struct Heap {                                // repr(C); the header array at offset 128
    shared:      *const Shared,
    tid:         u32,
    arena:       u16,                        // home arena: plain malloc allocates here
    slow_ticks:  u32,                        // counts slow-path entries for periodic work
    last_gc:     u64,                        // monotonic ns of the last cache GC
    uncached:    ThreadStats,                // bytes that never went through a cache (huge, direct)
    others:      *mut ArenaCaches,           // one node of [CacheHeader; NCLASSES] per other arena allocated from explicitly (MALLOCX_ARENA, thread.arena), plus the FOREIGN node plain frees of other arenas' slots fill (6.3 step 4)
    snapshot:    ThreadStats,                // thread.allocated/deallocated as of the last read
    tcache_enabled: bool,                    // thread.tcache.enabled
    bound:       bool,                       // counted in the home arena's load (thread heaps only)
    caches:      [CacheHeader; NCLASSES],    // home arena, inline, one 64-byte line per class
}

struct CacheHeader {                         // one per (arena, class), 64 bytes, line aligned
    len:      u32,
    cap:      u32,                           // 0 until first use; then the configured capacity
    low_water: u32,                          // for GC
    zeroed:   u32,                           // slots[..zeroed] read as zero
    slots:    *mut *mut u8,                  // this class's stack, its own metadata object
    allocs:   u64,                           // pops served since creation
    compact_mask: u32,                       // probe mask, (1 << tcache_compact_shift) - 1, or off
    run_end:  u32,                           // slots[..run_end] is the run of low addresses (6.4)
    moved:    u64,                           // refilled - flushed (for thread.deallocated)
    probe_refilled: u32,                     // the class's refill counter at the previous probe
    arena:    u16,
    mode:     u16,                           // off, probe, compact (6.4)
}
```

The per-thread byte counters are not kept per operation: `allocs` is bumped
on the header line a pop writes anyway and the free count follows from the
stack arithmetic (`heap/`, `Heap::stats`). An explicit cache
(`tcache.create`) is a `Heap` owned by no thread.

Signed entries (plan Phase 2.5 task 7). With `Shared::pac()` set, every
pointer in `slots[..len]` is stored signed with the data key A and the
stack's base address `slots` as the modifier (`pacda p, slots`): the push
signs, the refill and the run rebuild sign the batch they claimed, and the
pop, the scheduled pop and the flush authenticate (`autda`). The modifier
is the stack, not the entry's position, so entries move within a stack
(the scheduled pop's swap, the rebuild's rotation, the flush's selection)
as they are; it is loaded by every pop and push anyway (it is where the
entry is read from or written to), so the sign and the authenticate are
one instruction each and a predicted branch on a byte already loaded; it
distinguishes caches (each stack is its own metadata object) and survives
`thread.arena`'s swap of whole header arrays (the stacks stay put). The
flush orders entries by their address bits alone (`!pac::mask()`, the PAC
field found once by `xpacd`, one `and` per compare: the sortedness test,
the selection and the kept sort all take the masked key), authenticates
only the half it releases (`release_owned` needs plain pointers) and
leaves the kept half as it is, so an entry costs one `pacda` on the way in
and one `autda` on the way out, whichever way it leaves; authenticating
the whole stack and re-signing the kept half was measured first and cost
xmalloc-test 15.8% on the C4A (the consumer's flushes take the sorted
path, 1.25k cycles each, and the two passes doubled them). The
double-free scan compares signed against signed, the same test (one key,
one modifier), and so does the straddle compare at the split. `CacheHeader::cached()` and `run()` return the entries as
stored; `CacheHeader::plain` decodes one for diagnostics. A corrupted
entry, whether an attacker's write into the metadata region or a bug,
never comes back from `malloc` usable: with FEAT_FPAC (Neoverse V2, Apple
M2 and later) the `autda` in the pop faults (`SIGILL`); on the original
PAuth (Neoverse N1, Apple M1) the pointer comes out non-canonical (an
error code in the PAC field) and the program's first access to it faults
(`SIGSEGV`); the PAC is 7 bits wide on Linux's default 48-bit VA with
top-byte-ignore (bits 54:48), so a forged entry passes one time in 128 and
every failure is a crash. `heap/` `corrupted_cache_entry_or_heap_pointer_faults`
is the test, in a forked child.

Heaps and slot stacks live in the metadata region and are allocated by a
small internal bump-and-freelist allocator that is only ever used for
allocator metadata (the object side of section 2, one `rw-p` mapping
shared with the small tables carved from it). Heaps are created on a
thread's first allocation. The
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
above. One function, `Config::cap_for(home, arena, class)`, is the whole
rule (zero means "never cached here", and a heap creates no `others` node
for such a class); `Config::shared_class` (above `tcache_max`) is the
default path's one routing decision (13.3). The reason for the foreign
limit: a thread caches slots of every arena it frees into, so a foreign
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

Cache geometry (plan Phase 2.5 task 12). The two constants above were
chosen on the reference CPUs, both with a 2 MiB L2 per core (Axion 64 KiB
L1D, Emerald Rapids 48 KiB); the CPU matrix (task 4,
`bench/results/2026-09-19-cpu-matrix.md`) has 1 MiB L2s (Cascade Lake,
Genoa, Altra) and a 512 KiB one (Milan), 32 KiB L1Ds on the x86 parts. The
model of what a cache costs in cache footprint: the fast path touches one
line of the header and one of the slot stack per class whatever the
capacity, so the L1D does not bind; what scales with the capacity is what
a flush or refill of `cap / 2` slots streams, the half stack (4 KiB at
1024) and, above all, the descriptor line and bitmap word of every slab it
touches, at worst one 64-byte line per slot (32 KiB at 1024, 1.6% of a
2 MiB L2, 6% of a 512 KiB one), which has to stay L2-resident next to the
program's working set for the flush not to miss on every slot; the largest
cached class bounds what a cache of `MIN_CAP` slots of it keeps warm (8 x
32 KiB at the default `tcache_max`). The rule that follows: the ladder's
top and `tcache_max` halve per full halving of the L2 below 2 MiB (1 MiB:
512 slots and 16 KiB; 512 KiB: 256 and 8 KiB), floors 128 slots and 8 KiB,
an unknown L2 keeps the constants, a key a source set keeps its value, and
at 2 MiB and above the rule gives the constants exactly, so the reference
scorecards would not move. It was implemented, priced on the Cascade Lake
(32 KiB / 1 MiB) and the Altra (64 KiB / 1 MiB) against the constants
through `MALLOC_CONF`, and dropped
(`bench/results/2026-09-19-lane-c-spin-and-geometry.md`): the rows it was
meant to help do not move or move the wrong way (Cascade Lake, 512 slots
against 1024: larson1 +1.3% with disjoint score lists, larson10 +5.4%
inside its noise, valkey1 +0.7%, redis and alloc-testN flat; 256 slots:
larson1 +6.2%), and what it buys is RSS (larson10 80 to 69 MiB, the
smaller residue of caches), which the ladder was never sized for. The
constants stand on every CPU; `tcache_nslots_small_max:512` is the knob for
a deployment that wants the residue back on a small-cache part.

### 6.3 Fast paths

`malloc(n)`:
1. `c = class_of(n)`; if huge, slow path.
2. `heap = tls()`; if null or a sentinel, slow path (create heap, may init
   the allocator).
3. `cache = heap.caches[c]` (the home arena's header line); if `len == 0`,
   slow path (refill).
4. `p = slots[--len]` (`autda p, slots` when `PUSH_PAC`, 6.2); return `p`.

`free(p)`:
1. If `p` is null return. Classify `p` (section 2). Huge: slow path. Invalid:
   abort.
2. Compute slab and slot; verify `slot * size == offset`; load the class
   bitmap word; verify bit set. Otherwise abort.
3. The opt-in hardening, one byte (`Shared::push_flags`) on the line step
   2's `superblock`/`lg_stride` come from, zero by default on a CPU
   without pointer authentication, so the default push tests it once and
   branches past both; with `pac_pointers:true` on an arm64 host with PAC
   the byte is `PUSH_PAC` alone, the second compare, and the push stores
   `pacda p, slots` (6.2). With `double_free_scan`:
   compare `p` against the top 4 cache entries; a
   match is a double free whose first copy is still cached (the bit test
   cannot see it) and aborts before the push. The same compare guards the
   sized path's push. Off by default: priced at +3.1% / +3.7% on larson1
   and up to +14% on the Valkey model on x86_64
   (`bench/results/2026-09-18-*-gce-detection-gaps.md`), above the 1% the
   plan set for making it the default. With `zero_on_free`: `memset` the
   slot's class-size bytes (known from step 2, or from the size hint on
   the sized path) to zero before the push, whether or not the push then
   succeeds (a slot that goes on to the flush path is not cleared twice);
   a direct release (`MALLOCX_TCACHE_NONE`) clears the same way and a
   huge free clears the run's usable bytes before the release. Off by
   default: a `memset` per free is more than the 3% budget on Valkey's
   1 KiB values (review 2026-09-18 section 5.1); priced in
   `bench/results/2026-09-18-*-gce-tunables.md`.
4. `heap = tls()`; with several arenas load `owner[slab / group]` and, if it
   is not the heap's home arena, take the cold path (`free_other`, which
   pushes onto the heap's one *foreign* cache for the class, `Heap::others`
   keyed `FOREIGN`, creating it on first use); otherwise `cache =
   heap.caches[c]`; if `len == cap`, slow path (flush half). `slots[len++]
   = p`. The foreign cache is flush-only: nothing pops from it, its
   capacity is half the ladder's and it is released whole when full (the
   same batch as a home cache's half flush, so the release cost per slot
   is the home cache's, and a quarter of the ladder is held on average
   instead of three quarters), its flush routes each run of slabs to the
   slab's arena (`release_routed`, 6.4), and there is one per thread, so a
   thread that frees other threads' memory holds at most half a cache's
   worth per class of it, less than jemalloc's tcache. Until 2026-09-21
   such a free went onto a cache per `(arena, class)`
   (one node per arena a thread freed into), which the default path never
   pops from, so a slot left it only by a flush or the cache GC: a 16-thread
   Ruby process, whose GC sweep frees every thread's garbage from whichever
   thread runs it, held ~200 MiB of dead slots in those 16 x 16 caches
   (three quarters of their capacity on average, the sawtooth between a
   half flush and full), twice jemalloc's RSS
   (`bench/results/2026-09-21-*-ruby-rss-attribution.md`). Pushing every
   foreign slot onto the home cache instead, jemalloc's exact shape, was
   measured first (`4dc352c`): it hands a thread that inherits another
   thread's blocks that arena's slots back for its own allocations, and
   larson10 / larson16 on the Xeon lost 10% / 6%
   (`2026-09-21-x86_64-ruby-rss-ab-split.md`; the Axion did not move), so
   the arena locality of the home cache is kept and only the foreign side
   is one cache instead of one per arena. The per-arena `others` nodes
   remain for explicit `MALLOCX_ARENA` allocations and `thread.arena`.

`sdallocx(p, n, flags)`, `free_sized(p, n)`, `free_aligned_sized(p, a, n)`
(the sized path; Phase 1.5 task 4e):
1. If `p` is null return. `c_hint = class_for(n, align)` (`None` for a huge
   size: slow path). The hint names the region, so the class is not
   derived from the pointer a second time: `rel = p - class_base[c_hint]`;
   if `rel` is not below the region length (which covers every other
   class's region, the gaps and the other reservations), or `rel`'s offset
   in its slab is not a slot start (the reciprocal multiply of `free` step
   2), slow path. If the slab is a guard slab, slow path: one 8-byte load
   from a dense per-class pair (`Shared::guard_test[c]`: `(slab +
   slabs_per_page) & mask == 0`, `is_guard_slab`'s shift, add and mask
   folded into an add and a mask, the interval being a power of two). The
   class table line is not touched; whether the slab has been handed out
   is not read (see below).
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
section 4; a release into a guard slab or a `Fresh` slab, one the arena
has not committed or never claimed from, aborts by name), up to a cache's
worth of frees later, and until then the slot can be handed out twice, as
under jemalloc. The size check stands in for the bit test: a stale or
foreign pointer that happens to be a slot start of the right class, in a
slab that is not a guard, is the case that slips to the flush. Whether a
handed-out slab is below its arena's commit cursor is not knowable on this
path (the cursor is under the class lock and the arena is not read), so a
slot start in an uncommitted slab of a handed-out group is also caught at
the flush; and since Phase 2.5 task 4c the class table's watermark is not
read either (it was the path's only use of that line), so a slot start in
a slab no arena has been handed is caught the same way (`SlabTable::owner`
aborts on a slab past the watermark, by name). Until the flush such a slot
can be popped by `malloc`, and a program that writes to it faults on the
reserved `PROT_NONE` page instead of aborting by name; the exact `free`
and `malloc_usable_size` keep their at-the-call checks. Because the arena is
not read either, the home cache holds slots of any arena on this path; the
flush (6.4) groups the batch by slab and releases each run of slabs through
the arena the owner array names (`release_owned`), which costs nothing
extra when every slab is the cache's own. Plain `free` keeps step 2's load, so a
double `free` of a slot that has left the cache aborts at the call.
`MALLOCX_ARENA(a)` with the default cache pops whatever the cache holds,
as jemalloc's does; `MALLOCX_TCACHE_NONE` releases straight to the slab
with the exact check.

`calloc` clears with `memset` unless the slots came fresh from the kernel,
which is tracked as a per-cache zero prefix: `slots[..zeroed]` are known
zero, set by a refill whose batch came from a `Fresh` or `Purged` slab,
clipped by a push that lands below it and dropped by a flush.

`malloc_usable_size(p)` (and `sallocx`, and the size read of `realloc` and
`rallocx`): classify `p` as `free` step 1 does; for a slab slot test the bit
as step 2 does, from the class, slab and slot the dense arrays gave (the
watermark, `bits_base` and `slab_bits` on the class table line, then the
bitmap word; the slot geometry is not derived a second time from the table
line) and return the class size; a clear bit aborts as a free of that slot
would. A huge run start reads its page count from the huge table. Same
loads as `free` minus the owner word and the cache push.

`realloc` returns the same pointer if the new size maps to the same class,
otherwise allocates, copies `min(old, new)`, frees. `rallocx` follows
jemalloc semantics; `xallocx` never resizes in place (challoc never changes
a block's class or a run's page count in place) and simply reports the
current usable size, `sallocx(p)`, so a caller that needs more falls back
to `rallocx` exactly as jemalloc's contract requires.

### 6.4 Slow paths

- **Refill**: claim up to `cap / 2` slots from the current or next partial
  slab (section 5), lowest address on top; the batch is the cache's *run*
  (below) and `low_water` restarts at its length. Count slow paths in
  `slow_ticks` and run periodic work every 64 of them: cache GC if the
  interval has elapsed, then decay if a class's deadline has passed (or the
  start of the background thread that will do it). Thread statistics are
  merged by `epoch`, not here; the periodic work also takes the rate-limited
  cgroup sample (section 11), which is two small file reads at most once
  per 10 ms process-wide.
- **Flush**: release the `cap / 2` highest-address entries grouped by slab
  and keep the lowest (the run is dropped and rebuilt on demand). Update
  `live`; slabs reaching zero become `Empty`. The selection is by address,
  not by age, so that every flush sends holes to the high slabs while
  refills and rebuilds keep taking the lowest partial slab. The split: a
  slot array already in address order (a cache pushed in the order a
  producer's refill batch was handed out; `is_sorted` stops at the first
  inversion) is split directly and its kept half stays sorted; otherwise
  `select_nth_unstable` on the address (O(cap), in place) puts the released
  half on top and an in-place counting pass on the slab number
  (`(p - class_base) >> lg_slab`) groups it by slab when it spans fewer
  than 256 slabs (one slab: no pass; more: a sort of that half). The
  release needs a slab's slots contiguous and nothing more: it
  accumulates each slab's bits in a scratch bitmap and clears them at the
  slab switch, one atomic per touched word, so the order inside a slab is
  free. The kept half is left as the selection leaves it except on the
  first two consecutive unsorted flushes of a cache and every 32nd after
  that, when it is sorted: a cache pushed in order that met one unsorted
  flush gets back onto the sorted path (a scrambled kept half would keep
  it on the selection forever, xmalloc-test +16%), while a cache pushed in
  arbitrary order pays two sorts of half the array and then one in 32. Until
  Phase 2.5 task 1b the flush sorted the whole slot array; under tiny-object
  churn that sort was 8..9% of glibc-simple and 5% of the Python json row
  on both VMs, and the sort's O(n) path on sorted input was what made the
  kept half's order matter (`bench/results/2026-09-19-*-gce-flush-sort.md`,
  which also has the rejected forms: selection plus a sort of the released
  half, a bucket pass over the whole array, the selection alone). Double
  frees: the release aborts on a duplicate inside its batch (two copies of
  one slot land in one slab group, so the second hits a set scratch bit);
  every kept entry is at most the kept maximum and every released entry
  at least that, so a pair that straddles the split, which would otherwise
  release one copy (bit cleared) and hand the other out over a slot a
  refill can claim again, is exactly a released entry equal to the kept
  maximum, and one compare of the lowest released address with it aborts.
  A pair the split keeps together stays cached and consistent until a
  flush releases it (a pass over the kept half was measured at 5..7% of
  xmalloc-test).
- **Scheduled pop** (the compacting cache): every `2^tcache_compact_shift`
  pops of a class (default `k = 3`, one pop in 8) are served from the *run* of low addresses
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
  the class is dense. Under the per-CPU mode (section 15) the run is a
  header of its own per (id, class) rather than the bottom of the stack,
  rebuilt by the same block claim and served one entry per scheduled pop
  through the `pop` section. Why:
  a LIFO cache hands every allocation the slot just freed, so under a
  balanced alloc/free churn (Valkey's DEL:SET steady state) the cache never
  reaches either boundary, no refill or flush runs, and the slab layout is
  frozen at whatever occupancy the delete phase left; the flush selection
  alone cannot change that (measured with the churn probe
  `bench/probes/churn.c` and the Valkey soak). The scheduled pop makes
  one allocation in `2^k` land in the lowest slab with room, so the live
  set migrates downward at `1/2^k` of the allocation rate and the high
  slabs drain, empty and are purged. The gate exists because compacting a
  class costs slab traffic (claims low, releases high) and cold slots that
  larson measured at -15..-22% when paid unconditionally at `k = 3`; a
  dense class pays one counter test per `malloc`. `tcache_compact_shift:0`
  turns the schedule off.
- **Cache GC**: every `tcache_gc_interval_ms` (default 10 s) flush three
  quarters of `low_water`, the entries that went unused since the last GC,
  then reset `low_water = len`. `cap` is fixed; it never shrinks. This is
  jemalloc's incremental tcache GC in spirit. Above 90% of the cgroup
  limit the interval is 0 (section 11): the GC runs at every periodic
  tick, which under a flush-driven workload is every 64 flushes or
  refills; the flush itself is not touched.

### 6.5 Thread exit

A `pthread_key` created at init with a destructor that flushes all caches and
frees the heap back to the metadata allocator. `pthread_setspecific` is called
once at heap creation, guarded against recursion by a per-thread "creating"
flag stored in the TLS slot as a sentinel value; if `pthread_setspecific`
allocates during that window the request is served straight from the slab
bitmaps with no cache (`alloc_direct`). glibc does not allocate for the first 32 keys, and challoc creates
its key during init, so the recursion path is expected never to run but must
be correct.

## 7. Huge allocations

Requests above the largest medium class are served from the huge region with
page granularity.

- A page-granular `class::Set` (the `TwoLevelBitmap` view the arenas' set
  tables use) tracks free pages; allocation is lowest-address first fit of
  `ceil(n / page) + guard_pages` contiguous pages, under the huge lock.
- An open-addressing hash table keyed by page number holds `HugeDesc {
  start, pages, usable, arena, state, dirty_at, prev, next }`: a run is keyed
  by its start page, a free extent by both its first and its last page so
  that a free finds both neighbours and free extents are always coalesced;
  `free` of a pointer inside the huge region that is not a run start aborts.
  The table keeps its load at or below one half through `ensure`; `insert`
  aborts rather than fill the last slot, so a probe for an absent key
  always ends at an empty slot.
- Runs are followed by a guard: `MADV_GUARD_INSTALL` on one page when
  available, `PROT_NONE` (the reserved default) otherwise. Since the region is
  reserved `PROT_NONE`, the fallback costs no memory, only VMAs: the
  `PROT_NONE` page between two committed runs keeps them separate VMAs, so
  the fallback costs one VMA per live huge run (bounded by the number of
  live huge allocations, unlike a `PROT_NONE` guard slab per eight slabs,
  which is why slabs have no such fallback; Phase 2 task 1). The guard
  install variant exists so that a later commit of the neighbouring run
  does not silently remove the guard.
- Alignment requests above page size are satisfied inside a free extent:
  `find` rounds the extent's start (or the allocation mark) up to the
  alignment and takes the run there when it fits below the extent's end;
  the pages before and after the run stay free extents (no over-allocation,
  nothing to give back). An alignment beyond the region is refused.
- Freed runs become `Empty` and decay like slabs. Runs whose usable size is
  above `huge_retain_max` (a `malloc_conf` key, default 64 MiB, readable as
  `opt.huge_retain_max`) are purged immediately.
- The huge tier's decay period is arena 0's (`dirty_decay_ms`, and
  `arena.0.dirty_decay_ms` at run time) unless `huge_dirty_decay_ms` gives
  it one of its own (a `malloc_conf` key, `-1`..`1000000000000` like
  `dirty_decay_ms`; unset, the default, follows arena 0, and
  `opt.huge_dirty_decay_ms` then reads the slab period). The decay driver
  (`purge::decay`) walks the huge extents on that period
  (`Shared::huge_decay_ns`, scaled by the cgroup policy's band like every
  arena's) through the huge `Decayer`; `0` purges a freed run as it is
  freed, whatever its size (the `huge_retain_max` path, `Huge::set_purge_on_free`),
  `-1` leaves freed runs `Empty` for `malloc_trim(0)` and `arena.<i>.purge`
  alone. The report's `Huge:` line names the huge period when it differs
  from arena 0's. Why a separate period: a program that churns buffers of
  hundreds of KiB (the JVM's direct `ByteBuffer`s, freed in bursts by the
  Cleaner) holds a load lull's worth of them in the huge tier's `Empty`
  list for the whole slab window, and the huge extents are whole runs
  refaulted at once on reuse, not slots of a shared slab, so a shorter
  window costs them less than it would cost the slabs
  (`bench/results/2026-09-20-{aarch64,x86_64}-jvm-huge-decay.md`; the
  shipped default stays the slab period until the human decides
  otherwise).
- `MADV_HUGEPAGE` on runs of a PMD (2 MiB) or more when the `thp` key is
  `always` (`prepare`, after the commit; `thp.rs`, plan Phase 2.5 task 9).
  Never under `auto`: `auto` is the multi-size path for the slab regions and
  promises no 2 MiB page anywhere, because fork-based persistence in Redis
  and Valkey (BGSAVE) pays for huge pages at every copy-on-write fault, and
  never under the default `never`. A run is page-aligned, not PMD-aligned,
  so the kernel backs the PMD-aligned ranges inside it; a refusal is
  counted (`stats.thp.failures`) and the run works with small pages.

## 8. Decay and purge

Each `(arena, class)` and the huge allocator keep an `empty` set ordered by
address, with `dirty_at` per entry. Decay policy is jemalloc's in shape:

- `dirty_decay_ms` (default 10000) is the window of jemalloc's decay curve:
  the time for memory to go from dirty to purged, with the amount still
  dirty following a smootherstep (`6x^5 - 15x^4 + 10x^3`, jemalloc's
  `SMOOTHSTEP_NSTEPS = 200` steps) in elapsed time over the window, so
  about half of what went `Empty` at once is purged by mid-window and all
  of it by the end. At each step a class's allowance is the sum over its
  `Empty` slabs of the curve's weight at their age, rounded up; its oldest
  `Empty` slabs are purged with `MADV_DONTNEED` until the rest fit and
  become `Purged`, their bytes moving from `resident` to `retained`. Within
  the cut-off step the highest addresses go first, so the lowest, which a
  refill takes first, stay. The huge allocator applies the same allowance
  in pages over its `Empty` extents, oldest first, whole extents only and
  never below the allowance, so a lone extent lasts the whole window. A
  lone slab is likewise never purged before its weight reaches zero (the
  allowance rounds up). A walk runs at most once per step (window / 200):
  the gate is one `purge::Decayer` (oldest `dirty_at`, next deadline) per
  class state and one in the huge allocator; the walks differ, the gate
  does not. `-1` is never. The huge allocator's window is arena 0's, or
  `huge_dirty_decay_ms` when set (section 7).
- `muzzy_decay_ms` is accepted and ignored. Two advices (plan Phase 2.5
  task 10, `policy::advice`): a purge is `MADV_DONTNEED` (eager: the
  pages leave RSS and the cgroup's charge at once, the next touch faults
  in a zero page) except under the cgroup policy's headroom band, usage
  under 70% of a known limit (section 11), where it is `MADV_FREE` (lazy:
  the pages stay mapped and charged until the kernel reclaims them, which
  it does before anything else when the cgroup nears its limit, and a
  reuse before that keeps the old contents and costs no fault). With no
  limit at all, the common case, the advice is eager: lazily freed pages
  stay charged and visible in RSS until reclaim, which misleads both
  autoscalers and Redis's fragmentation ratio, so the lazy advice is a
  saving the policy grants only where it knows the headroom. The scaled
  and immediate bands are eager (the kernel is about to want the memory),
  and so is every explicit purge (`malloc_trim(0)`, `arena.<i>.purge`, the
  full purge a `memory.events` `high` rise owes: the caller asked for RSS
  to drop), and so is the whole of an excess over the retention cap that
  was first seen at 70% or above, until the retention is back within the
  cap (`Policy::headroom`, the floor fix of 2026-09-19): the first eager
  walks of such an excess bring usage under 70% before the rest is
  purged, and without the rule the rest went out lazily and stayed
  resident and charged, the cap met in the allocator's accounting and not
  in the cgroup's, with the split decided by which walks shared a
  periodic tick (the container churn test, 400 MiB freed at 79% of 512
  MiB, failed 7 of 16 runs on a 4-vCPU 6.1 host and 5 of 16 on a 2-vCPU
  5.10 host, RSS 309 MiB at 2 s with `lazy_purges 18 eager_purges 1`;
  `bench/results/2026-09-19-lane-b-floor-fixes.md`). An excess first
  seen under 70% is lazy throughout (the lazy container test). `lazy_free:true`
  is the override, lazy in every band;
  `lazy_band:false` (default true) turns the saving off. The advice is read
  at each batch flush from the band as last evaluated, so it is at most
  one sample (10 ms) behind; `stats.cgroup.purge_advice`,
  `stats.cgroup.lazy_purges` and `stats.cgroup.eager_purges` and the
  `purge advice:` line of the report say which is in force and how many
  batches each has issued. Measured (`tests/c/cgroup_lazy.c`, `make
  test-cgroup-lazy`, `bench/results/2026-09-19-lane-b-two-advice-purge.md`):
  200 MiB freed at 40% of a 512 MiB container is purged lazily 80 ms
  later, RSS and `memory.current` stay at 202 and 205 MiB while
  `memory.stat` `pglazyfree` rises by 189 MiB; allocating the same 200 MiB
  again takes 5 ms on the C4A against 44 ms after an eager purge; and
  under pressure from a fresh 380 MiB set the kernel reclaims the lazy
  pages (`pglazyfreed`) and the process fits in the limit.
- `Purged` memory and zero: `MADV_DONTNEED`ed pages read as zero and
  `calloc` trusts a `Purged` slab or extent (sections 4 and 7); `MADV_FREE`d
  pages do not. The answer is one process-wide sticky flag
  (`purge::purged_is_zero`): true until the first lazy batch of the
  process, false for good after it, published before the batch's discard
  under the lock the walk holds, so `calloc` clears `Purged` memory it
  reuses from then on (a `memset` of the slot or run, on pages that are
  mostly still resident) and `Fresh` memory stays known zero. Tracking the
  advice per slab or extent would take a state bit in the descriptors and
  a read of it in the claim paths for a saving bounded to `calloc` on
  `Purged` reuse; the sticky flag is the smallest correct form.
- Purges are batched: collect up to 64 ranges and issue one `process_madvise`
  with `PIDFD_SELF` (kernel 6.15) or a pidfd of self (5.10), falling back to
  a loop of `madvise`. Init probes `MADV_FREE` through the same target
  separately (a kernel may take `MADV_DONTNEED` from a self-targeting
  `process_madvise` and refuse `MADV_FREE`); where it does not, lazy
  batches alone take the loop.
- The period a walk uses is the arena's configured one scaled by the
  cgroup policy (section 11): unchanged without a limit or under 70% of
  it, shrinking linearly to 1000 ms between 70% and 90%, 0 above; `-1`
  stays never. Scaling the period scales the curve's window, nothing
  else; and under a limit the walk's allowance is further capped at its
  share of the 5% retention cap (a keep ratio the policy publishes), the
  curve still deciding which slabs go first.
- Decay is driven from the slow path (section 6.4) with a per-class deadline
  at the end of the current step, and from the background thread when
  enabled. Without the background thread
  an idle process does not purge, exactly like jemalloc; Valkey enables the
  background thread by default through `mallctl("background_thread")`.
- `arena.<i>.purge` and `malloc_trim(0)` purge everything immediately.

### 8.1 Background thread

Created only when `background_thread:true` is set via `malloc_conf` or
written through `mallctl`. It is a plain `clone`-based thread created through
`pthread_create` on the first periodic tick (6.4) after the request, never
from init (`mallctl("background_thread", true)` creates it synchronously).
It sleeps on a futex with a timeout equal to the next decay deadline, is woken
when a class's first slab goes `Empty`, and after `fork` is not recreated in
the child until the child asks for it again. Where it holds a PSI trigger
it sleeps in `ppoll` instead and a fired trigger runs a decay pass at once
(section 11). Its stack is one reservation in the
metadata region, made by the first start and never freed: a thread that was
stopped (`background_thread:false`) exits at its next wake-up, and the next
start joins it before creating the new thread on the same stack, so nothing
ever runs on the stack while it changes hands. A fork child forgets the
parent's thread (it does not exist there) and reuses the stack directly.
Creating the thread from a `free` slow path saves and restores `errno`
around the pthread calls, since `free` must leave it alone (section 1).

### 8.2 Prefaulted fresh slabs (plan Phase 2.5 task 11)

Behind `prefault:true` (section 12), and only with the background thread
(`Config::prefault_active`: the pass runs on it, so the key without
`background_thread:true` is stored, readable and inert, and the report
says so; both keys must be in the configuration at init, since the gate is
read once there and a thread started later through
`mallctl("background_thread", true)`, as Valkey's is, does not turn the
pass on: `docs/tuning.md` section 3), the thread runs one pass after each
decay pass
(`purge::prefault::pass`): every `(arena, class)` whose `refilled`
counter (`ClassState::refilled`, one add per cache refill, already kept
for the compaction gate) moved since the pass last looked
(`prefault_due`, a snapshot the thread alone writes, in the state's tail
line) is asked for its next fresh slab (`ClassState::prefault_next`).
Under the class lock: if the arena has an `Empty` or `Purged` slab of the
class, nothing is done, since the next refill takes that; otherwise
`next_slab`, the refill's own selection (the current group's next slab,
else an idle group adopted from another arena, else a fresh group from
the region, guard slabs committed as guards and skipped), yields the slab
the refill would have committed, `commit_slab` and all. If it is `Fresh`
it is populated with the lock still held, before anything can see it:
one `madvise(MADV_POPULATE_WRITE)` over the slab where the init probe
found the kernel takes it (5.14+, `populate_write=yes` in the feature
line), else one write of zero per page (a zero over the kernel's zero,
this thread's alone); then it is published as `Purged` in the `purged`
set. That state is chosen because it is the one `next_slab` already
prefers right after `Empty` (so the next refill takes the prefaulted slab
first, at the lowest address), because its memory reads zero (the
populate leaves the fresh pages as they are, so `take` still reports the
claim zero for `calloc` wherever `purged_is_zero` holds) and because the
decay walks never touch the `purged` set (the pages stay resident until
the refill). The one thing the word gets wrong is "not resident": the
`purged` count of the bins table includes prefaulted slabs until they are
taken, and the RSS the statistics derive from slab counts is under by
those slabs. A slab the adoption yields as `Empty` or `Purged` goes back
into its set untouched: the next refill takes it and nothing needs
prefaulting. A prefaulted slab in an otherwise used-up group is an idle
group by the handoff's definition (13.3) and may be adopted by another
arena's refill, which is the same memory serving the same class one
arena over.

As many slabs per class per pass as the class refilled since the last pass,
at least one and at most `MAX_AHEAD` (8; `ClassState::prefault_ahead`), so a
bounded number of prefaulted slabs per `(arena, class)` waits at any time;
nothing at all unless the cgroup policy shows
headroom (`Policy::prefault_allowed`: no limit, or under 70% of it with
no eager return of the excess over the cap in progress, section 11), since
resident memory nobody asked for yet is the first thing the scaled and
immediate bands exist to take back. The commit path (`SlabTable::
commit_slab`, on a usable slab) wakes the thread once per fresh slab it
puts into service (`prefault::note_commit`, one relaxed load when the key
is off), so the pass runs right after the refill that consumed the
previous prefaulted slab, and a thread sleeping with no decay deadline
still runs it; the pass's own commits wake nobody. A wake per fresh
commit alone would leave every other slab of a steady stream unprefaulted
(the refill that takes a prefaulted slab commits nothing), so a pass that
found any class refilling re-arms the thread's sleep to at most
`prefault::POLL_NS` (1 ms: a hot Valkey class takes a 64 KiB slab about
that often, a pass is a few microseconds) and a quiet pass lets the sleep
fall back to the decay deadline and the wakes. The fast paths are
untouched: the pop and push never see the pass, and the refill's
selection is unchanged (it finds a `Purged` slab where it would have
committed a fresh one). Report: `Prefault: on (MADV_POPULATE_WRITE |
touch per page) passes N (active N) slabs N (B bytes) skipped_no_headroom
N fallbacks N wakes N taken N` or `off (<why>)`; `stats.prefault.{enabled,
populate_write, passes, slabs, bytes, skipped, fallbacks, wakes, active}`.
Tests: `class::tests::prefault_next_*` (the selection, the veto by the
arena's own `Empty`/`Purged` slabs, the guard skip, the adoption),
`policy::tests::prefault_allowed_only_with_headroom`,
`purge::tests::prefault_populate_and_wakes_are_counted`, and
`tests/c/prefault.c` both ways (under the key the next fresh slab of the
64-byte class is fully resident per `mincore` before any allocation
touched it, exactly one, zero, and the next refills come from it; under
the default nothing past the slabs in service is resident and the
counters stay zero). The acceptance measure is the Valkey soak's
write-phase p99 and the main thread's page faults, the first reading in
`bench/results/2026-09-20-aarch64-lane-b-task11-prefault.md`.

## 9. Fork

`pthread_atfork` handlers registered at init: the prepare handler blocks every
signal on the forking thread, then takes every lock in the allocator in
nesting order: the explicit-cache table lock (held across heap creation and
destruction, so outermost), the arena creation lock, every arena's class
locks and slab-set lock, the slab table locks, the huge lock, the metadata
lock, the process and per-arena statistics locks (leaves, taken by `epoch`
and `malloc_stats_print` with nothing else held), and the background thread's
state lock last; parent and child handlers release them in reverse and
restore the signal mask. A unit test derives the list of `Lock` fields from
the source tree and checks each against the handlers, so a lock added later
fails the test until the handler takes it. The blocking is
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

Init must not call anything that allocates. Steps, in the order
`api::init` performs them:

1. Read `AT_PAGESZ` from `getauxval` and probe features by attempting them
   on a scratch page and recording the error: `MADV_GUARD_INSTALL`, `mseal`,
   `process_madvise` (with `PIDFD_SELF`, then with a pidfd of self, which is
   kept; then `MADV_FREE` through whichever worked, for the lazy batches
   of section 8), `PR_SET_VMA_ANON_NAME`, and the transparent-huge-page
   table: `/sys/kernel/mm/transparent_hugepage/enabled`, `hpage_pmd_size`
   and every `hugepages-<n>kB/enabled` from two pages to the PMD looked up
   by path (a size the kernel lists without an `enabled` file, as
   `hugepages-8kB` on 7.0, or not at all, is absent; a kernel before 6.8
   lists the PMD size alone under the top-level setting), `inherit`
   resolved through the top level, about ten sysfs reads (`sys::features`,
   plan Phase 2.5 task 9). Then pointer authentication (`AT_HWCAP`,
   `sys::pac`, section 17). The whole probe is formatted
   once into the static feature line that `challoc_features()` returns.
2. Parse configuration (section 12). This happens before anything is
   reserved so that `abort_conf` aborts with nothing mapped; it reads only
   the weak symbols, one file and `environ`.
3. Decide the THP mode from the key and the table (`thp::configure`,
   section 17; forced off after step 7 when the cgroup rule of section 11
   applies), configure the purge backend from the probe and `lazy_free`,
   the two-advice purge from the `MADV_FREE` probe and `lazy_band`,
   `check_zero`, apply `group_handoff`, and record a `background_thread`
   request (the thread itself is created later, 8.1).
4. Read secrets from `getrandom`, probe reservation sizes (section 2) and
   reserve the three regions in random order. Name them with
   `PR_SET_VMA_ANON_NAME` when the kernel accepts it.
5. When the probe said `mseal=yes`, seal the never-committed ranges of the
   reservations (section 2, `Regions::seal`): one `mseal` call per
   contiguous range, at most `NCLASSES + 5` of them. Sealing forbids
   `munmap`, `mremap`, `mprotect` and `MAP_FIXED` on those ranges for the
   rest of the process; the regions themselves are not sealed because they
   are `mprotect`ed on commit and `madvise`d on purge. A refusal after a
   successful probe stops the sweep without aborting (sealing is a
   hardening extra, not a heap invariant); `malloc_stats_print` reports
   `Sealed ranges: N of M`.
6. Build the per-class slab tables, arena 0, the huge allocator, the arena
   and explicit-cache registries and the thread-exit key (`Shared::init`).
7. Discover the cgroup and read its limits (section 11; inside
   `Shared::init`, skipped with `cgroup:false`).
8. Register the fork handlers (section 9).

Any pointer passed to `free` before init completes, or that does not classify,
is treated as invalid. ld.so's minimal startup allocator only frees its own
last block, so in practice nothing foreign reaches us; if it ever does, abort
with a clear message rather than corrupt.

## 11. cgroup awareness

The measurement layer is `src/sys/cgroup.rs` (plan Phase 2 task 4), the
policy that acts on it `src/policy.rs` (task 5). Everything is gated by
the `cgroup` key (default `true`; `cgroup:false` reads nothing, applies
nothing and reports `cgroup: not tracked (cgroup:false)`).

Discovery, once at init (section 10 step 7):

- The cgroup path is the `0::` line of `/proc/self/cgroup`; inside a cgroup
  namespace it is `/`. Without such a line (a cgroup v1 only host) nothing
  is tracked.
- The cgroup2 mount point is the first `cgroup2` line of
  `/proc/self/mountinfo` (field 5 before the ` - ` separator), normally
  `/sys/fs/cgroup`; a hybrid host's `/sys/fs/cgroup/unified` works the same
  way. Without one nothing is tracked. The file is scanned line by line
  through a 2 KiB buffer, so a container with hundreds of mounts costs no
  more stack.
- The directory is `mount + path` (`/sys/fs/cgroup` itself for `/`), capped
  at 2 KiB; the leaf's `memory.max`, `memory.current`, `memory.events` and
  `memory.pressure` are probed for existence (a root cgroup has no
  `memory.current`; a v1 memory controller has none of them).
- Effective limits: walk from the leaf up to the mount, inclusive, reading
  `memory.max` and `memory.high` at each level and keeping the minimum of
  the numeric values per file. `max` and an absent file mean unlimited
  (`u64::MAX`). `memory.max.effective` does not exist in mainline kernels.
  The walk is repeated at most once per second, from the sampler, so a
  `docker update --memory` is seen.
- `cpu.max` and `cpuset.cpus.effective` are parsed at init and kept for the
  per-CPU mode (section 15), which is the only consumer.

All reads use raw `open`/`read`/`close` on fixed-size stack buffers through
`src/sys`, never libc stdio, and nothing allocates.

Sampling: `memory.current` and `memory.events` (`high` and `max`, kept with
a count of the samples that saw each one rise) are read at most every 10 ms,
measured with `clock_gettime(CLOCK_MONOTONIC_COARSE)`; one compare-exchange
on the timestamp lets one thread per window do the reads. The sampler runs
from the periodic tick of the slow path (`purge::on_tick`, every 64 slow-path
entries of a heap), from the background thread's loop, from a
`stats.cgroup.current` read and from `malloc_stats_print`; the fast paths
never see it.

PSI: when the background thread exists it, and only it, opens
`memory.pressure` read-write and writes the trigger `some 150000 1000000`
(a `some` stall of 150 ms in a 1 s window). If the kernel answers `EINVAL`
the unprivileged form `some 300000 2000000` is tried (windows under 2 s
need `CAP_SYS_RESOURCE`; unprivileged triggers exist since 6.4). Success is
the only evidence: any other error (`ENOENT` without PSI, `EPERM`, `EROFS`
under Docker's read-only cgroup mount) records `psi unavailable (<errno>)`
and the thread sleeps on its futex as before. Two container facts, both
measured: Docker's default profile mounts
the container's cgroup read-only, so under it the trigger reads
`unavailable (EROFS)` on every kernel; and on a host whose cgroup2 is
mounted with `nsdelegate` (systemd hosts, the GCE image) the kernel refuses
writes to non-delegatable files at the root of a cgroup namespace
(`cgroup_file_write`, `EPERM`), so even a `--privileged` container cannot
arm a trigger on its own root `memory.pressure`; a process in a child
cgroup of the container (systemd or a runtime that makes one) can. Docker
Desktop's linuxkit kernel mounts without `nsdelegate` and a `--privileged`
container there arms the 1 s trigger. The fallback is the same in every
case: the tick's `memory.current` sample. Armed, the thread sleeps in
`ppoll` on the trigger (`POLLPRI`) and an eventfd that `wake` writes; a
fired trigger counts one `pressure_events`, takes an out-of-window sample,
re-evaluates the policy on it and runs the thread's decay pass at once; `POLLERR`/`POLLHUP` (the cgroup
removed) disarms. The trigger descriptor is closed when the thread exits
and in a fork child (whose copy would otherwise wake the parent's thread
through the shared eventfd).

Policy (`src/policy.rs`). It is evaluated where a sample was actually
taken (the periodic tick, the background thread's loop, a fired PSI
trigger), so at most once per 10 ms process-wide and never on a fast
path; the outcome is a few atomics on `Shared` (the band, a keep ratio, a
pending full purge) that the decay driver and the periodic tick read.
Usage is the last `memory.current` sample against the effective limit
`min(high, max)`:

| Usage vs effective limit          | Behaviour                                                  | Purge advice (section 8) |
|-----------------------------------|------------------------------------------------------------|---------------------------|
| No limit                          | Defaults; nothing beyond the sample is done                | `MADV_DONTNEED`           |
| < 70% of `min(high, max)`         | Defaults, retention capped at 5% of the limit              | `MADV_FREE` (`lazy_band`) |
| 70% .. 90%                        | `dirty_decay_ms` scaled down linearly to 1000; the cap     | `MADV_DONTNEED`           |
| >= 90%                            | Period 0: purge at the next pass; cache GC every tick      | `MADV_DONTNEED`           |
| `memory.events` `high` increased  | Full purge once, at the next decay pass                    | `MADV_DONTNEED`           |
| PSI trigger fired                 | Sample and decay pass at once (the background thread)      | as the band               |
| < 70% while an excess over the cap first seen at 70%+ is still being returned | as the `< 70%` row | `MADV_DONTNEED` until the retention is within the cap |

The advice column is the two-advice purge of section 8: lazy only where
the headroom is known and wide, eager everywhere else and for every
explicit purge; `lazy_free:true` makes every row lazy, `lazy_band:false`
every row eager. The last row is the rule of section 8 for an excess
over the cap: the band it was first seen in decides its advice for the
whole return (`Policy::eager_excess`, set when the retention is over the
cap at 70% or above, cleared when it is within the cap again), because
the eager walks of the excess move usage into the `Low` band before they
are done, and the cap is a promise about resident, charged memory that a
lazy purge does not keep. One consequence to know: lazily returned pages stay
charged until the kernel reclaims them, so the `memory.current` sample
counts them and the band moves toward 70% sooner than live memory alone
would; that flips the advice to eager and shortens the window while the
memory is in fact plentiful, a conservative error in the safe direction,
and the pages already lazy are the first thing the kernel reclaims when
the cgroup nears `memory.high` or `memory.max` (`memory.stat`
`pglazyfreed`), so they never stand between the program and its limit.

*Retention* is the `Empty` memory of every class of every arena and of
the huge allocator: freed, dirty, not yet purged. It is the only memory
the allocator holds that costs RSS without serving anything; `Purged`
memory is already out of RSS and cached slots are live as far as the
kernel is concerned. The cap is applied inside the curve walks of section
8 as a keep ratio `cap / retention` (1 when within the cap): a walk keeps
the smaller of the curve's allowance and that share of its `Empty` slabs
or pages, so the sum kept stays under the cap while the curve still
decides what goes first. The huge walk never splits an extent: along the
curve alone it skips an extent larger than the remaining excess (a lone
extent lasts the window), but under the cap it purges the oldest extents
whole until within the allowance, overshooting rather than staying over
the cap, because free neighbours coalesce and what a program frees in
pieces is usually one extent. The sum is taken by the evaluator, under each
class lock, for classes that have something `Empty` (a lock-free test),
only when a limit exists. The cap applies in both bands under 90% (a cap
that vanished at 70% would let retention jump there); at 90% and above
the zero period empties everything anyway.

Scaling the period scales the curve's window (section 8): at 70% the
configured `dirty_decay_ms`, just under 90% 1000 ms, linear in usage
between; a period already at or under 1000 ms is left alone, and `-1`
(never) is left alone in every band as an explicit choice. "Immediately"
above 90% means a zero window: the next decay pass (the next periodic
tick, or the background thread woken by the first `Empty` slab of a
class) purges everything `Empty`; the `Empty` transition itself is not
touched. The cache GC at every periodic tick replaces "every flush": the
tick already sits on every flush and refill path at 1/64, and the flush
itself stays as it is.

A rise of `memory.events` `high` (the kernel started reclaiming from this
cgroup) requests one full purge of every `Empty` slab and extent, run by
the next decay pass; the count at init is the baseline, so a container
that already had `high` events does not start with one. A limit that
goes away (`docker update`) resets the outcome to the defaults. Testing
this row needs `memory.high` below `memory.max`, which `docker run`
cannot set; what `tests/c/cgroup_policy.c high` found: the container's own cgroup cannot enable the memory controller
for a child (`+memory` to its `cgroup.subtree_control` is `EBUSY` under
the no-internal-process rule while the container's processes live there,
`--privileged` or not), but with `--privileged --cgroupns=host` the parent
is visible and a *sibling* cgroup made there has `memory.high` (the parent
already delegates memory to the container); a child process moved into
it and run past its `memory.high` sees the `high` count rise and one
`full_purges`, on Docker Desktop (`/sys/fs/cgroup/docker/...`) and on the
systemd GCE host (`/sys/fs/cgroup/system.slice/...`) alike; `nsdelegate`
does not enter into it, since the cgroup is ours.

The container tests (plan Phase 2 task 6; `tests/c/cgroup_churn.c`,
`tests/c/cgroup_oscillate.c`, `scripts/cgroup-tests.sh`, `make
test-cgroup`, one `docker run --memory=L --memory-swap=L` container per
run) are the policy's end-to-end evidence and fix two facts. First, the
policy acts at a decay pass, and without a background thread a decay
pass runs only from the periodic tick, so a process that frees 400 MiB
and then allocates nothing at all keeps it under every setting; the
tests' idle phase is therefore an idle event loop (a burst of small
allocations every 10 ms), under which the first walk after the frees
(46..82 ms later) purges the whole retention over the cap, RSS 403 to
13 MiB in 512 MiB and 202 to 8 MiB in 256 MiB, while `cgroup:false`
leaves 319 and 161 MiB at 2 s (the 10 s curve). Second, the oscillation
(200..480 MiB live for 5 minutes in 512 MiB) survives with the policy
and also with it off: `Empty` slabs and extents stay in their classes
and the next climb reuses them in place, so the policy does not change
the peak (502 vs 507 MiB of `memory.current`), it changes what is
resident at the bottom of the swing (~293 vs ~490 MiB). jemalloc 5.3.1
and mimalloc 3.5.2 are OOM-killed in the same container at the fifth and
second climb (`bench/results/2026-09-18-aarch64-gce-cgroup-container-tests.md`).

What the policy can and cannot do at the peak, measured (Phase 2.5 task 3,
`bench/results/2026-09-19-matrix.md` F1, the `composition` lines of
`tests/c/cgroup_oscillate.c`): at the top of a climb `stats.cgroup.empty_bytes`
is 0 on a 4 KiB kernel and 0..2 MiB on a 64 KiB one, and the 2 MiB are
`Empty` sub-page slabs in mixed page groups that no walk can purge
(section 5). The peak is `allocated` plus the slack of partially used
slabs (`active - allocated`, 1..13 MiB over the swing, the classes' top
slabs drained by random frees and not yet `Empty`), plus metadata; the
bands, the cap and the sample interval have nothing to discharge there,
on either page size. On a 64 KiB kernel the top of the same object
sequence is 5 MiB of anon higher (the allocator's page-rounded tables +1,
the stuck `Empty` slabs +2, the program's own data segments +2) and 2 MiB
of page tables higher, with `active` identical, which is what OOM-killed
the 480-of-512 MiB oscillation there; the test therefore lowers its
default target by that envelope (8 MiB) when the page is larger than
4 KiB and says so, and keeps 480 MiB at 4 KiB. Re-measured after page-sized
slabs (Phase 2.5 task 4e, `bench/results/2026-09-19-aarch64-64k-valkey-soak-2.md`):
at HIGH=480 on the same kernel the run is still OOM-killed (`memory.peak`
512 MiB; the tops at 492..509 MiB `current`, `anon` 479..503, page tables 3,
with the allocator's `empty` 0), so the stuck sub-page slabs were the
smallest of the three terms and the 8 MiB envelope stays. Reacting sooner on a fast
climb (a 64 KiB climb of 280 MiB takes 15 ms, 3.5 times faster than at
4 KiB, so the 10 ms sample window sees it at most once) would purge
nothing more: `dirty_decay_ms:0` leaves the same 2 MiB at the top.

THP: "disabled under small limits" is `thp:always` forced to `never` in
the stored configuration at init when the effective limit at init is under
1 GiB (`opt.thp` reads `never`, the report says why, `thp.rs` mirrors it as
`forced never`): a 2 MiB page is the unit a small container cannot afford
to have partially used. `thp:auto` is not forced: its 64 KiB folio is the
slab's own granularity, and the metadata rounding it adds is bounded by
the tables in use. A forced `never` is a `never` (section 17, plan Phase
2.5 task 16): the class and metadata reservations already made are
`MADV_NOHUGEPAGE`d when the key is forced, as they are at reservation under
a configured `never`, and the huge region follows the host's policy either
way (`never` is the slab regions' RSS discipline: a 2 MiB page under a few
touched slabs is what a small container cannot afford; a huge run is a
whole allocation the program asked for, and it takes what the host gives
it, as jemalloc's large extents do).

Statistics through `mallctl` (13.4): `stats.cgroup.limit`,
`stats.cgroup.high` (both `SIZE_MAX` for unlimited), `stats.cgroup.current`
(the last sample, refreshed by the read if the window allows),
`stats.cgroup.pressure_events`, and the policy's `stats.cgroup.decay_ms`
(arena 0's effective period, `SIZE_MAX` for never),
`stats.cgroup.retention_cap` (bytes, `SIZE_MAX` without a limit),
`stats.cgroup.empty_bytes` (the retention, summed by the read) and
`stats.cgroup.full_purges`; all `size_t` and read-only; `ENOENT` when
nothing is tracked, like jemalloc's keys behind a disabled build option.
The two-advice purge's keys (section 8) sit in the same directory and read
whether or not a cgroup is tracked, since the advice exists either way:
`stats.cgroup.purge_advice` (1 lazy, 0 eager, the advice the next purge
issues), `stats.cgroup.lazy_purges` and `stats.cgroup.eager_purges` (batches
issued with each). `malloc_stats_print` prints one `cgroup:` line, one
`cgroup policy:` line and one `purge advice:` line (section 16).

## 12. Configuration

`docs/tuning.md` is the operator's reading of this section: one row per
key with when to change it and what it costs, the consumer settings, the
cgroup and THP behaviour, and the report's lines. This table stays the
source of truth for the key list, the defaults and the value grammar,
with `src/config.rs` (`KEYS`, `Config::DEFAULT`, `Config::set`) as the
code it describes.

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
| `prefault`                | false   | challoc only; opt-in (plan Phase 2.5 task 11): the background thread commits and `MADV_POPULATE_WRITE`s (5.14+, probed; fallback: touch) the next fresh slabs of every class that refilled since its last pass (as many as it refilled, at least one and at most 8 per class per pass) within the cgroup headroom (11), so the claim path's first write into a fresh slab takes no page faults; the claim takes a prefaulted slab as any committed empty slab. Needs `background_thread:true` in the same configuration at init (`Config::prefault_active` is read once there; a background thread Valkey starts later through `mallctl` does not turn the pass on, so Valkey is given both keys, `docs/tuning.md` section 3); off by default: it removes most of the Valkey main thread's write-phase page faults with p99 unmoved, but on by default it would mean a background thread and a few MiB of prefaulted slabs in every process (`docs/tuning.md` section 2) |
| `dirty_decay_ms`          | 10000   |                                         |
| `muzzy_decay_ms`          | ignored | accepted for compatibility              |
| `narenas`                 | 0       | automatic arenas; 0 = `min(4 * ncpus, 64)`; see 13.3 |
| `percpu_arena`            | auto    | `auto`, `percpu`, `off` (15, plan Phase 2.5 task 8): caches and arenas keyed by the running CPU's rseq `mm_cid` instead of by thread. `percpu` turns the mode on where the runtime probe passes (`rseq::probe`, checked at run time, never assumed from a version: glibc exports `__rseq_offset`, `__rseq_size` and `__rseq_flags` (2.35+) and the registered `__rseq_size >= 28`, so the area covers the `mm_cid` the kernel writes on 6.3+ with `CONFIG_SCHED_MM_CID`; glibc reports the kernel's feature size there since 2.41, `research.md`) and falls back to thread caches with a note in the report when it fails; `off` is thread caches whatever the probe says; `auto`, the default, keeps thread caches for now (`heap::PercpuState::OffAuto`): the mode measured 8..24% behind thread caches on the `quick` rows and far behind on the Valkey model on both reference VMs (section 15), so thread caches stay the default. jemalloc's `disabled` and `phycpu` are accepted as `off` and `percpu` (the key used to be accepted with any value) |
| `tcache`                  | true    | false disables caches                   |
| `tcache_max`              | 32768   | largest cached class size (bytes); `opt.tcache_max` reads as `size_t` |
| `tcache_nslots_small_max` | 1024    |                                         |
| `lg_tcache_nslots_mul`    | 1       |                                         |
| `tcache_gc_interval_ms`   | 10000   | challoc only                            |
| `tcache_compact_shift`    | 3       | challoc only; one pop in 2^k compacts, 0 = off (6.4) |
| `group_handoff`           | false   | challoc only; opt-in: an arena takes another arena's idle slab group before a fresh one (5, 13.3); off because it did not move the mstress row it was built for |
| `thp`                     | never   | `never`, `auto`, `always` (7, 17, `thp.rs`): `auto` advises slab groups of classes whose slab holds an enabled sub-PMD folio and rounds metadata commits to it, never a 2 MiB page (inert where the PMD size is `madvise`-gated or no such size is enabled); the opt-in for aarch64 hosts with `hugepages-64kB` enabled and Valkey-shaped heaps (measured neutral there, a real 1.6% loss on larson and more RSS on small heaps, so not the default); `always` commits and advises PMD chunks (at a 2 MiB PMD only: 4 KiB pages; at 16 and 64 KiB pages it is the folio path over the enabled sizes below the PMD, inert when none is), huge runs of a PMD or more and the metadata (jemalloc's `thp:always`, priced in `bench/results/2026-09-19-lane-b-mthp.md`); `always` under a cgroup limit below 1 GiB is forced to `never` (11); `never` (and a forced `never`) issues `MADV_NOHUGEPAGE` on the class superblock and the metadata region, so the slab regions get no huge page on a host whose global mode is `always`, and leaves the huge region to the host's policy, as jemalloc's `never` leaves its large extents (17, plan Phase 2.5 task 16; the 2026-09-20 decision on `bench/results/2026-09-20-aarch64-cgos-thp-ab.md`, option (d): the huge runs' 2 MiB pages were 10 points of `valkey1` on Chainguard OS) |
| `slice_ext`               | auto    | challoc only; `auto`, `on`, `off` (17, "Time slice extension"; plan Phase 2.5 task 15): whether a lock holder asks the kernel, through its rseq `slice_ctrl` bytes, to defer a preemption until the release. `auto` and `on` turn it on where the runtime probe passes (glibc's `__rseq_size >= 32`, the kernel's `RSEQ_CS_FLAG_SLICE_EXT_AVAILABLE` bit in the thread's area, the `prctl` enable confirmed by the `ENABLED` bit: kernel 7.0 with `CONFIG_RSEQ_SLICE_EXTENSION`, x86_64 only so far); `on` additionally makes the report note a refused request; `off` never requests. No kernel field but `request` is written |
| `lazy_free`               | false   | `MADV_FREE` for every purge, the override of the two-advice purge (8) |
| `lazy_band`               | true    | challoc only; `MADV_FREE` for purges under 70% of a known cgroup limit (8, 11); `false` makes every purge `MADV_DONTNEED` unless `lazy_free` |
| `guards`                  | true    | guard slabs and huge runs               |
| `guard_slab_interval`     | 8       | one guard per N x 64 KiB of slabs, N a power of two (else rounded up, reported); 0 off, 1 out of range (clamps to 2) |
| `huge_retain_max`         | 67108864 | challoc only; bytes; freed huge runs above it are purged at once (7); `opt.huge_retain_max` reads as `size_t` |
| `huge_dirty_decay_ms`     | follows `dirty_decay_ms` | challoc only; the huge tier's own decay window in ms, `-1`..`1000000000000` (7): unset it follows arena 0's period (`dirty_decay_ms`, `arena.0.dirty_decay_ms`), and `opt.huge_dirty_decay_ms` reads that; `0` purges a freed huge run at the free; `-1` never by decay. Measured on the JVM soak at 2000, 500 and 0 (`bench/results/2026-09-20-{aarch64,x86_64}-jvm-huge-decay.md`); the default is the human's call with those numbers |
| `zero_on_free`            | false   | opt-in: clear a block on free (6.3 step 3); priced in `bench/results/2026-09-18-*-gce-tunables.md` |
| `pac_pointers`            | false   | challoc only; opt-in: sign cache entries and the TLS heap pointer with `pacda` where the kernel exposes `HWCAP_PACA`/`PACG` (6.1, 6.2, 17); inert elsewhere; priced in `bench/results/2026-09-19-lane-a-pac-pointers.md` |
| `slot_randomize`          | false   | opt-in: refills claim from a random word and shuffle the batch (4); fights 6.4's compaction |
| `double_free_scan`        | false   | compare against the top 4 cache entries |
| `abort_conf`              | false   | abort on unknown keys, bad values and inert keys |
| `cgroup`                  | true    | read the memory cgroup (11) and apply its policy: limits, `memory.current`, events, PSI; `false` reads and applies nothing |
| `check_zero`              | false   | challoc only; debug: read every slot/page handed out as zero, abort on a dirty word |
| `stats_print`             | accepted | inert: nothing prints at exit; no `opt.` value |
| `junk`, `zero`, `retain`, `oversize_threshold`, `metadata_thp`, `confirm_conf` | accepted | jemalloc keys, ignored with any value (`metadata_thp` because the metadata follows `thp`, section 17) |

*Inert* keys are parsed and stored (so `opt.<key>` reads back what was set)
but read by no code; each one is reported like a bad key, so `abort_conf`
refuses a configuration that relies on one, and `malloc_stats_print` marks
them "accepted, no effect" (`config::INERT_KEYS`). Unknown keys, bad values
and inert keys are reported on stderr and otherwise ignored unless
`abort_conf` is set. `opt.<key>` is readable through `mallctl` for every key
that has a value above; `stats_print` and the ignored jemalloc keys return
`ENOENT`.

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
an abort with a message instead of the throw. The sized `delete` forms
(`operator delete(void*, size_t)`, its array form, and the sized
`align_val_t` forms) are `free_sized` and `free_aligned_sized`: the size
and alignment name a class through the same `class_for(size, align)` the
matching `new` used, so a correct program's hint always matches, the slot
is pushed without the slab descriptor's bit test (6.3), and a mismatched
size aborts as `free_sized` does (jemalloc's sized `delete` is `sdallocx`
with the same contract). The unsized aligned forms have no size to check
and take the plain `free` path with the alignment ignored.

### 13.2 jemalloc

Symbols: `mallocx`, `rallocx`, `xallocx`, `sallocx`, `dallocx`, `sdallocx`,
`nallocx`, `mallctl`, `mallctlnametomib`, `mallctlbymib`,
`malloc_stats_print`, `malloc_conf` (data symbol: defined null and exported so
a program that only references it links, as Ruby's configure probe does;
read through the GOT so a program's own definition interposes; `je_malloc_conf`
is only read, as a weak reference),
`malloc_message` (function pointer data symbol, default writes to stderr;
`je_malloc_message` is a second object, consulted when `malloc_message` is
null). Each function is also exported with the `je_` prefix via
`#[unsafe(export_name)]` wrappers. The `jemalloc-soname` Cargo feature names the output
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
is detected at the cache flush, not at the call (6.3); a slot start in a
guard slab is rejected at the call. A huge run passes
the size check and is judged by the huge allocator. `dallocx` has no size
and is `free` with flags. C23 `free_sized` and `free_aligned_sized` follow
the same rule with the size (and alignment) the allocation was requested
with; `free_sized` on memory from `aligned_alloc`/`memalign` is undefined
in C23 and aborts here when the alignment put the block in a larger class.

### 13.3 Arenas and explicit caches

An arena is a set of `ClassState`s (one per class, over the shared per-class
`SlabTable`s) with its `SetTable`, its decay period, per-arena counters of
the huge runs allocated through it (the huge allocator itself is one and
shared by every arena), plus statistics. Arena 0 exists at init. `arenas.create` allocates a
new one. Slab groups record their arena (section 4). A slot belongs to the
arena that committed its slab for good: a refill claims from the home
arena, a flush releases each slot to its slab's arena
(`release_routed`), so per-arena statistics (`curregs`, `active`,
`dirty`) stay exact whichever cache a slot passes through. A pointer freed
with plain `free` goes onto the home cache when its slab is the home
arena's and onto the thread's one flush-only foreign cache otherwise (6.3
step 4, since 2026-09-21), so a live slot never migrates between arenas
and Redis's Lua arena keeps its accounting through `MALLOCX_ARENA`. When
the last thread heap homed on an arena is destroyed, its `Empty` slabs are
purged at once (jemalloc's `tcache_destroy` rule; not with the background
thread, which decays on its own schedule, and at most once per decay
period per arena, `Arena::take_exit_purge`, so a pool that retires and
recreates its threads does not purge and refault the same slabs at every
exit): the caches that flush just before are most of them, and nothing
will refill that arena soon.
An arena owns its groups. With `group_handoff:true` (an opt-in; the
default is `false`) it lends idle ones: when an arena's current group of a
class is used up and it has no `Empty` or `Purged` slab of that class, it
takes over a whole group of that class from another arena provided no slab
of the group is `Active` (every slab `Fresh`, `Empty` or `Purged`, `live ==
0`, so no slot of it is in the program or in a cache), before it takes a
fresh group from the region (section 5, `ClassState::adopt_idle_group`).
Owner word, descriptors, set bits, slab counts and, for the donor's current
group, the commit cursor move together under both arenas' class locks; the
slabs' `dirty_at` stay and the taker's decay walk owns them from then on.
Whole groups only, so the line ownership of section 4 holds throughout. It
was built for the 16-arena `mstress` row, where challoc's peak RSS exceeds
jemalloc's by 8..9% on aarch64 and 18..19% on x86_64 while `narenas:1`
equals jemalloc (`bench/results/2026-09-18-phase-1.5-acceptance-3.md` 3.1),
and it does not move that row: it halves the `Empty` small slabs idle at
the peak (20 -> 10 MiB of the 55 MiB excess on x86_64) and takes some of the
64 KiB-slab classes' idle groups, but the peak RSS over five protocol rounds
is unchanged on x86_64 and 2.4% higher on aarch64 (against the same
library with the key off), because the rest of the excess is `Active` at
the peak (one partial slab per class per arena, the touched pages of the
64 KiB-slab classes' current groups, which are never whole-group idle under
churn, and the per-arena metadata) and every arena reaches its peak at once
(`2026-09-18-{aarch64,x86_64}-gce-group-handoff.md`). The row is therefore
accepted as the price of per-class regions at 16 arenas (the review's (c)):
an `Empty` slab is reusable only by its own class (section 3) and, by
default, its own arena, where jemalloc's one dirty pool per arena serves
every bin and large size. What the handoff does take is the idle groups a
producer-consumer pattern leaves in the producers' arenas: `xmalloc-test`
RSS 56 -> 45 MiB (aarch64) and 53 -> 45 (x86_64) with every fast-tier row
within 2.5% of the parent on both VMs; that is what `group_handoff:true`
is for.

`narenas` shards automatic allocations across arenas by thread, jemalloc
style. The default resolves to `min(4 * ncpus, 64)` (`ncpus` from the
affinity mask, so cpusets count); a new thread heap is bound to the automatic
arena with the fewest bound heaps (ties: an already built arena, then the
lowest index; jemalloc's `arena_choose_hard`), the binding is released when
the heap is destroyed and moves with `thread.arena`. So `t` concurrently live
threads use `min(t, narenas)` arenas, a single-threaded program stays on
arena 0 and never pays the `free` routing (the `multi_arena` flag is only set
when a second arena is built), and thread churn reuses the arena an exited
thread left; so the "N arenas" of a benchmark row is this materialised
count, not the default. Slab classes above `tcache_max` (32 KiB) are the exception: they
are uncached and served under the class lock anyway, their slabs are 1..4
MiB, and one partial slab per class per arena multiplied by 16 arenas was 60
MiB of mstress's RSS (`bench/results/2026-09-18-aarch64-gce-arena-rss.md`),
so the default path allocates them from arena 0 whatever the thread's arena
(`Config::shared_class`; `MALLOCX_TCACHE(t)` without an arena flag follows
the default path); `MALLOCX_ARENA(a)` allocations keep `a`, home arena or
not. Slabs are not thread-owned, so
arenas are not needed for correctness or lock scalability; they are needed
for cache locality: under one
shared arena every refill interleaves the threads' slots in the same slabs and
the 10-thread `larson` row paid about 1 L1D miss per operation for it
(`bench/results/2026-09-17-gce-review-counters.md`,
`2026-09-17-{aarch64,x86_64}-gce-thread-affinity.md`; a per-heap hint slab
did not move the counter, only partitioning the slab sets did). The cost is
one partial slab per class per live thread (as in jemalloc), plus the
owner-array load on `free` once a second arena exists.

`tcache.create` allocates a `Heap` owned by no thread (the same caches,
refill and flush, no TLS); `MALLOCX_TCACHE(t)` uses it. Explicit caches are not thread safe, matching jemalloc. `MALLOCX_TCACHE_NONE`
bypasses caches and claims or releases directly against slab bitmaps: a
direct claim lands in the arena's lowest-address partial slab, the slab a
refill or a compaction rebuild takes next, which is where jemalloc's
`slabcur` (its lowest non-full slab) puts the same request. Valkey's
defragmenter only needs its copies to land outside the below-average slabs
it is emptying, which the lowest partial slab is once the low slabs are
packed (`bench/results/2026-09-19-aarch64-64k-defrag-attribution.md`); it
does not need, and does not get, the fullest slab.

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
stats.arenas.<i>.mapped, stats.arenas.<i>.retained,
stats.arenas.<i>.bins.<j>.curregs, stats.arenas.<i>.bins.<j>.curslabs,
stats.arenas.<i>.bins.<j>.nonfull_slabs
stats.arenas.<MALLCTL_ARENAS_DESTROYED>.*  (all zero; see below)
thread.tcache.flush, thread.tcache.enabled, thread.allocated, thread.allocatedp,
thread.deallocated, thread.deallocatedp, thread.arena
tcache.create, tcache.flush, tcache.destroy
experimental.utilization.query, experimental.utilization.batch_query
config.malloc_conf, config.debug, config.stats
prof.* -> ENOENT
stats.cgroup.limit, stats.cgroup.high, stats.cgroup.current, stats.cgroup.pressure_events,
stats.cgroup.decay_ms, stats.cgroup.retention_cap, stats.cgroup.empty_bytes, stats.cgroup.full_purges
  (section 11; ENOENT when no cgroup2 is tracked or cgroup:false)
stats.cgroup.purge_advice, stats.cgroup.lazy_purges, stats.cgroup.eager_purges
  (section 11; readable without a tracked cgroup)
stats.thp.mode, stats.thp.class_ranges, stats.thp.class_bytes, stats.thp.huge_ranges,
stats.thp.meta_ranges, stats.thp.nohuge_ranges, stats.thp.failures (section 10)
stats.prefault.enabled, stats.prefault.populate_write, stats.prefault.passes, stats.prefault.slabs,
stats.prefault.bytes, stats.prefault.skipped, stats.prefault.fallbacks, stats.prefault.wakes,
stats.prefault.active (section 8.2)
stats.percpu.enabled, stats.percpu.ids, stats.percpu.headers, stats.percpu.cached, stats.percpu.aborts (section 15)
```

Semantics that differ from jemalloc's, deliberately: `arena.<i>.destroy`
flushes and purges the arena and leaves it registered and usable
(`narenas` does not move; `MALLOCX_ARENA(i)` keeps working; `EFAULT` for
arena 0 and `MALLCTL_ARENAS_ALL`). Reading `thread.arena` builds the
calling thread's heap if it has none (`EAGAIN` while a heap is being
created or torn down); writing it to an unbuilt automatic arena builds
that arena. `opt.tcache_max` and `opt.huge_retain_max` read back as
`size_t` (byte counts, as jemalloc's `opt.tcache_max`); the other unsigned
keys (`opt.narenas`, `opt.tcache_nslots_small_max`,
`opt.tcache_compact_shift`, `opt.guard_slab_interval`) as `unsigned`; the
signed ones (`opt.dirty_decay_ms`, `opt.muzzy_decay_ms`,
`opt.lg_tcache_nslots_mul`, `opt.tcache_gc_interval_ms`) as `ssize_t`.
`stats.arenas.4097.*` (`MALLCTL_ARENAS_DESTROYED`) answers all-zero
statistics, bins included: challoc never destroys an arena, so the merged
figures of the destroyed ones are zero; `arena.4097.*` answers `ENOENT`.

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
rule is Valkey's, challoc only has to answer honestly and cheaply. Valkey
9.1.2's rule (`allocator_defrag.c`) reads `nfree`, `nregs` and `size` from
the batch query and `curregs`, `curslabs` and `nonfull_slabs` from
`stats.arenas.4096.bins.<j>`, nothing from the verbose query; it moves an
object whose slab is more than 12.5% below the average utilisation of the
bin's non-full slabs, and starts a cycle only while `frag_smallbins` is at
least `active-defrag-ignore-bytes`. challoc's `nonfull_slabs` is the
partial set, which includes the slab a claim takes from next; jemalloc's
excludes `slabcur`, so for the same heap challoc reports one more non-full
slab and a slightly higher average, i.e. moves slightly more. `MALLOCX_TCACHE_NONE`
claims take the lowest partial slab, where jemalloc's take `slabcur`, the
lowest non-full slab by (serial, address): the same placement. The
attribution of the 64 KiB post-delete gap
(`bench/results/2026-09-19-aarch64-64k-defrag-attribution.md`) found every
answer exact and the gap in where Valkey's absolute stop falls relative
to each allocator's waste.

### 13.5 Redis-specific extensions

`get_defrag_hint(void *ptr)` and the `*_with_usize` family exist only in
Redis's patched bundled jemalloc. Valkey uses the standard
`experimental.utilization` keys (13.4). `get_defrag_hint` is exported
(both spellings, `get_defrag_hint` and `je_get_defrag_hint`; the shipped
headers declare it and define `JEMALLOC_FRAG_HINT`, from which KeyDB's
`zmalloc.h` derives `HAVE_DEFRAG`) because KeyDB, a Wolfi package, calls
`je_get_defrag_hint` in `activeDefragAlloc` and has no other defragmenter
input (plan Phase 3 task 3). It answers 1 when moving the allocation is
worth it by the rule of Redis's `iget_defrag_hint`, computed from the same
data as the utilization queries: the pointer is a live slot of a slab
class, its slab has a free slot, the slab is not the one a
`MALLOCX_TCACHE_NONE` claim takes from next (jemalloc's `slabcur`,
challoc's lowest partial slab in the owning arena: the move would land
the object back in it), and its live count is at or below the average
live count of the class's other slabs in that arena
(`live * (curslabs - 1) <= curregs - live(slabcur)`). Every slab class
answers, the ones above `BIN_MAX` included: the utilization queries report
those as one region each because Valkey's bin arithmetic is calibrated to
jemalloc's bins (13.4), but the hint carries no geometry, and a live
16 KiB block pinning a 16-slot slab is exactly the waste a move recovers
(on KeyDB's memefficiency suite,
`bench/results/2026-09-20-phase3-task3-keydb-package.md`, the classes
above 4 KiB held about half of the fragmentation KeyDB's defragmenter
could not reduce with the bin-only hint; answering for them recovered
none of it there, because the blocks pinning those slabs, client and
query buffers and internal structures, are not objects the defragmenter
asks about, so the wider hint is the right rule for a slab allocator
rather than a measured gain). It answers 0 for everything
else, a huge run (no slab to consolidate), a full slab, a freed or foreign
pointer, null, and never aborts: the defragmenter asks about pointers it
owns, and a wrong answer only costs it a move. It is a cold statistics
read (two class-lock acquisitions, as the verbose utilization query) with
no effect on any allocation path. The `*_with_usize` family stays out of
scope: no Wolfi package calls it.

What the hint cannot recover, measured on the same suite: free slots the
freeing thread's cache still holds pin their slabs (KeyDB frees through
the cache and its defragmenter asks only about live objects), so the
`allocator_frag_bytes` KeyDB's stop rule reads (`stats.active -
stats.allocated`, Redis 6's rule; Valkey 9 reads the bins' figures from
the utilization queries instead) keeps the cached slots' slabs until the
cache GC (`tcache_gc_interval_ms`, 10 s) returns them, plus the slab tails
(`slab_size - nregs * size`) that `active` counts and the queries do not.

### 13.6 Library name, soname and packaging

`cargo build --release` writes `libchalloc.so` and `libchalloc.a`. The
shared library's soname is `libchalloc.so.0` (the ABI major; the crate
version is 0.x), so a program linked with `-lchalloc` records
`libchalloc.so.0` in `DT_NEEDED` and a package installs the file under that
name with a `libchalloc.so` link for the linker. Cargo cannot name the
output file after the soname, so a build directory used as a link and run
path (`tests/consumers/valkey-build.sh`) puts the `libchalloc.so.0` link beside
the file. With the `jemalloc-soname` feature the soname is
`libjemalloc.so.2` instead (13.2), staged by `scripts/jemalloc-compat.sh`.

One quantum per build (section 3): the class table is compile time, as
jemalloc's `lg_quantum` is, and a run-time quantum would put a table
select on every class lookup. Wolfi's `jemalloc` (5.3.1) is configured
without `--with-lg-quantum`, so its quantum is jemalloc's default 16 on
both architectures; the system library `libchalloc.so.0` and the
`challoc-jemalloc-compat` `libjemalloc.so.2` are therefore `quantum16`
builds, the class table a jemalloc consumer of the system library already
runs on (Ruby's stage). Valkey is the one `quantum8` consumer (its
defragmenter asserts `arenas.quantum == 8`, from the `--with-lg-quantum=3`
of its bundled jemalloc) and gets it the way it gets jemalloc today: a
private `quantum8` build of the same source in its own package, installed
under a private library directory (`/usr/lib/valkey/`) named in the
binary's rpath (`DT_RPATH` or `DT_RUNPATH`: Wolfi's binutils 2.47 emits
`DT_RPATH` for `-Wl,-rpath` without `--enable-new-dtags`, which for a
private library is the better of the two, since `DT_RPATH` is searched
before `LD_LIBRARY_PATH` and only `LD_PRELOAD` can displace the file;
`contrib/wolfi/valkey/valkey-9.1.yaml` and its test accept either), or
linked statically from `libchalloc.a`. Two
libraries with one soname cannot share `/usr/lib`, so the system library
is never `quantum8`. This is a packaging rule for Phase 3; it does not
change the builds this repository tests (`quantum16` by default,
`quantum8` for the Valkey tree).

`challoc_version()` returns the crate version as a NUL-terminated static
string (section 16); with `challoc_features()` and the size-returning
entry points of 13.7 it is the whole of the challoc-specific surface,
declared in `<challoc/challoc.h>`; there is no exported accessor for the
thread heap slot.

`DT_NEEDED` under `--as-needed` (plan Phase 3 prerequisite (e), measured
2026-09-19 with `tests/as-needed.sh` on both reference architectures):
Wolfi's gcc links with `--as-needed` by default, flag or no flag. The
linker keeps a library only if it resolves a reference made before the
library is scanned, so an executable that itself calls `malloc` (or any
symbol of the library) keeps `DT_NEEDED libchalloc.so.0` because
`-lchalloc` precedes the implicit `-lc` and `malloc` is defined by the
first library scanned (`-Wl,-y,malloc` shows "definition of malloc" in
`libchalloc.so`). An executable that references nothing of the library
drops the entry. The dangerous case is the two-level shape (Ruby: the
`ruby` binary allocates only through `libruby.so`): the library keeps its
own `DT_NEEDED`, the executable drops its, and the loader's breadth-first
search order then puts `libc.so.6` (a direct dependency of the
executable) ahead of `libchalloc.so.0` (a dependency of the library), so
`malloc` binds to glibc for the whole process, the library's own calls
included, with nothing failing. Ruby avoids it by appending
`-Wl,--no-as-needed` to its LDFLAGS (ruby/ruby#4627), which is why the
built `ruby` carries `libjemalloc.so.2` without referencing it. A package
therefore (a) links the executable with `-Wl,--no-as-needed -lchalloc`
(or `-ljemalloc` for the compat library; `-Wl,--push-state,--no-as-needed
-lchalloc -Wl,--pop-state` scopes it to the one library), or makes sure
the executable itself references the allocator, and (b) runs
`scripts/check-needed.sh <binary> libchalloc.so.0` (or `libjemalloc.so.2`)
in its package test on every installed executable that must run on
challoc, plus the shared library that does the allocating; the check is
`readelf -d` only and exits 1 when the soname is missing.
`tests/consumers/valkey-build.sh` and `tests/consumers/ruby-build.sh` run it, and `make
check-needed` runs the link experiments and the built consumers.

### 13.7 challoc extensions

The rule of 13.5 holds for challoc's own names too: an export that no
other allocator has exists only for a consumer's measured cost, and the
measurement is named here. The four size-returning entry points, declared
in `<challoc/challoc.h>` (installed by `challoc-dev` beside
`<challoc/jemalloc.h>`, which defines `JEMALLOC_CHALLOC_USABLE` so a
consumer can test for them at compile time; the `challoc-jemalloc-compat-dev`
`<jemalloc/jemalloc.h>` defines the macro and declares the four itself, so
the two `-dev` packages ship no file in common), exist for Valkey:

```c
void  *challoc_malloc_usable(size_t size, size_t *usable);
void  *challoc_calloc_usable(size_t n, size_t size, size_t *usable);
void  *challoc_realloc_usable(void *p, size_t size, size_t *usable);
size_t challoc_free_usable(void *p);   /* frees p, returns its usable size */
```

Why. Valkey's `zmalloc.c` calls `zmalloc_size` (`je_malloc_usable_size`)
after every `je_malloc`, `je_calloc` and `je_realloc`
(`ztry*_usable_internal`, for the `used_memory` accounting) and before
every unsized `zfree` (then `je_sdallocx`). On the C4 that query was the
one allocator-side term above jemalloc's cost in the soak's write phase:
+117 cycles per valkey-benchmark SET and +63 per memtier SET (346 against
229; `bench/results/2026-09-20-x86_64-valkey-write-phase-attribution.md`,
section 6), because it classifies the pointer a second time and its exact
liveness test loads a bitmap word that nothing has touched since the
refill (the sized free of 4e stopped touching it, so `malloc` hands out a
slot whose word is cold, and the query right after `malloc` is its first
touch, an L1 miss per allocation). The note proposed dropping the
liveness test from `malloc_usable_size`; the decision (2026-09-21) was to
keep every check and remove the second lookup instead: the allocation
knows its class from the pop, the free from the classification it makes
anyway, and each hands the class size to the caller.

Semantics: each is the standard function of the same name, then the
usable size, exactly what `malloc_usable_size(p)` would return for that
pointer at that moment, on every tier (a slab class's slot size, the
classes above `BIN_MAX` included; a huge run's page count times the page
size), stored through `usable` or returned. `errno`, null handling,
alignment, the huge tier and the aborts are `malloc`/`calloc`/`realloc`/
`free`'s: a failed allocation returns null with `errno = ENOMEM` and
`*usable = 0`; `usable` may be null for the three allocating functions
(then they are plain `malloc`/`calloc`/`realloc`); `challoc_free_usable(NULL)`
is 0; `challoc_realloc_usable(p, 0)` frees and reports 0. Two points where
a choice existed: `challoc_calloc_usable` zeroes the whole usable size
(the bytes the caller is told it owns, which jemalloc's `calloc` zeroes
too), where plain `calloc` clears the requested bytes; and
`challoc_free_usable` is the exact `free` of 6.3 (the bitmap-word test,
the owner word with several arenas) reporting the class it verified, not
the sized path: a stale, interior or foreign pointer aborts at the call
with `free`'s message, and a double free of a still-cached slot is caught
at the flush, or at the call with `double_free_scan`, as for `free`.

Implementation (`src/api/libc.rs`): `challoc_malloc_usable` is `malloc`'s
pop (6.3) plus `CLASSES[c].size()` from the class the pop used, every other
outcome one cold tail call away; `challoc_free_usable` is `free`'s check
and push (`heap::free_fast_class`, `free_fast` naming the class) plus the
same lookup; the cold paths take the size from the same classification the
exact slow path then repeats; the huge tier reads the run's usable bytes
from the huge table as `malloc_usable_size` does. The fast paths of
`malloc`, `free`, `sdallocx` and every other existing entry point are
byte-identical to what they were (`scripts/codegen-diff.sh`). In the
per-CPU mode (15) the four take their cold paths (`heap::alloc`,
`heap::free`, which dispatch the mode); the mode is opt-in and measured on
its own. Valkey uses them under `USE_CHALLOC`
(`contrib/valkey/0002-zmalloc-use-challoc-usable-size-entry-points.patch`: `ztrymalloc_usable_internal`,
`ztrycalloc_usable_internal` and `ztryrealloc_usable_internal` call the
`*_usable` pair instead of the allocation plus `zmalloc_size`; the unsized
`zfree` calls `challoc_free_usable` instead of `zmalloc_size` plus
`je_sdallocx`; `zmalloc_size` stays `je_malloc_usable_size` for its other
callers). Measured in `bench/results/2026-09-21-*-sized-path-usable*.md`.

## 14. Hardening summary

Always on:

- Out-of-band metadata; no allocator pointers or sizes inside user memory.
- Size-class isolation in separate randomised regions with unmapped gaps.
- Invalid-free and stale double-free detection on every plain `free` (the
  sized path defers the bit test to the flush, 6.3).
- Guard after every huge run (`MADV_GUARD_INSTALL`, else `PROT_NONE`).
- Abort, never continue, on any inconsistency. Abort messages go to stderr
  via `write(2)` and then `abort()`.

On when the kernel has `MADV_GUARD_INSTALL` (6.13+), otherwise absent:

- A guard slab every `guard_slab_interval` x 64 KiB of slabs (section 5).

On when the kernel has `mseal` (6.10+) and seccomp does not filter it,
otherwise absent:

- The never-committed parts of the reservations (the gaps around every
  class region, the slack around the huge and metadata regions) are sealed
  at init: no `munmap`, `mremap`, `mprotect` or `MAP_FIXED` over them for
  the life of the process (section 2, section 10 step 5). The regions and
  the region table are not sealed, for the reasons given in section 2.

Off by default, tunable:

- `double_free_scan` (compare each pushed slot against the top four cache
  entries; priced in 6.3); `guard_slab_interval` below the default of 8 for
  denser guard slabs.
- `zero_on_free` (clear a slot's class-size bytes, or a huge run's usable
  bytes, on free; 6.3 step 3) and `slot_randomize` (claim from a random
  bitmap word and shuffle each refill batch; section 4). Both priced in
  `bench/results/2026-09-18-*-gce-tunables.md`; they stay off because a
  `memset` per free is more than the 3% budget on Valkey's 1 KiB values
  and randomisation fights the compaction of 6.4 (review 2026-09-18
  section 5.1).
- `pac_pointers` (arm64 with `HWCAP_PACA`/`PACG`: Axion, Apple silicon;
  not Altra, inert on x86_64): every slot pointer in a thread cache and
  the heap pointer in TLS are signed with `pacda` (data key A, the stack
  base or the thread pointer as the modifier) and authenticated with
  `autda` on the way out (6.1, 6.2), so a write into the metadata region
  cannot make `malloc` return a chosen address: the pop faults (FEAT_FPAC)
  or hands back a non-canonical pointer that faults on first use. Off
  because an entry's two PAC instructions are 11% of xmalloc-test's
  cross-thread frees and 1..2% of larson on the C4A, over the 1% budget
  for a default-on hardening (`bench/results/2026-09-19-lane-a-pac-pointers.md`).

Planned, hardware-gated (section 18; plan Phase 2.5 tasks 5 and 6):
tagged pointers behind the `tag_pointers` key, default `off`. On MTE
hosts a random 4-bit allocation tag per block, retagged on free, checked
by the hardware on every access (a read through a freed block faults);
on LAM, UAI or TBI hosts a six-bit class-and-generation tag in the
ignored top bits that `free`, `realloc`, `malloc_usable_size` and the
sized path verify (a stale, forged or cross-class pointer aborts; loads
are not checked). No GCE CPU exposes MTE, LAM or UAI to a guest, so the
implementation waits for a host that does; the software tag over TBI is
not gated.

Explicitly not attempted: quarantine (defeats defragmentation and costs
throughput), canaries (needs bytes inside the block). Type-isolated
heaps via Clang's `-fsanitize=alloc-token` are a plausible later addition
because Wolfi controls compiler flags; a roadmap item.

## 15. Optional per-CPU mode

Behind the `percpu_arena` key (section 12: `auto`, the default, `percpu`,
`off`; `config::PercpuArena`, plan Phase 2.5 task 8). With `percpu`, where
glibc's registered rseq area covers the `mm_cid` field (`rseq::probe`,
checked at run time and never assumed from a version: glibc exports
`__rseq_offset`, `__rseq_size` and `__rseq_flags` (2.35+) and the
registered `__rseq_size >= 28`, so the area covers the field the kernel
writes on 6.3+ with `CONFIG_SCHED_MM_CID`; glibc reports the kernel's
feature size there since 2.41, `research.md`; `PercpuArena::enabled`), the
plain `malloc`/`free` caches are keyed by the running thread's *concurrency
id* instead of by thread. When the probe fails `percpu` keeps thread caches
and says so (`PercpuArena::forced`, the `Per-CPU caches:` line of
`malloc_stats_print`, `stats.percpu.enabled`). `auto`, the key's default,
keeps thread caches as well (`heap::PercpuState::OffAuto`, "opt in with
percpu_arena:percpu"): the mode is implemented and measured, and the plan's
own rule (default-on only if `quick` and `ab` are within noise on the
reference VMs) failed by a margin (below), so thread caches stay the
default. `double_free_scan:true` also
keeps thread caches (the scan of the top entries is not written into the
critical sections; the hardening wins).

What the kernel provides (`src/rseq.rs`). `mm_cid` is a small integer,
unique among the threads of the process that are running at any instant
and bounded by the number of possible CPUs (`nr_cpu_ids`, read from
`/sys/devices/system/cpu/possible`), so a process with 200 mostly idle
threads on 4 CPUs uses ids 0..3: cache memory is bounded by cores, not
threads. Two implementations exist and the design relies only on what
they share (docs/research.md, the 6.19 row): before 6.19 (Chainguard OS
6.18) ids are `< min(threads, cpus)`, a thread gives its id up when it is
switched out and may come back with another, and the kernel hands ids
over through fully ordered atomics on the process's id mask; since 6.19
(the reference VMs' 7.0) a process with no more threads than possible
CPUs runs in *per-task* mode, every thread keeping its own id across
migrations (the per-CPU caches then behave like thread caches and no
header is ever shared), and the shared per-CPU ids are used only above
that count, switched at fork or exit. In both, an id has one running
holder at any instant and stays below `nr_cpu_ids`; the sections need
nothing else, and the aborted-section counter (`stats.percpu.aborts`)
shows the abort path running once threads outnumber CPUs. The area is at `thread pointer + __rseq_offset`
(`%fs:0` or `tpidr_el0`; the offset moves with the static TLS layout and
is read from the symbol, never assumed: on the reference VMs 192 (arm64)
and -192 (x86_64) bare, 320 and -384 inside the test binaries), the field
at byte 24, readable only when `__rseq_size >= 28` (33 on the 7.0 VMs, 20
on the Mac's Docker kernel, where the mode is off). The allocator writes
exactly one field of the area, `rseq_cs` (byte 8), the address of a
critical-section descriptor; `cpu_id`, `mm_cid`, `node_id` and `flags` are
the kernel's and are never stored to (writing them is what broke tcmalloc
on Linux 6.19). A critical section is a straight run of instructions
ending in one commit store; a thread preempted, migrated or signalled
inside it is moved to the abort handler (the kernel checks the signature
before it, `0x53053053` as a `ud1` on x86_64, `0xd428bc00` on aarch64) and
runs the section again, re-reading `mm_cid`. Every section here is
written in `asm!` on both architectures with its descriptor in the
`__rseq_cs` section beside it, and any store before the commit leaves the
header consistent if the section is aborted after it (a slot written into
the dead part of a stack, a lower bound shrunk); the kernel clears
`rseq_cs` when it finds the thread outside the section, so a completed
section is one store plus the work.

The caches (`heap/`, `Percpu`, `CpuCache`). One table of `nr_cpu_ids`
`[CacheHeader; NCLASSES]` arrays, the same headers and slot stacks as a
heap's home caches, reserved zeroed from the metadata region at init
(3.3 KiB per id, touched only as ids are used) and indexed
`base + mm_cid * CPU_CACHE_SIZE + class * 64` inside the section. A zeroed
header is `len == cap == 0`, which both fast paths treat as "slow path";
the first slow path on it installs its stack (`rseq::install`: the
idempotent fields with plain stores, then `slots` and the commit of `cap`
in a section that refuses to publish a header the thread did not prepare).
The sections: `pop` (count `allocs`, honour the compaction schedule, clip
`low_water`, load the entry, authenticate it when `pac_pointers` is on,
commit `len - 1`), `push` (store the entry above `len`, signed when
`pac_pointers` is on, clip the zero prefix, commit `len + 1`),
`push_batch` (a refill's batch or a flush's kept half, copied in a loop
inside the section; when the stack was empty the zero prefix and
`low_water` restart at the batch), `take_all` (a flush or drain copies the
whole stack out and commits `len = 0`). Capacities follow the thread
ladder clamped to `MAX_CAP` (the slow paths' buffers are stack arrays of
that size).

The paths (the zero-cost dispatch, lane percpu-dispatch, 2026-09-21).
The entry points carry nothing for the mode: no flag bit, no test, no
tail call. A heap in the mode (`Heap::percpu`) never installs a home
cache (`Heap::ensure_cache` refuses its home arena; an explicit
`MALLOCX_ARENA` request for that arena is served directly from the
slab), so every home header stays `CacheHeader::EMPTY`, `len == cap ==
0`, which is the state section 6.3's two fast paths already treat as
"slow path": `malloc`'s inlined pop falls through on `len == 0`, `free`'s
and the sized free's inlined push on `len == cap`, and each takes the
cold tail call it has anyway (`malloc_slow`, `free_slow`,
`free_sized_slow`), where `Heap::alloc`, `Heap::alloc_zeroed` and
`Heap::free` test the heap's `percpu` byte and go to
`Heap::percpu_alloc` / `Heap::percpu_free` (out of line, not cold: the
`pop` or `push` section on the running CPU's header, the byte count, the
refill or flush on a miss). So the thread paths are byte for byte the
allocator without the mode (the codegen list in
`bench/results/2026-09-21-*-percpu-dispatch*.md`), and what the mode
pays is the entry point's wasted header load and the slow path's
prologue (the init-state load, the second TLS read, `class_of` again or
the exact `classify_free` in place of the inlined check) before the
section. In the mode a pop or push is therefore that prologue plus the
thread-pointer load, the `mm_cid` load, the table index and the section;
the `free` path skips the owner word (a CPU cache takes verified slots
of every arena, section 6.3's sized path already does the same). The
earlier forms: a wrapper calling a second function that held the section
and returned an enum cost +65..83 instructions per larson operation in
the mode, the inlined section with cold tail-call misses +33..47
(`bench/results/2026-09-19-*-task8-percpu-fastpath.md`), and both were
reached through a `PUSH_PERCPU` bit of `Shared::push_flags` tested by
the entry points with a tail call, which cost the *default* path +9
instructions and +1.8% cycles per larson operation on the C4A (three of
them the dispatch on the hit paths, `bench/results/2026-09-20-aarch64-larson1-drift.md`),
the reason the dispatch moved. The thread heap stays: it is the init
witness the fast paths load anyway, it keeps the thread's byte counters
(every per-CPU pop and push is counted there per operation, so
`thread.allocated` stays exact), it serves huge runs as before, and a
heap off the mode takes the thread paths (its home caches are real):
explicit caches
(`tcache.create`), `thread.tcache.enabled:false` (served directly), and a
thread `thread.arena` moved to an explicit arena (its own caches on that
arena; moving it back to an automatic one returns it to the CPU caches;
jemalloc refuses the automatic range with `EPERM` under `percpu_arena`,
nothing here needs refusing). *Arenas are per id*: the header of
`(id, class)` binds to arena `id % narenas` at install and its refills
claim there, so with the default `narenas` (`4 * ncpus`, at most 64) every
CPU has its own arena; a slot freed on another CPU is pushed onto that
CPU's cache and the flush routes it back through the owner array
(`release_routed`). Slow paths: a refill claims up to `cap / 2` slots from
the id's arena onto the stack, hands the lowest to the caller and
`push_batch`es the rest, lowest on top; a flush (`take_all`, the address
selection and straddle compare of 6.4 over the taken array, release the
highest half, `push_batch` the kept half back, lowest on top when the
stack was in order); if the thread migrated between the two sections the
batch lands on the new CPU's cache and what does not fit is released to
its slabs. Compaction keeps its gate per header (probe every 4096 pops,
compact while the class is fragmented and refills are not draining it)
and its run as a header of its own: the thread run sits at the bottom of
the cache's stack and a run pop moves the top entry into the hole, a
two-word update no single commit can make consistent under an abort, so
under the mode the run is a second table of `nr_cpu_ids`
`[CacheHeader; NCLASSES]` (`Percpu::runs`), the header of `(id, class)`
holding up to `cap / 4` slots (`compact_mask == u32::MAX`, so it never
schedules), served by the same `pop` section a cache is. A scheduled pop
in compact mode pops the id's run (`Heap::percpu_run_pop`); an exhausted
or uninstalled run is *rebuilt* as the thread run is: the class
re-checked (dense: back to probing), else a block of `cap / 4` claimed
from the arena's lowest partial slab under the class lock, one hold per
block instead of one per scheduled pop, and pushed onto the run with
`push_batch`, lowest on top, the zero prefix travelling with it; what
does not fit after a migration goes back to its slab. (The first form
claimed one slot under the lock per scheduled pop, one pop in 8 on a
fragmented class: on the Valkey model that was most of the mode's cost,
+148 instructions per operation on the C4 against +58 with the run;
`bench/results/2026-09-19-*-task8-percpu-fastpath.md`.) The cache GC and
`thread.tcache.flush`, `malloc_trim` and the purges' caller flush act on
the caches of the CPU the thread is running on (the drain takes the run
as well; the GC returns a run whose class stopped compacting and leaves a
compacting one, which is being consumed); an idle CPU's caches are
drained the next time a thread runs there and ticks, and that residue,
at most `cap + cap / 4` slots per (id, class), is what the mode bounds by
cores. `stats.percpu.headers` and `.cached` count both tables. Thread exit leaves the CPU
caches alone (they are not the thread's); fork needs nothing (no lock, and
every section's pre-commit stores leave a consistent header; a child's
ids start over on the same table). Memory ordering across CPUs comes from
the kernel: an id changes hands only through the scheduler's fully ordered
atomics on the process's id mask (or, in the 6.19+ per-task mode, never
while threads fit the CPUs), so the stores of an id's previous holder are
visible to its next.

Measured (plan task 8, `bench/results/2026-09-19-{aarch64,x86_64}-task8-percpu.md`,
the `ab` files beside them; `old` main before the task, positive worse).
The mode on, protocol tier, C4A / C4: larson1 +13.3% / +9.0%, larson16
+18.1% / +28.0%, xmalloc +19.9% / +20.5%, alloc-test1 +10.2% / +10.6%,
cfrac +11.5% / +12.8%, glibc-simple (aarch64) and the Valkey model
(`valkey1` +38% / +58%, `valkey1-free` +49.8% on x86_64), mstress +3.5% /
+2.4% wall with 328 / 318 MiB against 330 / 342 (jemalloc 299 / 288: the
x86_64 row moves from +18.8% to +10.6% against jemalloc, 0.6 points
outside the Phase 1.5 band, at one arena per CPU). Per operation
(`quick-perf`): +83 / +65 instructions and +9.7 / +6.3 cycles on larson1
with L1D misses flat, so the price is instructions (the thread pointer,
the `mm_cid` load, the table index, the descriptor store, the section's
bookkeeping, in a called function against an inline two-load pop); on
the Valkey model +183 / +180 instructions and +68 / +107 cycles, the
extra being the compact-mode scheduled pop's locked one-slot claim (one
pop in 8 on a fragmented 500 MiB heap, where the thread run amortises one
lock over `cap / 4`). The default's own cost under the tail-call
dispatch (`percpu_arena:off` against `old`): +9 instructions and +1.3 /
+0.1 cycles per larson operation, the `PUSH_PERCPU` test in the entry
points; `quick` under the default was GO on both VMs, and the cost was
then removed by the zero-cost dispatch above
(`bench/results/2026-09-21-*-percpu-dispatch*.md`). What the mode buys is memory bounded by cores, visible only
with more threads than CPUs (the Ruby idle-thread soak rows in the same
files: the Sidekiq-shaped `bench/ruby/soak.rb` at 16 / 64 / 256 mostly
idle threads peaks at 305 / 1649 / 2426 MiB with thread caches on the C4A
and 90 / 106 / 115 MiB under the mode, 326 / 2073 / 3570 against 107 /
150 / 231 on the C4, the live set about 21 MiB in every run; a 64-thread
larson on 16 vCPUs runs at -31% / -34% throughput and -30% / -22% RSS).

Observability: `stats.percpu.enabled`, `stats.percpu.ids` (table
entries), `stats.percpu.headers` (installed), `stats.percpu.cached`
(slots, a racy snapshot) and `stats.percpu.aborts` (sections the kernel
restarted), the `Per-CPU caches:` line of
`malloc_stats_print` (state, the same counts, the probe's `__rseq_size`
and offset, and the note when `percpu_arena:percpu` fell back), and lane
B's `rseq`/`mm_cid` keys of the feature report. Tests: `rseq.rs` (the
both-ways probe, `mm_cid` distinct across busy threads), `heap/`
`linux_tests::percpu_*` (the mode follows the probe and the key; plain
allocations through the CPU caches with exact thread counters and the
zero prefix; the flush's high-half release and the drain; slot
conservation under 2x-CPU threads, migrations and aborts; heaps off the
mode), `tests/c/percpu.c` under `percpu` and `off`. Cost and the default
decision: `bench/results/2026-09-19-*-task8-percpu.md`.

The feature report's `rseq=` words (`sys::features::rseq_words`, derived
from `rseq::probe`) come from the same three symbols plus the area's
kernel-written `cpu_id` (the fact of registration) and print `rseq=yes rseq_size=33 mm_cid=yes` (the
7.0 GCE kernels under glibc 2.44), `rseq=yes rseq_size=20 mm_cid=no` (a
kernel that reports no `AT_RSEQ_FEATURE_SIZE`, the Mac's 6.10 linuxkit VM)
or `rseq=no` (no registration, or a libc without the symbols). It is the
report's verdict; the mode reads `mm_cid` itself through `rseq.rs`.

The protocol is model-checked before it is implemented (section 17, the
model row; `src/model.rs`, the `percpu_*` scenarios, 2026-09-19, lane C),
and an implementation is to be read against these properties at merge:
`mm_cid` is read inside the critical section and the cache index is
derived from that read only; the section ends with a single commit store
(the `len` write; a push's entry store before it is dead until that
write); the abort handler restarts at the section's start and re-reads
`mm_cid`; no kernel-owned field of the rseq area is ever written; the
refill and the flush run outside the section, the batch is pushed back
through a second section on whatever cid the thread is on then (a thread
that popped on one cid may push on another, and a cache then holds slots
of another cid's arena), and a flushed batch is released slab by slab
through the owner word's arena (`release_routed`); the cache's zero prefix
follows design 6.3 (a zero batch extends it, a push below it clips it, a
flush drops it). The model shows that a pop or push done as plain loads
and stores under the cid last read, or with `mm_cid` read before the
section, hands one slot to two threads within 40 states, and that the
protocol as stated passes every scenario.

## 16. Observability

- `malloc_stats_print` emits a human-readable report in jemalloc's general
  shape: totals, per-arena, per-class table with `size nregs curslabs curregs`,
  and one `cgroup:` line (section 11): the directory, mount and path, the
  effective `limit` and `high` (`max` for unlimited), the last `current`
  sample, the `memory.events` `high`/`max` counters with the number of
  rises seen, the PSI state (`not armed (no background thread)`, `armed
  (some ...)`, `unavailable (EROFS)`, `disarmed (background thread gone)`)
  with `pressure_events`, and the CPU figures; or `cgroup: not tracked
  (<reason>)`; and one `cgroup policy:` line with the band as last
  evaluated, arena 0's effective `decay_ms`, the `retention_cap`, the
  `empty_bytes` held, the `full_purges` run and whether `thp` was forced
  to `never`; and one `purge advice:` line (section 8) with the advice the
  next purge issues and why (`under 70% of the limit`, `no headroom
  known`, `lazy_free`, `lazy_band:false`, `explicit purge`), the batches
  and bytes issued with each advice, whether `Purged` memory is still
  known zero, and whether lazy batches go through `process_madvise`; and
  one `thp:` line (section 17) with the key, the mode in force (`off`,
  `folios`, `chunks`) and why, the slab-size threshold, the PMD size, the
  metadata folio, whether huge runs are advised, and the ranges advised so
  far (`class_ranges` with bytes and the chunks among them, `huge_ranges`,
  `meta_ranges`, `failures`; under `never` also `nohuge N (classes,
  metadata; huge follows host)`, the reservations `MADV_NOHUGEPAGE`d and
  what the advice leaves to the host), also readable as `stats.thp.mode`,
  `stats.thp.class_ranges`, `stats.thp.class_bytes`, `stats.thp.huge_ranges`,
  `stats.thp.meta_ranges`, `stats.thp.failures` and
  `stats.thp.nohuge_ranges` (`size_t`, read-only). The
  JSON form has a matching `"cgroup"` object with the policy fields and
  `purge_advice`, `lazy_purges`, `eager_purges`, `purged_is_zero`, and a
  `"thp"` object with the same fields as the line.
  The run-time options list marks the inert keys "accepted, no effect". The per-class table also carries the compaction
  diagnostics of section 6.4 (`refilled`, `compacted`, caches that entered
  and left compact mode, probes the refill-rate gate declined) and is
  followed by a per-class occupancy histogram of the non-empty slabs
  (slabs per eighth of `nslots`), so a soak's `MEMORY MALLOC-STATS` shows
  where the live data sits and whether the schedule is running.
- The heads of the text report's lines (`Kernel and CPU features: `,
  `Prefault: `, `cgroup policy: `, ...) and the fixed words a line can
  continue with are spelled once, in `src/api/print.rs` (`head!`, listed
  in `LINES`); the writers build their format strings from them. The test
  `print::tests::quoted_needles_match_the_report` reads `tests/c/*.c`,
  `docs/tuning.md` and sections 1..16 of this document, takes every
  quoted string that starts like one of those lines and checks it, with
  the figures masked, against a report, so a renamed or dropped line
  fails the suite at the stale quote.
- Mappings named `challoc:classes` (the one class superblock), `challoc:huge`
  and `challoc:meta` (`PR_SET_VMA_ANON_NAME`, when the kernel has it).
- `stats_print:true` is accepted and inert: nothing prints at exit and there
  is no `atexit` handler (jemalloc's `stats_print` has no equivalent here;
  call `malloc_stats_print` or `malloc_info`).
- Abort messages include the faulting pointer, its classification, and the
  reason, prefixed `challoc: `.
- `challoc_version()` (`const char *`, the crate version, e.g. `0.1.0`) is
  the probe for "is challoc the loaded allocator": `dlsym` it. It reads
  and writes no allocator state and does not initialise the allocator;
  `mallctl("version")` stays the jemalloc-shaped `5.3.0-0-gchalloc` for
  parsers of that string.
- `challoc_features()` (`const char *`) is the feature report: one
  NUL-terminated line of `key=value` pairs separated by single spaces, the
  kernel probes first (`page_size=4096 guard_install=yes mseal=yes
  process_madvise=pidfd_self process_madvise_free=yes anon_name=yes
  populate_write=yes thp=madvise mthp=2M rseq=yes rseq_size=33
  mm_cid=yes slice_ext=kernel-disabled`: whether `MADV_POPULATE_WRITE` works (5.14,
  section 8), the THP setting or `none`, the
  folio sizes a `MADV_HUGEPAGE`d range can get or `none`, glibc's rseq
  registration with whether it covers `mm_cid`, section 15, and the
  kernel's answer for the time slice extension, section 17) then `pac=yes`
  or `pac=no` (pointer authentication, section 17). The string is static,
  built once at init without allocating (rebuilt in a fork child, whose
  purge backend is re-probed), and byte-identical to the `Kernel and CPU
  features:` line of `malloc_stats_print`. Calling it initialises the
  allocator. Values are `yes`/`no` for bits, so a script can `grep -q
  'pac=yes'`; keys are only ever added, never renamed.

## 17. Floor and ceiling

The rule is stated in section 1. This section is the inventory: every
feature the allocator probes, who provides it, what the allocator turns on
with it today, and what it does without it. Nothing in this table is decided at build
time: the compiler baseline stays where Wolfi sets it and the same binary
runs on every row.

| Feature | Provided by | challoc turns on | Fallback |
|---|---|---|---|
| Page size (`AT_PAGESZ`) | kernel, the auxiliary vector | the granularity of every mapping, slab and purge (4, 16 or 64 KiB) | none: init aborts without it |
| `MADV_GUARD_INSTALL` | kernel 6.13 | guard slabs between slabs and after huge runs (section 14) | a `PROT_NONE` page after huge runs only; no guard slabs |
| `process_madvise` on self | kernel 6.13 with a pidfd of self (the syscall exists since 5.10 but accepts `MADV_DONTNEED` only from 6.13; the matrix of Phase 2.5 task 3 found it unavailable on 5.10, 6.1 and 6.8 even without seccomp, lit on 7.0); 6.15 with `PIDFD_SELF` | batched purge, one call per decay pass (section 8) | a loop of `madvise(MADV_DONTNEED)` |
| `process_madvise(MADV_FREE)` on self (`process_madvise_free`) | the same kernels, probed separately through the target the `MADV_DONTNEED` probe found (yes on the 7.0 reference VMs) | the lazy batches of the two-advice purge (section 8) take the batched path | lazy batches through a loop of `madvise(MADV_FREE)`; eager batches unchanged |
| `MADV_FREE` (the advice itself) | every kernel since 4.5 | `Empty` slabs and huge extents under 70% of a known cgroup limit are returned lazily (sections 8 and 11); `lazy_free` everywhere | `MADV_DONTNEED` everywhere (no limit known, `lazy_band:false`) |
| `mseal` | kernel 6.10, not filtered by seccomp | seals the never-committed ranges of the reservations at init (section 10 step 5) | skip; `Sealed ranges: 0 of N` |
| `PR_SET_VMA_ANON_NAME` | kernel 5.17 with `CONFIG_ANON_VMA_NAME` | named `challoc:*` mappings in `/proc/pid/maps` | skip |
| `MADV_POPULATE_WRITE` (`populate_write`) | kernel 5.14 (probed on a scratch page: the call and `mincore` afterwards) | the prefault pass (section 8, plan Phase 2.5 task 11; `purge::prefault`, `ClassState::prefault_next`), behind `prefault:true` with `background_thread:true`: the background thread commits the slab the next refill of a class would commit and populates it with one call while it is still unpublished, then publishes it as `Purged` for the refill to take; `stats.prefault.*` and the `Prefault:` report line | the same pass with one write of zero per page; `Prefault: off (prefault:false)` by default |
| PSI triggers on `memory.pressure` | kernel 6.4 (unprivileged), cgroup2 mounted writable | the background thread arms a trigger and reacts to pressure at once (section 11) | sample `memory.current` on the tick |
| rseq `mm_cid` (`rseq`, `rseq_size`, `mm_cid`) | kernel 6.3 (`CONFIG_RSEQ`, `CONFIG_SCHED_MM_CID`) reporting `AT_RSEQ_FEATURE_SIZE >= 28`, glibc 2.35+'s registered area (`__rseq_size`, `__rseq_offset`, `__rseq_flags`, read from the weak symbols at init, and the area's own `cpu_id` for the fact of registration; `rseq::probe` for both the mode and the report's `rseq=` words; `__rseq_size >= 28` is the gate) | the per-CPU mode (section 15, plan Phase 2.5 task 8): caches and arenas keyed by the running thread's concurrency id, pop and push in rseq critical sections, behind `percpu_arena:percpu` (the opt-in; `auto`, the default, is thread caches); the cost and the default decision are in section 15; `rseq=yes rseq_size=33 mm_cid=yes` on the 7.0 GCE VMs | thread caches (`percpu_arena:off`, `double_free_scan:true`, or `rseq=yes rseq_size=20 mm_cid=no` as on the Mac's 6.10 linuxkit VM, `rseq=no rseq_size=0` without a registration: `GLIBC_TUNABLES=glibc.pthread.rseq=0`, no `CONFIG_RSEQ`, a libc without the symbols); `percpu` says so in the report |
| Transparent huge pages (`thp`, `mthp`) | any kernel with `CONFIG_TRANSPARENT_HUGEPAGE`; the per-size `hugepages-<n>kB` knobs since 6.8 | off by default (`thp:never`: byte for byte the allocator without the key on the fast paths, plus one `MADV_NOHUGEPAGE` each on the class superblock and the metadata reservation at init (plan Phase 2.5 task 16, `thp::advise_reservation` from `layout::Regions::reserve`, and again from `force_never`), a VMA flag every later split inherits, so that on a host whose global mode is `always` (Chainguard OS ships `always` with the 2 MiB size on `inherit`; Debian's kernels boot `always`) no slab region of challoc's gets a huge page; the huge region is not advised and follows the host's policy (the 2026-09-20 decision, option (d) of `bench/results/2026-09-20-aarch64-cgos-thp-ab.md`: `never` is the slab regions' RSS discipline, and a huge run is a whole allocation the program asked for, which takes 2 MiB pages on an `always` host as jemalloc's large extents do; advising it too cost the Valkey model 10 points of `valkey1` on Chainguard OS, the 32 MiB of tables it keeps in huge runs, for no RSS gain on that heap). The both-ways test `tests/c/thp.c never` reads the global mode from sysfs, checks `nh` on the class region and no `nh` on the huge run on every kernel with THP, `stats.thp.nohuge_ranges` 2, and, on an `always` host, `AnonHugePages` 0 kB and `THPeligible` 0 on the class region after 8 MiB of small blocks and `THPeligible` 1 with `AnonHugePages` back on an 8 MiB huge run; the before/after numbers on the 7.0 VM with the global mode set to `always` are in `bench/results/2026-09-19-lane-b-floor-fixes.md`, the same-host A/B of the four settings on Chainguard OS in `2026-09-20-aarch64-cgos-thp-ab.md`. `auto` and `always` issue no `MADV_NOHUGEPAGE`: on an `always` host their unadvised regions stay eligible for the host's PMD pages. `thp:auto` (plan Phase 2.5 task 9, `thp.rs`): `MADV_HUGEPAGE` on the slab groups of every class whose slab holds an enabled folio size below the PMD (64 KiB slabs and up with `hugepages-64kB` enabled), and metadata commits rounded to and advised with that folio, never a 2 MiB page (the fork copy-on-write cost that makes Redis and Valkey forbid huge pages stays absent); inert, and the report says why, where the kernel enables no such size or where the PMD size is `madvise`-gated (`hugepages-2048kB/enabled` `madvise`, or `inherit` under a `madvise` top level, Ubuntu's default), because the same advice would then make the regions eligible for 2 MiB pages at the fault and through khugepaged; the host requirement is `hugepages-64kB/enabled=madvise` (or `always`) with `hugepages-2048kB/enabled=never` (or `always`, the host's own decision), root-only knobs. `thp:always`: with the PMD size enabled and no larger than 2 MiB (`thp::LG_PMD_CHUNK_MAX`: the PMD is `page * page / 8`, 2 MiB at 4 KiB pages, 32 MiB at 16 KiB, 512 MiB at 64 KiB, and a chunk of the larger sizes committed and advised per class would be the RSS floor of the process: the 64 KiB matrix cell showed 1.7 GiB of advised chunks for an 8 MiB test heap and the kernel gave none of them a folio, `THPeligible 0`), class regions are committed and advised in PMD-sized chunks so the first touch maps a 2 MiB page (guard slabs inside a chunk stay unused but unguarded: a marker there splits the page), huge runs of a PMD or more are advised (section 7), and the metadata takes the sub-PMD folio if one is enabled; a purge of a slab inside a 2 MiB page splits it, the kernel's cost of the mode. On a PMD over 2 MiB `always` is the folio path over the enabled sizes below the PMD (2 MiB slabs and up when `hugepages-2048kB` is enabled at 64 KiB pages) and inert, reason `PmdTooLarge`, when none is. Measured in `bench/results/2026-09-19-lane-b-mthp.md`: `auto` takes a quarter of larson1's dTLB refills on the C4A (4.0 -> 3.05 per operation, jemalloc 1.75) and puts valkey1's below jemalloc's (2.81 -> 2.44), leaves the Valkey-shaped rows flat, makes `fork()` 18% cheaper on a 500 MiB heap and the copy-on-write burst unchanged, and costs larson 1.6% with 7..19% more RSS on 16..100 MiB heaps (a folio per touched slab and table), which is why it is the documented opt-in and not the default; `always` is +10..15% on valkey1 at +10% RSS on 585 MiB and 2..4x on small heaps | small pages; `thp=none mthp=none` on a kernel without THP |
| PAC (`HWCAP_PACA` and `HWCAP_PACG`) | arm64 CPU (Armv8.3) and kernel | signed cache entries and TLS heap pointer (`pacda`/`autda`, sections 6.1 and 6.2) behind `pac_pointers:true`, off by default on the cost (section 14); `pac=yes` on Axion and the Mac VM, `no` on Altra; FEAT_FPAC decides whether a failure faults at the `autda` or on first use | plain pointers, the same binary, one predicted branch on a byte the fast paths load anyway |
| rseq time slice extension (`slice_ctrl`, `RSEQ_CS_FLAG_SLICE_EXT_AVAILABLE`) | kernel 7.0 with `CONFIG_RSEQ_SLICE_EXTENSION` (default N; needs `GENERIC_ENTRY` and `HAVE_GENERIC_TIF_BITS`, which x86_64 has and arm64 does not, through master) and glibc registering the v2 size (`__rseq_size >= 32`: 2.44 registers 33); Ubuntu's `7.0.0-1011-gcp` leaves the option off, so the GCE reference VMs read `kernel-disabled` and the numbers come from a 7.0.14 built with it (`bench/results/2026-09-19-x86_64-lane-c-task15-slice-ext.md`) | every lock holder's request to defer a preemption until the release (`slice.rs`, the subsection below; `slice_ext:auto`), the `prctl` enable per thread on its first hold, `rseq_slice_yield` (or `sched_yield` where seccomp hides the new syscall) after a granted release; the feature line's `slice_ext=` key (`sys::features::SliceExt`, read through `slice::probe` and `slice::verdict`, plan task 15) says `on` when lock holders request it and otherwise the kernel's answer (`available` under `slice_ext:off`, `kernel-disabled`, `no-slice_ctrl`, `no-rseq`, `prctl-failed`) | the lock without the request: one relaxed load per hold; `Slice extension: off (<why>)` in the report and the same word after `slice_ext=` in the feature line |
| Model-checked protocol (`cargo test --features std model::`, `src/model.rs`) | the host test suite, no hardware: an exhaustive interleaving model of the claim, release, `on_live_zero`, purge and group-handoff steps of `class/`, memoised on the whole state, with the invariants of section 4 and 5 checked at every state (no slot handed to two claimers, a slot reported zero reads zero, `live` equals set bits plus in-flight reservations, a slab in an arena's set is `Empty`/`Purged` with `live == 0` and owned by that arena, a `Purged` slab reads zero) | nothing at run time: it is the proof obligation for a change to the protocol. Replaying the pre-fix step orders finds the calloc race (Phase 1.5 task 7) and the ownership gap the handoff's owner check closes (task 15); the current order passes every scenario. Since 2026-09-19 (lane C, plan Phase 2.5 task 8) the `percpu_*` scenarios model the per-CPU claim protocol of section 15 (caches owned by an `mm_cid` read inside an rseq critical section with one commit store, the kernel's abort of a section whose thread was descheduled, threads that pop on one cid and push on another, the refill and the flush through the cid's arena, the zero prefix) with the further invariants that no slot is in two caches or in a cache and owned by a thread, that every set bit has exactly one holder (a lost push is a bit with none) and that a cache is mutated only by a section whose thread kept its cid to the commit; the variants without a section, or with `mm_cid` read outside it, hand a slot to two threads within 40 states, and lane A's implementation is compared against the properties listed in section 15 at merge | none needed |

The kernel keys of the feature report are `page_size guard_install mseal
process_madvise process_madvise_free anon_name populate_write thp mthp
rseq rseq_size mm_cid slice_ext`
(`populate_write` since plan Phase 2.5 task 11: the prefault pass's
populate; `rseq`, `rseq_size` and `mm_cid` since plan
Phase 2.5 task 8, lane B: the glibc registration is probed like a kernel
feature and printed with them; `slice_ext` since task 15: the kernel's
answer for the time slice extension, `kernel-disabled` on the 7.0 GCE
kernels, `on` where the module is in force);
`pac` follows them, the one CPU key (`HWCAP_PACA` and `HWCAP_PACG`; the
container test checks it against `/proc/cpuinfo`'s `paca` and `pacg`).
The reference hosts' lines are recorded in `bench/results/2026-09-19-feature-report.md`; the
matrix of plan tasks 3 and 4 adds one line per kernel and CPU.

### Time slice extension

Kernel 7.0's rseq time slice extension (`CONFIG_RSEQ_SLICE_EXTENSION`,
`Documentation/userspace-api/rseq.rst`) lets a thread ask the scheduler
to defer a preemption that would land inside a short critical section:
`struct rseq` gained `slice_ctrl` at byte 28 (`request` u8 at 28,
`granted` u8 at 29), the kernel advertises the feature per registered area
with `RSEQ_CS_FLAG_SLICE_EXT_AVAILABLE` (bit 4 of the area's `flags`,
byte 16) once the area is registered with the v2 size (`__rseq_size >=
32`; glibc 2.44 registers 33), and `prctl(PR_RSEQ_SLICE_EXTENSION = 79,
SET = 2, ENABLE = 1)` turns it on for the calling thread, confirmed by
`RSEQ_CS_FLAG_SLICE_EXT_ENABLED` (bit 5). A thread that stores `request =
1` and is hit by a tick that wants to reschedule it is granted up to 5 us
more (50 at most, `debugfs rseq/slice_ext_nsec`): the kernel clears
`request`, sets `granted`, arms a timer and returns to user space; the
thread finishes its section, clears `request`, and if `granted` reads 1
calls `rseq_slice_yield` (471 on both architectures, the unified table),
which hands the CPU back at once. Any other syscall inside a grant also
ends it (the kernel revokes and zeroes both bytes at syscall entry); a
grant nobody yields expires with the timer. User space may write nothing
but `request`; the kernel's debug mode kills a process whose `slice_ctrl`
disagrees with its own state at a syscall entry.

challoc's use (`slice.rs`, plan Phase 2.5 task 15): every `lock::RawLock`
hold, that is the class, table, huge, metadata, statistics and background
locks, none of which is on the `malloc`/`free` fast path. `RawLock::lock`
and `try_lock` call `slice::acquired` after the CAS (the request is made
once held, never while waiting: a waiter that spins while the holder is
extended is what the extension exists for), `RawLock::unlock` calls
`slice::releasing` before the release swap and `slice::released` after
it. Per hold, when on: one load of the thread's `flags` (the same cache
line as `slice_ctrl`), one byte store of `request = 1`, one byte store of
`0`, one byte load of `granted`; when off (the key, or a failed probe):
one relaxed load of a process-wide byte. The enable is per task, not per
process: `rseq_fork` resets a `CLONE_VM` child's rseq state and glibc
registers the new thread's area without the enable, while a plain `fork`
child inherits the forking thread's state (confirmed by
`tests/c/slice_ext.c` through `prctl(GET)` on 32 new threads and a fork
child). So init enables the init thread from `config::load`
(`Config::apply_slice_ext`) and every other thread is enabled on its first
lock hold, when `flags` shows bit 4 without bit 5: one `prctl` per thread
lifetime, and a `prctl` that fails switches the process off rather than
retry a syscall per hold. Nested holds (the fork handlers, heap creation
under the cache-table lock) request per lock; the inner release withdraws
the request and the outer hold goes on unextended, which costs nothing
but the extension. A `FUTEX_WAKE` issued by the release ends a grant
itself, so the yield after it reads `granted == 0` and does nothing. Where
a seccomp filter returns `ENOSYS` for the new syscall (Docker's default
profile does, for every syscall past its table, while letting the `prctl`
through) the grant is ended with `sched_yield` instead and the report
says so; the probe calls the dedicated syscall once at init to find out.

The probe (`slice::probe`): `rseq::probe` for the symbols and the size,
then bit 4 of this thread's `flags`, then the `prctl` and bit 5, then the
syscall; each failure is a word in `Slice extension: off (<word>)`:
`no-rseq`, `no-slice_ctrl` (6.18 and earlier), `kernel-disabled` (the
option compiled out or `rseq_slice_ext=0`; Ubuntu's `7.0.0-1011-gcp` on
the GCE reference VMs, whose config has no `CONFIG_RSEQ_SLICE_EXTENSION`
line), `prctl-failed`. The feature line carries the same word after
`slice_ext=` (`sys::features::SliceExt`, plan task 15, lane B): `Features::
probe` reads `slice::probe().verdict()` before the configuration is read,
which gives the kernel's answer (`available` when it has the option), and
init folds `slice::verdict()` in after `config::load` has run `configure`,
so the line says `on` exactly when lock holders request the extension,
`available` when the key turned it off on a kernel that has it, and the
probe's own word otherwise; `tests/c/features.c` checks the word against
`rseq_size` and against the `Slice extension:` line of the same report,
and `sys::features::linux_tests` against `slice::probe` on the running
kernel. On aarch64 the probe fails on every kernel to
date: the option depends on `GENERIC_ENTRY` and `HAVE_GENERIC_TIF_BITS`,
which arm64 selects neither of through 7.2 and master, so the aarch64 rows
measure the fallback's one load. The `on` way was measured on the lane's
C4 under a 7.0.14 kernel built with the option
(`bench/results/2026-09-19-x86_64-lane-c-task15-slice-ext.md`): what a
grant costs, how many holds are granted on the contended rows (the count
of holder preemptions the kernel deferred, `Slice extension: on (N
threads enabled, M grants yielded via ...)`), and the `quick` and `ab`
verdicts that decide the default; the aarch64 fallback rows are in the
`-aarch64-` file next to it.

## 18. Tagged pointers

Plan Phase 2.5 tasks 5 (this section) and 6 (the implementation, gated on
hardware). Everything below is design against the architecture manuals
and the kernel's documented ABI; no code path exists yet, the
`tag_pointers` key is not parsed, and nothing here changes a default.
Section 14's rule holds: an opt-in per consumer until the Valkey and Ruby
suites pass with it on.

### 18.1 What the hardware we can reach offers

A `cpuid` and HWCAP probe ran on every CPU GCE sells
(`bench/results/2026-09-19-cpu-matrix.md` section 1,
`2026-09-19-feature-report.md`):

| Feature | Probe | Result on GCE | Hardware that does expose it to a Linux guest or host | Confidence |
|---|---|---|---|---|
| MTE (Armv8.5 `FEAT_MTE`) | `HWCAP2_MTE`, `HWCAP2_MTE3` | `mte=no` on Axion (Neoverse V2, `hwcap2=0x3f3ff`, bits 18 and 22 clear) and on Altra (Neoverse N1, Armv8.2, no MTE in silicon) | AmpereOne bare metal: confirmed by Ampere's documentation and by measured numbers (tags in ECC bits, about 7.6% geomean SPECrate for whole-system tagging; `docs/research.md`). AWS Graviton4 (Neoverse V2, the same core as Axion, which hides it) and Azure Cobalt 100 (Neoverse N2): unconfirmed, neither vendor lists it; a `grep mte /proc/cpuinfo` on the smallest instance settles each for cents. Android phones since Tensor G3 / Dimensity 9300 expose it, not a server. | AmpereOne high; Graviton4 and Cobalt low, expect no |
| LAM (Intel Linear Address Masking, `LAM_U57`) | `cpuid` 7.1 `EAX[26]` | `lam=no` on every x86 cell: Emerald Rapids 8581C (C4, N4), Sapphire Rapids 8481C (C3), Cascade Lake (C2) predate it | Granite Rapids and Sierra Forest (Xeon 6) with kernel 6.4 or later (`arch_prctl(ARCH_ENABLE_TAGGED_ADDR)`); KVM can pass LAM to a guest since 6.8, so the cloud instance generations built on Xeon 6 (AWS's 8th-generation Intel shapes) may show it, unchecked. No cloud host known to us exposes it today. | hardware high; any cloud guest unknown |
| UAI (AMD Upper Address Ignore) | `cpuid` `0x80000021` `EAX[7]` | `uai=no`: the Genoa guest (C3D, EPYC 9B14) reads `0x10000067`, bit 7 clear, the hypervisor hides it (and LA57); Milan (Zen 3) has none | Zen 4 and later silicon has it (`EFER.UAIE`). Linux has no user-space enable: the 2022 "x86: Implement Upper Address Ignore" series was not merged, there is no `arch_prctl` and no `/proc/cpuinfo` flag, so `uai=yes` from a bare-metal Genoa (a GCE `-metal` shape would show the real CPUID) is still unusable. Re-check upstream at part 2. | silicon high; Linux support: none, to be re-checked |
| TBI (Armv8.0 top byte ignore) | none needed: every aarch64 Linux ignores bits 63:56 of a user address in loads and stores; `prctl(PR_SET_TAGGED_ADDR_CTRL, PR_TAGGED_ADDR_ENABLE)` (kernel 5.4) makes syscalls accept them | available on Axion and Altra | every arm64 host, the reference C4A included | measured (the ABI is documented; the reference VMs run 7.0) |

Consequence: neither hardware-checked mode runs on any GCE guest.
The software-tag mode (18.3) needs only ignored address bits, which TBI
gives on every arm64 host, so that half of part 2 is not gated; the MTE
half and the LAM cost rows are. Section 18.6 has the cheapest path.

### 18.2 MTE

MTE gives every 16-byte granule of a `PROT_MTE` mapping a 4-bit
*allocation tag* stored out of line by the hardware, and every pointer a
4-bit *logical tag* in bits 59:56 (inside the byte TBI ignores). A load
or store through a pointer whose logical tag differs from the granule's
allocation tag is a *tag check fault*. The allocator's job is to keep the
two equal for live blocks and different for everything else.

Mappings. When the mode is on, every `mprotect` that commits a slab or a
page group (section 5) passes `PROT_READ | PROT_WRITE | PROT_MTE`
(`PROT_MTE` is per `mprotect` call, not sticky: passing it every time is
the form that is right on every kernel). Guard slabs and the reserved
`PROT_NONE` ranges are unchanged. Huge runs are not tagged: a run is
page-granular and a tag sweep over megabytes per `malloc` is the wrong
trade when the run's guard page already does the bounds job; huge
pointers carry tag 0 and their pages have tag 0, so they match. Tag 0 is
excluded from the tags the allocator generates (`irg` with the exclusion
mask `GCR_EL1.Exclude` set through the `prctl` below), so an untagged
pointer into a slab, a forged one, faults. Fresh pages of a `PROT_MTE`
mapping have tag 0 with their zero data, and `MADV_DONTNEED` resets tags
with the data, so a `Fresh` or `Purged` slab reads tag 0 everywhere; that
is the state `claim_prefix` and the zero prefix of section 6.3 already
know about.

Tagging. The invariant is *the cache holds pointers whose logical tag
equals their slot's allocation tag*, so the pop of section 6.3 step 4 is
untouched: `p = slots[--len]` returns a pointer that already checks. The
tag work sits on the three paths that put a pointer into a cache:

- `free` (and the sized path, and a direct release): after the
  classification of step 1 (on the stripped address, 18.4) and the checks
  of step 2, `ldg` the slot's allocation tag (one tag load; the slot's
  granules all carry one tag, so the first is enough) and compare it with
  the pointer's. A mismatch aborts (`free of <p>: pointer tag 0x5,
  allocation tag 0xa (class c, slab s, slot k): stale or forged pointer`):
  this is the double free after reuse and the stale-copy case the bit
  test cannot see (the slot is live again, its bit is set). Then `irg`
  a new tag excluding 0 and the old one (14 candidates, from the CPU's
  generator; not the heap's xorshift), *retag the slot* to it with
  `st2g` per 32 bytes (or `dc gva` per `DCZID_EL0` block, 64 bytes on
  every core we know, when the slot is at least that and aligned), and
  push `p` with the new tag. From this instant every pointer the program
  still holds to the block is stale in hardware: a read through it faults
  (in the mode of 18.2's check paragraph).
- A refill (`ClassState::finish`, section 5): an `Active` pick's slots
  were retagged at their last free, so the refill reads each slot's tag
  with `ldg` and pushes the tagged pointer (one tag load per slot, in the
  loop that already writes the pointer). A zero pick's slots (a `Fresh`
  or `Purged` slab, tag 0) are left at tag 0 in the cache's zero prefix
  and tagged at pop: the pop tests the zero-prefix boundary it already
  tracks (`len <= zeroed` on a branch that is cold in steady state),
  `irg`s and sweeps `stg` over the slot, or `stzg`/`dc gzva` on the
  `calloc` path, which zeroes and tags in one sweep, so the zero prefix
  still saves `calloc` its `memset`. Tagging at pop rather than at refill
  keeps the refill from touching (and committing) every page of a batch
  the program has not asked for yet.
- A slot that leaves a cache for the slab (`Heap::flush`, thread exit)
  keeps its allocation tag in memory; nothing to do. `release_owned`
  receives stripped addresses (18.4).

`realloc(p, n)` checks `p`'s tag as `free` does (abort on a mismatch);
a same-class resize returns `p` unchanged, tag included; a class change
allocates (new tag), copies through the new pointer and frees `p` (the
retag). `calloc` is `malloc` with the zero path above. `malloc_usable_size`,
`sallocx` and the size read of `rallocx`/`xallocx` check the tag as
`free` step 2 does and abort on a mismatch with the same message shape.
`mallctl` lookups that take a pointer (`arenas.lookup`, the batch query,
`je_get_defrag_hint`, 13.4 and 13.5) classify the stripped address and
do not check the tag (they answer questions about the slot, not the
pointer).

Check mode. `prctl(PR_SET_TAGGED_ADDR_CTRL, PR_TAGGED_ADDR_ENABLE |
PR_MTE_TCF_<mode> | (0xfffe << PR_MTE_TAG_SHIFT))` at activation, then
in every thread on its first contact with the allocator (the setting is
per thread, inherited by `clone`; threads that existed before init would
otherwise run unchecked, so the heap-creation path of 6.4, cold, applies
it). The modes, and the `tag_check` key that selects them:

- `async` (default): the core records a fault in `TFSR_EL1` and the
  kernel delivers `SIGSEGV` with `si_code = SEGV_MTEAERR` at the thread's
  next kernel entry or context switch, *without the faulting address*
  (`si_addr` is meaningless). Cheapest: the check is off the load path.
- `asymm` (needs `mte3=yes`, `FEAT_MTE3`, kernel 5.18): reads checked
  synchronously, writes asynchronously. The read is the leak; the write
  through a stale pointer is caught a little later. The recommended mode
  where the CPU has it; not the default only because the plan named
  `async`.
- `sync`: every access checked at the instruction, `SIGSEGV` with
  `SEGV_MTESERR` and `si_addr`. The mode the negative tests run under,
  and the one to switch a deployment to when an `async` fault has to be
  located.

Reporting. When the mode is on, challoc installs a `SIGSEGV` handler
(`rt_sigaction`, `SA_SIGINFO | SA_ONSTACK | SA_NODEFER`, saving the
previous action). For `SEGV_MTESERR` it writes `challoc: tag check fault
at <addr> (class c, slab s, slot k; pointer tag 0x5, allocation tag
0xa)` (or `outside the heap`), for `SEGV_MTEAERR` `challoc: asynchronous
tag check fault in this thread since its last kernel entry (tag_check:sync
gives the address)`, then `abort()` through the existing path (section
16: `write(2)` then `abort`). Any other `si_code` is handed to the
saved action, so a consumer's own handler still sees its own faults. A
consumer that installs its own `SIGSEGV` handler after ours (Ruby does,
for its crash report) replaces it: the fault is still fatal and Ruby's
report shows the MTE `si_code`; the allocator's line is best-effort and
is documented as such.

Fork. Allocation tags are part of the page. The kernel copies them with
the data on a copy-on-write fault of a tagged page (`PG_mte_tagged`,
`copy_highpage`), 1/32 of the page in bytes, done as a granule loop; the
child inherits `PROT_MTE` and the checks. Under a Valkey `BGSAVE` this
is a per-page cost on the parent's write faults for the life of the
child; part 2 measures fork time and the write-phase p99 with the child
alive (design 9's shape). `MADV_DONTNEED` of a tagged page in the parent
is unchanged.

Cost, expected. Retag on free is `zero_on_free`'s store pattern in
hardware: one store instruction per 32 or 64 bytes of the slot, the same
instruction count as the `dc zva` `memset`, and on AmpereOne, where tags
live in the line's ECC bits, the same line fills, since a tag store is a
read-modify-write of the line. `zero_on_free` measured +87% / +102% on
`larson1`, +77% / +80% on `larson10`, +55% / +183% on `valkey1` and
+1.6% / +1.3% on `redis` (aarch64 / x86_64,
`bench/results/2026-09-18-*-gce-tunables.md`), so the working
expectation for `tag_pointers:mte` is that envelope on the same rows,
plus one `ldg` per free and per refilled slot (a tag load from a line
the path may not otherwise touch), plus the check mode's own cost
(AmpereOne's whole-system figure is 7.6%, which bounds `sync`). That
puts the mode far outside the 3% budget and confirms the phase rule:
opt-in, the human decides with part 2's rows. The cheaper variant, for
part 2 to price next to it: no retag on free, `irg` and sweep at pop
only. Its sweep touches lines the program is about to write anyway (a
new block is written before it is read), so it moves the line fills
rather than adding them; what it gives up is the fault on a read through
a freed block *before* the slot is reused (the tag still matches until
the next pop), while the double free after reuse and the forged pointer
are still caught. The plan's acceptance test "a read through a freed
pointer faults" needs the retag, so the retag is the design and the
variant is a measurement.

Consumer contract. A consumer must store nothing in the top byte of a
pointer it got from the allocator and must pass pointers back unchanged.
Valkey and Ruby store nothing there (Ruby's immediates live in the low
bits of a `VALUE`; Valkey's `robj`, `kvobj`, `dict`/`hashtable`, `sds`
and `listpack` keep pointers whole; both compare pointers only for
equality). A consumer that packs data into pointer top bits, or sorts
pointers by address across allocations, must not enable the mode. Every
tagged pointer is accepted by the kernel's syscalls (`PR_TAGGED_ADDR_ENABLE`
makes `read`, `write`, `sendmsg`, `io_uring` registration and the rest
untag their user pointers), except the memory-management calls
(`mmap`, `mprotect`, `madvise`, `munmap`, `mremap`, `mlock`), which take
untagged addresses only; those are never called on a user block by a
sane program, and challoc itself computes their arguments from region
bases. glibc's aarch64 string functions are MTE-safe by construction
(16-byte-aligned loads never cross into the next granule past a
terminator), and glibc 2.44 removed only its own heap tagging, not that
property.

### 18.3 LAM, UAI and TBI: the software tag

Where the hardware ignores the top bits of an address but does not check
them, the allocator can still carry a tag there and verify it itself at
the points where the pointer comes back. Nothing checks a load or a
store; `free`, `realloc`, `malloc_usable_size` and the sized path do.

Bits. LAM_U57 ignores bits 62:57 (six bits; Linux does not implement
LAM_U48, and bit 63 stays 0 for a user address). TBI ignores bits 63:56
(eight); the design uses the same six, 62:57, on both. UAI ignores 63:57
and would use the same six when Linux gets an enable path.

Enable. x86_64: `arch_prctl(ARCH_ENABLE_TAGGED_ADDR, 6)` (`0x4002`;
`ARCH_GET_MAX_TAG_BITS` `0x4003` first, `ARCH_GET_UNTAG_MASK` `0x4001` to
confirm) in init step 3 (section 10), after the configuration and before
any reservation. The kernel refuses it with `EBUSY` in a process that
already has more than one thread, `EINVAL` without LAM or on a kernel
before 6.4; either turns the mode off with a reported reason (`abort_conf`
turns a refused explicit `lam` into an abort; `auto` never reports). Init
is lazy on the first `malloc`, which ld.so issues before `main`, so the
process is single-threaded there in every consumer we have. aarch64:
`prctl(PR_SET_TAGGED_ADDR_CTRL, PR_TAGGED_ADDR_ENABLE)` per thread as in
18.2, without the `TCF` bits.

The tag. Six bits: `tag = gen(slot) ^ key[class]`, where `gen(slot)` is a
per-slot generation byte in a *tag map* next to the class's slot bitmap
in the metadata region (one byte per slot, `nslots` bytes per slab,
committed with the slab's bitmap page; 6.25% of the 16-byte class's
memory, 0.1% of the 1 KiB class's, priced as metadata, and eight times
the bitmap's footprint, so a 550 MiB Valkey heap of 1 KiB values carries
a 550 KB map), and `key[class]` six bits per class from the init
`getrandom`. The class is already exact from the address (section 2), so
the class key's job is the cross-class substitution: a pointer of class A
relocated into class B's region classifies as B and is checked against
`gen ^ key[B]`, which it matches with probability 1/64 rather than 1
(the generations of two young slots are both 0). A `Fresh` slab's map is
zero from the kernel; the map is not purged with the slab, so a
generation keeps counting across a purge and a stale pointer from before
it still fails.

Where the tag is read and written, mirroring 18.2: `free` (both paths)
loads the slot's generation byte after the checks of step 2, compares
`gen ^ key[c]` with the pointer's six bits, aborts on a mismatch (`free
of <p>: pointer tag 0x2a, expected 0x13 (class c, slab s, slot k): stale
or forged pointer`), stores `gen + 1` (a relaxed byte store: only the
slot's owner frees it, and two owners is the double free the compare is
there to catch) and pushes `p` retagged with the new generation. A refill
reads each `Active` slot's byte in `finish`'s loop and tags the pointer;
a zero pick's slots have generation 0 and get `0 ^ key[c]` without a
load. The pop is untouched. `realloc`, `malloc_usable_size`, `sallocx`
verify and abort as `free` does; `mallctl` lookups classify the stripped
address. The sized path verifies at the call too: under this key it
gives up the "no metadata load per sized free" property of 6.3 for the
one byte load and store, since the key exists to check pointers; the bit
test stays deferred to the flush.

What is caught: a free, realloc or size query through a stale pointer
(the slot was freed and the generation moved on: caught even when the
slot is live again, which the bit test misses), a forged pointer (63/64),
a pointer of one class presented as another (63/64), and a consumer
that clears the top bits before passing a pointer back (a tag of 0 is
right only for the one generation equal to `key[c]`; making the
increment skip that value removes the exception, and part 2 should).
What is not caught: a read or
write through a stale pointer (the hardware ignores the bits, so the
access lands on the current owner's data); a double free whose first copy
is still in the cache (the generation has not moved; `double_free_scan`
covers the top four entries, as today).

Cost, expected: one byte load and store on the line of the tag map per
free and one load per refilled slot, no sweep. The map is dense and eight
times the bitmap, so on Valkey's 550 MiB heap it is a second beyond-L2
line per free next to the bitmap word (section 4 measured the bitmap
word's own miss). Part 2 measures it on the C4A under TBI, which is the
same code as LAM with a different mask.

### 18.4 Stripping the tag before classification

Every entry point that takes a pointer classifies it first (section 2:
`c = (p - superblock) >> lg_stride`). With tags in the top bits that
subtraction is wrong, so the address is stripped first: `a = p &
addr_mask`, where `addr_mask` is a word on the `Shared` line that step 1
already loads (`superblock`, `lg_stride`, `push_flags`; section 6.2):
`!0` with tagging off, `!(0xff << 56)` under MTE and TBI, `!(0x3f << 57)`
under LAM. One load from a hot line and one `and`, no branch, the same
instructions with the key off, so the default configuration's cost is
one ALU operation per `free`-family call (part 2 records it on
`quick-perf`, as CLAUDE.md requires for any fast-path change). The
pointer's tag is `p >> 56 & 0xf` (MTE) or `p >> 57 & 0x3f` (software),
taken from `p` after the strip. Paths that strip: `free`, the sized path
(step 1, before `rel = p - class_base[c_hint]`), `malloc_usable_size`,
`sallocx`, `realloc`/`rallocx`/`xallocx`, `dallocx`, the `mallctl`
pointer lookups and `je_get_defrag_hint`, and `Heap::flush` (the cache
holds tagged pointers; the bucket pass groups by slab from the stripped
address and `release_owned` receives stripped pointers, so `class/`
never sees a tag). `double_free_scan` compares the tagged values: a
duplicate carries the same tag.

### 18.5 The `tag_pointers` key, `opt.` and the report

| Key | Default | Values |
|---|---|---|
| `tag_pointers` | `off` | `off`; `mte` (requires `mte=yes` in the probe, else a configuration issue and `off`); `lam` (the software tag: LAM on x86_64 when `lam=yes` and the `arch_prctl` succeeds, TBI on aarch64 always; refused, a configuration issue and `off`); `auto` (the strongest the host has, `mte` then `lam`, else `off`, never reported) |
| `tag_check` | `async` | `async`, `asymm` (needs `mte3=yes`), `sync`; MTE only, ignored otherwise |

`opt.tag_pointers` and `opt.tag_check` read back the configured strings
(as `opt.thp` does). The effective mode is a `malloc_stats_print` line,
`Tagged pointers: mte (check async; PROT_MTE regions)`, `Tagged pointers:
lam (6 bits at 57, arch_prctl ok)`, `Tagged pointers: tbi (6 bits at
57)`, or `Tagged pointers: off (auto: mte=no lam=no)` / `off (mte
requested, mte=no)`, with a matching `"tag_pointers": {"mode", "check",
"reason"}` object in the JSON form. `challoc_features()` is unchanged:
it reports hardware, and the effective mode is a decision; it is built
in init step 1 before the configuration is parsed. The abort messages
of 18.2 and 18.3 are the third surface.

### 18.6 Part 2: tests, cost rows, and what it waits for

Tests part 2 must have (plan task 6): negative modes in
`tests/c/negative.c`, run under the key: `mte_read_after_free` (a read
through a freed pointer faults with `SEGV_MTESERR` under
`tag_check:sync`, and the process dies of `SIGSEGV` before its next
syscall returns under `async`); `stale_tag_free_aborts` under both modes
(`free(p)`, `q = malloc` of the same class until `q`'s stripped address
equals `p`'s, `free(p)` again: the abort of 18.2 or 18.3, the case the
bit test cannot catch); `forged_untagged_free_aborts` (a slot address
with a zero top byte); `cross_class_free_aborts` (a class-A pointer with
class-B tag bits, software mode); `make fuzz` with
`MALLOC_CONF=tag_pointers:auto`; `make test` (the glibc and mimalloc
suites), the calloc-zero campaign's `check_zero` configuration, and the
Valkey and Ruby suites under the key; a host test of the encode, decode
and strip arithmetic and of the mask table. Cost rows, in the tunables
files next to `zero_on_free`'s columns and with its protocol (base, off,
mode, `zero_on_free`, jemalloc; five rounds; same run): `valkey1`,
`larson1`, `larson10`, `redis`, the Ruby rows of `make ruby-bench`, and
for MTE the fork row (fork time and write-phase p99 with a `BGSAVE`
child alive). Then a `quick-perf` attribution of the strip's `and` in
the default configuration on both reference VMs, since it is the one
instruction the default pays.

What part 2 waits for, cheapest path first:

1. Nothing, for the software tag on aarch64: `tag_pointers:lam` is TBI
   on the reference C4A (kernel 7.0), and the arm64 cost rows and the
   `stale_tag_free_aborts`, forged and cross-class tests run there
   today. It is the same code as LAM with a different mask, so the x86_64
   half is one mask and one `arch_prctl` away once a host has LAM.
2. QEMU TCG for MTE and LAM correctness on any host (the Mac container
   or a GCE VM): `qemu-system-aarch64 -M virt,mte=on -cpu max` boots an
   Ubuntu arm64 kernel with `CONFIG_ARM64_MTE` and exposes `mte`;
   `qemu-system-x86_64 -cpu max` has LAM since QEMU 8.1 with a 6.4+
   guest. The negative tests and the suites run there; no numbers (TCG is
   20..50x slower and has no PMU).
3. MTE numbers: an AmpereOne host. Ampere's own documentation confirms
   MTE reaches Linux user space on bare metal; the cloud shapes built on
   AmpereOne (Oracle's A2 family) have not been checked for `mte` in a
   guest and a bare-metal shape would show the real `HWCAP2`. Graviton4
   and Cobalt 100 are a `/proc/cpuinfo` away each; expect no, since the
   same Neoverse V2 core hides it on Axion.
4. LAM numbers: a Granite Rapids or Sierra Forest host with kernel 6.4+,
   bare metal or a guest whose hypervisor passes `lam` (KVM 6.8+); no
   GCE shape does. UAI: nothing to do until Linux has an enable path.

## 19. Source layout

```
src/lib.rs            crate root, quantum feature selection, panic handler
src/sys/mod.rs        the kernel constants; typed syscall wrappers (mmap, mprotect, madvise, process_madvise, mseal, prctl, getrandom, futex, clock_gettime, open/read/close, write, ppoll, eventfd, signals, sched_yield, rseq_slice_yield); read_small_file, parse_decimal, yn
src/sys/abort.rs      abort (`challoc: <msg>` on stderr, SIGABRT the way glibc delivers it), the stderr sink, the `abort!` macro
src/sys/auxv.rs       getauxval and the AT_* keys (AT_PAGESZ, AT_HWCAP, AT_SECURE)
src/sys/raw.rs        the asm syscall instructions (x86_64, aarch64)
src/sys/nr.rs         syscall numbers, header-checked (tests/c/sysnr.c)
src/sys/errno.rs      kernel error decoding
src/sys/features.rs   kernel feature probing; the feature line as a key table (words) that Display and the tests iterate
src/sys/cgroup.rs     cgroup2 discovery, limit walk, sampling (memory.current, every memory.events counter), PSI trigger arming
src/sys/pac.rs        pointer authentication of the TLS heap pointer (arm64 FEAT_PAuth)
src/tls.rs            initial-exec TLS accessor
src/rseq.rs           restartable sequences: the rseq probe (__rseq_offset/size/flags, mm_cid; also the feature line's rseq= words) and the per-CPU cache critical sections (asm, both architectures, one descriptor and abort-handler macro)
src/slice.rs          the rseq time-slice extension for lock holders (`slice_ext`)
src/lock.rs           futex lock, padded lock
src/layout/mod.rs     size classes (Class, CLASSES, class_of), address-space parameters
src/layout/regions.rs Regions: the reservations, pointer classification (slab_slot, slot_in_class), seal and release
src/layout/tests.rs, src/layout/linux_tests.rs   layout tests
src/bitmap.rs         atomic bitmap and TwoLevelBitmap
src/slab.rs           SlabDesc (the descriptor only)
src/class/mod.rs      ClassState: claim/plan/take, release/reopen, the stats accessors; Pick, Claim, Inner (with the set/count helpers); xorshift, shuffle; the test-only `protocol` step enums the model matches
src/class/table.rs    SlabTable: descriptors, slot bitmap, owner array, guard slabs, group hand-out (take_group) and slab commit (commit_slab, commit_page_group, install_guard)
src/class/sets.rs     Lazy (a lazily committed metadata table), Set, SetTable (one arena's interleaved slab sets)
src/class/handoff.rs  next_slab, next_fresh_slab, the idle-group handoff between arenas (verify_idle_group, move_group; section 13.3)
src/class/decay.rs    purge_empty, purge_curve (walk_empty), decay (section 8)
src/class/prefault.rs the prefault pass's commit-ahead (prefault_next, prefault_ahead)
src/class/verify.rs   the check_zero debug mode and the test-only pointer checks
src/class/abort.rs    the #[cold] heap-inconsistency aborts of the class protocol
src/class/tests.rs    the class tests over a synthetic region (Env)
src/heap/mod.rs       CacheHeader, Heap: thread-cache fast and slow paths, refill, flush, compaction, cache GC; the crate-facing alloc/free entry functions
src/heap/shared.rs    Shared: process-wide state, init, the free checks and routing (owner lookup never materialises an arena), GuardTest, the thread-exit key
src/heap/percpu.rs    the per-CPU mode (section 15): Percpu, CpuCache, the per-CPU Heap paths and push wrappers
src/heap/thread.rs    thread lifecycle (6.5): TLS sentinels, heap_for_thread/existing_heap, the exit destructor
src/heap/flush.rs     the flush's grouping pass (group_by_slab, flag_sort) and the straddling double-free abort
src/heap/mallocx.rs   MALLOCX_* flag decoding, explicit tcache ids, the mallocx/dallocx routing (re-exported by arena.rs)
src/heap/tests.rs, src/heap/linux_tests.rs   heap tests; tests.rs holds the Shared/TestHeap builders stats.rs and api/ctests.rs reuse
src/huge/mod.rs       huge allocator: extents (add_extent/drop_extent/retire), guards, first fit, decay, the audit
src/huge/table.rs     the open-addressing descriptor table (page number to HugeDesc); never fills its last slot
src/huge/tests.rs, src/huge/linux_tests.rs  huge tests: host, and the ones that need a kernel
src/meta.rs           metadata region allocator
src/model/            test-only: the exhaustive interleaving model of the slab protocol (mod, state, steps, percpu, scenarios; section 17's row); steps.rs matches class::protocol exhaustively
src/purge.rs          decay driver, batching and advice, Decayer; root of purge/
src/purge/curve.rs    the decay period and curve (NEVER, STEPS, WEIGHTS, Curve)
src/purge/background.rs, src/purge/background_host.rs   the background thread (section 8.1) and the host stub with the same names, selected by cfg + #[path]
src/purge/prefault.rs the prefault pass (section 8.2)
src/purge/tests.rs, src/purge/linux_tests.rs   purge tests; purge::tests::serial
src/policy.rs         the cgroup policy table (section 11): bands, retention cap, full purge, Advice (name, madv)
src/arena.rs          arenas, explicit caches; re-exports heap::mallocx
src/config.rs         malloc_conf parsing: one key table (name, parse, read) behind set/get/KEYS
src/stats.rs          counters and epoch merge
src/thp.rs            THP: the `thp` key against the sysfs table, reservation advice, metadata folios, MADV_HUGEPAGE on huge runs, forced_by_limit
src/api/mod.rs        lazy init, Shared/Features statics, challoc_version/challoc_features, fork handlers, export_pair!
src/api/libc.rs       standard entry points, malloc_trim
src/api/glibc.rs      mallopt, mallinfo, mallinfo2
src/api/jemalloc.rs   *allocx, mallctl entry points, malloc_conf/malloc_message symbols, je_ aliases
src/api/cxx.rs        operator new / delete
src/api/mallctl.rs    control namespace: the name tree carries each read-only key's reader; ctl() keeps the keys that do something
src/api/print.rs      malloc_stats_print, malloc_stats, malloc_info
src/api/ctests.rs     the C-test table and its driver: tests/c, fuzz/, tests/glibc, tests/mimalloc (test builds only)
tests/c/              C programs run under LD_PRELOAD against the cdylib (common.h is the shared harness; scripts/ctest-cc.sh the one compiler line)
tests/glibc/, tests/mimalloc/  vendored suites
bench/                benchmark drivers, GCE scripts, results, probes
fuzz/                 randomized alloc/free driver with seeds
contrib/              Valkey build patch, the jemalloc-compat header, the Wolfi package definitions
scripts/              the dev scripts; codegen-diff.sh proves a refactor left the machine code unchanged
docs/                 design, benchmarking, research, tuning, testing
```

Pure logic (`layout`, `bitmap`, size-class math, config parsing, and the
cgroup file parsing and limit walk) must be testable with `cargo test` on the host under
a `std` dev feature. Everything touching syscalls, TLS, or asm is tested inside a Linux
container.

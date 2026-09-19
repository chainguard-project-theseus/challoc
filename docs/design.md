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
the region grows. Descriptor layout, a fixed 16-byte header:

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

Each class has one process-wide `SlabTable` (`class.rs`): the class's
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
tier, one commit-lock hold and one shared-line write per slab transition
(PROGRESS, task 15). Committing means
`mprotect` of the slab's bytes (or of its page group, for slabs smaller than
a page) to read-write. Fresh slabs are zero from the kernel.

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

### 6.2 Heap structure

```
struct Heap {                                // repr(C); the header array at offset 128
    shared:      *const Shared,
    tid:         u32,
    arena:       u16,                        // home arena: plain malloc allocates here
    slow_ticks:  u32,                        // counts slow-path entries for periodic work
    last_gc:     u64,                        // monotonic ns of the last cache GC
    uncached:    ThreadStats,                // bytes that never went through a cache (huge, direct)
    others:      *mut ArenaCaches,           // one node of [CacheHeader; NCLASSES] per other arena touched
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
stack arithmetic (`heap.rs`, `Heap::stats`). An explicit cache
(`tcache.create`) is a `Heap` owned by no thread.

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

### 6.3 Fast paths

`malloc(n)`:
1. `c = class_of(n)`; if huge, slow path.
2. `heap = tls()`; if null or a sentinel, slow path (create heap, may init
   the allocator).
3. `cache = heap.caches[c]` (the home arena's header line); if `len == 0`,
   slow path (refill).
4. `p = slots[--len]`; return `p`.

`free(p)`:
1. If `p` is null return. Classify `p` (section 2). Huge: slow path. Invalid:
   abort.
2. Compute slab and slot; verify `slot * size == offset`; load the class
   bitmap word; verify bit set. Otherwise abort.
3. The opt-in hardening, one byte (`Shared::push_flags`) on the line step
   2's `superblock`/`lg_stride` come from, zero by default, so the default
   push tests it once and branches past both. With `double_free_scan`:
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
   pushes into the heap's cache for that arena, creating the node on first
   use); otherwise `cache = heap.caches[c]`; if `len == cap`, slow path
   (flush half). `slots[len++] = p`.

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

- A `TwoLevelBitmap` over pages tracks free pages; allocation is lowest-address
  first fit of `ceil(n / page) + guard_pages` contiguous pages, under the huge
  lock.
- An open-addressing hash table keyed by page number holds `HugeDesc {
  start, pages, usable, arena, state, dirty_at, prev, next }`: a run is keyed
  by its start page, a free extent by both its first and its last page so
  that a free finds both neighbours and free extents are always coalesced;
  `free` of a pointer inside the huge region that is not a run start aborts.
- Runs are followed by a guard: `MADV_GUARD_INSTALL` on one page when
  available, `PROT_NONE` (the reserved default) otherwise. Since the region is
  reserved `PROT_NONE`, the fallback costs no memory, only VMAs: the
  `PROT_NONE` page between two committed runs keeps them separate VMAs, so
  the fallback costs one VMA per live huge run (bounded by the number of
  live huge allocations, unlike a `PROT_NONE` guard slab per eight slabs,
  which is why slabs have no such fallback; Phase 2 task 1). The guard
  install variant exists so that a later commit of the neighbouring run
  does not silently remove the guard.
- Alignment requests above page size are satisfied by over-allocating and
  releasing the head and tail pages back to the bitmap.
- Freed runs become `Empty` and decay like slabs. Runs whose usable size is
  above `huge_retain_max` (a `malloc_conf` key, default 64 MiB, readable as
  `opt.huge_retain_max`) are purged immediately.
- Planned (with the THP work): optional `MADV_HUGEPAGE` on runs of 2 MiB or
  more when the `thp` tunable is `always`. Today the `thp` key is accepted
  and inert and nothing issues `MADV_HUGEPAGE`. The default will stay
  `never`, because fork-based persistence in Redis and Valkey suffers from
  copy-on-write amplification with huge pages.

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
  does not. `-1` is never.
- `muzzy_decay_ms` is accepted and ignored. challoc does not use `MADV_FREE`
  by default: lazily freed pages remain charged to the cgroup and visible in
  RSS until reclaim, which misleads both autoscalers and Redis's fragmentation
  ratio. A `lazy_free` tunable enables `MADV_FREE` for callers who want it.
- Purges are batched: collect up to 64 ranges and issue one `process_madvise`
  with `PIDFD_SELF` (kernel 6.14) or a pidfd of self (5.10), falling back to
  a loop of `madvise`.
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
   kept), `PR_SET_VMA_ANON_NAME` (`sys::features`). Then the CPU report
   (`sys::cpu`, section 17): `AT_HWCAP`/`AT_HWCAP2` and `CTR_EL0` on arm64,
   `cpuid` and a hinted `mmap` above 2^47 on x86_64, the sysfs cache files
   where the registers do not give the sizes. The whole probe is formatted
   once into the static feature line that `challoc_features()` returns.
2. Parse configuration (section 12). This happens before anything is
   reserved so that `abort_conf` aborts with nothing mapped; it reads only
   the weak symbols, one file and `environ`.
3. Configure the purge backend from the probe and `lazy_free`, `check_zero`,
   apply `group_handoff`, and record a `background_thread` request (the thread itself is created
   later, 8.1).
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

Planned, not yet performed:

- Probing `MADV_COLLAPSE` and reading `/sys/kernel/mm/transparent_hugepage/
  enabled` (with the THP work; the `thp` key is inert).

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
measured (`PROGRESS.md`, Phase 2 task 4): Docker's default profile mounts
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

| Usage vs effective limit          | Behaviour                                                  |
|-----------------------------------|------------------------------------------------------------|
| No limit                          | Defaults; nothing beyond the sample is done                |
| < 70% of `min(high, max)`         | Defaults, retention capped at 5% of the limit              |
| 70% .. 90%                        | `dirty_decay_ms` scaled down linearly to 1000; the cap     |
| >= 90%                            | Period 0: purge at the next pass; cache GC every tick      |
| `memory.events` `high` increased  | Full purge once, at the next decay pass                    |
| PSI trigger fired                 | Sample and decay pass at once (the background thread)      |

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
cannot set; what `tests/c/cgroup_policy.c high` found (PROGRESS.md, Phase
2 task 5): the container's own cgroup cannot enable the memory controller
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

THP: `MADV_HUGEPAGE` is not wired yet (section 10, the `thp` key is
inert), so "disabled under small limits" is, for now, `thp` forced to
`never` in the stored configuration at init when the effective limit at
init is under 1 GiB; `opt.thp` reads `never`, the report says why, and
whatever wires THP later reads the stored setting.

Statistics through `mallctl` (13.4): `stats.cgroup.limit`,
`stats.cgroup.high` (both `SIZE_MAX` for unlimited), `stats.cgroup.current`
(the last sample, refreshed by the read if the window allows),
`stats.cgroup.pressure_events`, and the policy's `stats.cgroup.decay_ms`
(arena 0's effective period, `SIZE_MAX` for never),
`stats.cgroup.retention_cap` (bytes, `SIZE_MAX` without a limit),
`stats.cgroup.empty_bytes` (the retention, summed by the read) and
`stats.cgroup.full_purges`; all `size_t` and read-only; `ENOENT` when
nothing is tracked, like jemalloc's keys behind a disabled build option.
`malloc_stats_print` prints one `cgroup:` line and one `cgroup policy:`
line (section 16).

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
| `group_handoff`           | false   | challoc only; opt-in: an arena takes another arena's idle slab group before a fresh one (5, 13.3); off because it did not move the mstress row it was built for |
| `thp`                     | never   | `never`, `always`; inert (planned)      |
| `lazy_free`               | false   | use `MADV_FREE` for purge               |
| `guards`                  | true    | guard slabs and huge runs               |
| `guard_slab_interval`     | 8       | one guard per N x 64 KiB of slabs, N a power of two (else rounded up, reported); 0 off, 1 out of range (clamps to 2) |
| `huge_retain_max`         | 67108864 | challoc only; bytes; freed huge runs above it are purged at once (7) |
| `zero_on_free`            | false   | opt-in: clear a block on free (6.3 step 3); priced in `bench/results/2026-09-18-*-gce-tunables.md` |
| `slot_randomize`          | false   | opt-in: refills claim from a random word and shuffle the batch (4); fights 6.4's compaction |
| `double_free_scan`        | false   | compare against the top 4 cache entries |
| `abort_conf`              | false   | abort on unknown keys, bad values and inert keys |
| `cgroup`                  | true    | read the memory cgroup (11) and apply its policy: limits, `memory.current`, events, PSI; `false` reads and applies nothing |
| `check_zero`              | false   | challoc only; debug: read every slot/page handed out as zero, abort on a dirty word |
| `stats_print`             | accepted | inert: nothing prints at exit; no `opt.` value |
| `junk`, `zero`, `retain`, `oversize_threshold`, `percpu_arena`, `metadata_thp`, `confirm_conf` | accepted | jemalloc keys, ignored with any value |

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
new one. Slab groups record their arena (section 4). A pointer freed with
plain `free` is returned to a cache keyed by its slab's arena, so a live
slot never migrates between arenas and per-arena statistics stay exact.
This is what lets Redis exclude its Lua heap from fragmentation accounting.
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
thread.tcache.flush, thread.tcache.enabled, thread.allocated, thread.allocatedp,
thread.deallocated, thread.deallocatedp, thread.arena
tcache.create, tcache.flush, tcache.destroy
experimental.utilization.query, experimental.utilization.batch_query
config.malloc_conf, config.debug, config.stats
prof.* -> ENOENT
stats.cgroup.limit, stats.cgroup.high, stats.cgroup.current, stats.cgroup.pressure_events,
stats.cgroup.decay_ms, stats.cgroup.retention_cap, stats.cgroup.empty_bytes, stats.cgroup.full_purges
  (section 11; ENOENT when no cgroup2 is tracked or cgroup:false)
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
Redis's patched bundled jemalloc. They are out of scope unless a Wolfi Redis
package appears; Valkey uses the standard `experimental.utilization` keys.

### 13.6 Library name, soname and packaging

`cargo build --release` writes `libchalloc.so` and `libchalloc.a`. The
shared library's soname is `libchalloc.so.0` (the ABI major; the crate
version is 0.x), so a program linked with `-lchalloc` records
`libchalloc.so.0` in `DT_NEEDED` and a package installs the file under that
name with a `libchalloc.so` link for the linker. Cargo cannot name the
output file after the soname, so a build directory used as a link and run
path (`contrib/valkey/build.sh`) puts the `libchalloc.so.0` link beside
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
binary's `RUNPATH`, or linked statically from `libchalloc.a`. Two
libraries with one soname cannot share `/usr/lib`, so the system library
is never `quantum8`. This is a packaging rule for Phase 3; it does not
change the builds this repository tests (`quantum16` by default,
`quantum8` for the Valkey tree).

`challoc_version()` returns the crate version as a NUL-terminated static
string and is the one challoc-specific export (section 16); there is no
exported accessor for the thread heap slot.

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
  and one `cgroup:` line (section 11): the directory, mount and path, the
  effective `limit` and `high` (`max` for unlimited), the last `current`
  sample, the `memory.events` `high`/`max` counters with the number of
  rises seen, the PSI state (`not armed (no background thread)`, `armed
  (some ...)`, `unavailable (EROFS)`, `disarmed (background thread gone)`)
  with `pressure_events`, and the CPU figures; or `cgroup: not tracked
  (<reason>)`; and one `cgroup policy:` line with the band as last
  evaluated, arena 0's effective `decay_ms`, the `retention_cap`, the
  `empty_bytes` held, the `full_purges` run and whether `thp` was forced
  to `never`. The JSON form has a matching `"cgroup"` object with the
  policy fields.
  The run-time options list marks the inert keys "accepted, no effect". The per-class table also carries the compaction
  diagnostics of section 6.4 (`refilled`, `compacted`, caches that entered
  and left compact mode, probes the refill-rate gate declined) and is
  followed by a per-class occupancy histogram of the non-empty slabs
  (slabs per eighth of `nslots`), so a soak's `MEMORY MALLOC-STATS` shows
  where the live data sits and whether the schedule is running.
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
  process_madvise=pidfd_self anon_name=yes`) then the CPU (section 17;
  `mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes` on arm64,
  `lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes
  fsrm=yes avx512f=yes` on x86_64, then `cacheline=64 l1d=64K l2=2M` on
  both, `unknown` where no source had a figure). The string is static,
  built once at init without allocating (rebuilt in a fork child, whose
  purge backend is re-probed), and byte-identical to the `Kernel and CPU
  features:` line of `malloc_stats_print`. Calling it initialises the
  allocator. Values are `yes`/`no` for bits, so a script can `grep -q
  'mte=yes'`; keys are only ever added, never renamed.

## 17. Floor and ceiling

The rule is stated in section 1. This section is the inventory: every
feature the allocator probes, who provides it, what the allocator turns on
with it today, and what it does without it. "Reported only" means the probe
and its line in the feature report exist and no code path reads the result
yet; the task that will is named. Nothing in this table is decided at build
time: the compiler baseline stays where Wolfi sets it and the same binary
runs on every row.

| Feature | Provided by | challoc turns on | Fallback |
|---|---|---|---|
| Page size (`AT_PAGESZ`) | kernel, the auxiliary vector | the granularity of every mapping, slab and purge (4, 16 or 64 KiB) | none: init aborts without it (`/proc/self/auxv` is tried first) |
| `MADV_GUARD_INSTALL` | kernel 6.13 | guard slabs between slabs and after huge runs (section 14) | a `PROT_NONE` page after huge runs only; no guard slabs |
| `process_madvise` on self | kernel 6.13 with a pidfd of self (the syscall exists since 5.10 but accepts `MADV_DONTNEED` only from 6.13; the matrix of Phase 2.5 task 3 found it unavailable on 5.10, 6.1 and 6.8 even without seccomp, lit on 7.0); 6.14 with `PIDFD_SELF` | batched purge, one call per decay pass (section 8) | a loop of `madvise(MADV_DONTNEED)` |
| `mseal` | kernel 6.10, not filtered by seccomp | seals the never-committed ranges of the reservations at init (section 10 step 5) | skip; `Sealed ranges: 0 of N` |
| `PR_SET_VMA_ANON_NAME` | kernel 5.17 with `CONFIG_ANON_VMA_NAME` | named `challoc:*` mappings in `/proc/pid/maps` | skip |
| PSI triggers on `memory.pressure` | kernel 6.4 (unprivileged), cgroup2 mounted writable | the background thread arms a trigger and reacts to pressure at once (section 11) | sample `memory.current` on the tick |
| rseq `mm_cid` | kernel 6.3, glibc's registered rseq area | nothing yet: the per-CPU mode (section 15, plan Phase 2.5 task 8) | thread caches |
| Transparent huge pages | any kernel with THP | nothing: the `thp` key is accepted and inert (plan Phase 2.5 task 9) | off |
| MTE (`HWCAP2_MTE`, `HWCAP2_MTE3`) | arm64 CPU (Armv8.5) and a kernel with `CONFIG_ARM64_MTE` that exposes it | reported only: tagged pointers (plan Phase 2.5 tasks 5 and 6) | untagged pointers |
| PAC (`HWCAP_PACA` and `HWCAP_PACG`) | arm64 CPU (Armv8.3) and kernel | reported only: signed cache and heap pointers (task 7) | plain pointers |
| LSE (`HWCAP_ATOMICS`) | arm64 CPU (Armv8.1) | reported only; the atomics the crate emits follow the compiler baseline (Rust's outlined `__aarch64_*` helpers already pick LSE at run time) | LL/SC loops in the same helpers |
| LSE2 (`HWCAP_USCAT`) | arm64 CPU (Armv8.4) | reported only: single-copy 16-byte descriptor atomics (later task) | 64-bit atomics |
| BTI (`HWCAP2_BTI`), SVE (`HWCAP_SVE`) | arm64 CPU and kernel | reported only (BTI landing pads are the compiler baseline's decision) | none needed |
| LAM (`cpuid` 7.1 `EAX[26]`) | x86_64 CPU and kernel 6.4 (`arch_prctl(ARCH_ENABLE_TAGGED_ADDR)`) | reported only: tagged pointers (tasks 5 and 6) | untagged pointers |
| UAI (`cpuid` `0x80000021` `EAX[7]`, read only when leaf `0x80000000` reports an extended range that reaches it) | AMD CPU (Zen 4 and later: upper address ignore, the LAM analogue); no upstream kernel enables it for user space yet (no `arch_prctl`, no `/proc/cpuinfo` flag) | reported only: tagged pointers (tasks 5 and 6) | untagged pointers |
| LA57 (`cpuid` 7.0 `ECX[16]`), `la57_enabled` (a hinted `mmap` above 2^47 honoured) | x86_64 CPU; kernel with `CONFIG_X86_5LEVEL` booted on it | reported only: the address-space budget for reservations and tag bits (task 5) | 47-bit user addresses |
| CLDEMOTE (`cpuid` 7.0 `ECX[25]`), WAITPKG (`ECX[5]`) | x86_64 CPU (Sapphire Rapids and later; a hypervisor may hide WAITPKG) | reported only (task 12) | none needed |
| ERMS, FSRM, AVX-512F (`cpuid` 7.0 `EBX[9]`, `EDX[4]`, `EBX[16]`) | x86_64 CPU | reported only: `memcpy`/`memset` selection is glibc's; the crate's fills use the compiler baseline | none needed |
| Cache line, L1D and L2 sizes | `CTR_EL0` (arm64), `cpuid` leaf 4 or `0x8000001D` (x86_64), else `/sys/devices/system/cpu/cpu0/cache/index*/` | reported only: the layout constants (`lock.rs` padding, the 512-byte stride rule) stay fixed | `unknown` in the line; nothing changes |

The feature report's arm64 keys are `mte mte3 pac lse lse2 bti sve`, the
x86_64 keys `lam uai la57 la57_enabled cldemote waitpkg erms fsrm avx512f`, and
`cacheline l1d l2` on both, after the kernel keys of section 10. The
container test checks each CPU bit against `/proc/cpuinfo` (`Features` or
`flags`) on the host it runs on; the host tests decode captured HWCAP words
and `cpuid` registers from the reference machines. The reference hosts'
lines are recorded in `bench/results/2026-09-19-feature-report.md`; the
matrix of plan tasks 3 and 4 adds one line per kernel and CPU.

## 18. Source layout

```
src/lib.rs            crate root, quantum feature selection, panic handler
src/sys/mod.rs        typed syscall wrappers (mmap, mprotect, madvise, process_madvise, mseal, prctl, getrandom, futex, clock_gettime, open/read/close, write, ppoll, eventfd, signals), abort, stderr sink
src/sys/raw.rs        the asm syscall instructions (x86_64, aarch64)
src/sys/nr.rs         syscall numbers, header-checked
src/sys/errno.rs      kernel error decoding
src/sys/features.rs   kernel feature probing, the feature line
src/sys/cpu.rs        CPU feature report (HWCAP, cpuid, CTR_EL0, sysfs cache sizes)
src/sys/cgroup.rs     cgroup2 discovery, limit walk, sampling, PSI trigger arming
src/tls.rs            initial-exec TLS accessor
src/lock.rs           futex lock, padded lock
src/layout.rs         size classes, regions, pointer classification
src/bitmap.rs         atomic bitmap and TwoLevelBitmap
src/slab.rs           SlabDesc (the descriptor only)
src/class.rs          SlabTable (descriptors, slot bitmap, owner array, guard slabs), SetTable, ClassState: claim/release, slab selection, decay
src/heap.rs           Heap, CacheHeader, Shared, fast and slow paths, cache GC, compaction
src/huge.rs           huge allocator
src/meta.rs           metadata region allocator
src/purge.rs          decay driver, batching, background thread
src/policy.rs         the cgroup policy table (section 11): bands, retention cap, full purge
src/arena.rs          arenas, explicit caches, MALLOCX_* routing
src/config.rs         malloc_conf parsing
src/stats.rs          counters and epoch merge
src/api/mod.rs        lazy init, Shared/Features statics, challoc_version/challoc_features, fork handlers
src/api/libc.rs       standard entry points, malloc_trim
src/api/glibc.rs      mallopt, mallinfo, mallinfo2
src/api/jemalloc.rs   *allocx, mallctl entry points, malloc_conf/malloc_message symbols, je_ aliases
src/api/cxx.rs        operator new / delete
src/api/mallctl.rs    control namespace
src/api/print.rs      malloc_stats_print, malloc_stats, malloc_info
src/api/ctests.rs     drivers for tests/c, fuzz/, tests/glibc, tests/mimalloc (test builds only)
tests/c/              C programs run under LD_PRELOAD against the cdylib
tests/glibc/, tests/mimalloc/  vendored suites
bench/                benchmark drivers, GCE scripts, results
fuzz/                 randomized alloc/free driver with seeds
contrib/              Valkey build patch
docs/
```

Pure logic (`layout`, `bitmap`, size-class math, config parsing, and the
cgroup file parsing and limit walk) must be testable with `cargo test` on the host under
a `std` dev feature. Everything touching syscalls, TLS, or asm is tested inside a Linux
container.
